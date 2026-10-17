//! DirectSound: a software-mixing device whose state evolves on guest time.
//!
//! Buffer play cursors are exact functions of virtual time (anchored at the last
//! Play/SetCurrentPosition/SetFrequency), so status, cursors and notifications are
//! identical whether audio is played on a host device, captured, or discarded. The mixer
//! renders the same timeline into the session's audio sink when one is attached.

use super::*;
use crate::{export, iunknown, unimpl_method};
use serde::{Deserialize, Serialize};

pub const DS_OK: u32 = 0;
pub const DSERR_ALLOCATED: u32 = 0x8878_000a;
pub const DSERR_CONTROLUNAVAIL: u32 = 0x8878_001e;
pub const DSERR_INVALIDPARAM: u32 = E_INVALIDARG;
pub const DSERR_INVALIDCALL: u32 = 0x8878_0032;
pub const DSERR_PRIOLEVELNEEDED: u32 = 0x8878_0046;
pub const DSERR_BADFORMAT: u32 = 0x8878_0064;
pub const DSERR_UNSUPPORTED: u32 = E_NOTIMPL;
pub const DSERR_ALREADYINITIALIZED: u32 = 0x8878_0082;
pub const DSERR_BUFFERLOST: u32 = 0x8878_0096;

const DSBCAPS_PRIMARYBUFFER: u32 = 0x1;
const DSBCAPS_STATIC: u32 = 0x2;
const DSBCAPS_LOCSOFTWARE: u32 = 0x8;
const DSBCAPS_CTRL3D: u32 = 0x10;
const DSBCAPS_CTRLFREQUENCY: u32 = 0x20;
const DSBCAPS_CTRLPAN: u32 = 0x40;
const DSBCAPS_CTRLVOLUME: u32 = 0x80;
const DSBCAPS_CTRLPOSITIONNOTIFY: u32 = 0x100;
const DSBCAPS_MUTE3DATMAXDISTANCE: u32 = 0x2_0000;

/// Mixer tick when an audio sink is attached.
const MIX_TICK_NS: u64 = 5_000_000;
/// Write-cursor lead over the play cursor.
const WRITE_LEAD_NS: u64 = 10_000_000;

#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Wfx {
    pub tag: u16,
    pub channels: u16,
    pub rate: u32,
    pub bits: u16,
    pub block: u16,
}

impl Wfx {
    fn valid(&self) -> bool {
        self.tag == 1 && (1..=2).contains(&self.channels) && (self.bits == 8 || self.bits == 16) && (100..=200_000).contains(&self.rate)
            && self.block == self.channels * self.bits / 8
    }
    fn bytes(&self) -> Vec<u8> {
        let mut b = Vec::with_capacity(18);
        b.extend(self.tag.to_le_bytes());
        b.extend(self.channels.to_le_bytes());
        b.extend(self.rate.to_le_bytes());
        b.extend((self.rate * self.block as u32).to_le_bytes());
        b.extend(self.block.to_le_bytes());
        b.extend(self.bits.to_le_bytes());
        b.extend(0u16.to_le_bytes());
        b
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Params3d {
    pub position: [f32; 3],
    pub velocity: [f32; 3],
    pub cone_inside: u32,
    pub cone_outside: u32,
    pub cone_orientation: [f32; 3],
    pub cone_outside_volume: i32,
    pub min_distance: f32,
    pub max_distance: f32,
    pub mode: u32,
}

impl Default for Params3d {
    fn default() -> Self {
        Params3d {
            position: [0.0; 3],
            velocity: [0.0; 3],
            cone_inside: 360,
            cone_outside: 360,
            cone_orientation: [0.0, 0.0, 1.0],
            cone_outside_volume: 0,
            min_distance: 1.0,
            max_distance: 1e9,
            mode: 0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Listener {
    pub position: [f32; 3],
    pub velocity: [f32; 3],
    pub front: [f32; 3],
    pub top: [f32; 3],
    pub distance_factor: f32,
    pub rolloff: f32,
    pub doppler: f32,
}

impl Default for Listener {
    fn default() -> Self {
        Listener {
            position: [0.0; 3],
            velocity: [0.0; 3],
            front: [0.0, 0.0, 1.0],
            top: [0.0, 1.0, 0.0],
            distance_factor: 1.0,
            rolloff: 1.0,
            doppler: 1.0,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DSoundGlobal {
    pub created: bool,
    pub coop: u32,
    pub primary_fmt: Wfx,
    pub speaker: u32,
    pub listener: Listener,
    pub pending_listener: Option<Listener>,
    /// Virtual time up to which audio has been mixed.
    pub mixed_until: u64,
    pub out_frac: u64,
    pub primary_volume: i32,
    pub frames_mixed: u64,
    /// Object ids of all sound buffers (kept explicitly so per-tick work does not scan
    /// every COM object).
    pub buffers: std::collections::BTreeSet<u32>,
    /// Subset of `buffers` that are currently playing.
    pub playing: std::collections::BTreeSet<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DSoundObj {}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DsBuffer {
    pub primary: bool,
    pub flags: u32,
    pub mem: u32,
    pub size: u32,
    pub fmt: Wfx,
    /// Object whose memory this duplicate shares (memory is freed with the last user).
    pub mem_owner: u32,
    pub playing: bool,
    pub looping: bool,
    pub anchor_ns: u64,
    pub anchor_frame: u64,
    pub freq: u32,
    pub volume: i32,
    pub pan: i32,
    pub notify: Vec<(u32, u32)>,
    /// Play position (bytes) at the last notification check.
    pub last_pos: u32,
    pub p3d: Params3d,
    pub pending_3d: Option<Params3d>,
}

impl DsBuffer {
    fn frames(&self) -> u64 {
        (self.size / self.fmt.block.max(1) as u32) as u64
    }
    /// Absolute (unwrapped) frame position at time `t`.
    fn frame_at(&self, t: u64) -> u64 {
        if !self.playing {
            return self.anchor_frame;
        }
        let dt = t.saturating_sub(self.anchor_ns) as u128;
        self.anchor_frame + (dt * self.freq as u128 / 1_000_000_000) as u64
    }
    /// Virtual time at which absolute frame `f` is reached.
    fn time_of_frame(&self, f: u64) -> u64 {
        if f <= self.anchor_frame {
            return self.anchor_ns;
        }
        self.anchor_ns + ((f - self.anchor_frame) as u128 * 1_000_000_000).div_ceil(self.freq.max(1) as u128) as u64
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DsCont {}

pub fn resume(_s: &mut Session, c: DsCont, _v: Resume) -> ApiResult {
    match c {}
}

impl Session {
    fn dsbuf(&self, id: u32) -> Option<&DsBuffer> {
        match &self.com_obj(id)?.kind {
            ComKind::DSBuffer(b) => Some(b),
            _ => None,
        }
    }
    fn dsbuf_mut(&mut self, id: u32) -> Option<&mut DsBuffer> {
        match &mut self.com_obj_mut(id)?.kind {
            ComKind::DSBuffer(b) => Some(b),
            _ => None,
        }
    }
    fn dsbuf_ids(&self) -> Vec<u32> {
        self.com.dsound.buffers.iter().copied().collect()
    }
    fn playing_ids(&self) -> Vec<u32> {
        self.com.dsound.playing.iter().copied().collect()
    }
}

/// Bring a buffer's state up to virtual time `now`: end one-shot playback, fire
/// notifications that the play cursor has passed.
fn settle(s: &mut Session, id: u32, now: u64) {
    let Some(b) = s.dsbuf(id).cloned() else {
        return;
    };
    if !b.playing || b.primary {
        return;
    }
    let frames = b.frames().max(1);
    let abs = b.frame_at(now);
    let mut fire = Vec::new();
    let mut stopped = false;
    let new_pos;
    if !b.looping && abs >= frames {
        stopped = true;
        new_pos = 0;
        // Offsets between the last position and the end, then the stop notification.
        for (off, ev) in &b.notify {
            if *off == 0xffff_ffff || (*off >= b.last_pos && *off < b.size) {
                fire.push(*ev);
            }
        }
    } else {
        let pos = ((abs % frames) * b.fmt.block as u64) as u32;
        let wrapped_times = (abs / frames).saturating_sub(b.anchor_frame / frames);
        for (off, ev) in &b.notify {
            if *off == 0xffff_ffff {
                continue;
            }
            let passed = if pos >= b.last_pos && wrapped_times == 0 {
                *off >= b.last_pos && *off < pos
            } else {
                *off >= b.last_pos || *off < pos
            };
            if passed {
                fire.push(*ev);
            }
        }
        new_pos = pos;
    }
    if let Some(bm) = s.dsbuf_mut(id) {
        if stopped {
            bm.playing = false;
            bm.anchor_frame = 0;
        } else {
            // Re-anchor to keep the unwrapped position bounded.
            let cur = bm.frame_at(now);
            bm.anchor_frame = cur % frames;
            bm.anchor_ns = now;
        }
        bm.last_pos = new_pos;
    }
    if stopped {
        s.com.dsound.playing.remove(&id);
    }
    for h in fire {
        if let Some(crate::objects::KObj::Event { signaled, .. }) = s.objects.by_handle_mut(h) {
            *signaled = true;
        }
    }
}

pub fn next_event(s: &Session) -> Option<u64> {
    let now = s.clock.now_ns();
    let mut t: Option<u64> = None;
    let mut take = |x: u64| {
        if x > now {
            t = Some(t.map_or(x, |y| y.min(x)));
        }
    };
    if s.host.audio.is_some() && s.com.dsound.created {
        take(s.com.dsound.mixed_until + MIX_TICK_NS);
    }
    for id in &s.com.dsound.playing {
        let Some(b) = s.dsbuf(*id) else { continue };
        if !b.playing || b.primary {
            continue;
        }
        let frames = b.frames().max(1);
        let abs = b.frame_at(now);
        if !b.looping {
            take(b.time_of_frame(frames));
        }
        let base = abs - abs % frames;
        for (off, _) in &b.notify {
            if *off == 0xffff_ffff {
                continue;
            }
            let f = (*off / b.fmt.block.max(1) as u32) as u64;
            let mut target = base + f;
            if target <= abs {
                target += frames;
            }
            if !b.looping && target > frames {
                continue;
            }
            take(b.time_of_frame(target + 1));
        }
    }
    t
}

pub fn service(s: &mut Session) {
    if !s.com.dsound.created {
        return;
    }
    let now = s.clock.now_ns();
    // Mix before settling: settling re-anchors each buffer's timeline at `now`, and
    // the mixer needs the anchors that were valid across the window it renders.
    if s.host.audio.is_some() && now >= s.com.dsound.mixed_until + MIX_TICK_NS {
        mix_to(s, now);
    }
    for id in s.playing_ids() {
        settle(s, id, now);
    }
}

fn db_to_gain(v: i32) -> f32 {
    if v <= -10000 { 0.0 } else { 10f32.powf(v as f32 / 2000.0) }
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}
fn norm(a: [f32; 3]) -> [f32; 3] {
    let l = dot(a, a).sqrt().max(1e-12);
    [a[0] / l, a[1] / l, a[2] / l]
}

/// (left gain, right gain, pitch factor) for a buffer.
fn spatial(g: &DSoundGlobal, b: &DsBuffer) -> (f32, f32, f32) {
    let base = db_to_gain(b.volume) * db_to_gain(g.primary_volume);
    let pan = b.pan.clamp(-10000, 10000);
    let (mut l, mut r) = if pan < 0 { (1.0, db_to_gain(pan)) } else { (db_to_gain(-pan), 1.0) };
    let mut pitch = 1.0;
    if b.flags & DSBCAPS_CTRL3D != 0 && b.p3d.mode != 2 {
        let lis = &g.listener;
        let rel = if b.p3d.mode == 1 { b.p3d.position } else { sub(b.p3d.position, lis.position) };
        let d = dot(rel, rel).sqrt() * lis.distance_factor;
        let minr = b.p3d.min_distance * lis.distance_factor;
        let maxr = b.p3d.max_distance * lis.distance_factor;
        let dc = d.clamp(minr, maxr.max(minr));
        let mut att = minr / (minr + lis.rolloff * (dc - minr)).max(1e-6);
        if d > maxr && b.flags & DSBCAPS_MUTE3DATMAXDISTANCE != 0 {
            att = 0.0;
        }
        // Azimuth relative to the listener's right vector (left-handed coordinates).
        let (front, top) = if b.p3d.mode == 1 { ([0.0, 0.0, 1.0], [0.0, 1.0, 0.0]) } else { (norm(lis.front), norm(lis.top)) };
        let right = norm(cross(top, front));
        let x = if d > 1e-6 { dot(norm(rel), right) } else { 0.0 };
        let p = (x.clamp(-1.0, 1.0) + 1.0) * std::f32::consts::FRAC_PI_4;
        l *= p.cos() * std::f32::consts::SQRT_2 * att;
        r *= p.sin() * std::f32::consts::SQRT_2 * att;
        // Doppler shift along the line of sight (speed of sound 343 m/s).
        if lis.doppler > 0.0 && d > 1e-6 && b.p3d.mode == 0 {
            let dir = norm(rel);
            let c = 343.0 / lis.distance_factor.max(1e-6);
            let vl = dot(lis.velocity, dir) * lis.doppler;
            let vs = dot(b.p3d.velocity, dir) * lis.doppler;
            pitch = ((c + vl) / (c + vs).max(1e-3)).clamp(0.1, 4.0);
        }
    }
    (l * base, r * base, pitch)
}

fn sample(data: &[u8], fmt: &Wfx, frame: usize, ch: usize) -> f32 {
    let c = ch.min(fmt.channels as usize - 1);
    let o = frame * fmt.block as usize + c * (fmt.bits as usize / 8);
    if fmt.bits == 8 {
        (data.get(o).copied().unwrap_or(128) as f32 - 128.0) / 128.0
    } else {
        let lo = data.get(o).copied().unwrap_or(0);
        let hi = data.get(o + 1).copied().unwrap_or(0);
        i16::from_le_bytes([lo, hi]) as f32 / 32768.0
    }
}

/// Mix all playing buffers from `mixed_until` to `now` into the attached sink.
fn mix_to(s: &mut Session, now: u64) {
    let g = s.com.dsound.clone();
    let t0 = g.mixed_until.min(now);
    let span = now - t0;
    let total = span as u128 * drydock_audio::OUTPUT_RATE as u128 + g.out_frac as u128;
    let n = (total / 1_000_000_000) as usize;
    s.com.dsound.out_frac = (total % 1_000_000_000) as u64;
    s.com.dsound.mixed_until = now;
    if n == 0 {
        return;
    }
    let mut out = vec![0f32; n * 2];
    for id in s.playing_ids() {
        let Some(b) = s.dsbuf(id).cloned() else { continue };
        if b.primary {
            continue;
        }
        // The buffer may have started or stopped inside the window; sample positions
        // follow the anchored timeline and are clipped to the playing interval.
        let frames = b.frames().max(1) as f64;
        let Ok(data) = s.mem.read_vec(b.mem, b.size) else { continue };
        let (gl, gr, pitch) = spatial(&g, &b);
        if gl == 0.0 && gr == 0.0 {
            continue;
        }
        let rate = drydock_audio::OUTPUT_RATE as f64;
        for k in 0..n {
            let t = t0 as f64 + k as f64 * 1e9 / rate;
            if !b.playing && (t as u64) >= b.anchor_ns {
                break;
            }
            if (t as u64) < b.anchor_ns {
                continue;
            }
            let pos = b.anchor_frame as f64 + (t - b.anchor_ns as f64) * b.freq as f64 * pitch as f64 / 1e9;
            if !b.looping && pos >= frames {
                break;
            }
            let p = pos % frames;
            let i0 = p.floor() as usize;
            let i1 = if i0 + 1 >= frames as usize { if b.looping { 0 } else { i0 } } else { i0 + 1 };
            let f = (p - i0 as f64) as f32;
            let lv = sample(&data, &b.fmt, i0, 0) * (1.0 - f) + sample(&data, &b.fmt, i1, 0) * f;
            let rv = sample(&data, &b.fmt, i0, 1) * (1.0 - f) + sample(&data, &b.fmt, i1, 1) * f;
            out[k * 2] += lv * gl;
            out[k * 2 + 1] += rv * gr;
        }
    }
    for v in out.iter_mut() {
        *v = v.clamp(-1.0, 1.0);
    }
    s.com.dsound.frames_mixed += n as u64;
    if let Some(sink) = s.host.audio.as_mut() {
        sink.push(&out);
    }
}

// ---------------------------------------------------------------- creation

fn ds_new(s: &mut Session) -> u32 {
    if !s.com.dsound.created {
        s.com.dsound.created = true;
        s.com.dsound.primary_fmt = Wfx {
            tag: 1,
            channels: 2,
            rate: 22050,
            bits: 8,
            block: 2,
        };
        s.com.dsound.mixed_until = s.clock.now_ns();
        s.com.dsound.speaker = 0x0004; // DSSPEAKER_STEREO
    }
    s.com_new(ComKind::DSound(DSoundObj {}))
}

fn direct_sound_create(s: &mut Session) -> ApiResult {
    let [guid_p, pp, outer] = s.args::<3>()?;
    if outer != 0 {
        return Ok(ApiRet::Ret(0x8004_0110));
    }
    if guid_p != 0 {
        let g = s.read_guid(guid_p)?;
        if g != [0u8; 16] && g != DEVICE_GUID {
            return Ok(ApiRet::Ret(0x8878_0078)); // DSERR_NODRIVER
        }
    }
    let id = ds_new(s);
    s.out_iface(pp, id, &IDIRECTSOUND)?;
    Ok(ApiRet::Ret(DS_OK))
}

pub fn create_via_com(s: &mut Session, iid: &Guid, ppv: u32) -> ApiResult {
    if *iid != IDIRECTSOUND.iid && *iid != IID_IUNKNOWN {
        return Ok(ApiRet::Ret(E_NOINTERFACE));
    }
    let id = ds_new(s);
    s.out_iface(ppv, id, &IDIRECTSOUND)?;
    Ok(ApiRet::Ret(S_OK))
}

const DEVICE_GUID: Guid = guid("bd6dd71a-3deb-11d1-b171-00c04fc20000");

fn cstr(t: &str) -> CallArg {
    let mut b = crate::strings::encode_ansi(t);
    b.push(0);
    CallArg::Buf(b)
}

fn direct_sound_enumerate_a(s: &mut Session) -> ApiResult {
    let [cb, ctx] = s.args::<2>()?;
    let items = vec![
        vec![CallArg::Val(0), cstr("Primary Sound Driver"), cstr(""), CallArg::Val(ctx)],
        vec![CallArg::Buf(DEVICE_GUID.to_vec()), cstr("Drydock Audio"), cstr("drydock.vxd"), CallArg::Val(ctx)],
    ];
    enum_callbacks(cb, items, 0, DS_OK)
}

// ---------------------------------------------------------------- IDirectSound

fn ds_set_cooperative_level(s: &mut Session) -> ApiResult {
    let [_this, _hwnd, level] = s.args::<3>()?;
    s.com.dsound.coop = level;
    Ok(ApiRet::Ret(DS_OK))
}

fn ds_get_caps(s: &mut Session) -> ApiResult {
    let [_this, p] = s.args::<2>()?;
    let mut b = vec![0u8; 96];
    let put = |b: &mut Vec<u8>, o: usize, v: u32| b[o..o + 4].copy_from_slice(&v.to_le_bytes());
    put(&mut b, 0, 96);
    put(&mut b, 4, 0xf5f);
    put(&mut b, 8, 100);
    put(&mut b, 12, 100_000);
    put(&mut b, 16, 1);
    // All mixing is done in software: no hardware buffers or memory.
    put(&mut b, 84, 0);
    s.mem.write(p, &b)?;
    Ok(ApiRet::Ret(DS_OK))
}

fn read_wfx(s: &Session, p: u32) -> Result<Wfx, ApiError> {
    Ok(Wfx {
        tag: s.mem.read_u16(p)?,
        channels: s.mem.read_u16(p + 2)?,
        rate: s.mem.read_u32(p + 4)?,
        bits: s.mem.read_u16(p + 14)?,
        block: s.mem.read_u16(p + 12)?,
    })
}

fn ds_create_sound_buffer(s: &mut Session) -> ApiResult {
    let [_this, pdesc, pp, outer] = s.args::<4>()?;
    if outer != 0 {
        return Ok(ApiRet::Ret(0x8004_0110));
    }
    let size = s.mem.read_u32(pdesc)?;
    if size != 20 && size != 36 {
        return Ok(ApiRet::Ret(DSERR_INVALIDPARAM));
    }
    let flags = s.mem.read_u32(pdesc + 4)?;
    let bytes = s.mem.read_u32(pdesc + 8)?;
    let pwfx = s.mem.read_u32(pdesc + 16)?;
    let primary = flags & DSBCAPS_PRIMARYBUFFER != 0;
    let mut b = DsBuffer {
        primary,
        flags: flags | DSBCAPS_LOCSOFTWARE,
        mem: 0,
        size: 0,
        fmt: s.com.dsound.primary_fmt,
        mem_owner: 0,
        playing: false,
        looping: false,
        anchor_ns: s.clock.now_ns(),
        anchor_frame: 0,
        freq: 0,
        volume: 0,
        pan: 0,
        notify: Vec::new(),
        last_pos: 0,
        p3d: Params3d::default(),
        pending_3d: None,
    };
    if primary {
        if bytes != 0 || pwfx != 0 {
            return Ok(ApiRet::Ret(DSERR_INVALIDPARAM));
        }
        b.size = 4410 * b.fmt.block as u32; // ~200 ms
    } else {
        if pwfx == 0 || !(4..=0x0fff_ffff).contains(&bytes) {
            return Ok(ApiRet::Ret(DSERR_INVALIDPARAM));
        }
        let f = read_wfx(s, pwfx)?;
        if !f.valid() {
            return Ok(ApiRet::Ret(DSERR_BADFORMAT));
        }
        if flags & DSBCAPS_CTRL3D != 0 && f.channels != 1 {
            return Ok(ApiRet::Ret(DSERR_INVALIDPARAM));
        }
        b.fmt = f;
        b.size = bytes / f.block as u32 * f.block as u32;
    }
    b.freq = b.fmt.rate;
    let mem = s
        .heaps
        .alloc(&mut s.mem, s.heaps.process_heap, b.size.max(4), false)
        .ok_or(ApiError::Fatal("sound buffer allocation".into()))?;
    // Fresh buffers contain silence.
    let fill = if b.fmt.bits == 8 { 0x80 } else { 0 };
    s.mem.fill(mem, b.size.max(4), fill)?;
    b.mem = mem;
    let id = s.com_new(ComKind::DSBuffer(Box::new(b)));
    if let Some(bm) = s.dsbuf_mut(id) {
        bm.mem_owner = id;
    }
    s.com.dsound.buffers.insert(id);
    s.out_iface(pp, id, &IDIRECTSOUNDBUFFER)?;
    Ok(ApiRet::Ret(DS_OK))
}

fn ds_duplicate_sound_buffer(s: &mut Session) -> ApiResult {
    let [_this, orig, pp] = s.args::<3>()?;
    let Some(src) = s.obj_of(orig) else {
        return Ok(ApiRet::Ret(DSERR_INVALIDPARAM));
    };
    let Some(b) = s.dsbuf(src).cloned() else {
        return Ok(ApiRet::Ret(DSERR_INVALIDPARAM));
    };
    if b.primary {
        return Ok(ApiRet::Ret(DSERR_INVALIDCALL));
    }
    let mut nb = b.clone();
    nb.playing = false;
    nb.anchor_frame = 0;
    nb.notify.clear();
    nb.last_pos = 0;
    let owner = b.mem_owner;
    s.com_addref(owner);
    let id = s.com_new(ComKind::DSBuffer(Box::new(nb)));
    s.com.dsound.buffers.insert(id);
    s.out_iface(pp, id, &IDIRECTSOUNDBUFFER)?;
    Ok(ApiRet::Ret(DS_OK))
}

pub fn buffer_destroyed(s: &mut Session, id: u32, b: DsBuffer) {
    s.com.dsound.buffers.remove(&id);
    s.com.dsound.playing.remove(&id);
    if b.mem_owner == id {
        // Duplicates hold a reference on the owner, so the owner dies last.
        let ph = s.heaps.process_heap;
        s.heaps.free(ph, b.mem);
    } else if b.mem_owner != 0 {
        s.com_release(b.mem_owner);
    }
}

fn ds_get_speaker_config(s: &mut Session) -> ApiResult {
    let [_this, p] = s.args::<2>()?;
    s.mem.write_u32(p, s.com.dsound.speaker)?;
    Ok(ApiRet::Ret(DS_OK))
}
fn ds_set_speaker_config(s: &mut Session) -> ApiResult {
    let [_this, c] = s.args::<2>()?;
    s.com.dsound.speaker = c;
    Ok(ApiRet::Ret(DS_OK))
}
fn ds_ok(_s: &mut Session) -> ApiResult {
    Ok(ApiRet::Ret(DS_OK))
}
fn ds_initialize(_s: &mut Session) -> ApiResult {
    Ok(ApiRet::Ret(DSERR_ALREADYINITIALIZED))
}

// ---------------------------------------------------------------- IDirectSoundBuffer

fn buf_self(s: &mut Session) -> Result<(u32, DsBuffer), ApiError> {
    let id = s.this_obj()?;
    let now = s.clock.now_ns();
    settle(s, id, now);
    s.dsbuf(id).cloned().map(|b| (id, b)).ok_or_else(|| ApiError::Fatal("not a sound buffer".into()))
}

fn buf_get_caps(s: &mut Session) -> ApiResult {
    let [_this, p] = s.args::<2>()?;
    let (_id, b) = buf_self(s)?;
    let size = s.mem.read_u32(p)?;
    if size < 20 {
        return Ok(ApiRet::Ret(DSERR_INVALIDPARAM));
    }
    s.mem.write_u32(p + 4, b.flags)?;
    s.mem.write_u32(p + 8, b.size)?;
    s.mem.write_u32(p + 12, 0)?;
    s.mem.write_u32(p + 16, 0)?;
    Ok(ApiRet::Ret(DS_OK))
}

fn cursors(s: &Session, b: &DsBuffer) -> (u32, u32) {
    let now = s.clock.now_ns();
    if b.primary {
        let g = &s.com.dsound;
        let frames = (b.size / b.fmt.block as u32) as u64;
        let f = (now as u128 * b.fmt.rate as u128 / 1_000_000_000) as u64 % frames.max(1);
        let play = (f * b.fmt.block as u64) as u32;
        let lead = (WRITE_LEAD_NS * b.fmt.rate as u64 / 1_000_000_000) as u32 * b.fmt.block as u32;
        let _ = g;
        return (play, (play + lead) % b.size.max(1));
    }
    let frames = b.frames().max(1);
    let f = b.frame_at(now) % frames;
    let play = (f * b.fmt.block as u64) as u32;
    if !b.playing {
        return (play, play);
    }
    let lead = (WRITE_LEAD_NS * b.freq as u64 / 1_000_000_000) as u32 * b.fmt.block as u32;
    (play, (play + lead) % b.size.max(1))
}

fn buf_get_current_position(s: &mut Session) -> ApiResult {
    let [_this, pplay, pwrite] = s.args::<3>()?;
    let (_id, b) = buf_self(s)?;
    let (play, write) = cursors(s, &b);
    if pplay != 0 {
        s.mem.write_u32(pplay, play)?;
    }
    if pwrite != 0 {
        s.mem.write_u32(pwrite, write)?;
    }
    Ok(ApiRet::Ret(DS_OK))
}

fn buf_set_current_position(s: &mut Session) -> ApiResult {
    let [_this, pos] = s.args::<2>()?;
    let (id, b) = buf_self(s)?;
    if b.primary {
        return Ok(ApiRet::Ret(DSERR_INVALIDCALL));
    }
    let now = s.clock.now_ns();
    let bm = s.dsbuf_mut(id).unwrap();
    let p = pos.min(b.size.saturating_sub(1)) / b.fmt.block as u32;
    bm.anchor_frame = p as u64;
    bm.anchor_ns = now;
    bm.last_pos = p * b.fmt.block as u32;
    Ok(ApiRet::Ret(DS_OK))
}

fn buf_get_format(s: &mut Session) -> ApiResult {
    let [_this, p, size, written] = s.args::<4>()?;
    let (_id, b) = buf_self(s)?;
    let w = b.fmt.bytes();
    if p == 0 {
        if written != 0 {
            s.mem.write_u32(written, w.len() as u32)?;
        }
        return Ok(ApiRet::Ret(DS_OK));
    }
    let n = (size as usize).min(w.len());
    s.mem.write(p, &w[..n])?;
    if written != 0 {
        s.mem.write_u32(written, n as u32)?;
    }
    Ok(ApiRet::Ret(DS_OK))
}

fn buf_set_format(s: &mut Session) -> ApiResult {
    let [_this, p] = s.args::<2>()?;
    let (id, b) = buf_self(s)?;
    if !b.primary {
        return Ok(ApiRet::Ret(DSERR_INVALIDCALL));
    }
    if s.com.dsound.coop < 2 {
        return Ok(ApiRet::Ret(DSERR_PRIOLEVELNEEDED));
    }
    let f = read_wfx(s, p)?;
    if !f.valid() {
        return Ok(ApiRet::Ret(DSERR_BADFORMAT));
    }
    s.com.dsound.primary_fmt = f;
    let bm = s.dsbuf_mut(id).unwrap();
    bm.fmt = f;
    bm.freq = f.rate;
    Ok(ApiRet::Ret(DS_OK))
}

fn buf_get_status(s: &mut Session) -> ApiResult {
    let [_this, p] = s.args::<2>()?;
    let (_id, b) = buf_self(s)?;
    let st = if b.playing { 1 | if b.looping { 4 } else { 0 } } else { 0 };
    s.mem.write_u32(p, st)?;
    Ok(ApiRet::Ret(DS_OK))
}

fn buf_play(s: &mut Session) -> ApiResult {
    let [_this, _r, _prio, flags] = s.args::<4>()?;
    let (id, b) = buf_self(s)?;
    let now = s.clock.now_ns();
    // Mix what came before so the start lands at the right output position.
    if s.host.audio.is_some() {
        mix_to(s, now);
    }
    let bm = s.dsbuf_mut(id).unwrap();
    if !b.playing {
        bm.anchor_ns = now;
    }
    bm.playing = true;
    bm.looping = flags & 1 != 0;
    if !b.primary {
        s.com.dsound.playing.insert(id);
    }
    Ok(ApiRet::Ret(DS_OK))
}

fn buf_stop(s: &mut Session) -> ApiResult {
    let (id, b) = buf_self(s)?;
    let now = s.clock.now_ns();
    if s.host.audio.is_some() {
        mix_to(s, now);
    }
    if b.playing && !b.primary {
        let frames = b.frames().max(1);
        let f = b.frame_at(now) % frames;
        let stop_events: Vec<u32> = b.notify.iter().filter(|(o, _)| *o == 0xffff_ffff).map(|(_, e)| *e).collect();
        let bm = s.dsbuf_mut(id).unwrap();
        bm.playing = false;
        bm.anchor_frame = f;
        bm.anchor_ns = now;
        s.com.dsound.playing.remove(&id);
        for h in stop_events {
            if let Some(crate::objects::KObj::Event { signaled, .. }) = s.objects.by_handle_mut(h) {
                *signaled = true;
            }
        }
    } else if let Some(bm) = s.dsbuf_mut(id) {
        bm.playing = false;
    }
    Ok(ApiRet::Ret(DS_OK))
}

fn buf_lock(s: &mut Session) -> ApiResult {
    let [_this, off, bytes, p1, n1, p2, n2, flags] = s.args::<8>()?;
    let (_id, b) = buf_self(s)?;
    if b.primary && s.com.dsound.coop < 3 {
        // DSSCL_WRITEPRIMARY is required to lock the primary buffer.
        return Ok(ApiRet::Ret(DSERR_PRIOLEVELNEEDED));
    }
    let (_, write) = cursors(s, &b);
    let off = if flags & 1 != 0 { write } else { off };
    let bytes = if flags & 2 != 0 { b.size } else { bytes };
    if off >= b.size || bytes > b.size || bytes == 0 {
        return Ok(ApiRet::Ret(DSERR_INVALIDPARAM));
    }
    let first = bytes.min(b.size - off);
    s.mem.write_u32(p1, b.mem + off)?;
    s.mem.write_u32(n1, first)?;
    if p2 != 0 {
        let rest = bytes - first;
        s.mem.write_u32(p2, if rest > 0 { b.mem } else { 0 })?;
        if n2 != 0 {
            s.mem.write_u32(n2, rest)?;
        }
    }
    Ok(ApiRet::Ret(DS_OK))
}

fn buf_unlock(s: &mut Session) -> ApiResult {
    let [_this, _p1, _n1, _p2, _n2] = s.args::<5>()?;
    Ok(ApiRet::Ret(DS_OK))
}

fn buf_set_volume(s: &mut Session) -> ApiResult {
    let [_this, v] = s.args::<2>()?;
    let (id, b) = buf_self(s)?;
    let v = v as i32;
    if !(-10000..=0).contains(&v) {
        return Ok(ApiRet::Ret(DSERR_INVALIDPARAM));
    }
    if b.flags & DSBCAPS_CTRLVOLUME == 0 && !b.primary {
        return Ok(ApiRet::Ret(DSERR_CONTROLUNAVAIL));
    }
    if b.primary {
        s.com.dsound.primary_volume = v;
    }
    s.dsbuf_mut(id).unwrap().volume = v;
    Ok(ApiRet::Ret(DS_OK))
}
fn buf_get_volume(s: &mut Session) -> ApiResult {
    let [_this, p] = s.args::<2>()?;
    let (_id, b) = buf_self(s)?;
    s.mem.write_u32(p, b.volume as u32)?;
    Ok(ApiRet::Ret(DS_OK))
}
fn buf_set_pan(s: &mut Session) -> ApiResult {
    let [_this, v] = s.args::<2>()?;
    let (id, b) = buf_self(s)?;
    let v = v as i32;
    if !(-10000..=10000).contains(&v) {
        return Ok(ApiRet::Ret(DSERR_INVALIDPARAM));
    }
    if b.flags & DSBCAPS_CTRLPAN == 0 {
        return Ok(ApiRet::Ret(DSERR_CONTROLUNAVAIL));
    }
    s.dsbuf_mut(id).unwrap().pan = v;
    Ok(ApiRet::Ret(DS_OK))
}
fn buf_get_pan(s: &mut Session) -> ApiResult {
    let [_this, p] = s.args::<2>()?;
    let (_id, b) = buf_self(s)?;
    s.mem.write_u32(p, b.pan as u32)?;
    Ok(ApiRet::Ret(DS_OK))
}
fn buf_set_frequency(s: &mut Session) -> ApiResult {
    let [_this, f] = s.args::<2>()?;
    let (id, b) = buf_self(s)?;
    if b.flags & DSBCAPS_CTRLFREQUENCY == 0 || b.primary {
        return Ok(ApiRet::Ret(DSERR_CONTROLUNAVAIL));
    }
    let f = if f == 0 { b.fmt.rate } else { f };
    if !(100..=100_000).contains(&f) {
        return Ok(ApiRet::Ret(DSERR_INVALIDPARAM));
    }
    let now = s.clock.now_ns();
    if s.host.audio.is_some() {
        mix_to(s, now);
    }
    let bm = s.dsbuf_mut(id).unwrap();
    let cur = bm.frame_at(now) % bm.frames().max(1);
    bm.anchor_frame = cur;
    bm.anchor_ns = now;
    bm.freq = f;
    Ok(ApiRet::Ret(DS_OK))
}
fn buf_get_frequency(s: &mut Session) -> ApiResult {
    let [_this, p] = s.args::<2>()?;
    let (_id, b) = buf_self(s)?;
    s.mem.write_u32(p, b.freq)?;
    Ok(ApiRet::Ret(DS_OK))
}
fn buf_restore(_s: &mut Session) -> ApiResult {
    Ok(ApiRet::Ret(DS_OK))
}
fn buf_initialize(_s: &mut Session) -> ApiResult {
    Ok(ApiRet::Ret(DSERR_ALREADYINITIALIZED))
}

pub fn buffer_qi(s: &mut Session, id: u32, iid: &Guid, ppv: u32) -> Result<Option<u32>, ApiError> {
    let Some(b) = s.dsbuf(id).cloned() else {
        return Ok(None);
    };
    if *iid == IDIRECTSOUND3DBUFFER.iid {
        if b.flags & DSBCAPS_CTRL3D == 0 || b.primary {
            return Ok(Some(E_NOINTERFACE));
        }
        s.out_iface(ppv, id, &IDIRECTSOUND3DBUFFER)?;
        s.com_addref(id);
        return Ok(Some(S_OK));
    }
    if *iid == IDIRECTSOUND3DLISTENER.iid {
        if !b.primary || b.flags & DSBCAPS_CTRL3D == 0 {
            return Ok(Some(E_NOINTERFACE));
        }
        s.out_iface(ppv, id, &IDIRECTSOUND3DLISTENER)?;
        s.com_addref(id);
        return Ok(Some(S_OK));
    }
    if *iid == IDIRECTSOUNDNOTIFY.iid {
        if b.flags & DSBCAPS_CTRLPOSITIONNOTIFY == 0 {
            return Ok(Some(E_NOINTERFACE));
        }
        s.out_iface(ppv, id, &IDIRECTSOUNDNOTIFY)?;
        s.com_addref(id);
        return Ok(Some(S_OK));
    }
    Ok(None)
}

fn notify_set_positions(s: &mut Session) -> ApiResult {
    let [_this, n, p] = s.args::<3>()?;
    let (id, b) = buf_self(s)?;
    if b.playing {
        return Ok(ApiRet::Ret(DSERR_INVALIDCALL));
    }
    let mut v = Vec::new();
    for i in 0..n.min(1024) {
        let off = s.mem.read_u32(p + 8 * i)?;
        let ev = s.mem.read_u32(p + 8 * i + 4)?;
        if off != 0xffff_ffff && off >= b.size {
            return Ok(ApiRet::Ret(DSERR_INVALIDPARAM));
        }
        v.push((off, ev));
    }
    s.dsbuf_mut(id).unwrap().notify = v;
    Ok(ApiRet::Ret(DS_OK))
}

// ---------------------------------------------------------------- 3D

fn read_vec3(s: &Session, p: u32) -> Result<[f32; 3], ApiError> {
    Ok([s.mem.read_f32(p)?, s.mem.read_f32(p + 4)?, s.mem.read_f32(p + 8)?])
}
fn write_vec3(s: &mut Session, p: u32, v: [f32; 3]) -> Result<(), ApiError> {
    for (i, x) in v.iter().enumerate() {
        s.mem.write_f32(p + 4 * i as u32, *x)?;
    }
    Ok(())
}

fn with_3d(s: &mut Session, deferred: bool, f: impl FnOnce(&mut Params3d)) -> ApiResult {
    let (id, _b) = buf_self(s)?;
    let bm = s.dsbuf_mut(id).unwrap();
    if deferred {
        let mut p = bm.pending_3d.unwrap_or(bm.p3d);
        f(&mut p);
        bm.pending_3d = Some(p);
    } else {
        f(&mut bm.p3d);
    }
    Ok(ApiRet::Ret(DS_OK))
}

fn b3_set_position(s: &mut Session) -> ApiResult {
    let [_this, x, y, z, apply] = s.args::<5>()?;
    let v = [f32::from_bits(x), f32::from_bits(y), f32::from_bits(z)];
    with_3d(s, apply == 1, |p| p.position = v)
}
fn b3_set_velocity(s: &mut Session) -> ApiResult {
    let [_this, x, y, z, apply] = s.args::<5>()?;
    let v = [f32::from_bits(x), f32::from_bits(y), f32::from_bits(z)];
    with_3d(s, apply == 1, |p| p.velocity = v)
}
fn b3_set_min_distance(s: &mut Session) -> ApiResult {
    let [_this, d, apply] = s.args::<3>()?;
    with_3d(s, apply == 1, |p| p.min_distance = f32::from_bits(d))
}
fn b3_set_max_distance(s: &mut Session) -> ApiResult {
    let [_this, d, apply] = s.args::<3>()?;
    with_3d(s, apply == 1, |p| p.max_distance = f32::from_bits(d))
}
fn b3_set_mode(s: &mut Session) -> ApiResult {
    let [_this, m, apply] = s.args::<3>()?;
    with_3d(s, apply == 1, |p| p.mode = m)
}
fn b3_set_cone_angles(s: &mut Session) -> ApiResult {
    let [_this, a, b, apply] = s.args::<4>()?;
    with_3d(s, apply == 1, |p| {
        p.cone_inside = a;
        p.cone_outside = b;
    })
}
fn b3_set_cone_orientation(s: &mut Session) -> ApiResult {
    let [_this, x, y, z, apply] = s.args::<5>()?;
    let v = [f32::from_bits(x), f32::from_bits(y), f32::from_bits(z)];
    with_3d(s, apply == 1, |p| p.cone_orientation = v)
}
fn b3_set_cone_outside_volume(s: &mut Session) -> ApiResult {
    let [_this, v, apply] = s.args::<3>()?;
    with_3d(s, apply == 1, |p| p.cone_outside_volume = v as i32)
}

fn params_bytes(p: &Params3d) -> Vec<u8> {
    let mut b = Vec::with_capacity(64);
    b.extend(64u32.to_le_bytes());
    for v in p.position.iter().chain(&p.velocity) {
        b.extend(v.to_le_bytes());
    }
    b.extend(p.cone_inside.to_le_bytes());
    b.extend(p.cone_outside.to_le_bytes());
    for v in &p.cone_orientation {
        b.extend(v.to_le_bytes());
    }
    b.extend(p.cone_outside_volume.to_le_bytes());
    b.extend(p.min_distance.to_le_bytes());
    b.extend(p.max_distance.to_le_bytes());
    b.extend(p.mode.to_le_bytes());
    b
}

fn b3_get_all(s: &mut Session) -> ApiResult {
    let [_this, p] = s.args::<2>()?;
    let (_id, b) = buf_self(s)?;
    s.mem.write(p, &params_bytes(&b.p3d))?;
    Ok(ApiRet::Ret(DS_OK))
}
fn b3_set_all(s: &mut Session) -> ApiResult {
    let [_this, p, apply] = s.args::<3>()?;
    let pos = read_vec3(s, p + 4)?;
    let vel = read_vec3(s, p + 16)?;
    let ci = s.mem.read_u32(p + 28)?;
    let co = s.mem.read_u32(p + 32)?;
    let cor = read_vec3(s, p + 36)?;
    let cov = s.mem.read_i32(p + 48)?;
    let mind = s.mem.read_f32(p + 52)?;
    let maxd = s.mem.read_f32(p + 56)?;
    let mode = s.mem.read_u32(p + 60)?;
    with_3d(s, apply == 1, |x| {
        *x = Params3d {
            position: pos,
            velocity: vel,
            cone_inside: ci,
            cone_outside: co,
            cone_orientation: cor,
            cone_outside_volume: cov,
            min_distance: mind,
            max_distance: maxd,
            mode,
        }
    })
}
fn b3_get_position(s: &mut Session) -> ApiResult {
    let [_this, p] = s.args::<2>()?;
    let (_id, b) = buf_self(s)?;
    write_vec3(s, p, b.p3d.position)?;
    Ok(ApiRet::Ret(DS_OK))
}
fn b3_get_velocity(s: &mut Session) -> ApiResult {
    let [_this, p] = s.args::<2>()?;
    let (_id, b) = buf_self(s)?;
    write_vec3(s, p, b.p3d.velocity)?;
    Ok(ApiRet::Ret(DS_OK))
}
fn b3_get_min_distance(s: &mut Session) -> ApiResult {
    let [_this, p] = s.args::<2>()?;
    let (_id, b) = buf_self(s)?;
    s.mem.write_f32(p, b.p3d.min_distance)?;
    Ok(ApiRet::Ret(DS_OK))
}
fn b3_get_max_distance(s: &mut Session) -> ApiResult {
    let [_this, p] = s.args::<2>()?;
    let (_id, b) = buf_self(s)?;
    s.mem.write_f32(p, b.p3d.max_distance)?;
    Ok(ApiRet::Ret(DS_OK))
}
fn b3_get_mode(s: &mut Session) -> ApiResult {
    let [_this, p] = s.args::<2>()?;
    let (_id, b) = buf_self(s)?;
    s.mem.write_u32(p, b.p3d.mode)?;
    Ok(ApiRet::Ret(DS_OK))
}

fn listener_set(s: &mut Session, deferred: bool, f: impl FnOnce(&mut Listener)) -> ApiResult {
    let g = &mut s.com.dsound;
    if deferred {
        let mut l = g.pending_listener.unwrap_or(g.listener);
        f(&mut l);
        g.pending_listener = Some(l);
    } else {
        f(&mut g.listener);
    }
    Ok(ApiRet::Ret(DS_OK))
}

fn l3_set_position(s: &mut Session) -> ApiResult {
    let [_this, x, y, z, apply] = s.args::<5>()?;
    let v = [f32::from_bits(x), f32::from_bits(y), f32::from_bits(z)];
    listener_set(s, apply == 1, |l| l.position = v)
}
fn l3_set_velocity(s: &mut Session) -> ApiResult {
    let [_this, x, y, z, apply] = s.args::<5>()?;
    let v = [f32::from_bits(x), f32::from_bits(y), f32::from_bits(z)];
    listener_set(s, apply == 1, |l| l.velocity = v)
}
fn l3_set_orientation(s: &mut Session) -> ApiResult {
    let [_this, fx, fy, fz, tx, ty, tz, apply] = s.args::<8>()?;
    let f = [f32::from_bits(fx), f32::from_bits(fy), f32::from_bits(fz)];
    let t = [f32::from_bits(tx), f32::from_bits(ty), f32::from_bits(tz)];
    listener_set(s, apply == 1, |l| {
        l.front = f;
        l.top = t;
    })
}
fn l3_set_distance_factor(s: &mut Session) -> ApiResult {
    let [_this, v, apply] = s.args::<3>()?;
    listener_set(s, apply == 1, |l| l.distance_factor = f32::from_bits(v))
}
fn l3_set_doppler_factor(s: &mut Session) -> ApiResult {
    let [_this, v, apply] = s.args::<3>()?;
    listener_set(s, apply == 1, |l| l.doppler = f32::from_bits(v))
}
fn l3_set_rolloff_factor(s: &mut Session) -> ApiResult {
    let [_this, v, apply] = s.args::<3>()?;
    listener_set(s, apply == 1, |l| l.rolloff = f32::from_bits(v))
}
fn l3_commit(s: &mut Session) -> ApiResult {
    if let Some(l) = s.com.dsound.pending_listener.take() {
        s.com.dsound.listener = l;
    }
    for id in s.dsbuf_ids() {
        if let Some(b) = s.dsbuf_mut(id)
            && let Some(p) = b.pending_3d.take()
        {
            b.p3d = p;
        }
    }
    Ok(ApiRet::Ret(DS_OK))
}
fn l3_get_position(s: &mut Session) -> ApiResult {
    let [_this, p] = s.args::<2>()?;
    let v = s.com.dsound.listener.position;
    write_vec3(s, p, v)?;
    Ok(ApiRet::Ret(DS_OK))
}
fn l3_get_orientation(s: &mut Session) -> ApiResult {
    let [_this, f, t] = s.args::<3>()?;
    let l = s.com.dsound.listener;
    write_vec3(s, f, l.front)?;
    write_vec3(s, t, l.top)?;
    Ok(ApiRet::Ret(DS_OK))
}
fn l3_get_factor(s: &mut Session, which: u32) -> ApiResult {
    let [_this, p] = s.args::<2>()?;
    let l = s.com.dsound.listener;
    let v = match which {
        0 => l.distance_factor,
        1 => l.doppler,
        _ => l.rolloff,
    };
    s.mem.write_f32(p, v)?;
    Ok(ApiRet::Ret(DS_OK))
}
fn l3_get_distance_factor(s: &mut Session) -> ApiResult {
    l3_get_factor(s, 0)
}
fn l3_get_doppler_factor(s: &mut Session) -> ApiResult {
    l3_get_factor(s, 1)
}
fn l3_get_rolloff_factor(s: &mut Session) -> ApiResult {
    l3_get_factor(s, 2)
}

// ---------------------------------------------------------------- tables

pub static IDIRECTSOUND: IfaceDef = IfaceDef {
    name: "IDirectSound",
    iid: guid("279afa83-4981-11ce-a521-0020af0be560"),
    module: "DSOUND.DLL",
    methods: &[
        iunknown!()[0],
        iunknown!()[1],
        iunknown!()[2],
        export!("CreateSoundBuffer", 4, ds_create_sound_buffer),
        export!("GetCaps", 2, ds_get_caps),
        export!("DuplicateSoundBuffer", 3, ds_duplicate_sound_buffer),
        export!("SetCooperativeLevel", 3, ds_set_cooperative_level),
        export!("Compact", 1, ds_ok),
        export!("GetSpeakerConfig", 2, ds_get_speaker_config),
        export!("SetSpeakerConfig", 2, ds_set_speaker_config),
        export!("Initialize", 2, ds_initialize),
    ],
};

pub static IDIRECTSOUNDBUFFER: IfaceDef = IfaceDef {
    name: "IDirectSoundBuffer",
    iid: guid("279afa85-4981-11ce-a521-0020af0be560"),
    module: "DSOUND.DLL",
    methods: &[
        iunknown!()[0],
        iunknown!()[1],
        iunknown!()[2],
        export!("GetCaps", 2, buf_get_caps),
        export!("GetCurrentPosition", 3, buf_get_current_position),
        export!("GetFormat", 4, buf_get_format),
        export!("GetVolume", 2, buf_get_volume),
        export!("GetPan", 2, buf_get_pan),
        export!("GetFrequency", 2, buf_get_frequency),
        export!("GetStatus", 2, buf_get_status),
        export!("Initialize", 3, buf_initialize),
        export!("Lock", 8, buf_lock),
        export!("Play", 4, buf_play),
        export!("SetCurrentPosition", 2, buf_set_current_position),
        export!("SetFormat", 2, buf_set_format),
        export!("SetVolume", 2, buf_set_volume),
        export!("SetPan", 2, buf_set_pan),
        export!("SetFrequency", 2, buf_set_frequency),
        export!("Stop", 1, buf_stop),
        export!("Unlock", 5, buf_unlock),
        export!("Restore", 1, buf_restore),
    ],
};

pub static IDIRECTSOUNDNOTIFY: IfaceDef = IfaceDef {
    name: "IDirectSoundNotify",
    iid: guid("b0210783-89cd-11d0-af08-00a0c925cd16"),
    module: "DSOUND.DLL",
    methods: &[
        iunknown!()[0],
        iunknown!()[1],
        iunknown!()[2],
        export!("SetNotificationPositions", 3, notify_set_positions),
    ],
};

pub static IDIRECTSOUND3DLISTENER: IfaceDef = IfaceDef {
    name: "IDirectSound3DListener",
    iid: guid("279afa84-4981-11ce-a521-0020af0be560"),
    module: "DSOUND.DLL",
    methods: &[
        iunknown!()[0],
        iunknown!()[1],
        iunknown!()[2],
        unimpl_method!("GetAllParameters", 2),
        export!("GetDistanceFactor", 2, l3_get_distance_factor),
        export!("GetDopplerFactor", 2, l3_get_doppler_factor),
        export!("GetOrientation", 3, l3_get_orientation),
        export!("GetPosition", 2, l3_get_position),
        export!("GetRolloffFactor", 2, l3_get_rolloff_factor),
        unimpl_method!("GetVelocity", 2),
        unimpl_method!("SetAllParameters", 3),
        export!("SetDistanceFactor", 3, l3_set_distance_factor),
        export!("SetDopplerFactor", 3, l3_set_doppler_factor),
        export!("SetOrientation", 8, l3_set_orientation),
        export!("SetPosition", 5, l3_set_position),
        export!("SetRolloffFactor", 3, l3_set_rolloff_factor),
        export!("SetVelocity", 5, l3_set_velocity),
        export!("CommitDeferredSettings", 1, l3_commit),
    ],
};

pub static IDIRECTSOUND3DBUFFER: IfaceDef = IfaceDef {
    name: "IDirectSound3DBuffer",
    iid: guid("279afa86-4981-11ce-a521-0020af0be560"),
    module: "DSOUND.DLL",
    methods: &[
        iunknown!()[0],
        iunknown!()[1],
        iunknown!()[2],
        export!("GetAllParameters", 2, b3_get_all),
        unimpl_method!("GetConeAngles", 3),
        unimpl_method!("GetConeOrientation", 2),
        unimpl_method!("GetConeOutsideVolume", 2),
        export!("GetMaxDistance", 2, b3_get_max_distance),
        export!("GetMinDistance", 2, b3_get_min_distance),
        export!("GetMode", 2, b3_get_mode),
        export!("GetPosition", 2, b3_get_position),
        export!("GetVelocity", 2, b3_get_velocity),
        export!("SetAllParameters", 3, b3_set_all),
        export!("SetConeAngles", 4, b3_set_cone_angles),
        export!("SetConeOrientation", 5, b3_set_cone_orientation),
        export!("SetConeOutsideVolume", 3, b3_set_cone_outside_volume),
        export!("SetMaxDistance", 3, b3_set_max_distance),
        export!("SetMinDistance", 3, b3_set_min_distance),
        export!("SetMode", 3, b3_set_mode),
        export!("SetPosition", 5, b3_set_position),
        export!("SetVelocity", 5, b3_set_velocity),
    ],
};

pub static BUFFER_IFACES: &[&IfaceDef] = &[&IDIRECTSOUNDBUFFER];

pub static DSOUND_EXPORTS: &[ExportDef] = &[
    export!("DirectSoundCreate" @ 1, 3, direct_sound_create),
    export!("DirectSoundEnumerateA" @ 2, 2, direct_sound_enumerate_a),
];

/// Observable state of one playing secondary buffer.
#[derive(Debug, Clone, Serialize)]
pub struct VoiceInfo {
    pub object: u32,
    pub looping: bool,
    pub freq: u32,
    pub volume: i32,
    pub pan: i32,
    pub is_3d: bool,
    pub mode_3d: u32,
    pub position: [f32; 3],
    pub min_distance: f32,
    pub max_distance: f32,
    /// Effective (left, right) gains and pitch factor applied by the mixer.
    pub gain: (f32, f32),
    pub pitch: f32,
    /// Peak absolute sample value in the buffer (0..1), to spot silent data.
    pub peak: f32,
    pub bytes: u32,
}

/// Mixer-level summary of what is audible at this moment.
#[derive(Debug, Clone, Serialize)]
pub struct AudioInfo {
    pub frames_mixed: u64,
    pub primary_volume: i32,
    pub listener: Listener,
    pub voices: Vec<VoiceInfo>,
}

impl Session {
    pub fn audio_info(&self) -> AudioInfo {
        let g = &self.com.dsound;
        let voices = g
            .playing
            .iter()
            .filter_map(|id| self.dsbuf(*id).map(|b| (*id, b)))
            .filter(|(_, b)| !b.primary)
            .map(|(id, b)| {
                let (l, r, pitch) = spatial(g, b);
                let data = self.mem.read_vec(b.mem, b.size).unwrap_or_default();
                let frames = b.frames() as usize;
                let peak = (0..frames).map(|f| sample(&data, &b.fmt, f, 0).abs()).fold(0.0, f32::max);
                VoiceInfo {
                    peak,
                    bytes: b.size,
                    object: id,
                    looping: b.looping,
                    freq: b.freq,
                    volume: b.volume,
                    pan: b.pan,
                    is_3d: b.flags & DSBCAPS_CTRL3D != 0,
                    mode_3d: b.p3d.mode,
                    position: b.p3d.position,
                    min_distance: b.p3d.min_distance,
                    max_distance: b.p3d.max_distance,
                    gain: (l, r),
                    pitch,
                }
            })
            .collect();
        AudioInfo { frames_mixed: g.frames_mixed, primary_volume: g.primary_volume, listener: g.listener, voices }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    struct Capture(Arc<Mutex<Vec<f32>>>);
    impl drydock_audio::AudioSink for Capture {
        fn push(&mut self, stereo: &[f32]) {
            self.0.lock().unwrap().extend_from_slice(stereo);
        }
        fn name(&self) -> String {
            "capture".into()
        }
    }

    #[test]
    fn looping_buffer_is_audible_across_service_ticks() {
        let mut s = crate::testutil::session_with(&drydock_fixtures::exit_with(0));
        let out = Arc::new(Mutex::new(Vec::new()));
        s.host.audio = Some(Box::new(Capture(out.clone())));
        s.com.dsound.created = true;
        // 100 frames of mono 16-bit DC at half scale.
        let size = 200;
        let mem = s.mem.alloc(None, 0x1000, drydock_core::mem::RegionKind::Private, drydock_core::mem::PAGE_READWRITE, "dsb").unwrap();
        let data: Vec<u8> = (0..100).flat_map(|_| 0x4000i16.to_le_bytes()).collect();
        s.mem.write(mem, &data).unwrap();
        let b = DsBuffer {
            primary: false,
            flags: DSBCAPS_CTRLVOLUME,
            mem,
            size,
            fmt: Wfx { tag: 1, channels: 1, rate: 22050, bits: 16, block: 2 },
            mem_owner: 0,
            playing: true,
            looping: true,
            anchor_ns: 0,
            anchor_frame: 0,
            freq: 22050,
            volume: 0,
            pan: 0,
            notify: Vec::new(),
            last_pos: 0,
            p3d: Params3d::default(),
            pending_3d: None,
        };
        let id = s.com_new(ComKind::DSBuffer(Box::new(b)));
        s.com.dsound.buffers.insert(id);
        s.com.dsound.playing.insert(id);
        for k in 1..=20 {
            s.clock.advance_to(k * MIX_TICK_NS);
            service(&mut s);
        }
        let out = out.lock().unwrap();
        assert_eq!(out.len(), 2 * 4410, "100 ms of stereo output");
        let silent = out.iter().filter(|v| (**v - 0.5).abs() > 1e-3).count();
        assert_eq!(silent, 0, "every output sample carries the buffer's signal");
    }
}
