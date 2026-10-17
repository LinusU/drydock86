//! msvfw32: the MCIWnd movie window (Video for Windows).
//!
//! `MCIWndCreate` creates a window of the "MCIWndClass" class whose procedure is a
//! builtin thunk. Opening a file parses the AVI container; playback then runs on the
//! virtual clock, independent of the guest thread (Windows plays AVI files from a
//! separate MCI thread): the next frame's due time is a scheduler event, due frames
//! are decoded and drawn into the window's area of the scanned-out surface, and the
//! PCM sound track is rendered by the shared audio mixer.
//!
//! Supported content: Indeo 5 (`IV50`) video with optional PCM audio. Anything else
//! fails to open with a "missing" diagnostic, and the MCI commands then fail.
//!
//! Decoded pictures are a host-side cache. Checkpoints keep the playback position;
//! after a restore the decoder is rebuilt by decoding from the first frame again,
//! which reproduces the same pictures.

use super::user32::*;
use crate::api::*;
use crate::diag::Severity;
use crate::session::Session;
use crate::surface::Fmt;
use crate::vfs::Disposition;
use crate::{cdecl, export};
use drydock_media::avi::{Avi, Kind, WaveFormat};
use drydock_media::indeo5::{Decoder, Frame};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

const CLASS: &str = "MCIWNDCLASS";

// MCI commands sent as window messages, and MCIWnd messages.
const MCI_PLAY: u32 = 0x0806;
const MCI_STOP: u32 = 0x0808;
const MCI_PAUSE: u32 = 0x0809;
const MCI_RESUME: u32 = 0x0855;
const MCIWNDM_GETMODEA: u32 = WM_USER + 106;
const MCIWNDM_GETLENGTH: u32 = WM_USER + 104;
const MCIWNDM_GETPOSITIONA: u32 = WM_USER + 102;

const MCI_MODE_NOT_READY: u32 = 0x020c;
const MCI_MODE_STOP: u32 = 0x020d;
const MCI_MODE_PLAY: u32 = 0x020e;
const MCI_MODE_PAUSE: u32 = 0x0211;

/// MCIERR_DEVICE_NOT_READY: no media is open.
const MCIERR_DEVICE_NOT_READY: u32 = 0x0114;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Mode {
    NotReady,
    Stopped,
    Playing,
    Paused,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VfwState {
    /// File named in MCIWndCreate, opened by the new window's WM_CREATE.
    pending_open: Option<String>,
    pub players: BTreeMap<u32, Player>,
}

/// One MCIWnd window's movie.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Player {
    /// The file as named by the guest (resolved against the current directory at open).
    pub path: String,
    pub mode: Mode,
    /// Movie position (ns) at virtual time `anchor_ns`; it advances while playing.
    pub pos_ns: u64,
    pub anchor_ns: u64,
    /// Video frame currently on display.
    pub shown: Option<usize>,
    #[serde(skip)]
    media: MediaCache,
}

/// Parsed file, decoder and decoded pictures. Not part of checkpoints: a cloned or
/// restored player starts with an empty cache and rebuilds it deterministically.
#[derive(Default)]
struct MediaCache(Option<Box<Media>>);

impl Clone for MediaCache {
    fn clone(&self) -> Self {
        MediaCache(None)
    }
}

impl std::fmt::Debug for MediaCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.0.is_some() {
            "MediaCache(loaded)"
        } else {
            "MediaCache(empty)"
        })
    }
}

struct Media {
    data: Vec<u8>,
    /// Data chunks of the video stream, one per frame.
    video: Vec<drydock_media::avi::Chunk>,
    width: u16,
    height: u16,
    /// Frame duration as a fraction of seconds: `scale / rate`.
    scale: u64,
    rate: u64,
    frames: usize,
    decoder: Decoder,
    /// Last chunk passed to the decoder, and the picture on display after it.
    decoded: Option<usize>,
    picture: Option<Frame>,
    rgb: Vec<u8>,
    audio: Option<(WaveFormat, Vec<u8>)>,
}

impl Media {
    fn open(data: Vec<u8>) -> Result<Media, String> {
        let avi = Avi::parse(&data).map_err(|e| e.to_string())?;
        let vs = avi
            .streams
            .iter()
            .find(|s| s.kind == Kind::Video)
            .ok_or("no video stream")?;
        let bi = vs.bitmap_info().ok_or("video stream without BITMAPINFOHEADER")?;
        if &bi.compression != b"IV50" && &vs.handler.to_ascii_uppercase() != b"IV50" {
            return Err(format!(
                "video codec {:?} (only Indeo 5, IV50, is supported)",
                String::from_utf8_lossy(&bi.compression)
            ));
        }
        let (width, height) = (bi.width as u16, bi.height.unsigned_abs() as u16);
        let decoder = Decoder::new(width, height).map_err(|e| e.to_string())?;
        let mut audio = None;
        if let Some(a) = avi.streams.iter().find(|s| s.kind == Kind::Audio) {
            let wf = a.wave_format().ok_or("audio stream without WAVEFORMATEX")?;
            if wf.format_tag != 1 || !matches!(wf.bits_per_sample, 8 | 16) || !matches!(wf.channels, 1 | 2) {
                return Err(format!(
                    "audio format tag {} ({} channels, {} bits; only PCM is supported)",
                    wf.format_tag, wf.channels, wf.bits_per_sample
                ));
            }
            let mut pcm = Vec::new();
            for c in &a.chunks {
                pcm.extend_from_slice(&data[c.offset..c.offset + c.size]);
            }
            audio = Some((wf, pcm));
        }
        let frames = (vs.length as usize).min(vs.chunks.len());
        if vs.rate == 0 || vs.scale == 0 || frames == 0 {
            return Err("video stream without frames".into());
        }
        Ok(Media {
            video: vs.chunks.clone(),
            width,
            height,
            scale: vs.scale as u64,
            rate: vs.rate as u64,
            frames,
            decoder,
            decoded: None,
            picture: None,
            rgb: Vec::new(),
            audio,
            data,
        })
    }

    /// Start time of frame `i` in ns.
    fn frame_ns(&self, i: usize) -> u64 {
        (i as u128 * self.scale as u128 * 1_000_000_000 / self.rate as u128) as u64
    }

    fn duration_ns(&self) -> u64 {
        self.frame_ns(self.frames)
    }

    /// The frame on display at movie position `pos`.
    fn frame_at(&self, pos: u64) -> usize {
        let i = (pos as u128 * self.rate as u128 / (self.scale as u128 * 1_000_000_000)) as usize;
        i.min(self.frames - 1)
    }

    /// Decodes up to frame `i` (from the start if the decoder is past it).
    fn decode_to(&mut self, i: usize) -> Result<(), String> {
        if self.decoded.is_some_and(|d| d > i) {
            self.decoder = Decoder::new(self.width, self.height).map_err(|e| e.to_string())?;
            self.decoded = None;
            self.picture = None;
        }
        let start = self.decoded.map_or(0, |d| d + 1);
        if start > i {
            return Ok(());
        }
        for k in start..=i {
            let c = self.video[k];
            self.decoded = Some(k);
            // An empty chunk repeats the previous picture, as does a null frame.
            if c.size == 0 {
                continue;
            }
            match self.decoder.decode(&self.data[c.offset..c.offset + c.size]) {
                Ok(Some(f)) => self.picture = Some(f),
                Ok(None) => {}
                Err(e) => return Err(format!("frame {k}: {e}")),
            }
        }
        self.rgb = self.picture.as_ref().map(Frame::to_rgb24).unwrap_or_default();
        Ok(())
    }
}

impl Player {
    /// Movie position at virtual time `now`.
    fn pos_at(&self, now: u64) -> u64 {
        match self.mode {
            Mode::Playing => self.pos_ns + now.saturating_sub(self.anchor_ns),
            _ => self.pos_ns,
        }
    }
}

// ---------------------------------------------------------------- media loading

fn read_guest_file(s: &mut Session, name: &str) -> Result<Vec<u8>, String> {
    let gp = s.guest_path(name).ok_or_else(|| format!("invalid path {name:?}"))?;
    let now = s.clock.now_ns();
    let (mut f, _) = s
        .vfs
        .open(&gp, true, false, Disposition::OpenExisting, now)
        .map_err(|e| format!("{}: {e:?}", gp.display()))?;
    let mut data = vec![0u8; s.vfs.file_size(&f) as usize];
    let n = s.vfs.read(&mut f, &mut data).map_err(|e| format!("{e:?}"))?;
    data.truncate(n);
    Ok(data)
}

/// Makes sure the player's media is loaded; reports and closes it if that fails.
fn ensure_media(s: &mut Session, hwnd: u32) -> bool {
    let Some(p) = s.vfw.players.get(&hwnd) else {
        return false;
    };
    if p.media.0.is_some() {
        return true;
    }
    if p.mode == Mode::NotReady {
        return false;
    }
    let path = p.path.clone();
    match read_guest_file(s, &path).and_then(Media::open) {
        Ok(m) => {
            s.vfw.players.get_mut(&hwnd).unwrap().media.0 = Some(Box::new(m));
            true
        }
        Err(e) => {
            s.missing("MCIWnd", format!("cannot play {path}: {e}"));
            let p = s.vfw.players.get_mut(&hwnd).unwrap();
            p.mode = Mode::NotReady;
            false
        }
    }
}

// ---------------------------------------------------------------- exports

fn register_class(s: &mut Session) {
    if s.user.classes.contains_key(CLASS) {
        return;
    }
    let wndproc = s.thunks.find("MSVFW32.DLL", "MCIWndProc").unwrap_or(0);
    let atom = s.user.next_atom;
    s.user.next_atom += 1;
    s.user.classes.insert(
        CLASS.into(),
        WndClass {
            name: "MCIWndClass".into(),
            style: 0x3, // CS_HREDRAW | CS_VREDRAW
            wndproc,
            cls_extra: 0,
            wnd_extra: 4,
            hinst: 0,
            icon: 0,
            cursor: 0,
            brush: 0,
            menu: 0,
            atom,
        },
    );
}

/// Process attach: registers the "MCIWndClass" window class.
pub fn on_load(s: &mut Session) {
    register_class(s);
}

fn mciwnd_register_class(s: &mut Session) -> ApiResult {
    register_class(s);
    Ok(ApiRet::Ret(1))
}

/// MCIWndCreate(hwndParent, hInstance, dwStyle, szFile) (cdecl).
fn mciwnd_create_a(s: &mut Session) -> ApiResult {
    let [parent, hinst, style, file] = s.args::<4>()?;
    register_class(s);
    let hinst = if hinst == 0 { s.process.image_base } else { hinst };
    // As on Windows: visible, with a border inside a parent, else an overlapped window.
    let style = if parent != 0 {
        style | WS_VISIBLE | WS_BORDER
    } else {
        style | WS_VISIBLE | WS_CAPTION | WS_THICKFRAME | 0x0008_0000 | 0x0002_0000 | 0x0001_0000
    };
    s.vfw.pending_open = if file != 0 { Some(s.read_str(file)?) } else { None };
    let Some(create) = s.thunks.find("USER32.DLL", "CreateWindowExA") else {
        return Err(ApiError::Fatal("CreateWindowExA thunk missing".into()));
    };
    let cw = 0x8000_0000u32; // CW_USEDEFAULT
    Ok(ApiRet::call(
        create,
        vec![
            0.into(),
            CallArg::Buf(b"MCIWndClass\0".to_vec()),
            0.into(),
            style.into(),
            cw.into(),
            cw.into(),
            0.into(),
            0.into(),
            parent.into(),
            0.into(),
            hinst.into(),
            0.into(),
        ],
        Cont::ReturnValue,
    ))
}

fn mciwnd_create_w(s: &mut Session) -> ApiResult {
    let [_parent, _hinst, _style, _file] = s.args::<4>()?;
    unimpl("MCIWndCreateW")
}

// ---------------------------------------------------------------- the window

/// Opens the pending file for a new window and sizes the window to the movie.
fn on_create(s: &mut Session, hwnd: u32) {
    let Some(path) = s.vfw.pending_open.take() else {
        s.vfw.players.insert(
            hwnd,
            Player {
                path: String::new(),
                mode: Mode::NotReady,
                pos_ns: 0,
                anchor_ns: 0,
                shown: None,
                media: MediaCache(None),
            },
        );
        return;
    };
    s.vfw.players.insert(
        hwnd,
        Player {
            path,
            mode: Mode::Stopped,
            pos_ns: 0,
            anchor_ns: 0,
            shown: None,
            media: MediaCache(None),
        },
    );
    if !ensure_media(s, hwnd) {
        return;
    }
    let m = s.vfw.players[&hwnd].media.0.as_ref().unwrap();
    let (vw, vh) = (m.width as i32, m.height as i32);
    if let Some(w) = s.user.windows.get_mut(&hwnd) {
        let i = nc_insets(w.style, w.exstyle, false);
        let r = w.rect;
        w.rect = Rect::new(
            r.left,
            r.top,
            r.left + vw + i.left + i.right,
            r.top + vh + i.top + i.bottom,
        );
        w.client = client_from_window(w);
    }
    // The first frame is shown once the window is painted.
    show_frame(s, hwnd, 0, false);
}

fn mode_code(m: Mode) -> u32 {
    match m {
        Mode::NotReady => MCI_MODE_NOT_READY,
        Mode::Stopped => MCI_MODE_STOP,
        Mode::Playing => MCI_MODE_PLAY,
        Mode::Paused => MCI_MODE_PAUSE,
    }
}

fn mode_name(m: Mode) -> &'static str {
    match m {
        Mode::NotReady => "not ready",
        Mode::Stopped => "stopped",
        Mode::Playing => "playing",
        Mode::Paused => "paused",
    }
}

/// Changes a player's mode at the current time, keeping its position continuous.
fn set_mode(s: &mut Session, hwnd: u32, mode: Mode) {
    // Render the audio of the elapsed interval under the old state first.
    crate::com::dsound::mix_now(s);
    if mode == Mode::Playing {
        crate::com::dsound::start_mixer(s);
    }
    let now = s.clock.now_ns();
    let Some(p) = s.vfw.players.get_mut(&hwnd) else {
        return;
    };
    p.pos_ns = p.pos_at(now);
    p.anchor_ns = now;
    p.mode = mode;
}

fn mciwnd_proc(s: &mut Session) -> ApiResult {
    let [hwnd, msg, wp, lp] = s.args::<4>()?;
    update(s, hwnd);
    let mode = s.vfw.players.get(&hwnd).map(|p| p.mode);
    let ready = mode.is_some_and(|m| m != Mode::NotReady);
    let r = match msg {
        WM_CREATE => {
            on_create(s, hwnd);
            0
        }
        WM_DESTROY => {
            crate::com::dsound::mix_now(s);
            s.vfw.players.remove(&hwnd);
            return def_window_proc(s, hwnd, msg, wp, lp);
        }
        WM_ERASEBKGND => 1,
        WM_PAINT => {
            if let Some(i) = s.vfw.players.get(&hwnd).and_then(|p| p.shown) {
                show_frame(s, hwnd, i, true);
            } else {
                paint_black(s, hwnd);
            }
            if let Some(w) = s.user.windows.get_mut(&hwnd) {
                w.invalid = None;
                w.erase_pending = false;
            }
            0
        }
        MCI_PLAY | MCI_RESUME | MCI_PAUSE | MCI_STOP if !ready => {
            s.event(
                Severity::Warning,
                "MCIWnd",
                format!("command {msg:#x} without open media"),
            );
            MCIERR_DEVICE_NOT_READY
        }
        MCI_PLAY => {
            if !ensure_media(s, hwnd) {
                return Ok(ApiRet::Ret(MCIERR_DEVICE_NOT_READY));
            }
            // Playing from the end restarts nothing (as in MCI); otherwise play on.
            set_mode(s, hwnd, Mode::Playing);
            update(s, hwnd);
            0
        }
        MCI_RESUME => {
            if mode == Some(Mode::Paused) {
                set_mode(s, hwnd, Mode::Playing);
            }
            0
        }
        MCI_PAUSE => {
            if mode == Some(Mode::Playing) {
                set_mode(s, hwnd, Mode::Paused);
            }
            0
        }
        MCI_STOP => {
            if matches!(mode, Some(Mode::Playing | Mode::Paused)) {
                set_mode(s, hwnd, Mode::Stopped);
            }
            0
        }
        MCIWNDM_GETMODEA => {
            let m = mode.unwrap_or(Mode::NotReady);
            if lp != 0 && wp > 0 {
                let mut b = mode_name(m).as_bytes().to_vec();
                b.truncate(wp as usize - 1);
                b.push(0);
                s.mem.write(lp, &b)?;
            }
            mode_code(m)
        }
        MCIWNDM_GETLENGTH | MCIWNDM_GETPOSITIONA => {
            // In frames (the default MCIWnd time format for video).
            if !ensure_media(s, hwnd) {
                return Ok(ApiRet::Ret(0));
            }
            let now = s.clock.now_ns();
            let p = &s.vfw.players[&hwnd];
            let m = p.media.0.as_ref().unwrap();
            if msg == MCIWNDM_GETLENGTH {
                m.frames as u32
            } else {
                m.frame_at(p.pos_at(now)) as u32
            }
        }
        m if (0x0800..0x0900).contains(&m) || (WM_USER..WM_USER + 0x100).contains(&m) => {
            s.missing("MCIWnd", format!("message {m:#x} (wParam {wp:#x}, lParam {lp:#x})"));
            return def_window_proc(s, hwnd, msg, wp, lp);
        }
        _ => return def_window_proc(s, hwnd, msg, wp, lp),
    };
    Ok(ApiRet::Ret(r))
}

/// Advances a player to the current time: stops it at the end of the movie and shows
/// the frame that is due.
fn update(s: &mut Session, hwnd: u32) {
    let now = s.clock.now_ns();
    let Some(p) = s.vfw.players.get(&hwnd) else {
        return;
    };
    if p.mode != Mode::Playing {
        return;
    }
    if !ensure_media(s, hwnd) {
        return;
    }
    let p = &s.vfw.players[&hwnd];
    let m = p.media.0.as_ref().unwrap();
    let dur = m.duration_ns();
    let pos = p.pos_at(now);
    let frame = m.frame_at(pos);
    if pos >= dur {
        // Stop exactly at the end of the movie.
        let end_at = p.anchor_ns + (dur - p.pos_ns.min(dur));
        crate::com::dsound::mix_now(s);
        let p = s.vfw.players.get_mut(&hwnd).unwrap();
        p.pos_ns = dur;
        p.anchor_ns = end_at;
        p.mode = Mode::Stopped;
    }
    if s.vfw.players[&hwnd].shown != Some(frame) {
        show_frame(s, hwnd, frame, true);
    }
}

/// Is the window and every ancestor visible?
fn visible(s: &Session, mut h: u32) -> bool {
    while let Some(w) = s.window(h) {
        if !w.visible {
            return false;
        }
        if w.parent == 0 {
            return true;
        }
        h = w.parent;
    }
    false
}

/// The part of a window's client area that is on screen (inside its ancestors).
fn visible_client(s: &mut Session, hwnd: u32) -> Option<(Rect, Rect, u32)> {
    if !visible(s, hwnd) {
        return None;
    }
    let front = s.front_surface();
    let sf = s.display.surfaces.get(&front)?;
    let client = s.window(hwnd)?.client;
    let mut clip = client.intersect(&Rect::new(0, 0, sf.w as i32, sf.h as i32));
    let mut p = s.window(hwnd)?.parent;
    while let Some(w) = s.window(p) {
        clip = clip.intersect(&w.client);
        p = w.parent;
    }
    (!clip.is_empty()).then_some((client, clip, front))
}

fn paint_black(s: &mut Session, hwnd: u32) {
    let Some((_, clip, front)) = visible_client(s, hwnd) else {
        return;
    };
    s.surface_cpu_access(front, true);
    let t = crate::surface::PixelTarget::of(&s.display.surfaces[&front]);
    let _ = t.fill_raw(&mut s.mem, &clip, 0);
}

/// Decodes frame `i` and, with `draw`, draws it stretched over the window's client
/// area and presents it.
fn show_frame(s: &mut Session, hwnd: u32, i: usize, draw: bool) {
    if !ensure_media(s, hwnd) {
        return;
    }
    let p = s.vfw.players.get_mut(&hwnd).unwrap();
    let m = p.media.0.as_mut().unwrap();
    if let Err(e) = m.decode_to(i) {
        let path = p.path.clone();
        p.mode = Mode::NotReady;
        p.media.0 = None;
        s.missing("MCIWnd", format!("cannot decode {path}: {e}"));
        return;
    }
    p.shown = Some(i);
    if !draw {
        return;
    }
    let Some((client, clip, front)) = visible_client(s, hwnd) else {
        return;
    };
    let fmt = s.display.surfaces[&front].fmt;
    if fmt == Fmt::P8 || fmt.is_compressed() || fmt.is_depth() {
        s.missing("MCIWnd", format!("drawing a movie on a {fmt:?} display"));
        return;
    }
    let m = s.vfw.players[&hwnd].media.0.as_ref().unwrap();
    if m.rgb.is_empty() {
        return;
    }
    let (fw, fh) = (m.width as i64, m.height as i64);
    let (cw, ch) = (client.w().max(1) as i64, client.h().max(1) as i64);
    let bpp = fmt.bytes_pp() as usize;
    let mut rows = Vec::with_capacity(clip.h() as usize);
    for y in clip.top..clip.bottom {
        let sy = ((y - client.top) as i64 * fh / ch) as usize;
        let mut row = vec![0u8; clip.w() as usize * bpp];
        for x in clip.left..clip.right {
            let sx = ((x - client.left) as i64 * fw / cw) as usize;
            let o = (sy * fw as usize + sx) * 3;
            let rgb = &m.rgb[o..o + 3];
            let argb = 0xff00_0000 | (rgb[0] as u32) << 16 | (rgb[1] as u32) << 8 | rgb[2] as u32;
            let d = (x - clip.left) as usize * bpp;
            fmt.encode(argb, &mut row[d..d + bpp]);
        }
        rows.push(row);
    }
    s.surface_cpu_access(front, true);
    let sf = &s.display.surfaces[&front];
    let (mem, pitch) = (sf.mem, sf.pitch);
    for (k, row) in rows.iter().enumerate() {
        let a = mem + (clip.top as u32 + k as u32) * pitch + clip.left as u32 * bpp as u32;
        if s.mem.write(a, row).is_err() {
            return;
        }
    }
    crate::com::ddraw::present(s);
}

// ---------------------------------------------------------------- scheduling and audio

/// The next virtual time at which a playing movie shows a new frame or ends.
pub fn next_event(s: &Session) -> Option<u64> {
    let now = s.clock.now_ns();
    let mut t: Option<u64> = None;
    for p in s.vfw.players.values() {
        if p.mode != Mode::Playing {
            continue;
        }
        let Some(m) = p.media.0.as_ref() else {
            // Media is reloaded by the next service call.
            return Some(now);
        };
        let pos = p.pos_at(now);
        let next = m.frame_ns(m.frame_at(pos) + 1).min(m.duration_ns()).max(pos);
        let at = now + (next - pos).max(1);
        t = Some(t.map_or(at, |x| x.min(at)));
    }
    t
}

pub fn service(s: &mut Session) {
    let playing: Vec<u32> = s
        .vfw
        .players
        .iter()
        .filter(|(_, p)| p.mode == Mode::Playing)
        .map(|(&h, _)| h)
        .collect();
    for h in playing {
        update(s, h);
    }
}

/// Adds the sound of playing movies to `out` (stereo frames at `out_rate`), which
/// starts at virtual time `t0`.
pub fn mix_into(s: &Session, t0: u64, out_rate: u32, out: &mut [f32]) {
    for p in s.vfw.players.values() {
        if p.mode != Mode::Playing {
            continue;
        }
        let Some(m) = p.media.0.as_ref() else { continue };
        let Some((wf, pcm)) = m.audio.as_ref() else { continue };
        let block = (wf.block_align.max(1)) as usize;
        let frames = pcm.len() / block;
        let rate = wf.samples_per_sec as f64;
        let stereo = wf.channels == 2;
        let read = |i: usize, c: usize| -> f32 {
            let o = i * block
                + if stereo {
                    c * (wf.bits_per_sample as usize / 8)
                } else {
                    0
                };
            if wf.bits_per_sample == 8 {
                (pcm[o] as f32 - 128.0) / 128.0
            } else {
                i16::from_le_bytes([pcm[o], pcm[o + 1]]) as f32 / 32768.0
            }
        };
        for k in 0..out.len() / 2 {
            let t = t0 + (k as u128 * 1_000_000_000 / out_rate as u128) as u64;
            if t < p.anchor_ns {
                continue;
            }
            let pos = p.pos_ns + (t - p.anchor_ns);
            let f = pos as f64 * rate / 1e9;
            let i0 = f as usize;
            if i0 + 1 >= frames {
                continue;
            }
            let frac = (f - i0 as f64) as f32;
            for c in 0..2 {
                let (a, b) = (read(i0, c), read(i0 + 1, c));
                out[k * 2 + c] += a + (b - a) * frac;
            }
        }
    }
}

pub static MSVFW32_EXPORTS: &[ExportDef] = &[
    cdecl!("MCIWndCreate", mciwnd_create_a),
    cdecl!("MCIWndCreateA", mciwnd_create_a),
    cdecl!("MCIWndCreateW", mciwnd_create_w),
    cdecl!("MCIWndRegisterClass", mciwnd_register_class),
    export!("MCIWndProc", 4, mciwnd_proc),
];

#[cfg(test)]
mod tests {
    use crate::session::{RunLimit, RunStop, Session};
    use crate::vfs::{self, Disposition, GuestPath};
    use drydock_fixtures::PeBuilder;
    use drydock_fixtures::media::{Pcm, avi, indeo5_flat_frame};
    use iced_x86::code_asm::*;
    use std::sync::{Arc, Mutex};

    const COLOURS: [(u8, u8, u8); 3] = [(81, 90, 240), (145, 54, 34), (210, 16, 146)];

    /// Plays C:\TEST\M.AVI in an MCIWnd window: MCI_PLAY, then polls the mode every
    /// 20 ms until it is stopped, closes the window and exits with the result of
    /// MCI_PLAY.
    fn player_pe() -> Vec<u8> {
        let strings = b"MSVFW32.DLL\0MCIWndCreate\0C:\\TEST\\M.AVI\0";
        PeBuilder::new()
            .import(
                "KERNEL32.DLL",
                &["LoadLibraryA", "GetProcAddress", "Sleep", "ExitProcess"],
            )
            .import("USER32.DLL", &["SendMessageA"])
            .data(strings)
            .build(|a, l| {
                let k = |f| l.import("KERNEL32.DLL", f);
                let send = l.import("USER32.DLL", "SendMessageA");
                a.push(l.data)?;
                a.call(dword_ptr(k("LoadLibraryA")))?;
                a.push(l.data + 12)?;
                a.push(eax)?;
                a.call(dword_ptr(k("GetProcAddress")))?;
                // MCIWndCreate(NULL, NULL, MCIWNDF_NOPLAYBAR | MCIWNDF_NOMENU, file)
                a.push(l.data + 25)?;
                a.push(0x0a)?;
                a.push(0)?;
                a.push(0)?;
                a.call(eax)?;
                a.add(esp, 16)?;
                a.mov(ebx, eax)?;
                a.push(0)?;
                a.push(0)?;
                a.push(super::MCI_PLAY)?;
                a.push(ebx)?;
                a.call(dword_ptr(send))?;
                a.mov(edi, eax)?;
                let mut poll = a.create_label();
                a.set_label(&mut poll)?;
                a.push(20)?;
                a.call(dword_ptr(k("Sleep")))?;
                a.push(0)?;
                a.push(0)?;
                a.push(super::MCIWNDM_GETMODEA)?;
                a.push(ebx)?;
                a.call(dword_ptr(send))?;
                a.cmp(eax, super::MCI_MODE_STOP)?;
                a.jne(poll)?;
                a.push(0)?;
                a.push(0)?;
                a.push(0x10)?; // WM_CLOSE
                a.push(ebx)?;
                a.call(dword_ptr(send))?;
                a.push(edi)?;
                a.call(dword_ptr(k("ExitProcess")))?;
                a.int3()
            })
    }

    struct Capture(Arc<Mutex<Vec<f32>>>);
    impl drydock_audio::AudioSink for Capture {
        fn push(&mut self, stereo: &[f32]) {
            self.0.lock().unwrap().extend_from_slice(stereo);
        }
        fn name(&self) -> String {
            "capture".into()
        }
    }

    /// A session with the player program, a 3-frame movie (15 fps) with a square-wave
    /// sound track, and an audio capture.
    fn movie_session() -> (Session, Arc<Mutex<Vec<f32>>>) {
        let mut s = crate::testutil::session_with(&player_pe());
        let frames: Vec<_> = COLOURS
            .iter()
            .enumerate()
            .map(|(i, &c)| indeo5_flat_frame(64, 48, i as u32, c))
            .collect();
        let samples: Vec<u8> = (0..2205).map(|i| if i / 10 % 2 == 0 { 200 } else { 56 }).collect();
        let file = avi(
            64,
            48,
            15,
            &frames,
            Some(Pcm {
                rate: 11025,
                samples: &samples,
            }),
        );
        let root = GuestPath {
            drive: 'C',
            comps: vec![],
        };
        let p = vfs::normalize("C:\\TEST\\M.AVI", &root).unwrap();
        let (mut f, _) = s.vfs.open(&p, false, true, Disposition::CreateAlways, 0).unwrap();
        s.vfs.write(&mut f, &file, 0).unwrap();
        let out = Arc::new(Mutex::new(Vec::new()));
        s.host.audio = Some(Box::new(Capture(out.clone())));
        (s, out)
    }

    fn run_to(s: &mut Session, ms: u64) -> RunStop {
        s.run(RunLimit {
            until_ns: ms * 1_000_000,
            max_insns: None,
            stop_on_present: false,
            jump_idle: true,
        })
    }

    /// The colour at the centre of the movie window's client area (0x00RRGGBB).
    fn centre_pixel(s: &mut Session, client: crate::dll::user32::Rect) -> u32 {
        let front = s.front_surface();
        let (w, _, px) = s.surface_rgba(front).unwrap();
        let (x, y) = ((client.left + client.right) / 2, (client.top + client.bottom) / 2);
        px[y as usize * w as usize + x as usize] & 0xff_ffff
    }

    fn rgb(c: (u8, u8, u8)) -> u32 {
        let f = drydock_media::indeo5::Frame {
            width: 1,
            height: 1,
            chroma_width: 1,
            chroma_height: 1,
            y: vec![c.0],
            u: vec![c.1],
            v: vec![c.2],
        };
        let p = f.to_rgb24();
        (p[0] as u32) << 16 | (p[1] as u32) << 8 | p[2] as u32
    }

    #[test]
    fn plays_a_movie_on_the_virtual_clock() {
        let (mut s, audio) = movie_session();
        // Mid-movie: the window exists, is sized to the movie and shows frame 1.
        assert_eq!(run_to(&mut s, 100), RunStop::TimeReached);
        let (&hwnd, p) = s.vfw.players.iter().next().expect("a movie window");
        assert_eq!(p.mode, super::Mode::Playing);
        let client = s.window(hwnd).unwrap().client;
        assert_eq!((client.w(), client.h()), (64, 48));
        assert_eq!(p.shown, Some(1));
        let presented = s.frames_presented;
        // A 16-bit desktop rounds colours; compare in 5/6/5 precision.
        let q = |c: u32| c & 0xf8fcf8;
        assert_eq!(q(centre_pixel(&mut s, client)), q(rgb(COLOURS[1])));

        // The movie ends after 3 frames (200 ms); the program sees the stop at its next
        // 20 ms poll and exits with MCI_PLAY's result (0).
        assert_eq!(run_to(&mut s, 1000), RunStop::Exited { code: 0 });
        let end = s.clock.now_ns();
        assert!((200_000_000..260_000_000).contains(&end), "ended at {end} ns");
        assert_eq!(s.frames_presented, presented + 1);
        assert_eq!(q(centre_pixel(&mut s, client)), q(rgb(COLOURS[2])));
        assert!(s.vfw.players.is_empty(), "WM_CLOSE destroys the window");
        assert!(s.diag.missing.is_empty(), "{:?}", s.diag.missing);

        // The sound track (200 ms of a +/-0.56 square wave, interpolated through zero
        // at each edge) was mixed.
        let a = audio.lock().unwrap();
        let loud = a.iter().filter(|v| v.abs() > 0.1).count() / 2;
        let ms = loud as u64 * 1000 / drydock_audio::OUTPUT_RATE as u64;
        assert!((190..=200).contains(&ms), "{ms} ms of sound");
    }

    #[test]
    fn checkpoint_mid_movie_resumes_identically() {
        let (mut s, _) = movie_session();
        assert_eq!(run_to(&mut s, 120), RunStop::TimeReached);
        let cp = s.checkpoint("movie");
        let observe = |s: &mut Session| {
            let front = s.front_surface();
            (
                s.clock.now_ns(),
                s.cpu.state().insns,
                s.frames_presented,
                s.surface_rgba(front).unwrap().2,
            )
        };
        assert_eq!(run_to(&mut s, 1000), RunStop::Exited { code: 0 });
        let a = observe(&mut s);

        let (mut t, _) = movie_session();
        t.restore(&cp, true).unwrap();
        assert_eq!(run_to(&mut t, 1000), RunStop::Exited { code: 0 });
        assert_eq!(observe(&mut t), a);
    }

    #[test]
    fn unsupported_media_is_reported_as_missing() {
        let mut s = crate::testutil::session_with(&player_pe());
        let root = GuestPath {
            drive: 'C',
            comps: vec![],
        };
        let p = vfs::normalize("C:\\TEST\\M.AVI", &root).unwrap();
        let (mut f, _) = s.vfs.open(&p, false, true, Disposition::CreateAlways, 0).unwrap();
        s.vfs.write(&mut f, b"RIFF\x04\0\0\0WAVE", 0).unwrap();
        // MCI_PLAY fails; the program exits with its error code once the mode reads
        // "stopped", which it never does: stop the run at a time limit instead.
        assert_eq!(run_to(&mut s, 200), RunStop::TimeReached);
        assert_eq!(s.diag.missing.get("MCIWnd"), Some(&1));
        let p = s.vfw.players.values().next().unwrap();
        assert_eq!(p.mode, super::Mode::NotReady);
    }
}
