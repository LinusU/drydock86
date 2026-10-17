//! DirectMusic (DirectX 7): performance, ports, loader, composer.
//!
//! Performance time runs on the virtual clock (reference time in 100 ns units, music
//! time at the current tempo with 768 ticks per quarter note). The software synthesizer
//! port renders through DirectSound. Loading and playing music content (segments,
//! styles, bands, DLS collections) requires the corresponding parsers and the
//! synthesizer; those paths report a structured "missing" diagnostic when reached.

use super::*;
use crate::{export, iunknown, unimpl_method};
use serde::{Deserialize, Serialize};

pub const DMUS_E_NOT_FOUND: u32 = 0x8878_1209;
pub const DMUS_E_LOADER_FAILEDOPEN: u32 = 0x8878_1129;
pub const DMUS_E_LOADER_FORMATNOTSUPPORTED: u32 = 0x8878_112a;
pub const DMUS_E_NOT_INIT: u32 = 0x8878_1201;
pub const DMUS_E_ALREADY_INITED: u32 = 0x8878_1203;
pub const DMUS_E_NO_MASTER_CLOCK: u32 = 0x8878_1102;
pub const DMUS_S_PARTIALLOAD: u32 = 0x0878_0091;
pub const DMUS_E_TYPE_UNSUPPORTED: u32 = 0x8878_1210;
pub const DMUS_E_PORT_NOT_CAPTURE: u32 = 0x8878_1105;

pub const PPQ: u64 = 768;

pub const CLSID_DIRECTMUSIC: Guid = guid("636b9f10-0c7d-11d1-95b2-0020afdc7421");
pub const CLSID_PERFORMANCE: Guid = guid("d2ac2881-b39b-11d1-8704-00600893b1bd");
pub const CLSID_SEGMENT: Guid = guid("d2ac2882-b39b-11d1-8704-00600893b1bd");
pub const CLSID_COMPOSER: Guid = guid("d2ac2890-b39b-11d1-8704-00600893b1bd");
pub const CLSID_LOADER: Guid = guid("d2ac2892-b39b-11d1-8704-00600893b1bd");
pub const CLSID_STYLE: Guid = guid("d2ac288a-b39b-11d1-8704-00600893b1bd");
pub const CLSID_BAND: Guid = guid("79ba9e00-b6ee-11d1-86be-00c04fbf8fef");
pub const CLSID_COLLECTION: Guid = guid("480ff4b0-28b2-11d1-bef7-00c04fbf8fef");
pub const CLSID_SYNTH: Guid = guid("58c2b4d0-46e7-11d1-89ac-00a0c9054129");
pub const GUID_ALL_TYPES: Guid = guid("d2ac2893-b39b-11d1-8704-00600893b1bd");
pub const GUID_PERF_MASTER_TEMPO: Guid = guid("d2ac28b0-b39b-11d1-8704-00600893b1bd");
pub const GUID_PERF_MASTER_VOLUME: Guid = guid("d2ac28b1-b39b-11d1-8704-00600893b1bd");
pub const GUID_PERF_MASTER_GROOVE: Guid = guid("d2ac28b2-b39b-11d1-8704-00600893b1bd");
pub const GUID_PERF_AUTO_DOWNLOAD: Guid = guid("fb09565b-3631-11d2-bcb8-00a0c922e6eb");

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DMusicGlobal {
    pub active: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Performance {
    pub inited: bool,
    pub dmusic: u32,
    /// Virtual time (ns) at Init: reference/music time zero.
    pub start_ns: u64,
    pub tempo: f32,
    pub volume: i32,
    pub auto_download: bool,
    pub ports: Vec<u32>,
    pub notify_event: u32,
    pub notify_types: Vec<Guid>,
    pub prepare_ms: u32,
    pub bumper_ms: u32,
    pub pchannel_blocks: Vec<(u32, u32, u32)>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Port {
    pub active: bool,
    pub groups: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Loader {
    /// Search directories per object class.
    pub dirs: Vec<(Guid, String)>,
    /// Files found by ScanDirectory: (class, guest path).
    pub scanned: Vec<(Guid, String)>,
    pub cache_enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DmKind {
    DirectMusic,
    Performance(Performance),
    Port(Port),
    Loader(Loader),
    Composer,
}

pub fn create_via_com(s: &mut Session, clsid: &Guid, iid: &Guid, ppv: u32) -> Option<ApiResult> {
    let (kind, defs): (DmKind, &[&'static IfaceDef]) = match *clsid {
        CLSID_DIRECTMUSIC => (DmKind::DirectMusic, &[&IDIRECTMUSIC, &IDIRECTMUSIC2]),
        CLSID_PERFORMANCE => (
            DmKind::Performance(Performance {
                tempo: 120.0,
                ..Default::default()
            }),
            &[&IDIRECTMUSICPERFORMANCE, &IDIRECTMUSICPERFORMANCE2],
        ),
        CLSID_LOADER => (
            DmKind::Loader(Loader {
                cache_enabled: true,
                ..Default::default()
            }),
            &[&IDIRECTMUSICLOADER],
        ),
        CLSID_COMPOSER => (DmKind::Composer, &[&IDIRECTMUSICCOMPOSER]),
        CLSID_SEGMENT | CLSID_STYLE | CLSID_BAND | CLSID_COLLECTION | CLSID_SYNTH => {
            s.missing("DirectMusic", format!("creating class {} directly", guid_str(clsid)));
            return Some(Ok(ApiRet::Ret(super::ole::REGDB_E_CLASSNOTREG)));
        }
        _ => return None,
    };
    let def = if *iid == IID_IUNKNOWN {
        Some(defs[0])
    } else {
        defs.iter().copied().find(|d| d.iid == *iid)
    };
    let Some(def) = def else {
        return Some(Ok(ApiRet::Ret(E_NOINTERFACE)));
    };
    let id = s.com_new(ComKind::DMusic(kind));
    Some(s.out_iface(ppv, id, def).map(|_| ApiRet::Ret(S_OK)))
}

pub fn supported(k: &DmKind) -> Vec<&'static IfaceDef> {
    match k {
        DmKind::DirectMusic => vec![&IDIRECTMUSIC, &IDIRECTMUSIC2],
        DmKind::Performance(_) => vec![&IDIRECTMUSICPERFORMANCE, &IDIRECTMUSICPERFORMANCE2],
        DmKind::Port(_) => vec![&IDIRECTMUSICPORT],
        DmKind::Loader(_) => vec![&IDIRECTMUSICLOADER],
        DmKind::Composer => vec![&IDIRECTMUSICCOMPOSER],
    }
}

fn dm_self(s: &Session) -> Result<(u32, DmKind), ApiError> {
    let id = s.this_obj()?;
    match &s.com_obj(id).unwrap().kind {
        ComKind::DMusic(k) => Ok((id, k.clone())),
        _ => Err(ApiError::Fatal("not a DirectMusic object".into())),
    }
}

fn set_kind(s: &mut Session, id: u32, k: DmKind) {
    if let Some(o) = s.com_obj_mut(id) {
        o.kind = ComKind::DMusic(k);
    }
}

fn perf_self(s: &Session) -> Result<(u32, Performance), ApiError> {
    match dm_self(s)? {
        (id, DmKind::Performance(p)) => Ok((id, p)),
        _ => Err(ApiError::Fatal("not a performance".into())),
    }
}

// ---------------------------------------------------------------- IDirectMusic

const SYNTH_PORT_GUID: Guid = guid("58c2b4d0-46e7-11d1-89ac-00a0c9054129");

fn portcaps() -> Vec<u8> {
    let mut b = vec![0u8; 304];
    let p = |b: &mut Vec<u8>, o: usize, v: u32| b[o..o + 4].copy_from_slice(&v.to_le_bytes());
    p(&mut b, 0, 304);
    // DLS | SOFTWARESYNTH | DIRECTSOUND | SHAREABLE | DLS2 (DX7 synth capabilities)
    p(&mut b, 4, 0x1 | 0x4 | 0x80 | 0x100);
    b[8..24].copy_from_slice(&SYNTH_PORT_GUID);
    p(&mut b, 24, 1); // DMUS_PC_OUTPUTCLASS
    p(&mut b, 28, 1); // DMUS_PORT_USER_MODE_SYNTH
    p(&mut b, 32, 0xffff_ffff); // DMUS_PC_SYSTEMMEMORY
    p(&mut b, 36, 1000);
    p(&mut b, 40, 128);
    p(&mut b, 44, 2);
    let desc: Vec<u8> = "Microsoft Synthesizer"
        .encode_utf16()
        .flat_map(|c| c.to_le_bytes())
        .collect();
    b[52..52 + desc.len()].copy_from_slice(&desc);
    b
}

fn dm_enum_port(s: &mut Session) -> ApiResult {
    let [_this, idx, p] = s.args::<3>()?;
    if idx > 0 {
        return Ok(ApiRet::Ret(S_FALSE));
    }
    let size = s.mem.read_u32(p)?;
    let b = portcaps();
    s.mem.write(p, &b[..(size as usize).min(b.len())])?;
    Ok(ApiRet::Ret(S_OK))
}

fn dm_create_port(s: &mut Session) -> ApiResult {
    let [_this, rclsid, params, pp, outer] = s.args::<5>()?;
    if outer != 0 {
        return Ok(ApiRet::Ret(0x8004_0110));
    }
    let g = s.read_guid(rclsid)?;
    if g != SYNTH_PORT_GUID && g != [0u8; 16] {
        return Ok(ApiRet::Ret(E_INVALIDARG));
    }
    let groups = if params != 0 && s.mem.read_u32(params + 4)? & 0x4 != 0 {
        s.mem.read_u32(params + 16)?.max(1)
    } else {
        1
    };
    let id = s.com_new(ComKind::DMusic(DmKind::Port(Port { active: false, groups })));
    s.out_iface(pp, id, &IDIRECTMUSICPORT)?;
    Ok(ApiRet::Ret(S_OK))
}

fn dm_activate(s: &mut Session) -> ApiResult {
    let [_this, en] = s.args::<2>()?;
    s.com.dmusic.active = en != 0;
    Ok(ApiRet::Ret(S_OK))
}

fn dm_get_default_port(s: &mut Session) -> ApiResult {
    let [_this, p] = s.args::<2>()?;
    s.mem.write(p, &SYNTH_PORT_GUID)?;
    Ok(ApiRet::Ret(S_OK))
}

fn dm_set_direct_sound(_s: &mut Session) -> ApiResult {
    Ok(ApiRet::Ret(S_OK))
}

fn dm_enum_master_clock(s: &mut Session) -> ApiResult {
    let [_this, idx, p] = s.args::<3>()?;
    if idx > 0 {
        return Ok(ApiRet::Ret(S_FALSE));
    }
    let size = s.mem.read_u32(p)?;
    let mut b = vec![0u8; size.clamp(4, 300) as usize];
    b[0..4].copy_from_slice(&size.to_le_bytes());
    s.mem.write(p, &b)?;
    Ok(ApiRet::Ret(S_OK))
}

// ---------------------------------------------------------------- IDirectMusicPort

fn port_activate(s: &mut Session) -> ApiResult {
    let [_this, en] = s.args::<2>()?;
    let (id, k) = dm_self(s)?;
    if let DmKind::Port(mut p) = k {
        p.active = en != 0;
        set_kind(s, id, DmKind::Port(p));
    }
    Ok(ApiRet::Ret(S_OK))
}

fn port_set_num_channel_groups(s: &mut Session) -> ApiResult {
    let [_this, n] = s.args::<2>()?;
    let (id, k) = dm_self(s)?;
    if let DmKind::Port(mut p) = k {
        p.groups = n;
        set_kind(s, id, DmKind::Port(p));
    }
    Ok(ApiRet::Ret(S_OK))
}

fn port_get_num_channel_groups(s: &mut Session) -> ApiResult {
    let [_this, pn] = s.args::<2>()?;
    let (_id, k) = dm_self(s)?;
    let n = match k {
        DmKind::Port(p) => p.groups,
        _ => 0,
    };
    s.mem.write_u32(pn, n)?;
    Ok(ApiRet::Ret(S_OK))
}

fn port_get_caps(s: &mut Session) -> ApiResult {
    let [_this, p] = s.args::<2>()?;
    let size = s.mem.read_u32(p)?;
    let b = portcaps();
    s.mem.write(p, &b[..(size as usize).min(b.len())])?;
    Ok(ApiRet::Ret(S_OK))
}

/// Output format of the software synthesizer (22.05 kHz, 16-bit stereo).
fn port_get_format(s: &mut Session) -> ApiResult {
    let [_this, pwfx, psize, pbuf] = s.args::<4>()?;
    let wfx: Vec<u8> = [
        &1u16.to_le_bytes()[..],
        &2u16.to_le_bytes(),
        &22050u32.to_le_bytes(),
        &(22050u32 * 4).to_le_bytes(),
        &4u16.to_le_bytes(),
        &16u16.to_le_bytes(),
        &0u16.to_le_bytes(),
    ]
    .concat();
    if pwfx == 0 {
        if psize != 0 {
            s.mem.write_u32(psize, wfx.len() as u32)?;
        }
    } else {
        let cap = if psize != 0 {
            s.mem.read_u32(psize)?
        } else {
            wfx.len() as u32
        };
        let n = (cap as usize).min(wfx.len());
        s.mem.write(pwfx, &wfx[..n])?;
        if psize != 0 {
            s.mem.write_u32(psize, n as u32)?;
        }
    }
    if pbuf != 0 {
        // One second of synthesizer output.
        s.mem.write_u32(pbuf, 22050 * 4)?;
    }
    Ok(ApiRet::Ret(S_OK))
}

fn port_set_direct_sound(_s: &mut Session) -> ApiResult {
    Ok(ApiRet::Ret(S_OK))
}

// ---------------------------------------------------------------- performance

impl Session {
    /// Reference time (100 ns) of a performance.
    fn perf_rt(&self, p: &Performance) -> u64 {
        self.clock.now_ns().saturating_sub(p.start_ns) / 100
    }
}

fn mt_of(p: &Performance, rt: u64) -> u64 {
    // ticks = seconds * beats/second * PPQ
    (rt as f64 / 1e7 * p.tempo as f64 / 60.0 * PPQ as f64) as u64
}

fn perf_init(s: &mut Session) -> ApiResult {
    let [_this, ppdm, _ds, _hwnd] = s.args::<4>()?;
    let (id, mut p) = perf_self(s)?;
    if p.inited {
        return Ok(ApiRet::Ret(DMUS_E_ALREADY_INITED));
    }
    // Use the caller's IDirectMusic if provided, else create one (returned if asked).
    let dm = if ppdm != 0 && s.mem.read_u32(ppdm)? != 0 {
        let ptr = s.mem.read_u32(ppdm)?;
        let o = s.obj_of(ptr).unwrap_or(0);
        s.com_addref(o);
        o
    } else {
        let o = s.com_new(ComKind::DMusic(DmKind::DirectMusic));
        if ppdm != 0 {
            s.out_iface(ppdm, o, &IDIRECTMUSIC)?;
            s.com_addref(o);
        }
        o
    };
    p.inited = true;
    p.dmusic = dm;
    p.start_ns = s.clock.now_ns();
    p.prepare_ms = 1000;
    p.bumper_ms = 50;
    set_kind(s, id, DmKind::Performance(p));
    Ok(ApiRet::Ret(S_OK))
}

fn perf_add_port(s: &mut Session) -> ApiResult {
    let [_this, port] = s.args::<2>()?;
    let (id, mut p) = perf_self(s)?;
    if !p.inited {
        return Ok(ApiRet::Ret(DMUS_E_NOT_INIT));
    }
    let o = if port == 0 {
        // NULL: the default software synthesizer port, activated by the performance.
        s.com_new(ComKind::DMusic(DmKind::Port(Port {
            active: true,
            groups: 1,
        })))
    } else {
        let o = s.obj_of(port).unwrap_or(0);
        s.com_addref(o);
        o
    };
    p.ports.push(o);
    if port == 0 {
        p.pchannel_blocks.push((0, o, 1));
    }
    set_kind(s, id, DmKind::Performance(p));
    Ok(ApiRet::Ret(S_OK))
}

fn perf_assign_pchannel_block(s: &mut Session) -> ApiResult {
    let [_this, block, port, group] = s.args::<4>()?;
    let (id, mut p) = perf_self(s)?;
    let o = s.obj_of(port).unwrap_or(0);
    if !p.ports.contains(&o) {
        return Ok(ApiRet::Ret(E_INVALIDARG));
    }
    p.pchannel_blocks.retain(|b| b.0 != block);
    p.pchannel_blocks.push((block, o, group));
    set_kind(s, id, DmKind::Performance(p));
    Ok(ApiRet::Ret(S_OK))
}

fn perf_set_notification_handle(s: &mut Session) -> ApiResult {
    let [_this, h, _lo, _hi] = s.args::<4>()?;
    let (id, mut p) = perf_self(s)?;
    p.notify_event = h;
    set_kind(s, id, DmKind::Performance(p));
    Ok(ApiRet::Ret(S_OK))
}

fn perf_add_notification_type(s: &mut Session) -> ApiResult {
    let [_this, g] = s.args::<2>()?;
    let (id, mut p) = perf_self(s)?;
    let g = s.read_guid(g)?;
    if !p.notify_types.contains(&g) {
        p.notify_types.push(g);
    }
    set_kind(s, id, DmKind::Performance(p));
    Ok(ApiRet::Ret(S_OK))
}

fn perf_remove_notification_type(s: &mut Session) -> ApiResult {
    let [_this, g] = s.args::<2>()?;
    let (id, mut p) = perf_self(s)?;
    let g = s.read_guid(g)?;
    let n = p.notify_types.len();
    p.notify_types.retain(|x| *x != g);
    let r = if n == p.notify_types.len() { S_FALSE } else { S_OK };
    set_kind(s, id, DmKind::Performance(p));
    Ok(ApiRet::Ret(r))
}

fn perf_get_notification_pmsg(s: &mut Session) -> ApiResult {
    let [_this, pp] = s.args::<2>()?;
    // No segments are playing, so no notifications are pending.
    s.mem.write_u32(pp, 0)?;
    Ok(ApiRet::Ret(S_FALSE))
}

fn perf_get_time(s: &mut Session) -> ApiResult {
    let [_this, prt, pmt] = s.args::<3>()?;
    let (_id, p) = perf_self(s)?;
    let rt = s.perf_rt(&p);
    if prt != 0 {
        s.mem.write_u64(prt, rt)?;
    }
    if pmt != 0 {
        s.mem.write_u32(pmt, mt_of(&p, rt) as u32)?;
    }
    Ok(ApiRet::Ret(S_OK))
}

fn perf_get_latency_time(s: &mut Session) -> ApiResult {
    let [_this, prt] = s.args::<2>()?;
    let (_id, p) = perf_self(s)?;
    let rt = s.perf_rt(&p) + 100_000; // 10 ms synth latency
    s.mem.write_u64(prt, rt)?;
    Ok(ApiRet::Ret(S_OK))
}

fn perf_get_queue_time(s: &mut Session) -> ApiResult {
    let [_this, prt] = s.args::<2>()?;
    let (_id, p) = perf_self(s)?;
    let rt = s.perf_rt(&p) + 100_000 + p.bumper_ms as u64 * 10_000;
    s.mem.write_u64(prt, rt)?;
    Ok(ApiRet::Ret(S_OK))
}

fn read_global(s: &Session, p: u32, size: u32) -> Result<Vec<u8>, ApiError> {
    Ok(s.mem.read_vec(p, size.min(64))?)
}

fn perf_set_global_param(s: &mut Session) -> ApiResult {
    let [_this, g, pp, size] = s.args::<4>()?;
    let (id, mut p) = perf_self(s)?;
    let g = s.read_guid(g)?;
    let v = read_global(s, pp, size)?;
    match g {
        GUID_PERF_MASTER_TEMPO if v.len() >= 4 => p.tempo = 120.0 * f32::from_le_bytes(v[..4].try_into().unwrap()),
        GUID_PERF_MASTER_VOLUME if v.len() >= 4 => p.volume = i32::from_le_bytes(v[..4].try_into().unwrap()),
        GUID_PERF_AUTO_DOWNLOAD if !v.is_empty() => p.auto_download = v[0] != 0,
        GUID_PERF_MASTER_GROOVE => {}
        _ => {
            s.missing("DirectMusic", format!("SetGlobalParam {}", guid_str(&g)));
        }
    }
    set_kind(s, id, DmKind::Performance(p));
    Ok(ApiRet::Ret(S_OK))
}

fn perf_get_global_param(s: &mut Session) -> ApiResult {
    let [_this, g, pp, size] = s.args::<4>()?;
    let (_id, p) = perf_self(s)?;
    let g = s.read_guid(g)?;
    let v: Vec<u8> = match g {
        GUID_PERF_MASTER_TEMPO => (p.tempo / 120.0).to_le_bytes().to_vec(),
        GUID_PERF_MASTER_VOLUME => p.volume.to_le_bytes().to_vec(),
        GUID_PERF_AUTO_DOWNLOAD => vec![p.auto_download as u8, 0, 0, 0],
        _ => return Ok(ApiRet::Ret(E_INVALIDARG)),
    };
    let n = (size as usize).min(v.len());
    s.mem.write(pp, &v[..n])?;
    Ok(ApiRet::Ret(S_OK))
}

fn perf_set_prepare_time(s: &mut Session) -> ApiResult {
    let [_this, ms] = s.args::<2>()?;
    let (id, mut p) = perf_self(s)?;
    p.prepare_ms = ms;
    set_kind(s, id, DmKind::Performance(p));
    Ok(ApiRet::Ret(S_OK))
}
fn perf_set_bumper_length(s: &mut Session) -> ApiResult {
    let [_this, ms] = s.args::<2>()?;
    let (id, mut p) = perf_self(s)?;
    p.bumper_ms = ms;
    set_kind(s, id, DmKind::Performance(p));
    Ok(ApiRet::Ret(S_OK))
}

fn perf_is_playing(_s: &mut Session) -> ApiResult {
    // Nothing can be playing: segments are never started (see module docs).
    Ok(ApiRet::Ret(S_FALSE))
}

fn perf_stop(_s: &mut Session) -> ApiResult {
    Ok(ApiRet::Ret(S_OK))
}

fn perf_close_down(s: &mut Session) -> ApiResult {
    let (id, mut p) = perf_self(s)?;
    for port in std::mem::take(&mut p.ports) {
        s.com_release(port);
    }
    if p.dmusic != 0 {
        s.com_release(p.dmusic);
        p.dmusic = 0;
    }
    p.inited = false;
    set_kind(s, id, DmKind::Performance(p));
    Ok(ApiRet::Ret(S_OK))
}

fn perf_play_segment(s: &mut Session) -> ApiResult {
    let [_this, seg, flags] = s.args::<3>()?;
    s.missing("DirectMusic", format!("PlaySegment({seg:#x}, flags {flags:#x})"));
    unimpl("DirectMusic segment playback")
}

fn perf_music_to_reference_time(s: &mut Session) -> ApiResult {
    let [_this, mt, prt] = s.args::<3>()?;
    let (_id, p) = perf_self(s)?;
    let rt = (mt as f64 / PPQ as f64 * 60.0 / p.tempo as f64 * 1e7) as u64;
    s.mem.write_u64(prt, rt)?;
    Ok(ApiRet::Ret(S_OK))
}

fn perf_reference_to_music_time(s: &mut Session) -> ApiResult {
    let [_this, lo, hi, pmt] = s.args::<4>()?;
    let (_id, p) = perf_self(s)?;
    let rt = (hi as u64) << 32 | lo as u64;
    s.mem.write_u32(pmt, mt_of(&p, rt) as u32)?;
    Ok(ApiRet::Ret(S_OK))
}

// ---------------------------------------------------------------- loader

fn loader_self(s: &Session) -> Result<(u32, Loader), ApiError> {
    match dm_self(s)? {
        (id, DmKind::Loader(l)) => Ok((id, l)),
        _ => Err(ApiError::Fatal("not a loader".into())),
    }
}

fn loader_set_search_directory(s: &mut Session) -> ApiResult {
    let [_this, rguid, pwz, clear] = s.args::<4>()?;
    let (id, mut l) = loader_self(s)?;
    let class = s.read_guid(rguid)?;
    let path = s.read_wstr(pwz)?;
    let Some(gp) = s.guest_path(&path) else {
        return Ok(ApiRet::Ret(E_INVALIDARG));
    };
    if !s.vfs.exists_dir(&gp) {
        return Ok(ApiRet::Ret(DMUS_E_LOADER_FAILEDOPEN));
    }
    if clear != 0 {
        l.scanned.retain(|(c, _)| *c != class && class != GUID_ALL_TYPES);
    }
    let same = l
        .dirs
        .iter()
        .any(|(c, d)| *c == class && d.eq_ignore_ascii_case(&gp.display()));
    l.dirs.retain(|(c, _)| *c != class);
    l.dirs.push((class, gp.display()));
    set_kind(s, id, DmKind::Loader(l));
    Ok(ApiRet::Ret(if same { S_FALSE } else { S_OK }))
}

fn loader_scan_directory(s: &mut Session) -> ApiResult {
    let [_this, rguid, pwz_ext, _pwz_cache] = s.args::<4>()?;
    let (id, mut l) = loader_self(s)?;
    let class = s.read_guid(rguid)?;
    let ext = s.read_wstr(pwz_ext)?.trim_start_matches('.').to_lowercase();
    let dir = l
        .dirs
        .iter()
        .rev()
        .find(|(c, _)| *c == class || *c == GUID_ALL_TYPES)
        .map(|(_, d)| d.clone());
    let Some(dir) = dir else {
        return Ok(ApiRet::Ret(DMUS_E_NOT_FOUND));
    };
    let gp = s.guest_path(&dir).unwrap();
    let mut found = 0;
    for e in s.vfs.list_dir(&gp) {
        if e.attrs & crate::vfs::FILE_ATTRIBUTE_DIRECTORY != 0 {
            continue;
        }
        let matches = ext == "*" || e.name.to_lowercase().rsplit('.').next() == Some(ext.as_str());
        if matches {
            l.scanned.push((class, gp.join(&e.name).display()));
            found += 1;
        }
    }
    set_kind(s, id, DmKind::Loader(l));
    // S_FALSE: the scan found no files of this type.
    Ok(ApiRet::Ret(if found == 0 { S_FALSE } else { S_OK }))
}

/// Decoded DMUS_OBJECTDESC (DirectX 7 layout).
#[derive(Debug, Clone, Default)]
struct ObjDesc {
    valid: u32,
    object: Guid,
    class: Guid,
    name: String,
    category: String,
    file: String,
}

fn read_objdesc(s: &Session, p: u32) -> Result<ObjDesc, ApiError> {
    let valid = s.mem.read_u32(p + 4)?;
    let w = |o: u32, n: u32| -> Result<String, ApiError> {
        let raw = s.mem.read_vec(p + o, n * 2)?;
        let v: Vec<u16> = raw
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .take_while(|&c| c != 0)
            .collect();
        Ok(String::from_utf16_lossy(&v))
    };
    Ok(ObjDesc {
        valid,
        object: if valid & 0x1 != 0 { s.read_guid(p + 8)? } else { [0; 16] },
        class: if valid & 0x2 != 0 {
            s.read_guid(p + 24)?
        } else {
            [0; 16]
        },
        name: if valid & 0x10 != 0 { w(56, 64)? } else { String::new() },
        category: if valid & 0x20 != 0 { w(184, 64)? } else { String::new() },
        file: if valid & 0x40 != 0 { w(312, 260)? } else { String::new() },
    })
}

pub const GUID_DEFAULT_GM_COLLECTION: Guid = guid("f17e8673-c3b4-11d1-870b-00600893b1bd");

fn loader_get_object(s: &mut Session) -> ApiResult {
    let [_this, pdesc, _riid, ppv] = s.args::<4>()?;
    let d = read_objdesc(s, pdesc)?;
    s.mem.write_u32(ppv, 0)?;
    if d.object == GUID_DEFAULT_GM_COLLECTION {
        // The default GM collection is the gm.dls file of a DirectX installation, which
        // is not part of this emulated system; loading it fails as when the file is absent.
        s.event(
            crate::diag::Severity::Warning,
            "DirectMusic",
            "default GM DLS collection requested; no gm.dls is installed".into(),
        );
        return Ok(ApiRet::Ret(DMUS_E_LOADER_FAILEDOPEN));
    }
    s.missing(
        "DirectMusic",
        format!(
            "loading content: object {} class {} name {:?} file {:?} (valid {:#x})",
            guid_str(&d.object),
            guid_str(&d.class),
            d.name,
            d.file,
            d.valid
        ),
    );
    let _ = &d.category;
    unimpl("DirectMusic content loading (segments, styles, bands, collections)")
}

fn loader_clear_cache(_s: &mut Session) -> ApiResult {
    Ok(ApiRet::Ret(S_OK))
}
fn loader_enable_cache(s: &mut Session) -> ApiResult {
    let [_this, _rguid, en] = s.args::<3>()?;
    let (id, mut l) = loader_self(s)?;
    let was = l.cache_enabled;
    l.cache_enabled = en != 0;
    set_kind(s, id, DmKind::Loader(l));
    Ok(ApiRet::Ret(if was == (en != 0) { S_FALSE } else { S_OK }))
}
fn loader_enum_object(s: &mut Session) -> ApiResult {
    let [_this, rguid, idx, pdesc] = s.args::<4>()?;
    let (_id, l) = loader_self(s)?;
    let class = s.read_guid(rguid)?;
    let list: Vec<&(Guid, String)> = l
        .scanned
        .iter()
        .filter(|(c, _)| *c == class || class == GUID_ALL_TYPES)
        .collect();
    let Some((c, path)) = list.get(idx as usize) else {
        return Ok(ApiRet::Ret(S_FALSE));
    };
    let size = s.mem.read_u32(pdesc)?;
    let mut b = vec![0u8; size.clamp(8, 0x370) as usize];
    b[0..4].copy_from_slice(&size.to_le_bytes());
    // DMUS_OBJ_CLASS | DMUS_OBJ_FILENAME | DMUS_OBJ_FULLPATH
    b[4..8].copy_from_slice(&(0x2u32 | 0x40 | 0x80).to_le_bytes());
    if b.len() >= 0x24 {
        b[0x14..0x24].copy_from_slice(c);
    }
    let w: Vec<u8> = path.encode_utf16().flat_map(|x| x.to_le_bytes()).collect();
    if b.len() >= 0x64 + w.len() + 2 {
        b[0x64..0x64 + w.len()].copy_from_slice(&w);
    }
    s.mem.write(pdesc, &b)?;
    Ok(ApiRet::Ret(S_OK))
}

// ---------------------------------------------------------------- composer

fn composer_auto_transition(s: &mut Session) -> ApiResult {
    let [_this, _perf, to_seg, cmd, flags] = s.args::<5>()?;
    s.missing(
        "DirectMusic",
        format!("AutoTransition(to {to_seg:#x}, cmd {cmd:#x}, flags {flags:#x})"),
    );
    unimpl("DirectMusic composition")
}

// ---------------------------------------------------------------- tables

pub static IDIRECTMUSIC: IfaceDef = IfaceDef {
    name: "IDirectMusic",
    iid: guid("6536115a-7b2d-11d2-ba18-0000f875ac12"),
    module: "DMUSIC.DLL",
    methods: &[
        iunknown!()[0],
        iunknown!()[1],
        iunknown!()[2],
        export!("EnumPort", 3, dm_enum_port),
        unimpl_method!("CreateMusicBuffer", 4),
        export!("CreatePort", 5, dm_create_port),
        export!("EnumMasterClock", 3, dm_enum_master_clock),
        unimpl_method!("GetMasterClock", 3),
        unimpl_method!("SetMasterClock", 2),
        export!("Activate", 2, dm_activate),
        export!("GetDefaultPort", 2, dm_get_default_port),
        export!("SetDirectSound", 3, dm_set_direct_sound),
    ],
};

pub static IDIRECTMUSIC2: IfaceDef = IfaceDef {
    name: "IDirectMusic2",
    iid: guid("6fc2cae1-bc78-11d2-afa6-00aa0024d8b6"),
    module: "DMUSIC.DLL",
    methods: &[
        iunknown!()[0],
        iunknown!()[1],
        iunknown!()[2],
        export!("EnumPort", 3, dm_enum_port),
        unimpl_method!("CreateMusicBuffer", 4),
        export!("CreatePort", 5, dm_create_port),
        export!("EnumMasterClock", 3, dm_enum_master_clock),
        unimpl_method!("GetMasterClock", 3),
        unimpl_method!("SetMasterClock", 2),
        export!("Activate", 2, dm_activate),
        export!("GetDefaultPort", 2, dm_get_default_port),
        export!("SetDirectSound", 3, dm_set_direct_sound),
        unimpl_method!("SetExternalMasterClock", 2),
    ],
};

pub static IDIRECTMUSICPORT: IfaceDef = IfaceDef {
    name: "IDirectMusicPort",
    iid: guid("08f2d8c9-37c2-11d2-b9f9-0000f875ac12"),
    module: "DMUSIC.DLL",
    methods: &[
        iunknown!()[0],
        iunknown!()[1],
        iunknown!()[2],
        unimpl_method!("PlayBuffer", 2),
        unimpl_method!("SetReadNotificationHandle", 2),
        unimpl_method!("Read", 2),
        unimpl_method!("DownloadInstrument", 5),
        unimpl_method!("UnloadInstrument", 2),
        unimpl_method!("GetLatencyClock", 2),
        unimpl_method!("GetRunningStats", 2),
        unimpl_method!("Compact", 1),
        export!("GetCaps", 2, port_get_caps),
        unimpl_method!("DeviceIoControl", 8),
        export!("SetNumChannelGroups", 2, port_set_num_channel_groups),
        export!("GetNumChannelGroups", 2, port_get_num_channel_groups),
        export!("Activate", 2, port_activate),
        unimpl_method!("SetChannelPriority", 4),
        unimpl_method!("GetChannelPriority", 4),
        export!("SetDirectSound", 3, port_set_direct_sound),
        export!("GetFormat", 4, port_get_format),
    ],
};

macro_rules! perf_methods {
    ($($extra:expr),*) => {
        &[
            iunknown!()[0],
            iunknown!()[1],
            iunknown!()[2],
            export!("Init", 4, perf_init),
            export!("PlaySegment", 6, perf_play_segment),
            export!("Stop", 5, perf_stop),
            unimpl_method!("GetSegmentState", 3),
            export!("SetPrepareTime", 2, perf_set_prepare_time),
            unimpl_method!("GetPrepareTime", 2),
            export!("SetBumperLength", 2, perf_set_bumper_length),
            unimpl_method!("GetBumperLength", 2),
            unimpl_method!("SendPMsg", 2),
            export!("MusicToReferenceTime", 3, perf_music_to_reference_time),
            export!("ReferenceToMusicTime", 4, perf_reference_to_music_time),
            export!("IsPlaying", 3, perf_is_playing),
            export!("GetTime", 3, perf_get_time),
            unimpl_method!("AllocPMsg", 3),
            unimpl_method!("FreePMsg", 2),
            unimpl_method!("GetGraph", 2),
            unimpl_method!("SetGraph", 2),
            export!("SetNotificationHandle", 4, perf_set_notification_handle),
            export!("GetNotificationPMsg", 2, perf_get_notification_pmsg),
            export!("AddNotificationType", 2, perf_add_notification_type),
            export!("RemoveNotificationType", 2, perf_remove_notification_type),
            export!("AddPort", 2, perf_add_port),
            unimpl_method!("RemovePort", 2),
            export!("AssignPChannelBlock", 4, perf_assign_pchannel_block),
            unimpl_method!("AssignPChannel", 5),
            unimpl_method!("PChannelInfo", 5),
            unimpl_method!("DownloadInstrument", 9),
            unimpl_method!("Invalidate", 3),
            unimpl_method!("GetParam", 7),
            unimpl_method!("SetParam", 6),
            export!("GetGlobalParam", 4, perf_get_global_param),
            export!("SetGlobalParam", 4, perf_set_global_param),
            export!("GetLatencyTime", 2, perf_get_latency_time),
            export!("GetQueueTime", 2, perf_get_queue_time),
            unimpl_method!("AdjustTime", 3),
            export!("CloseDown", 1, perf_close_down),
            unimpl_method!("GetResolvedTime", 5),
            unimpl_method!("MIDIToMusic", 6),
            unimpl_method!("MusicToMIDI", 6),
            unimpl_method!("TimeToRhythm", 7),
            unimpl_method!("RhythmToTime", 7)
            $(, $extra)*
        ]
    };
}

pub static IDIRECTMUSICPERFORMANCE: IfaceDef = IfaceDef {
    name: "IDirectMusicPerformance",
    iid: guid("07d43d03-6523-11d2-871d-00600893b1bd"),
    module: "DMUSIC.DLL",
    methods: perf_methods!(),
};

pub static IDIRECTMUSICPERFORMANCE2: IfaceDef = IfaceDef {
    name: "IDirectMusicPerformance2",
    iid: guid("6fc2cae0-bc78-11d2-afa6-00aa0024d8b6"),
    module: "DMUSIC.DLL",
    methods: perf_methods!(),
};

pub static IDIRECTMUSICLOADER: IfaceDef = IfaceDef {
    name: "IDirectMusicLoader",
    iid: guid("2ffaaca2-5dca-11d2-afa6-00aa0024d8b6"),
    module: "DMUSIC.DLL",
    methods: &[
        iunknown!()[0],
        iunknown!()[1],
        iunknown!()[2],
        export!("GetObject", 4, loader_get_object),
        unimpl_method!("SetObject", 2),
        export!("SetSearchDirectory", 4, loader_set_search_directory),
        export!("ScanDirectory", 4, loader_scan_directory),
        unimpl_method!("CacheObject", 2),
        unimpl_method!("ReleaseObject", 2),
        export!("ClearCache", 2, loader_clear_cache),
        export!("EnableCache", 3, loader_enable_cache),
        export!("EnumObject", 4, loader_enum_object),
    ],
};

pub static IDIRECTMUSICCOMPOSER: IfaceDef = IfaceDef {
    name: "IDirectMusicComposer",
    iid: guid("d2ac28bf-b39b-11d1-8704-00600893b1bd"),
    module: "DMUSIC.DLL",
    methods: &[
        iunknown!()[0],
        iunknown!()[1],
        iunknown!()[2],
        unimpl_method!("ComposeSegmentFromTemplate", 6),
        unimpl_method!("ComposeSegmentFromShape", 9),
        unimpl_method!("ComposeTransition", 8),
        export!("AutoTransition", 9, composer_auto_transition),
        unimpl_method!("ComposeTemplateFromShape", 7),
        unimpl_method!("ChangeChordMap", 4),
    ],
};

pub static DMUSIC_EXPORTS: &[ExportDef] = &[];
