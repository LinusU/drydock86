//! DirectShow multimedia streaming (amstream.dll): IAMMultiMediaStream with a
//! DirectDraw video stream and the default audio renderer, for MPEG-1 program streams.
//!
//! The stream runs on the virtual clock. `IDirectDrawStreamSample::Update` delivers
//! the video frame due at the current stream time into the sample's DirectDraw
//! surface, waiting (in virtual time) for the next frame when called early and
//! skipping frames when called late, as DirectShow's renderer does. The MP2 sound
//! track is decoded when the file is opened and played by the shared audio mixer.
//!
//! Decoders and decoded data are a host-side cache. Checkpoints keep the playback
//! state; after a restore the cache is rebuilt by decoding from the start again, which
//! reproduces the same pictures.

use super::*;
use crate::dll::user32::Rect;
use crate::surface::Fmt;
use crate::{export, iunknown, unimpl_method};
use std::io::Cursor;

pub const CLSID_AMMULTIMEDIASTREAM: Guid = guid("49c47ce5-9ba4-11d0-8212-00c04fc32c45");
const MSPID_PRIMARY_VIDEO: Guid = guid("a35ff56a-9fda-11d0-8fdf-00c04fd9189d");
const MSPID_PRIMARY_AUDIO: Guid = guid("a35ff56b-9fda-11d0-8fdf-00c04fd9189d");

const MS_S_ENDOFSTREAM: u32 = 0x0004_0003;
const fn ms_error(x: u32) -> u32 {
    0x8004_0400 + x
}
const MS_E_PURPOSEID: u32 = ms_error(2);
const MS_E_NOSTREAM: u32 = ms_error(3);
const MS_E_INCOMPATIBLE: u32 = ms_error(5);
const MS_E_NOTINIT: u32 = ms_error(7);
const MS_E_NOTRUNNING: u32 = ms_error(10);
const VFW_E_INVALID_FILE_FORMAT: u32 = 0x8004_022f;

const AMMSF_ADDDEFAULTRENDERER: u32 = 0x1;
const STREAMSTATE_RUN: u32 = 1;

/// One IAMMultiMediaStream.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MmStream {
    pub initialized: bool,
    /// Media stream objects for the primary video and audio purposes.
    pub video: u32,
    pub audio: u32,
    /// The audio stream plays through the default renderer.
    pub audio_rendered: bool,
    /// File named by OpenFile.
    pub path: Option<String>,
    pub running: bool,
    /// Stream time (ns) at virtual time `anchor_ns`; it advances while running.
    pub pos_ns: u64,
    pub anchor_ns: u64,
    /// Video frames taken from the decoder (delivered or skipped).
    pub frames_read: u64,
    pub ended: bool,
    #[serde(skip)]
    cache: MovieCache,
}

impl MmStream {
    fn pos_at(&self, now: u64) -> u64 {
        if self.running {
            self.pos_ns + now.saturating_sub(self.anchor_ns)
        } else {
            self.pos_ns
        }
    }
}

/// An IMediaStream (and, for video, IDirectDrawMediaStream).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MediaStream {
    pub parent: u32,
    pub purpose: Guid,
    /// DirectDraw object of a video stream.
    pub ddraw: u32,
}

/// An IDirectDrawStreamSample.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StreamSample {
    pub stream: u32,
    pub surface: u32,
    pub rect: Option<Rect>,
}

/// Decoded media, rebuilt on demand.
#[derive(Default)]
struct MovieCache(Option<Box<Movie>>);

impl Clone for MovieCache {
    fn clone(&self) -> Self {
        MovieCache(None)
    }
}

impl std::fmt::Debug for MovieCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.0.is_some() {
            "MovieCache(loaded)"
        } else {
            "MovieCache(empty)"
        })
    }
}

struct Movie {
    width: u32,
    height: u32,
    fps: f64,
    video: slopmpeg::Decoder<Cursor<Vec<u8>>>,
    /// The next frame, decoded ahead: (presentation time in ns from the first frame,
    /// RGBA pixels).
    next: Option<(u64, Vec<u8>)>,
    first_time: Option<f64>,
    frames_decoded: u64,
    /// Interleaved stereo samples of the whole sound track and their rate.
    audio: Vec<f32>,
    audio_rate: u32,
}

impl Movie {
    fn open(data: Vec<u8>, with_audio: bool) -> Result<Movie, String> {
        let mut video = slopmpeg::Decoder::new(Cursor::new(data.clone())).map_err(|e| e.to_string())?;
        let info = video.video_info().ok_or("no MPEG-1 video stream")?;
        video.set_audio_enabled(false);
        let mut audio = Vec::new();
        let mut audio_rate = 0;
        if with_audio {
            let mut a = slopmpeg::Decoder::new(Cursor::new(data)).map_err(|e| e.to_string())?;
            if let Some(ai) = a.audio_info() {
                audio_rate = ai.sample_rate;
                a.set_video_enabled(false);
                while let Some(samples) = a.decode_audio().map_err(|e| e.to_string())? {
                    audio.extend_from_slice(samples.interleaved());
                }
            }
        }
        Ok(Movie {
            width: info.width as u32,
            height: info.height as u32,
            fps: info.framerate,
            video,
            next: None,
            first_time: None,
            frames_decoded: 0,
            audio,
            audio_rate,
        })
    }

    /// Decode the next frame into `next` unless one is waiting. False at the end.
    fn fill(&mut self) -> Result<bool, String> {
        if self.next.is_some() {
            return Ok(true);
        }
        let Some(f) = self.video.decode_video().map_err(|e| e.to_string())? else {
            return Ok(false);
        };
        let t = f.time().as_secs_f64();
        let first = *self.first_time.get_or_insert(t);
        let ns = ((t - first).max(0.0) * 1e9) as u64;
        self.next = Some((ns, f.to_pixels(slopmpeg::PixelFormat::Bgra)));
        self.frames_decoded += 1;
        Ok(true)
    }
}

fn stream_of(s: &Session, id: u32) -> Option<&MmStream> {
    match &s.com_obj(id)?.kind {
        ComKind::MmStream(m) => Some(m),
        _ => None,
    }
}

fn stream_mut(s: &mut Session, id: u32) -> Option<&mut MmStream> {
    match &mut s.com_obj_mut(id)?.kind {
        ComKind::MmStream(m) => Some(m),
        _ => None,
    }
}

fn media_of(s: &Session, id: u32) -> Option<MediaStream> {
    match &s.com_obj(id)?.kind {
        ComKind::MediaStream(m) => Some(m.clone()),
        _ => None,
    }
}

/// Load the movie of a stream if needed; on failure the stream is reported and ended.
fn ensure_movie(s: &mut Session, id: u32) -> bool {
    let Some(m) = stream_of(s, id) else { return false };
    if m.cache.0.is_some() {
        return true;
    }
    let Some(path) = m.path.clone() else { return false };
    let (frames_read, rendered) = (m.frames_read, m.audio_rendered);
    let opened = crate::dll::msvfw32::read_guest_file(s, &path).and_then(|d| Movie::open(d, rendered));
    match opened {
        Ok(mut movie) => {
            // Restored stream: decode up to its position again.
            while movie.frames_decoded < frames_read {
                movie.next = None;
                if !movie.fill().unwrap_or(false) {
                    break;
                }
            }
            if frames_read > 0 {
                movie.next = None;
            }
            stream_mut(s, id).unwrap().cache.0 = Some(Box::new(movie));
            true
        }
        Err(e) => {
            s.missing("AMMultiMediaStream", format!("cannot play {path}: {e}"));
            stream_mut(s, id).unwrap().ended = true;
            false
        }
    }
}

pub fn create_via_com(s: &mut Session, iid: &Guid, ppv: u32) -> ApiResult {
    let id = s.com_new(ComKind::MmStream(Box::default()));
    let def = [&IAMMULTIMEDIASTREAM, &IMULTIMEDIASTREAM]
        .into_iter()
        .find(|d| d.iid == *iid || *iid == IID_IUNKNOWN);
    match def {
        Some(d) => {
            s.out_iface(ppv, id, d)?;
            Ok(ApiRet::Ret(S_OK))
        }
        None => {
            s.com_release(id);
            Ok(ApiRet::Ret(E_NOINTERFACE))
        }
    }
}

// ---------------------------------------------------------------- IAMMultiMediaStream

fn mms_self(s: &Session) -> Result<(u32, MmStream), ApiError> {
    let id = s.this_obj()?;
    let m = stream_of(s, id)
        .cloned()
        .ok_or_else(|| ApiError::Fatal("not a multimedia stream".into()))?;
    Ok((id, m))
}

fn mms_initialize(s: &mut Session) -> ApiResult {
    let [_this, ty, _flags, graph] = s.args::<4>()?;
    let (id, _) = mms_self(s)?;
    if ty != 0 {
        s.missing(
            "IAMMultiMediaStream::Initialize",
            format!("stream type {ty} (only reading is supported)"),
        );
        return Ok(ApiRet::Ret(E_NOTIMPL));
    }
    if graph != 0 {
        s.missing(
            "IAMMultiMediaStream::Initialize",
            "an application-supplied filter graph".into(),
        );
        return Ok(ApiRet::Ret(E_NOTIMPL));
    }
    stream_mut(s, id).unwrap().initialized = true;
    Ok(ApiRet::Ret(S_OK))
}

fn mms_add_media_stream(s: &mut Session) -> ApiResult {
    let [_this, obj, purpose_p, flags, pp] = s.args::<5>()?;
    let (id, m) = mms_self(s)?;
    if !m.initialized {
        return Ok(ApiRet::Ret(MS_E_NOTINIT));
    }
    let purpose = s.read_guid(purpose_p)?;
    let video = purpose == MSPID_PRIMARY_VIDEO;
    if !video && purpose != MSPID_PRIMARY_AUDIO {
        return Ok(ApiRet::Ret(MS_E_PURPOSEID));
    }
    if !video && flags & AMMSF_ADDDEFAULTRENDERER == 0 {
        s.missing(
            "IAMMultiMediaStream::AddMediaStream",
            "audio streams read by the application".into(),
        );
        return Ok(ApiRet::Ret(E_NOTIMPL));
    }
    let ddraw = if video && obj != 0 {
        match s.obj_of(obj) {
            Some(d) if matches!(s.com_obj(d).map(|o| &o.kind), Some(ComKind::DirectDraw(_))) => {
                s.com_addref(d);
                d
            }
            _ => return Ok(ApiRet::Ret(MS_E_INCOMPATIBLE)),
        }
    } else {
        0
    };
    let ms = s.com_new(ComKind::MediaStream(MediaStream {
        parent: id,
        purpose,
        ddraw,
    }));
    let st = stream_mut(s, id).unwrap();
    if video {
        st.video = ms;
    } else {
        st.audio = ms;
        st.audio_rendered = true;
    }
    if pp != 0 {
        let def = if video { &IDIRECTDRAWMEDIASTREAM } else { &IMEDIASTREAM };
        s.out_iface(pp, ms, def)?;
        s.com_addref(ms);
    }
    Ok(ApiRet::Ret(S_OK))
}

fn mms_open_file(s: &mut Session) -> ApiResult {
    let [_this, name_p, flags] = s.args::<3>()?;
    let (id, m) = mms_self(s)?;
    if !m.initialized {
        return Ok(ApiRet::Ret(MS_E_NOTINIT));
    }
    let name = s.read_wstr(name_p)?;
    let Some(gp) = s.guest_path(&name) else {
        return Ok(ApiRet::Ret(0x8007_0003)); // HRESULT_FROM_WIN32(ERROR_PATH_NOT_FOUND)
    };
    let path = gp.display();
    if s.vfs.attributes(&gp).is_err() {
        return Ok(ApiRet::Ret(0x8007_0002)); // HRESULT_FROM_WIN32(ERROR_FILE_NOT_FOUND)
    }
    if flags & 0x2 != 0 {
        // AMMSF_RUN: start immediately.
        stream_mut(s, id).unwrap().running = true;
    }
    let st = stream_mut(s, id).unwrap();
    st.path = Some(path);
    st.cache = MovieCache(None);
    st.frames_read = 0;
    st.pos_ns = 0;
    st.ended = false;
    if !ensure_movie(s, id) {
        return Ok(ApiRet::Ret(VFW_E_INVALID_FILE_FORMAT));
    }
    if stream_of(s, id).is_some_and(|m| m.running) {
        set_running(s, id, true);
    }
    Ok(ApiRet::Ret(S_OK))
}

fn mms_get_media_stream(s: &mut Session) -> ApiResult {
    let [_this, purpose_p, pp] = s.args::<3>()?;
    let (_id, m) = mms_self(s)?;
    let purpose = s.read_guid(purpose_p)?;
    let ms = if purpose == MSPID_PRIMARY_VIDEO {
        m.video
    } else if purpose == MSPID_PRIMARY_AUDIO {
        m.audio
    } else {
        0
    };
    if ms == 0 {
        s.mem.write_u32(pp, 0)?;
        return Ok(ApiRet::Ret(MS_E_NOSTREAM));
    }
    let def = if purpose == MSPID_PRIMARY_VIDEO {
        &IDIRECTDRAWMEDIASTREAM
    } else {
        &IMEDIASTREAM
    };
    s.out_iface(pp, ms, def)?;
    s.com_addref(ms);
    Ok(ApiRet::Ret(S_OK))
}

fn mms_get_information(s: &mut Session) -> ApiResult {
    let [_this, pflags, ptype] = s.args::<3>()?;
    let (_id, m) = mms_self(s)?;
    if pflags != 0 {
        // MMSSF_HASCLOCK | MMSSF_SUPPORTSEEK
        s.mem.write_u32(pflags, 0x1 | 0x4)?;
    }
    if ptype != 0 {
        s.mem.write_u32(ptype, 0)?;
    }
    let _ = m;
    Ok(ApiRet::Ret(S_OK))
}

fn mms_get_state(s: &mut Session) -> ApiResult {
    let [_this, p] = s.args::<2>()?;
    let (_id, m) = mms_self(s)?;
    s.mem.write_u32(p, m.running as u32)?;
    Ok(ApiRet::Ret(S_OK))
}

/// Change the running state at the current time, keeping the stream time continuous.
fn set_running(s: &mut Session, id: u32, run: bool) {
    // Render the audio of the elapsed interval under the old state first.
    crate::com::dsound::mix_now(s);
    if run {
        crate::com::dsound::start_mixer(s);
    }
    let now = s.clock.now_ns();
    if let Some(m) = stream_mut(s, id) {
        m.pos_ns = m.pos_at(now);
        m.anchor_ns = now;
        m.running = run;
    }
}

fn mms_set_state(s: &mut Session) -> ApiResult {
    let [_this, state] = s.args::<2>()?;
    let (id, m) = mms_self(s)?;
    if m.path.is_none() {
        return Ok(ApiRet::Ret(MS_E_NOTINIT));
    }
    set_running(s, id, state == STREAMSTATE_RUN);
    Ok(ApiRet::Ret(S_OK))
}

/// STREAM_TIME is in 100 ns units.
fn mms_get_time(s: &mut Session) -> ApiResult {
    let [_this, p] = s.args::<2>()?;
    let (_id, m) = mms_self(s)?;
    let now = s.clock.now_ns();
    s.mem.write_u64(p, m.pos_at(now) / 100)?;
    Ok(ApiRet::Ret(S_OK))
}

fn mms_get_duration(s: &mut Session) -> ApiResult {
    let [_this, p] = s.args::<2>()?;
    let (id, _) = mms_self(s)?;
    if !ensure_movie(s, id) {
        return Ok(ApiRet::Ret(MS_E_NOTINIT));
    }
    let mv = stream_mut(s, id).unwrap().cache.0.as_mut().unwrap();
    let d = mv.video.duration().ok().flatten();
    match d {
        Some(d) => {
            s.mem.write_u64(p, (d.as_nanos() / 100) as u64)?;
            Ok(ApiRet::Ret(S_OK))
        }
        None => Ok(ApiRet::Ret(S_FALSE)),
    }
}

fn mms_seek(s: &mut Session) -> ApiResult {
    let [_this, lo, hi] = s.args::<3>()?;
    let (id, m) = mms_self(s)?;
    let t = ((hi as u64) << 32 | lo as u64) * 100;
    if t != 0 {
        s.missing(
            "IMultiMediaStream::Seek",
            format!("seeking to {t} ns (only the start is supported)"),
        );
        return Ok(ApiRet::Ret(E_NOTIMPL));
    }
    if m.path.is_none() {
        return Ok(ApiRet::Ret(MS_E_NOTINIT));
    }
    crate::com::dsound::mix_now(s);
    let now = s.clock.now_ns();
    let st = stream_mut(s, id).unwrap();
    st.pos_ns = 0;
    st.anchor_ns = now;
    st.frames_read = 0;
    st.ended = false;
    st.cache = MovieCache(None);
    Ok(ApiRet::Ret(S_OK))
}

pub fn stream_destroyed(s: &mut Session, m: &MmStream) {
    for ms in [m.video, m.audio] {
        if ms != 0 && s.com.objs.contains_key(&ms) {
            s.com_release(ms);
        }
    }
}

// ---------------------------------------------------------------- media streams

fn ms_self(s: &Session) -> Result<(u32, MediaStream), ApiError> {
    let id = s.this_obj()?;
    let m = media_of(s, id).ok_or_else(|| ApiError::Fatal("not a media stream".into()))?;
    Ok((id, m))
}

fn ms_get_multimedia_stream(s: &mut Session) -> ApiResult {
    let [_this, pp] = s.args::<2>()?;
    let (_id, m) = ms_self(s)?;
    s.out_iface(pp, m.parent, &IAMMULTIMEDIASTREAM)?;
    s.com_addref(m.parent);
    Ok(ApiRet::Ret(S_OK))
}

fn ms_get_information(s: &mut Session) -> ApiResult {
    let [_this, ppurpose, ptype] = s.args::<3>()?;
    let (_id, m) = ms_self(s)?;
    if ppurpose != 0 {
        s.mem.write(ppurpose, &m.purpose)?;
    }
    if ptype != 0 {
        s.mem.write_u32(ptype, 0)?; // STREAMTYPE_READ
    }
    Ok(ApiRet::Ret(S_OK))
}

/// The video format: the movie's size in the display's pixel format.
fn video_ddsd(s: &mut Session, stream: u32, size: u32) -> Result<Option<Vec<u8>>, ApiError> {
    if !ensure_movie(s, stream) {
        return Ok(None);
    }
    let mv = stream_of(s, stream).unwrap().cache.0.as_ref().unwrap();
    let (w, h) = (mv.width, mv.height);
    let fmt = match s.user.mode.bpp {
        16 => Fmt::Rgb565,
        24 => Fmt::Rgb888,
        _ => Fmt::Xrgb8888,
    };
    let n = if size == 124 { 124 } else { 108 };
    let mut b = vec![0u8; n];
    let p = |b: &mut Vec<u8>, o: usize, v: u32| b[o..o + 4].copy_from_slice(&v.to_le_bytes());
    p(&mut b, 0, n as u32);
    p(&mut b, 4, 0x1 | 0x2 | 0x4 | 0x1000); // CAPS | HEIGHT | WIDTH | PIXELFORMAT
    p(&mut b, 8, h);
    p(&mut b, 12, w);
    b[72..104].copy_from_slice(&fmt.ddpf());
    p(&mut b, 104, 0x40 | 0x800); // OFFSCREENPLAIN | SYSTEMMEMORY
    Ok(Some(b))
}

fn dms_get_format(s: &mut Session) -> ApiResult {
    let [_this, pcur, ppal, pdesired, pflags] = s.args::<5>()?;
    let (_id, m) = ms_self(s)?;
    for p in [pcur, pdesired] {
        if p == 0 {
            continue;
        }
        let size = s.mem.read_u32(p)?;
        match video_ddsd(s, m.parent, size)? {
            Some(b) => s.mem.write(p, &b)?,
            None => return Ok(ApiRet::Ret(MS_E_NOTINIT)),
        }
    }
    if ppal != 0 {
        s.mem.write_u32(ppal, 0)?;
    }
    if pflags != 0 {
        s.mem.write_u32(pflags, 0)?;
    }
    Ok(ApiRet::Ret(S_OK))
}

fn dms_set_format(s: &mut Session) -> ApiResult {
    let [_this, p, _pal] = s.args::<3>()?;
    let flags = s.mem.read_u32(p + 4)?;
    if flags & 0x1000 != 0 && Fmt::from_ddpf(&s.mem.read_vec(p + 72, 32)?).is_none() {
        return Ok(ApiRet::Ret(MS_E_INCOMPATIBLE));
    }
    // Frames are converted to the sample surface's format when delivered.
    Ok(ApiRet::Ret(S_OK))
}

fn dms_get_direct_draw(s: &mut Session) -> ApiResult {
    let [_this, pp] = s.args::<2>()?;
    let (_id, m) = ms_self(s)?;
    if m.ddraw == 0 {
        s.mem.write_u32(pp, 0)?;
        return Ok(ApiRet::Ret(S_OK));
    }
    s.out_iface(pp, m.ddraw, &crate::com::ddraw::IDIRECTDRAW)?;
    s.com_addref(m.ddraw);
    Ok(ApiRet::Ret(S_OK))
}

fn dms_create_sample(s: &mut Session) -> ApiResult {
    let [_this, surf, prect, _flags, pp] = s.args::<5>()?;
    let (id, _m) = ms_self(s)?;
    if surf == 0 {
        s.missing(
            "IDirectDrawMediaStream::CreateSample",
            "samples with a stream-allocated surface".into(),
        );
        return Ok(ApiRet::Ret(E_NOTIMPL));
    }
    let Some(sobj) = s.surface_obj_of_ptr(surf) else {
        return Ok(ApiRet::Ret(E_INVALIDARG));
    };
    let rect = if prect != 0 { Some(s.read_rect(prect)?) } else { None };
    s.com_addref(sobj);
    let sample = s.com_new(ComKind::StreamSample(StreamSample {
        stream: id,
        surface: sobj,
        rect,
    }));
    s.com_addref(id);
    s.out_iface(pp, sample, &IDIRECTDRAWSTREAMSAMPLE)?;
    Ok(ApiRet::Ret(S_OK))
}

fn dms_get_time_per_frame(s: &mut Session) -> ApiResult {
    let [_this, p] = s.args::<2>()?;
    let (_id, m) = ms_self(s)?;
    if !ensure_movie(s, m.parent) {
        return Ok(ApiRet::Ret(MS_E_NOTINIT));
    }
    let fps = stream_of(s, m.parent).unwrap().cache.0.as_ref().unwrap().fps.max(1.0);
    s.mem.write_u64(p, (1e7 / fps) as u64)?;
    Ok(ApiRet::Ret(S_OK))
}

pub fn media_stream_destroyed(s: &mut Session, m: &MediaStream) {
    if m.ddraw != 0 && s.com.objs.contains_key(&m.ddraw) {
        s.com_release(m.ddraw);
    }
}

// ---------------------------------------------------------------- samples

fn sample_self(s: &Session) -> Result<(u32, StreamSample), ApiError> {
    let id = s.this_obj()?;
    match &s.com_obj(id).map(|o| &o.kind) {
        Some(ComKind::StreamSample(x)) => Ok((id, x.clone())),
        _ => Err(ApiError::Fatal("not a stream sample".into())),
    }
}

/// Write BGRA pixels into a region of a surface object, converting the format.
fn write_frame(s: &mut Session, obj: u32, rect: Option<Rect>, w: u32, h: u32, bgra: &[u8]) -> Result<(), ApiError> {
    let Some(sid) = s.surface_of_obj(obj) else {
        return Ok(());
    };
    s.surface_cpu_access(sid, true);
    let sf = s.display.surfaces[&sid].clone();
    let r = rect
        .unwrap_or(Rect::new(0, 0, w as i32, h as i32))
        .intersect(&Rect::new(0, 0, sf.w as i32, sf.h as i32));
    if r.is_empty() || sf.fmt.is_compressed() || sf.fmt.is_depth() || sf.fmt == Fmt::P8 {
        if sf.fmt == Fmt::P8 {
            s.missing("IDirectDrawStreamSample::Update", "8-bit sample surfaces".into());
        }
        return Ok(());
    }
    let bpp = sf.fmt.bytes_pp() as usize;
    let (cw, ch) = (r.w().min(w as i32) as usize, r.h().min(h as i32) as usize);
    let mut row = vec![0u8; cw * bpp];
    for y in 0..ch {
        let src = &bgra[y * w as usize * 4..];
        for x in 0..cw {
            let o = x * 4;
            let argb = 0xff00_0000 | u32::from_le_bytes([src[o], src[o + 1], src[o + 2], 0]);
            sf.fmt.encode(argb, &mut row[x * bpp..x * bpp + bpp]);
        }
        let a = sf.row_addr(r.top as u32 + y as u32) + r.left as u32 * bpp as u32;
        s.mem.write(a, &row)?;
    }
    if let Some(p) = s.display.surfaces.get_mut(&sid) {
        p.version += 1;
    }
    Ok(())
}

fn sample_update(s: &mut Session) -> ApiResult {
    let [_this, flags, event, apc, _data] = s.args::<5>()?;
    let (_id, sm) = sample_self(s)?;
    if flags != 0 || event != 0 || apc != 0 {
        s.missing(
            "IDirectDrawStreamSample::Update",
            format!("asynchronous updates (flags {flags:#x})"),
        );
        return Ok(ApiRet::Ret(E_NOTIMPL));
    }
    let Some(ms) = media_of(s, sm.stream) else {
        return Ok(ApiRet::Ret(E_FAIL));
    };
    let mid = ms.parent;
    let Some(m) = stream_of(s, mid) else {
        return Ok(ApiRet::Ret(E_FAIL));
    };
    if !m.running {
        return Ok(ApiRet::Ret(MS_E_NOTRUNNING));
    }
    if m.ended || !ensure_movie(s, mid) {
        return Ok(ApiRet::Ret(MS_S_ENDOFSTREAM));
    }
    let now = s.clock.now_ns();
    let pos = stream_of(s, mid).unwrap().pos_at(now);
    // Take frames until the newest one due now; wait for it if none is due yet.
    let mut shown: Option<Vec<u8>> = None;
    loop {
        let st = stream_mut(s, mid).unwrap();
        let mv = st.cache.0.as_mut().unwrap();
        match mv.fill() {
            Ok(true) => {}
            Ok(false) => break,
            Err(e) => {
                st.ended = true;
                s.missing("AMMultiMediaStream", format!("video decoding failed: {e}"));
                break;
            }
        }
        let due = mv.next.as_ref().unwrap().0;
        if due > pos {
            if shown.is_some() {
                break;
            }
            let anchor = st.anchor_ns.saturating_add(due - st.pos_ns.min(due));
            return Ok(ApiRet::Block(Block {
                wait: WaitKind::Sleep,
                deadline: Some(anchor.max(now + 1)),
                cont: Cont::Retry,
            }));
        }
        let (_, px) = mv.next.take().unwrap();
        st.frames_read += 1;
        shown = Some(px);
    }
    let Some(px) = shown else {
        stream_mut(s, mid).unwrap().ended = true;
        return Ok(ApiRet::Ret(MS_S_ENDOFSTREAM));
    };
    let (w, h) = {
        let mv = stream_of(s, mid).unwrap().cache.0.as_ref().unwrap();
        (mv.width, mv.height)
    };
    write_frame(s, sm.surface, sm.rect, w, h, &px)?;
    Ok(ApiRet::Ret(S_OK))
}

fn sample_completion_status(s: &mut Session) -> ApiResult {
    // Updates are synchronous, so none is ever pending.
    let [_this, _flags, _ms] = s.args::<3>()?;
    Ok(ApiRet::Ret(S_OK))
}

fn sample_get_media_stream(s: &mut Session) -> ApiResult {
    let [_this, pp] = s.args::<2>()?;
    let (_id, sm) = sample_self(s)?;
    s.out_iface(pp, sm.stream, &IDIRECTDRAWMEDIASTREAM)?;
    s.com_addref(sm.stream);
    Ok(ApiRet::Ret(S_OK))
}

fn sample_get_surface(s: &mut Session) -> ApiResult {
    let [_this, pp, prect] = s.args::<3>()?;
    let (_id, sm) = sample_self(s)?;
    if pp != 0 {
        s.out_iface(pp, sm.surface, &crate::com::ddraw::IDIRECTDRAWSURFACE)?;
        s.com_addref(sm.surface);
    }
    if prect != 0 {
        let r = match sm.rect {
            Some(r) => r,
            None => {
                let sid = s.surface_of_obj(sm.surface).unwrap_or(0);
                let p = s.display.surfaces.get(&sid);
                Rect::new(
                    0,
                    0,
                    p.map(|p| p.w).unwrap_or(0) as i32,
                    p.map(|p| p.h).unwrap_or(0) as i32,
                )
            }
        };
        s.write_rect(prect, &r)?;
    }
    Ok(ApiRet::Ret(S_OK))
}

pub fn sample_destroyed(s: &mut Session, x: &StreamSample) {
    for o in [x.surface, x.stream] {
        if s.com.objs.contains_key(&o) {
            s.com_release(o);
        }
    }
}

// ---------------------------------------------------------------- audio

/// Add the sound tracks of running streams to the mixer output starting at `t0`.
pub fn mix_into(s: &Session, t0: u64, out_rate: u32, out: &mut [f32]) {
    for o in s.com.objs.values() {
        let ComKind::MmStream(m) = &o.kind else { continue };
        if !m.running || !m.audio_rendered {
            continue;
        }
        let Some(mv) = m.cache.0.as_ref() else { continue };
        if mv.audio_rate == 0 {
            continue;
        }
        let frames = mv.audio.len() / 2;
        let rate = mv.audio_rate as f64;
        for k in 0..out.len() / 2 {
            let t = t0 + (k as u128 * 1_000_000_000 / out_rate as u128) as u64;
            if t < m.anchor_ns {
                continue;
            }
            let pos = m.pos_ns + (t - m.anchor_ns);
            let f = pos as f64 * rate / 1e9;
            let i0 = f as usize;
            if i0 + 1 >= frames {
                continue;
            }
            let frac = (f - i0 as f64) as f32;
            for c in 0..2 {
                let (a, b) = (mv.audio[i0 * 2 + c], mv.audio[(i0 + 1) * 2 + c]);
                out[k * 2 + c] += a + (b - a) * frac;
            }
        }
    }
}

// ---------------------------------------------------------------- interface tables

/// IAMMultiMediaStream methods; IMultiMediaStream has the first 12.
static MMS_METHODS: [ExportDef; 19] = [
    iunknown!()[0],
    iunknown!()[1],
    iunknown!()[2],
    export!("GetInformation", 3, mms_get_information),
    export!("GetMediaStream", 3, mms_get_media_stream),
    unimpl_method!("EnumMediaStreams", 3),
    export!("GetState", 2, mms_get_state),
    export!("SetState", 2, mms_set_state),
    export!("GetTime", 2, mms_get_time),
    export!("GetDuration", 2, mms_get_duration),
    export!("Seek", 3, mms_seek),
    unimpl_method!("GetEndOfStreamEventHandle", 2),
    export!("Initialize", 4, mms_initialize),
    unimpl_method!("GetFilterGraph", 2),
    unimpl_method!("GetFilter", 2),
    export!("AddMediaStream", 5, mms_add_media_stream),
    export!("OpenFile", 3, mms_open_file),
    unimpl_method!("OpenMoniker", 4),
    unimpl_method!("Render", 2),
];

pub static IAMMULTIMEDIASTREAM: IfaceDef = IfaceDef {
    name: "IAMMultiMediaStream",
    iid: guid("bebe595c-9a6f-11d0-8fde-00c04fd9189d"),
    module: "AMSTREAM.DLL",
    methods: &MMS_METHODS,
};

pub static IMULTIMEDIASTREAM: IfaceDef = IfaceDef {
    name: "IMultiMediaStream",
    iid: guid("b502d1bc-9a57-11d0-8fde-00c04fd9189d"),
    module: "AMSTREAM.DLL",
    methods: MMS_METHODS.split_at(12).0,
};

/// IDirectDrawMediaStream methods; IMediaStream has the first 9.
static DMS_METHODS: [ExportDef; 15] = [
    iunknown!()[0],
    iunknown!()[1],
    iunknown!()[2],
    export!("GetMultiMediaStream", 2, ms_get_multimedia_stream),
    export!("GetInformation", 3, ms_get_information),
    unimpl_method!("SetSameFormat", 3),
    unimpl_method!("AllocateSample", 3),
    unimpl_method!("CreateSharedSample", 4),
    unimpl_method!("SendEndOfStream", 2),
    export!("GetFormat", 5, dms_get_format),
    export!("SetFormat", 3, dms_set_format),
    export!("GetDirectDraw", 2, dms_get_direct_draw),
    unimpl_method!("SetDirectDraw", 2),
    export!("CreateSample", 5, dms_create_sample),
    export!("GetTimePerFrame", 2, dms_get_time_per_frame),
];

pub static IDIRECTDRAWMEDIASTREAM: IfaceDef = IfaceDef {
    name: "IDirectDrawMediaStream",
    iid: guid("f4104fce-9a70-11d0-8fde-00c04fd9189d"),
    module: "AMSTREAM.DLL",
    methods: &DMS_METHODS,
};

pub static IMEDIASTREAM: IfaceDef = IfaceDef {
    name: "IMediaStream",
    iid: guid("b502d1bd-9a57-11d0-8fde-00c04fd9189d"),
    module: "AMSTREAM.DLL",
    methods: DMS_METHODS.split_at(9).0,
};

pub static IDIRECTDRAWSTREAMSAMPLE: IfaceDef = IfaceDef {
    name: "IDirectDrawStreamSample",
    iid: guid("f4104fcf-9a70-11d0-8fde-00c04fd9189d"),
    module: "AMSTREAM.DLL",
    methods: &[
        iunknown!()[0],
        iunknown!()[1],
        iunknown!()[2],
        export!("GetMediaStream", 2, sample_get_media_stream),
        unimpl_method!("GetSampleTimes", 4),
        unimpl_method!("SetSampleTimes", 3),
        export!("Update", 5, sample_update),
        export!("CompletionStatus", 3, sample_completion_status),
        export!("GetSurface", 3, sample_get_surface),
        unimpl_method!("SetRect", 2),
    ],
};

/// Interfaces each object kind answers to QueryInterface with.
pub fn supported(kind: &ComKind) -> Vec<&'static IfaceDef> {
    match kind {
        ComKind::MmStream(_) => vec![&IAMMULTIMEDIASTREAM, &IMULTIMEDIASTREAM],
        ComKind::MediaStream(m) if m.purpose == MSPID_PRIMARY_VIDEO => {
            vec![&IMEDIASTREAM, &IDIRECTDRAWMEDIASTREAM]
        }
        ComKind::MediaStream(_) => vec![&IMEDIASTREAM],
        ComKind::StreamSample(_) => vec![&IDIRECTDRAWSTREAMSAMPLE],
        _ => vec![],
    }
}
