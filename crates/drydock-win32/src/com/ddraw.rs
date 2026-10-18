//! DirectDraw 7: display modes, surfaces, flipping, blits, palettes, clippers.
//!
//! Surface pixels live in guest memory ([`crate::surface`]). Flip exchanges the memory
//! (surface ids) behind the objects of a flip chain, as DirectDraw does, and is retired
//! at the next vertical blank of the virtual display, so frame pacing follows the
//! emulated refresh rate in virtual time.

use super::*;
use crate::dll::user32::Rect;
use crate::surface::{ColorKey, Fmt, PixelTarget};
use crate::{export, iunknown, unimpl_method};
use serde::{Deserialize, Serialize};

pub const DD_OK: u32 = 0;
pub const fn dderr(code: u32) -> u32 {
    0x8876_0000 | code
}
pub const DDERR_INVALIDPARAMS: u32 = E_INVALIDARG;
pub const DDERR_UNSUPPORTED: u32 = E_NOTIMPL;
pub const DDERR_GENERIC: u32 = E_FAIL;
pub const DDERR_OUTOFMEMORY: u32 = E_OUTOFMEMORY;
pub const DDERR_INVALIDCAPS: u32 = dderr(100);
pub const DDERR_INVALIDMODE: u32 = dderr(120);
pub const DDERR_INVALIDOBJECT: u32 = dderr(130);
pub const DDERR_INVALIDPIXELFORMAT: u32 = dderr(145);
pub const DDERR_INVALIDRECT: u32 = dderr(150);
pub const DDERR_NOCOOPERATIVELEVELSET: u32 = dderr(212);
pub const DDERR_NOCOLORKEY: u32 = dderr(215);
pub const DDERR_NOEXCLUSIVEMODE: u32 = dderr(225);
pub const DDERR_NOTFOUND: u32 = dderr(255);
pub const DDERR_OUTOFVIDEOMEMORY: u32 = dderr(380);
pub const DDERR_SURFACEBUSY: u32 = dderr(430);
pub const DDERR_SURFACELOST: u32 = dderr(450);
pub const DDERR_SURFACENOTATTACHED: u32 = dderr(460);
pub const DDERR_VERTICALBLANKINPROGRESS: u32 = dderr(537);
pub const DDERR_WASSTILLDRAWING: u32 = dderr(540);
pub const DDERR_NOCLIPPERATTACHED: u32 = dderr(568);
pub const DDERR_NOPALETTEATTACHED: u32 = dderr(572);
pub const DDERR_NOTFLIPPABLE: u32 = dderr(582);
pub const DDERR_NOTLOCKED: u32 = dderr(584);
pub const DDERR_NODC: u32 = dderr(586);
pub const DDERR_PRIMARYSURFACEALREADYEXISTS: u32 = dderr(564);
pub const DDERR_CANTCREATEDC: u32 = dderr(585);
pub const DDERR_DCALREADYCREATED: u32 = dderr(620);

pub const DDSCAPS_ALPHA: u32 = 0x2;
pub const DDSCAPS_BACKBUFFER: u32 = 0x4;
pub const DDSCAPS_COMPLEX: u32 = 0x8;
pub const DDSCAPS_FLIP: u32 = 0x10;
pub const DDSCAPS_FRONTBUFFER: u32 = 0x20;
pub const DDSCAPS_OFFSCREENPLAIN: u32 = 0x40;
pub const DDSCAPS_PALETTE: u32 = 0x100;
pub const DDSCAPS_PRIMARYSURFACE: u32 = 0x200;
pub const DDSCAPS_SYSTEMMEMORY: u32 = 0x800;
pub const DDSCAPS_TEXTURE: u32 = 0x1000;
pub const DDSCAPS_3DDEVICE: u32 = 0x2000;
pub const DDSCAPS_VIDEOMEMORY: u32 = 0x4000;
pub const DDSCAPS_VISIBLE: u32 = 0x8000;
pub const DDSCAPS_ZBUFFER: u32 = 0x20000;
pub const DDSCAPS_OWNDC: u32 = 0x40000;
pub const DDSCAPS_MIPMAP: u32 = 0x40_0000;
pub const DDSCAPS_LOCALVIDMEM: u32 = 0x1000_0000;
pub const DDSCAPS2_TEXTUREMANAGE: u32 = 0x10;
pub const DDSCAPS2_D3DTEXTUREMANAGE: u32 = 0x2_0000;
pub const DDSCAPS2_MIPMAPSUBLEVEL: u32 = 0x1_0000;

const DDSD_CAPS: u32 = 0x1;
const DDSD_HEIGHT: u32 = 0x2;
const DDSD_WIDTH: u32 = 0x4;
const DDSD_PITCH: u32 = 0x8;
const DDSD_BACKBUFFERCOUNT: u32 = 0x20;
const DDSD_ZBUFFERBITDEPTH: u32 = 0x40;
const DDSD_LPSURFACE: u32 = 0x800;
const DDSD_PIXELFORMAT: u32 = 0x1000;
const DDSD_CKDESTBLT: u32 = 0x4000;
const DDSD_CKSRCBLT: u32 = 0x10000;
const DDSD_MIPMAPCOUNT: u32 = 0x20000;
const DDSD_REFRESHRATE: u32 = 0x40000;
const DDSD_LINEARSIZE: u32 = 0x80000;

pub const DDSCL_FULLSCREEN: u32 = 0x1;
pub const DDSCL_NORMAL: u32 = 0x8;
pub const DDSCL_EXCLUSIVE: u32 = 0x10;
pub const DDSCL_FPUPRESERVE: u32 = 0x1000;
/// Busy answers a polling Flip gets before it waits for the vertical blank itself.
const FLIP_POLLS_BEFORE_WAIT: u32 = 4;
pub const DDSCL_FPUSETUP: u32 = 0x800;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DDrawGlobal {
    /// Object holding exclusive mode, if any.
    pub exclusive: Option<u32>,
    pub coop_hwnd: u32,
    pub coop_flags: u32,
    pub primary: Option<u32>,
    /// Virtual time at which the pending flip is retired (next vblank).
    pub flip_pending_until: Option<u64>,
    /// Flip calls without DDFLIP_WAIT answered "still drawing" since the last flip.
    #[serde(default)]
    pub flip_busy_polls: u32,
    /// Number of display mode changes (surfaces created before a change are lost).
    pub mode_epoch: u32,
    pub gamma: Vec<u16>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DDrawObj {
    pub coop: u32,
    pub hwnd: u32,
    pub mode_set: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SurfObj {
    /// Current pixel storage (changes on Flip).
    pub surface: u32,
    pub caps: u32,
    pub caps2: u32,
    pub ddraw: u32,
    pub primary: bool,
    /// Flip chain in order front, back1, back2… (object ids), owned by the front.
    pub chain: Vec<u32>,
    /// Explicitly attached surfaces (z-buffer, mip levels), object ids.
    pub attached: Vec<u32>,
    /// Surface this one is implicitly part of (back buffer / mip sublevel).
    pub parent: u32,
    pub clipper: u32,
    pub palette: u32,
    pub lost: bool,
    pub epoch: u32,
    pub priority: u32,
    pub lod: u32,
    pub dc: u32,
    pub lock_rect: Option<Rect>,
    pub texture_stage: u32,
    pub private: Vec<(Guid, Vec<u8>)>,
    pub uniqueness: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PalObj {
    pub palette: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ClipperObj {
    pub hwnd: u32,
    pub clip: Vec<Rect>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DdCont {
    /// Return a value after a message sequence (display mode change).
    Done(u32),
}

pub fn resume(_s: &mut Session, c: DdCont, _v: Resume) -> ApiResult {
    match c {
        DdCont::Done(r) => Ok(ApiRet::Ret(r)),
    }
}

impl Session {
    fn surf(&self, obj: u32) -> Option<&SurfObj> {
        match &self.com_obj(obj)?.kind {
            ComKind::Surface(s) => Some(s),
            _ => None,
        }
    }
    fn surf_mut(&mut self, obj: u32) -> Option<&mut SurfObj> {
        match &mut self.com_obj_mut(obj)?.kind {
            ComKind::Surface(s) => Some(s),
            _ => None,
        }
    }
    /// Pixel storage id of a surface object.
    pub fn surface_of_obj(&self, obj: u32) -> Option<u32> {
        self.surf(obj).map(|s| s.surface)
    }
    pub fn surface_obj_of_ptr(&self, ptr: u32) -> Option<u32> {
        let o = self.obj_of(ptr)?;
        self.surf(o).map(|_| o)
    }
    fn display_fmt(&self) -> Fmt {
        match self.user.mode.bpp {
            8 => Fmt::P8,
            16 => Fmt::Rgb565,
            24 => Fmt::Rgb888,
            _ => Fmt::Xrgb8888,
        }
    }
    fn vblank_period_ns(&self) -> u64 {
        1_000_000_000 / self.user.mode.refresh.max(1) as u64
    }
    pub fn next_vblank(&self) -> u64 {
        let p = self.vblank_period_ns();
        (self.clock.now_ns() / p + 1) * p
    }
}

// ---------------------------------------------------------------- creation

pub static DDRAW_IFACES: &[&IfaceDef] = &[&IDIRECTDRAW7, &IDIRECTDRAW4, &IDIRECTDRAW2, &IDIRECTDRAW];
pub static SURFACE_IFACES: &[&IfaceDef] = &[
    &IDIRECTDRAWSURFACE7,
    &IDIRECTDRAWSURFACE4,
    &IDIRECTDRAWSURFACE3,
    &IDIRECTDRAWSURFACE2,
    &IDIRECTDRAWSURFACE,
];

fn new_ddraw(s: &mut Session) -> u32 {
    s.com_new(ComKind::DirectDraw(DDrawObj {
        coop: 0,
        hwnd: 0,
        mode_set: false,
    }))
}

fn direct_draw_create_ex(s: &mut Session) -> ApiResult {
    let [guid_p, pp, iid_p, outer] = s.args::<4>()?;
    if outer != 0 {
        return Ok(ApiRet::Ret(0x8004_0110)); // CLASS_E_NOAGGREGATION
    }
    if guid_p > 2 {
        let g = s.read_guid(guid_p)?;
        if g != [0u8; 16] {
            return Ok(ApiRet::Ret(dderr(561))); // DDERR_INVALIDDIRECTDRAWGUID
        }
    }
    let iid = s.read_guid(iid_p)?;
    if iid != IDIRECTDRAW7.iid {
        return Ok(ApiRet::Ret(DDERR_INVALIDPARAMS));
    }
    let id = new_ddraw(s);
    s.out_iface(pp, id, &IDIRECTDRAW7)?;
    Ok(ApiRet::Ret(DD_OK))
}

fn direct_draw_create(s: &mut Session) -> ApiResult {
    let [guid_p, pp, outer] = s.args::<3>()?;
    if outer != 0 {
        return Ok(ApiRet::Ret(0x8004_0110));
    }
    let _ = guid_p;
    let id = new_ddraw(s);
    s.out_iface(pp, id, &IDIRECTDRAW)?;
    Ok(ApiRet::Ret(DD_OK))
}

const DRIVER_DESC: &str = "Primary Display Driver";
const DRIVER_NAME: &str = "display";

fn cstr_buf(t: &str) -> CallArg {
    let mut b = crate::strings::encode_ansi(t);
    b.push(0);
    CallArg::Buf(b)
}

fn direct_draw_enumerate_ex_a(s: &mut Session) -> ApiResult {
    let [cb, ctx, _flags] = s.args::<3>()?;
    // One display device: the primary driver (lpGUID == NULL), on the primary monitor.
    let items = vec![vec![
        CallArg::Val(0),
        cstr_buf(DRIVER_DESC),
        cstr_buf(DRIVER_NAME),
        CallArg::Val(ctx),
        CallArg::Val(0),
    ]];
    enum_callbacks(cb, items, 0, DD_OK)
}

fn direct_draw_enumerate_a(s: &mut Session) -> ApiResult {
    let [cb, ctx] = s.args::<2>()?;
    let items = vec![vec![
        CallArg::Val(0),
        cstr_buf(DRIVER_DESC),
        cstr_buf(DRIVER_NAME),
        CallArg::Val(ctx),
    ]];
    enum_callbacks(cb, items, 0, DD_OK)
}

// ---------------------------------------------------------------- IDirectDraw7

fn dd_self(s: &Session) -> Result<u32, ApiError> {
    s.this_obj()
}

fn dd_set_cooperative_level(s: &mut Session) -> ApiResult {
    let [_this, hwnd, flags] = s.args::<3>()?;
    let id = dd_self(s)?;
    let excl = flags & DDSCL_EXCLUSIVE != 0;
    if excl && flags & DDSCL_FULLSCREEN == 0 {
        return Ok(ApiRet::Ret(DDERR_INVALIDPARAMS));
    }
    if excl && hwnd == 0 {
        return Ok(ApiRet::Ret(DDERR_INVALIDPARAMS));
    }
    if excl
        && let Some(o) = s.com.ddraw.exclusive
        && o != id
    {
        return Ok(ApiRet::Ret(dderr(581))); // DDERR_EXCLUSIVEMODEALREADYSET
    }
    if let Some(ComKind::DirectDraw(d)) = s.com_obj_mut(id).map(|o| &mut o.kind) {
        d.coop = flags;
        d.hwnd = hwnd;
    }
    s.com.ddraw.coop_hwnd = hwnd;
    s.com.ddraw.coop_flags = flags;
    s.com.ddraw.exclusive = if excl { Some(id) } else { None };
    Ok(ApiRet::Ret(DD_OK))
}

fn mode_list(s: &Session) -> Vec<crate::dll::user32::DisplayMode> {
    crate::dll::user32::supported_modes(s.config.windows.refresh_hz)
}

fn dd_set_display_mode(s: &mut Session) -> ApiResult {
    let [_this, w, h, bpp, refresh, _flags] = s.args::<6>()?;
    set_display_mode(s, w, h, bpp, refresh)
}

fn set_display_mode(s: &mut Session, w: u32, h: u32, bpp: u32, refresh: u32) -> ApiResult {
    let id = dd_self(s)?;
    let ok = mode_list(s)
        .iter()
        .any(|m| m.width == w && m.height == h && m.bpp == bpp && (refresh == 0 || refresh == m.refresh));
    if !ok {
        return Ok(ApiRet::Ret(DDERR_INVALIDMODE));
    }
    let excl = s.com.ddraw.exclusive == Some(id);
    let changed = s.user.mode.width != w || s.user.mode.height != h || s.user.mode.bpp != bpp;
    s.user.mode = crate::dll::user32::DisplayMode {
        width: w,
        height: h,
        bpp,
        refresh: if refresh == 0 {
            s.config.windows.refresh_hz
        } else {
            refresh
        },
    };
    if let Some(ComKind::DirectDraw(d)) = s.com_obj_mut(id).map(|o| &mut o.kind) {
        d.mode_set = true;
    }
    if changed {
        s.com.ddraw.mode_epoch += 1;
        let epoch = s.com.ddraw.mode_epoch;
        // Video-memory surfaces created in the previous mode are lost.
        for o in s.com.objs.values_mut() {
            if let ComKind::Surface(sf) = &mut o.kind
                && sf.epoch != epoch
                && sf.caps & DDSCAPS_SYSTEMMEMORY == 0
            {
                sf.lost = true;
            }
        }
    }
    // The device window covers the new screen in exclusive mode; WM_DISPLAYCHANGE is
    // broadcast to top-level windows.
    let hwnd = s.com.ddraw.coop_hwnd;
    let mut steps = std::collections::VecDeque::new();
    if changed {
        let tops: Vec<u32> = s
            .user
            .windows
            .values()
            .filter(|w| w.style & crate::dll::user32::WS_CHILD == 0)
            .map(|w| w.hwnd)
            .collect();
        for t in tops {
            steps.push_back(crate::dll::user32::Step::Send {
                hwnd: t,
                msg: crate::dll::user32::WM_DISPLAYCHANGE,
                wp: bpp,
                lp: crate::dll::user32::LParam::Val(w | (h << 16)),
                check: crate::dll::user32::Check::None,
            });
        }
    }
    if excl && s.window(hwnd).is_some() {
        steps.push_back(crate::dll::user32::Step::Act(crate::dll::user32::Act::CommitPos {
            hwnd,
            rect: Rect::new(0, 0, w as i32, h as i32),
            flags: 0x4 | 0x10,
        }));
    }
    crate::dll::user32::run_seq(s, steps, DD_OK)
}

fn dd_restore_display_mode(s: &mut Session) -> ApiResult {
    let d = s.user.desktop_mode;
    if s.user.mode != d {
        s.user.mode = d;
        s.com.ddraw.mode_epoch += 1;
    }
    Ok(ApiRet::Ret(DD_OK))
}

fn dd_get_display_mode(s: &mut Session) -> ApiResult {
    let [_this, p] = s.args::<2>()?;
    let m = s.user.mode;
    let fmt = s.display_fmt();
    let size = s.mem.read_u32(p)?;
    let b = build_ddsd(
        size,
        DDSD_HEIGHT | DDSD_WIDTH | DDSD_PITCH | DDSD_PIXELFORMAT | DDSD_REFRESHRATE,
        m.width,
        m.height,
        fmt.pitch_for(m.width),
        fmt,
        0,
        0,
        0,
        m.refresh,
    );
    s.mem.write(p, &b)?;
    Ok(ApiRet::Ret(DD_OK))
}

#[allow(clippy::too_many_arguments)]
fn build_ddsd(
    size: u32,
    flags: u32,
    w: u32,
    h: u32,
    pitch: u32,
    fmt: Fmt,
    caps: u32,
    caps2: u32,
    mem: u32,
    extra24: u32,
) -> Vec<u8> {
    let n = if size == 108 { 108 } else { 124 };
    let mut b = vec![0u8; n];
    let p = |b: &mut Vec<u8>, o: usize, v: u32| b[o..o + 4].copy_from_slice(&v.to_le_bytes());
    p(&mut b, 0, n as u32);
    p(&mut b, 4, flags);
    p(&mut b, 8, h);
    p(&mut b, 12, w);
    p(&mut b, 16, pitch);
    p(&mut b, 24, extra24);
    p(&mut b, 36, mem);
    b[72..104].copy_from_slice(&fmt.ddpf());
    p(&mut b, 104, caps);
    if n == 124 {
        p(&mut b, 108, caps2);
    }
    b
}

fn dd_enum_display_modes(s: &mut Session) -> ApiResult {
    let [_this, _flags, filter, ctx, cb] = s.args::<5>()?;
    let mut fw = None;
    let mut fh = None;
    let mut fbpp = None;
    let mut size = 124;
    if filter != 0 {
        size = s.mem.read_u32(filter)?;
        let fl = s.mem.read_u32(filter + 4)?;
        if fl & DDSD_WIDTH != 0 {
            fw = Some(s.mem.read_u32(filter + 12)?);
        }
        if fl & DDSD_HEIGHT != 0 {
            fh = Some(s.mem.read_u32(filter + 8)?);
        }
        if fl & DDSD_PIXELFORMAT != 0 {
            fbpp = Some(s.mem.read_u32(filter + 72 + 12)?);
        }
    }
    let mut items = Vec::new();
    for m in mode_list(s) {
        if fw.is_some_and(|w| w != m.width) || fh.is_some_and(|h| h != m.height) || fbpp.is_some_and(|b| b != m.bpp) {
            continue;
        }
        let fmt = match m.bpp {
            8 => Fmt::P8,
            16 => Fmt::Rgb565,
            _ => Fmt::Xrgb8888,
        };
        let b = build_ddsd(
            size,
            DDSD_HEIGHT | DDSD_WIDTH | DDSD_PITCH | DDSD_PIXELFORMAT | DDSD_REFRESHRATE,
            m.width,
            m.height,
            fmt.pitch_for(m.width),
            fmt,
            0,
            0,
            0,
            m.refresh,
        );
        items.push(vec![CallArg::Buf(b), CallArg::Val(ctx)]);
    }
    enum_callbacks(cb, items, 0, DD_OK)
}

fn write_ddcaps(s: &mut Session, p: u32, hel: bool) -> Result<(), ApiError> {
    if p == 0 {
        return Ok(());
    }
    let size = s.mem.read_u32(p)?.clamp(4, 380);
    let mut b = vec![0u8; size as usize];
    let put = |b: &mut Vec<u8>, o: usize, v: u32| {
        if o + 4 <= b.len() {
            b[o..o + 4].copy_from_slice(&v.to_le_bytes());
        }
    };
    put(&mut b, 0, size);
    // DDCAPS_3D | BLT | BLTCOLORFILL | BLTSTRETCH | COLORKEY | PALETTE | CANBLTSYSMEM |
    // BLTDEPTHFILL | CANCLIP | CANCLIPSTRETCHED
    let caps = 0x1
        | 0x40
        | 0x0400_0000
        | 0x200
        | 0x40_0000
        | 0x8000_0000
        | 0x1000_0000
        | 0x200_0000
        | 0x2000_0000
        | 0x4000_0000;
    put(&mut b, 4, caps);
    // DDCAPS2: CANRENDERWINDOWED | NOPAGELOCKREQUIRED | FLIPNOVSYNC | WIDESURFACES | PRIMARYGAMMA
    put(&mut b, 8, 0x80000 | 0x4000_0000 | 0x800_0000 | 0x1000 | 0x20000);
    // dwCKeyCaps: SRCBLT | DESTBLT
    put(&mut b, 12, 0x200 | 0x2);
    // dwFXCaps: BLTSTRETCHX/Y, BLTSHRINKX/Y, BLTMIRRORLEFTRIGHT/UPDOWN
    put(
        &mut b,
        16,
        0x0000_0040 | 0x0000_0020 | 0x0000_0200 | 0x0000_0100 | 0x0000_0002 | 0x0000_0004,
    );
    // dwPalCaps: 8BIT | PRIMARYSURFACE
    put(&mut b, 24, 0x4 | 0x10);
    if !hel {
        put(&mut b, 60, s.display.vram_size.max(crate::surface::VRAM_SIZE));
        put(&mut b, 64, s.vram_free().max(1));
    }
    // ddsCaps (DDSCAPS at offset 104 in the DX6/7 layout's legacy field).
    let scaps = DDSCAPS_BACKBUFFER
        | DDSCAPS_COMPLEX
        | DDSCAPS_FLIP
        | DDSCAPS_FRONTBUFFER
        | DDSCAPS_OFFSCREENPLAIN
        | DDSCAPS_PALETTE
        | DDSCAPS_PRIMARYSURFACE
        | DDSCAPS_SYSTEMMEMORY
        | DDSCAPS_TEXTURE
        | DDSCAPS_3DDEVICE
        | DDSCAPS_VIDEOMEMORY
        | DDSCAPS_ZBUFFER
        | DDSCAPS_MIPMAP
        | DDSCAPS_LOCALVIDMEM;
    put(&mut b, 132, scaps);
    if size >= 380 {
        // DDSCAPS2 ddsCaps at the end of the DX7 structure.
        put(&mut b, 364, scaps);
    }
    s.mem.write(p, &b)?;
    Ok(())
}

fn surface_matches(s: &Session, sf: &SurfObj, filter: &[u8]) -> bool {
    let g = |o: usize| u32::from_le_bytes(filter[o..o + 4].try_into().unwrap());
    let flags = g(4);
    let px = &s.display.surfaces[&sf.surface];
    if flags & DDSD_CAPS != 0 && sf.caps & g(104) != g(104) {
        return false;
    }
    if flags & DDSD_WIDTH != 0 && px.w != g(12) {
        return false;
    }
    if flags & DDSD_HEIGHT != 0 && px.h != g(8) {
        return false;
    }
    if flags & DDSD_PIXELFORMAT != 0 && Fmt::from_ddpf(&filter[72..104]) != Some(px.fmt) {
        return false;
    }
    true
}

fn dd_enum_surfaces(s: &mut Session) -> ApiResult {
    let [_this, flags, pdesc, ctx, cb] = s.args::<5>()?;
    let dd = dd_self(s)?;
    if flags & 0x10 == 0 {
        // DDENUMSURFACES_CANBECREATED
        s.missing("EnumSurfaces", format!("flags {flags:#x}"));
        return Ok(ApiRet::Ret(DDERR_UNSUPPORTED));
    }
    let filter = if pdesc != 0 {
        let n = s.mem.read_u32(pdesc)?;
        Some(s.mem.read_vec(pdesc, n.max(108))?)
    } else {
        None
    };
    let all = flags & 1 != 0;
    let nomatch = flags & 4 != 0;
    let ids: Vec<u32> = s
        .com
        .objs
        .iter()
        .filter_map(|(id, o)| match &o.kind {
            ComKind::Surface(sf) if sf.ddraw == dd => Some((*id, sf.clone())),
            _ => None,
        })
        .filter(|(_, sf)| all || filter.as_ref().is_none_or(|f| surface_matches(s, sf, f) != nomatch))
        .map(|(id, _)| id)
        .collect();
    let mut items = Vec::new();
    for id in ids {
        let sf = s.surf(id).unwrap().clone();
        let px = s.display.surfaces[&sf.surface].clone();
        let def = surface_def(s);
        let p = s.iface(id, def)?;
        // The callback receives a reference it must release.
        s.com_addref(id);
        let d = build_ddsd(
            ddsd_size(s),
            DDSD_CAPS | DDSD_WIDTH | DDSD_HEIGHT | DDSD_PIXELFORMAT | DDSD_PITCH,
            px.w,
            px.h,
            px.pitch,
            px.fmt,
            sf.caps,
            sf.caps2,
            0,
            0,
        );
        items.push(vec![CallArg::Val(p), CallArg::Buf(d), CallArg::Val(ctx)]);
    }
    enum_callbacks(cb, items, 0, DD_OK)
}

fn dd_get_caps(s: &mut Session) -> ApiResult {
    let [_this, drv, hel] = s.args::<3>()?;
    write_ddcaps(s, drv, false)?;
    write_ddcaps(s, hel, true)?;
    Ok(ApiRet::Ret(DD_OK))
}

fn dd_get_available_vid_mem(s: &mut Session) -> ApiResult {
    let [_this, _caps, total, free] = s.args::<4>()?;
    if total != 0 {
        s.mem.write_u32(total, crate::surface::VRAM_SIZE)?;
    }
    if free != 0 {
        let f = if s.display.vram_heap == 0 {
            crate::surface::VRAM_SIZE
        } else {
            s.vram_free()
        };
        s.mem.write_u32(free, f)?;
    }
    Ok(ApiRet::Ret(DD_OK))
}

fn dd_get_device_identifier(s: &mut Session) -> ApiResult {
    let [_this, p, _flags] = s.args::<3>()?;
    // DDDEVICEIDENTIFIER2 (IDirectDraw7) adds dwWHQLLevel to DDDEVICEIDENTIFIER; both
    // are padded to a multiple of 8 by the LARGE_INTEGER driver version.
    let mut b = vec![
        0u8;
        if s.caller_version() == DdVersion::V7 {
            1072
        } else {
            1064
        }
    ];
    let drv = b"drydock.drv";
    b[..drv.len()].copy_from_slice(drv);
    let desc = b"Drydock Virtual Display Adapter";
    b[512..512 + desc.len()].copy_from_slice(desc);
    b[1024..1032].copy_from_slice(&0x0004_0005_0000_0001u64.to_le_bytes());
    // Vendor/device ids are zero: the virtual adapter does not impersonate real hardware.
    let g = guid("d7b71f83-6340-11cf-4c74-0420a1c2cb35");
    b[1048..1064].copy_from_slice(&g);
    s.mem.write(p, &b)?;
    Ok(ApiRet::Ret(DD_OK))
}

fn dd_get_monitor_frequency(s: &mut Session) -> ApiResult {
    let [_this, p] = s.args::<2>()?;
    s.mem.write_u32(p, s.user.mode.refresh)?;
    Ok(ApiRet::Ret(DD_OK))
}

fn dd_test_cooperative_level(s: &mut Session) -> ApiResult {
    let id = dd_self(s)?;
    let excl = s.com.ddraw.exclusive == Some(id);
    if excl && !s.user.app_active {
        // Exclusive-mode owner without focus: surfaces are unavailable until reactivated.
        return Ok(ApiRet::Ret(DDERR_NOEXCLUSIVEMODE));
    }
    Ok(ApiRet::Ret(DD_OK))
}

fn dd_get_vertical_blank_status(s: &mut Session) -> ApiResult {
    let [_this, p] = s.args::<2>()?;
    let period = s.vblank_period_ns();
    let phase = s.clock.now_ns() % period;
    let in_vb = phase >= period * 480 / 525;
    s.mem.write_u32(p, in_vb as u32)?;
    Ok(ApiRet::Ret(DD_OK))
}

fn dd_get_scan_line(s: &mut Session) -> ApiResult {
    let [_this, p] = s.args::<2>()?;
    let period = s.vblank_period_ns();
    let total = s.user.mode.height + s.user.mode.height / 20 + 1;
    let line = ((s.clock.now_ns() % period) * total as u64 / period) as u32;
    s.mem.write_u32(p, line)?;
    if line >= s.user.mode.height {
        return Ok(ApiRet::Ret(DDERR_VERTICALBLANKINPROGRESS));
    }
    Ok(ApiRet::Ret(DD_OK))
}

fn dd_wait_for_vertical_blank(s: &mut Session) -> ApiResult {
    let [_this, flags, _h] = s.args::<3>()?;
    if flags == 2 {
        // DDWAITVB_BLOCKBEGINEVENT is not supported by DirectDraw itself.
        return Ok(ApiRet::Ret(DDERR_UNSUPPORTED));
    }
    let t = s.next_vblank();
    Ok(ApiRet::Block(Block {
        wait: WaitKind::Sleep,
        deadline: Some(t),
        cont: Cont::ReturnConst(DD_OK),
    }))
}

fn dd_flip_to_gdi_surface(s: &mut Session) -> ApiResult {
    if let Some(p) = s.com.ddraw.primary
        && let Some(sf) = s.surf(p).cloned()
    {
        s.display.front = sf.surface;
    }
    Ok(ApiRet::Ret(DD_OK))
}

fn dd_get_gdi_surface(s: &mut Session) -> ApiResult {
    let [_this, pp] = s.args::<2>()?;
    match s.com.ddraw.primary {
        Some(p) => {
            let def = surface_def(s);
            s.out_iface(pp, p, def)?;
            s.com_addref(p);
            Ok(ApiRet::Ret(DD_OK))
        }
        None => Ok(ApiRet::Ret(DDERR_NOTFOUND)),
    }
}

fn dd_get_fourcc_codes(s: &mut Session) -> ApiResult {
    let [_this, pn, codes] = s.args::<3>()?;
    let list = [*b"DXT1", *b"DXT3", *b"DXT5"];
    let cap = s.mem.read_u32(pn)?;
    s.mem.write_u32(pn, list.len() as u32)?;
    if codes != 0 {
        for (i, c) in list.iter().enumerate().take(cap as usize) {
            s.mem.write(codes + 4 * i as u32, c)?;
        }
    }
    Ok(ApiRet::Ret(DD_OK))
}

fn dd_restore_all_surfaces(s: &mut Session) -> ApiResult {
    let epoch = s.com.ddraw.mode_epoch;
    for o in s.com.objs.values_mut() {
        if let ComKind::Surface(sf) = &mut o.kind {
            sf.lost = false;
            sf.epoch = epoch;
        }
    }
    Ok(ApiRet::Ret(DD_OK))
}

fn dd_create_palette(s: &mut Session) -> ApiResult {
    let [_this, flags, entries, pp, outer] = s.args::<5>()?;
    if outer != 0 {
        return Ok(ApiRet::Ret(0x8004_0110));
    }
    // DDPCAPS_8BIT = 4, 4BIT = 1, 2BIT/1BIT less common.
    let n = if flags & 0x4 != 0 {
        256
    } else if flags & 0x1 != 0 {
        16
    } else if flags & 0x200 != 0 {
        4
    } else if flags & 0x100 != 0 {
        2
    } else {
        return Ok(ApiRet::Ret(DDERR_INVALIDPARAMS));
    };
    let mut e = Vec::with_capacity(n);
    for i in 0..n as u32 {
        let pe = s.mem.read_u32(entries + 4 * i)?;
        // PALETTEENTRY: R, G, B, flags → 0x00RRGGBB
        e.push((pe & 0xff) << 16 | (pe & 0xff00) | (pe >> 16) & 0xff);
    }
    let pid = s.display.next_id.max(1);
    s.display.next_id = pid + 1;
    s.display.palettes.insert(
        pid,
        crate::surface::Palette {
            id: pid,
            entries: e,
            flags,
        },
    );
    let id = s.com_new(ComKind::Palette(PalObj { palette: pid }));
    s.out_iface(pp, id, &IDIRECTDRAWPALETTE)?;
    Ok(ApiRet::Ret(DD_OK))
}

fn dd_create_clipper(s: &mut Session) -> ApiResult {
    let [_this, _flags, pp, outer] = s.args::<4>()?;
    if outer != 0 {
        return Ok(ApiRet::Ret(0x8004_0110));
    }
    let id = s.com_new(ComKind::Clipper(ClipperObj::default()));
    s.out_iface(pp, id, &IDIRECTDRAWCLIPPER)?;
    Ok(ApiRet::Ret(DD_OK))
}

/// Pixel format requested in a DDSURFACEDESC2, or the default for the caps.
fn requested_format(s: &Session, ddsd: &[u8], caps: u32) -> Result<Fmt, u32> {
    let flags = u32::from_le_bytes(ddsd[4..8].try_into().unwrap());
    if flags & DDSD_PIXELFORMAT != 0 {
        return Fmt::from_ddpf(&ddsd[72..104]).ok_or(DDERR_INVALIDPIXELFORMAT);
    }
    if caps & DDSCAPS_ZBUFFER != 0 {
        let bits = u32::from_le_bytes(ddsd[24..28].try_into().unwrap());
        return Ok(if flags & DDSD_ZBUFFERBITDEPTH != 0 && bits == 32 {
            Fmt::D24S8
        } else {
            Fmt::D16
        });
    }
    Ok(s.display_fmt())
}

fn dd_create_surface(s: &mut Session) -> ApiResult {
    let [_this, pdesc, pp, outer] = s.args::<4>()?;
    if outer != 0 {
        return Ok(ApiRet::Ret(0x8004_0110));
    }
    let dd = dd_self(s)?;
    let size = s.mem.read_u32(pdesc)?;
    if size != 124 && size != 108 {
        return Ok(ApiRet::Ret(DDERR_INVALIDPARAMS));
    }
    let d = s.mem.read_vec(pdesc, size)?;
    let g = |o: usize| u32::from_le_bytes(d[o..o + 4].try_into().unwrap());
    let flags = g(4);
    let mut caps = if flags & DDSD_CAPS != 0 { g(104) } else { 0 };
    let caps2 = if size == 124 && flags & DDSD_CAPS != 0 {
        g(108)
    } else {
        0
    };
    if s.com.ddraw.coop_flags == 0 && s.com.ddraw.exclusive.is_none() && s.com.ddraw.coop_hwnd == 0 {
        let has_level = matches!(s.com_obj(dd).map(|o| &o.kind), Some(ComKind::DirectDraw(x)) if x.coop != 0);
        if !has_level {
            return Ok(ApiRet::Ret(DDERR_NOCOOPERATIVELEVELSET));
        }
    }
    let primary = caps & DDSCAPS_PRIMARYSURFACE != 0;
    let (w, h) = if primary {
        if flags & (DDSD_WIDTH | DDSD_HEIGHT) != 0 {
            return Ok(ApiRet::Ret(DDERR_INVALIDPARAMS));
        }
        (s.user.mode.width, s.user.mode.height)
    } else {
        if flags & DDSD_WIDTH == 0 || flags & DDSD_HEIGHT == 0 {
            return Ok(ApiRet::Ret(DDERR_INVALIDPARAMS));
        }
        (g(12), g(8))
    };
    if w == 0 || h == 0 || w > 4096 || h > 4096 {
        return Ok(ApiRet::Ret(DDERR_INVALIDPARAMS));
    }
    if primary && s.com.ddraw.primary.is_some() {
        return Ok(ApiRet::Ret(DDERR_PRIMARYSURFACEALREADYEXISTS));
    }
    let fmt = if primary {
        s.display_fmt()
    } else {
        match requested_format(s, &d, caps) {
            Ok(f) => f,
            Err(e) => return Ok(ApiRet::Ret(e)),
        }
    };
    // Memory placement: explicit caps win; managed textures and everything else default
    // to video memory (with system-memory fallback for textures, as drivers do).
    let sysmem = caps & DDSCAPS_SYSTEMMEMORY != 0;
    if !sysmem {
        caps |= DDSCAPS_VIDEOMEMORY | DDSCAPS_LOCALVIDMEM;
    }
    let backbuffers = if flags & DDSD_BACKBUFFERCOUNT != 0 && caps & DDSCAPS_FLIP != 0 {
        g(20).max(1)
    } else {
        0
    };
    let mips = if caps & DDSCAPS_MIPMAP != 0 && caps & DDSCAPS_COMPLEX != 0 {
        if flags & DDSD_MIPMAPCOUNT != 0 {
            g(24).max(1)
        } else {
            (32 - w.max(h).leading_zeros()).max(1)
        }
    } else {
        1
    };
    let ck_dst = (flags & DDSD_CKDESTBLT != 0).then(|| ColorKey {
        low: g(48),
        high: g(52),
    });
    let ck_src = (flags & DDSD_CKSRCBLT != 0).then(|| ColorKey {
        low: g(64),
        high: g(68),
    });
    let epoch = s.com.ddraw.mode_epoch;
    let mk = |s: &mut Session, w: u32, h: u32, caps: u32, caps2: u32, primary: bool, parent: u32| -> Result<u32, u32> {
        let id = s.alloc_surface(w, h, fmt, !sysmem, caps).map_err(|_| {
            if sysmem {
                DDERR_OUTOFMEMORY
            } else {
                DDERR_OUTOFVIDEOMEMORY
            }
        })?;
        if let Some(sf) = s.display.surfaces.get_mut(&id) {
            sf.ckey_src = ck_src;
            sf.ckey_dst = ck_dst;
            sf.caps2 = caps2;
        }
        Ok(s.com_new(ComKind::Surface(SurfObj {
            surface: id,
            caps,
            caps2,
            ddraw: dd,
            primary,
            chain: Vec::new(),
            attached: Vec::new(),
            parent,
            clipper: 0,
            palette: 0,
            lost: false,
            epoch,
            priority: 0,
            lod: 0,
            dc: 0,
            lock_rect: None,
            texture_stage: 0,
            private: Vec::new(),
            uniqueness: 1,
        })))
    };
    let front_caps = if backbuffers > 0 {
        caps | DDSCAPS_FRONTBUFFER | if primary { DDSCAPS_VISIBLE } else { 0 }
    } else if primary {
        caps | DDSCAPS_VISIBLE
    } else {
        caps
    };
    let root = match mk(s, w, h, front_caps, caps2, primary, 0) {
        Ok(r) => r,
        Err(e) => return Ok(ApiRet::Ret(e)),
    };
    let mut chain = vec![root];
    for i in 0..backbuffers {
        let mut c = caps & !(DDSCAPS_PRIMARYSURFACE | DDSCAPS_VISIBLE | DDSCAPS_FRONTBUFFER);
        if i == 0 {
            c |= DDSCAPS_BACKBUFFER;
        }
        match mk(s, w, h, c, caps2, false, root) {
            Ok(b) => chain.push(b),
            Err(e) => return Ok(ApiRet::Ret(e)),
        }
    }
    if backbuffers > 0
        && let Some(sf) = s.surf_mut(root)
    {
        sf.chain = chain.clone();
    }
    // Complex mipmap chains: each level attached to the previous one.
    let mut prev = root;
    let (mut mw, mut mh) = (w, h);
    for _ in 1..mips {
        mw = (mw / 2).max(1);
        mh = (mh / 2).max(1);
        let lvl = match mk(s, mw, mh, caps, caps2 | DDSCAPS2_MIPMAPSUBLEVEL, false, root) {
            Ok(l) => l,
            Err(e) => return Ok(ApiRet::Ret(e)),
        };
        if let Some(p) = s.surf_mut(prev) {
            p.attached.push(lvl);
        }
        prev = lvl;
    }
    if primary {
        s.com.ddraw.primary = Some(root);
        s.display.front = s.surface_of_obj(root).unwrap();
    }
    let def = surface_def(s);
    s.out_iface(pp, root, def)?;
    Ok(ApiRet::Ret(DD_OK))
}

pub fn surface_destroyed(s: &mut Session, id: u32, sf: SurfObj) {
    // Implicitly created surfaces (back buffers, mip levels) die with their root.
    for c in sf.chain.iter().skip(1) {
        if s.com.objs.contains_key(c) {
            s.com_destroy_pub(*c);
        }
    }
    for a in &sf.attached {
        if let Some(o) = s.com_obj(*a)
            && let ComKind::Surface(x) = &o.kind
            && x.parent == id
        {
            s.com_destroy_pub(*a);
        } else {
            s.com_release(*a);
        }
    }
    if s.com.ddraw.primary == Some(id) {
        s.com.ddraw.primary = None;
        s.display.front = 0;
    }
    s.free_surface(sf.surface);
}

pub fn ddraw_destroyed(s: &mut Session, id: u32, _d: DDrawObj) {
    // Releasing the DirectDraw object destroys every surface it created, whatever
    // references the application still holds.
    let owned: Vec<u32> = s
        .com
        .objs
        .iter()
        .filter(|(_, o)| matches!(&o.kind, ComKind::Surface(sf) if sf.ddraw == id && sf.parent == 0))
        .map(|(k, _)| *k)
        .collect();
    for o in owned {
        if s.com.objs.contains_key(&o) {
            s.com_destroy_pub(o);
        }
    }
    if s.com.ddraw.exclusive == Some(id) {
        s.com.ddraw.exclusive = None;
        let d = s.user.desktop_mode;
        s.user.mode = d;
    }
}

impl Session {
    pub fn com_destroy_pub(&mut self, id: u32) {
        if let Some(o) = self.com.objs.get_mut(&id) {
            o.refs = 1;
        }
        self.com_release(id);
    }
}

pub fn surface_qi(s: &mut Session, id: u32, iid: &Guid, ppv: u32) -> Result<Option<u32>, ApiError> {
    if *iid == IDIRECTDRAWGAMMACONTROL.iid {
        let primary = s.surf(id).map(|x| x.primary).unwrap_or(false);
        if primary {
            s.out_iface(ppv, id, &IDIRECTDRAWGAMMACONTROL)?;
            s.com_addref(id);
            return Ok(Some(S_OK));
        }
    }
    Ok(None)
}

// ---------------------------------------------------------------- IDirectDrawSurface7

fn sf_self(s: &Session) -> Result<(u32, SurfObj), ApiError> {
    let id = s.this_obj()?;
    let sf = s
        .surf(id)
        .cloned()
        .ok_or_else(|| ApiError::Fatal("not a surface".into()))?;
    Ok((id, sf))
}

fn sf_get_surface_desc(s: &mut Session) -> ApiResult {
    let [_this, p] = s.args::<2>()?;
    let (_id, sf) = sf_self(s)?;
    let size = s.mem.read_u32(p)?;
    if size != 124 && size != 108 {
        return Ok(ApiRet::Ret(DDERR_INVALIDPARAMS));
    }
    let px = s.display.surfaces[&sf.surface].clone();
    let mut flags = DDSD_CAPS | DDSD_HEIGHT | DDSD_WIDTH | DDSD_PIXELFORMAT;
    flags |= if px.fmt.is_compressed() {
        DDSD_LINEARSIZE
    } else {
        DDSD_PITCH
    };
    let bb = sf.chain.len().saturating_sub(1) as u32;
    if bb > 0 {
        flags |= DDSD_BACKBUFFERCOUNT;
    }
    let pitch = if px.fmt.is_compressed() { px.size } else { px.pitch };
    let mut b = build_ddsd(size, flags, px.w, px.h, pitch, px.fmt, sf.caps, sf.caps2, 0, 0);
    b[20..24].copy_from_slice(&bb.to_le_bytes());
    if let Some(k) = px.ckey_src {
        b[64..68].copy_from_slice(&k.low.to_le_bytes());
        b[68..72].copy_from_slice(&k.high.to_le_bytes());
        b[4..8].copy_from_slice(&(flags | DDSD_CKSRCBLT).to_le_bytes());
    }
    s.mem.write(p, &b)?;
    Ok(ApiRet::Ret(DD_OK))
}

fn sf_get_caps(s: &mut Session) -> ApiResult {
    let [_this, p] = s.args::<2>()?;
    let (_id, sf) = sf_self(s)?;
    s.mem.write_u32(p, sf.caps)?;
    // Legacy interfaces take a DDSCAPS (one DWORD), later ones a DDSCAPS2.
    if s.caller_version() != DdVersion::Legacy {
        s.mem.write_u32(p + 4, sf.caps2)?;
        s.mem.write_u32(p + 8, 0)?;
        s.mem.write_u32(p + 12, 0)?;
    }
    Ok(ApiRet::Ret(DD_OK))
}

fn sf_get_pixel_format(s: &mut Session) -> ApiResult {
    let [_this, p] = s.args::<2>()?;
    let (_id, sf) = sf_self(s)?;
    let f = s.display.surfaces[&sf.surface].fmt;
    s.mem.write(p, &f.ddpf())?;
    Ok(ApiRet::Ret(DD_OK))
}

fn sf_is_lost(s: &mut Session) -> ApiResult {
    let (_id, sf) = sf_self(s)?;
    Ok(ApiRet::Ret(if sf.lost { DDERR_SURFACELOST } else { DD_OK }))
}

fn sf_restore(s: &mut Session) -> ApiResult {
    let (id, _) = sf_self(s)?;
    let epoch = s.com.ddraw.mode_epoch;
    if let Some(sf) = s.surf_mut(id) {
        sf.lost = false;
        sf.epoch = epoch;
    }
    Ok(ApiRet::Ret(DD_OK))
}

fn sf_lock(s: &mut Session) -> ApiResult {
    let [_this, prect, pdesc, flags, _ev] = s.args::<5>()?;
    let (id, sf) = sf_self(s)?;
    if sf.lost {
        return Ok(ApiRet::Ret(DDERR_SURFACELOST));
    }
    // A surface still being scanned out by a pending flip cannot be locked yet.
    if let Some(until) = s.com.ddraw.flip_pending_until
        && s.clock.now_ns() < until
        && s.display.front == sf.surface
        && !sf.primary
    {
        if flags & 1 == 0 {
            return Ok(ApiRet::Ret(DDERR_WASSTILLDRAWING));
        }
        return Ok(ApiRet::Block(Block {
            wait: WaitKind::Sleep,
            deadline: Some(until),
            cont: Cont::Retry,
        }));
    }
    sf_lock_now(s, id, &sf, prect, pdesc, flags)
}

fn sf_lock_now(s: &mut Session, id: u32, sf: &SurfObj, prect: u32, pdesc: u32, flags: u32) -> ApiResult {
    let px = s.display.surfaces[&sf.surface].clone();
    if px.locks > 0 {
        return Ok(ApiRet::Ret(DDERR_SURFACEBUSY));
    }
    let size = s.mem.read_u32(pdesc)?;
    if size != 124 && size != 108 {
        return Ok(ApiRet::Ret(DDERR_INVALIDPARAMS));
    }
    let r = if prect != 0 {
        let r = s.read_rect(prect)?;
        if r.left < 0 || r.top < 0 || r.right > px.w as i32 || r.bottom > px.h as i32 || r.is_empty() {
            return Ok(ApiRet::Ret(DDERR_INVALIDRECT));
        }
        Some(r)
    } else {
        None
    };
    let readonly = flags & 0x10 != 0;
    s.surface_cpu_access(sf.surface, !readonly);
    let off = match r {
        Some(r) if px.fmt.is_compressed() => {
            (r.top as u32 / 4) * px.pitch + (r.left as u32 / 4) * if px.fmt == Fmt::Dxt1 { 8 } else { 16 }
        }
        Some(r) => r.top as u32 * px.pitch + r.left as u32 * px.fmt.bytes_pp(),
        None => 0,
    };
    let mut fl = DDSD_CAPS | DDSD_HEIGHT | DDSD_WIDTH | DDSD_PIXELFORMAT | DDSD_LPSURFACE;
    fl |= if px.fmt.is_compressed() {
        DDSD_LINEARSIZE
    } else {
        DDSD_PITCH
    };
    let pitch = if px.fmt.is_compressed() { px.size } else { px.pitch };
    let b = build_ddsd(size, fl, px.w, px.h, pitch, px.fmt, sf.caps, sf.caps2, px.mem + off, 0);
    s.mem.write(pdesc, &b)?;
    if let Some(p) = s.display.surfaces.get_mut(&sf.surface) {
        p.locks += 1;
    }
    if let Some(x) = s.surf_mut(id) {
        x.lock_rect = r;
        x.uniqueness += (!readonly) as u32;
    }
    Ok(ApiRet::Ret(DD_OK))
}

fn sf_unlock(s: &mut Session) -> ApiResult {
    let (id, sf) = sf_self(s)?;
    let Some(px) = s.display.surfaces.get_mut(&sf.surface) else {
        return Ok(ApiRet::Ret(DDERR_NOTLOCKED));
    };
    if px.locks == 0 {
        return Ok(ApiRet::Ret(DDERR_NOTLOCKED));
    }
    px.locks -= 1;
    px.cpu_dirty = true;
    px.version += 1;
    if let Some(x) = s.surf_mut(id) {
        x.lock_rect = None;
    }
    if sf.primary || s.display.front == sf.surface {
        present(s);
    }
    Ok(ApiRet::Ret(DD_OK))
}

fn sf_get_attached_surface(s: &mut Session) -> ApiResult {
    let [_this, pcaps, pp] = s.args::<3>()?;
    let (id, sf) = sf_self(s)?;
    let want = s.mem.read_u32(pcaps)?;
    let mut cands: Vec<u32> = Vec::new();
    // Flip chain successor (back buffer of the front, or next back buffer).
    let root = if sf.chain.is_empty() { sf.parent } else { id };
    if let Some(r) = s.surf(root).cloned()
        && !r.chain.is_empty()
        && let Some(pos) = r.chain.iter().position(|&x| x == id)
    {
        let next = r.chain[(pos + 1) % r.chain.len()];
        if next != id {
            cands.push(next);
        }
    }
    cands.extend(sf.attached.iter().copied());
    for c in cands {
        if let Some(x) = s.surf(c)
            && x.caps & want == want
        {
            let def = surface_def(s);
            s.out_iface(pp, c, def)?;
            s.com_addref(c);
            return Ok(ApiRet::Ret(DD_OK));
        }
    }
    s.mem.write_u32(pp, 0)?;
    Ok(ApiRet::Ret(DDERR_NOTFOUND))
}

fn sf_add_attached_surface(s: &mut Session) -> ApiResult {
    let [_this, other] = s.args::<2>()?;
    let (id, _sf) = sf_self(s)?;
    let Some(o) = s.surface_obj_of_ptr(other) else {
        return Ok(ApiRet::Ret(DDERR_INVALIDOBJECT));
    };
    let ocaps = s.surf(o).unwrap().caps;
    if ocaps & DDSCAPS_ZBUFFER == 0 && ocaps & DDSCAPS_MIPMAP == 0 {
        return Ok(ApiRet::Ret(dderr(10))); // DDERR_CANNOTATTACHSURFACE
    }
    if s.surf(id).unwrap().attached.contains(&o) {
        return Ok(ApiRet::Ret(dderr(410))); // DDERR_SURFACEALREADYATTACHED
    }
    s.surf_mut(id).unwrap().attached.push(o);
    s.com_addref(o);
    Ok(ApiRet::Ret(DD_OK))
}

fn sf_delete_attached_surface(s: &mut Session) -> ApiResult {
    let [_this, _flags, other] = s.args::<3>()?;
    let (id, sf) = sf_self(s)?;
    let targets: Vec<u32> = if other == 0 {
        sf.attached.clone()
    } else {
        match s.surface_obj_of_ptr(other) {
            Some(o) => vec![o],
            None => return Ok(ApiRet::Ret(DDERR_INVALIDOBJECT)),
        }
    };
    for t in targets {
        if !s.surf(id).unwrap().attached.contains(&t) {
            return Ok(ApiRet::Ret(DDERR_SURFACENOTATTACHED));
        }
        s.surf_mut(id).unwrap().attached.retain(|&x| x != t);
        s.com_release(t);
    }
    Ok(ApiRet::Ret(DD_OK))
}

fn sf_enum_attached_surfaces(s: &mut Session) -> ApiResult {
    let [_this, ctx, cb] = s.args::<3>()?;
    let (id, sf) = sf_self(s)?;
    let mut list = Vec::new();
    if sf.chain.len() > 1 {
        list.push(sf.chain[1]);
    }
    list.extend(sf.attached.iter().copied());
    let mut items = Vec::new();
    for o in list {
        // Each enumerated surface is AddRef'd; the callback releases it.
        let def = surface_def(s);
        let p = s.iface(o, def)?;
        s.com_addref(o);
        let px = s.display.surfaces[&s.surf(o).unwrap().surface].clone();
        let so = s.surf(o).unwrap().clone();
        let d = build_ddsd(
            ddsd_size(s),
            DDSD_CAPS | DDSD_WIDTH | DDSD_HEIGHT | DDSD_PIXELFORMAT | DDSD_PITCH,
            px.w,
            px.h,
            px.pitch,
            px.fmt,
            so.caps,
            so.caps2,
            0,
            0,
        );
        items.push(vec![CallArg::Val(p), CallArg::Buf(d), CallArg::Val(ctx)]);
    }
    let _ = id;
    enum_callbacks(cb, items, 0, DD_OK)
}

fn sf_flip(s: &mut Session) -> ApiResult {
    let [_this, target, flags] = s.args::<3>()?;
    let (id, sf) = sf_self(s)?;
    if sf.chain.len() < 2 {
        return Ok(ApiRet::Ret(DDERR_NOTFLIPPABLE));
    }
    if sf.lost {
        return Ok(ApiRet::Ret(DDERR_SURFACELOST));
    }
    let now = s.clock.now_ns();
    let novsync = flags & 0x8 != 0;
    if let Some(until) = s.com.ddraw.flip_pending_until
        && now < until
        && !novsync
    {
        // Without DDFLIP_WAIT the caller polls, usually by running its whole main loop
        // again. After a few polls, wait for the vertical blank in this call instead (as
        // if the flip was slow to return): the program sees the same outcome without
        // spending the rest of the frame in emulated busy work.
        if flags & 0x1 == 0 && s.com.ddraw.flip_busy_polls < FLIP_POLLS_BEFORE_WAIT {
            s.com.ddraw.flip_busy_polls += 1;
            return Ok(ApiRet::Ret(DDERR_WASSTILLDRAWING));
        }
        // Wait for the previous flip to be retired, then flip (re-run this method).
        return Ok(ApiRet::Block(Block {
            wait: WaitKind::Sleep,
            deadline: Some(until),
            cont: Cont::Retry,
        }));
    }
    let mut chain = sf.chain.clone();
    let ids: Vec<u32> = chain.iter().map(|&o| s.surface_of_obj(o).unwrap()).collect();
    if target != 0 {
        // Flip to a specific back buffer: swap front with it.
        let Some(t) = s.surface_obj_of_ptr(target) else {
            return Ok(ApiRet::Ret(DDERR_INVALIDOBJECT));
        };
        let Some(pos) = chain.iter().position(|&x| x == t) else {
            return Ok(ApiRet::Ret(DDERR_NOTFLIPPABLE));
        };
        let (a, b) = (ids[0], ids[pos]);
        s.surf_mut(chain[0]).unwrap().surface = b;
        s.surf_mut(chain[pos]).unwrap().surface = a;
    } else {
        // Rotate: front ← back1 ← back2 ← … ← front.
        let n = ids.len();
        for (i, o) in chain.iter_mut().enumerate() {
            let new = ids[(i + 1) % n];
            if let Some(x) = s.surf_mut(*o) {
                x.surface = new;
            }
        }
    }
    let front = s.surface_of_obj(sf.chain[0]).unwrap();
    if sf.primary {
        s.display.front = front;
    }
    s.com.ddraw.flip_pending_until = if novsync { None } else { Some(s.next_vblank()) };
    s.com.ddraw.flip_busy_polls = 0;
    let _ = id;
    present(s);
    Ok(ApiRet::Ret(DD_OK))
}

/// Mark a new frame as presented (scan-out content changed).
pub fn present(s: &mut Session) {
    s.frames_presented += 1;
    s.pending_present_stop = true;
    crate::com::d3d::flush(s);
}

fn sf_get_flip_status(s: &mut Session) -> ApiResult {
    let [_this, _flags] = s.args::<2>()?;
    let pending = s.com.ddraw.flip_pending_until.is_some_and(|u| s.clock.now_ns() < u);
    Ok(ApiRet::Ret(if pending { DDERR_WASSTILLDRAWING } else { DD_OK }))
}

fn sf_get_blt_status(_s: &mut Session) -> ApiResult {
    // Blits complete synchronously.
    Ok(ApiRet::Ret(DD_OK))
}

fn read_ckey(s: &Session, p: u32) -> Result<ColorKey, ApiError> {
    Ok(ColorKey {
        low: s.mem.read_u32(p)?,
        high: s.mem.read_u32(p + 4)?,
    })
}

fn sf_set_color_key(s: &mut Session) -> ApiResult {
    let [_this, flags, pk] = s.args::<3>()?;
    let (_id, sf) = sf_self(s)?;
    let key = if pk == 0 { None } else { Some(read_ckey(s, pk)?) };
    let Some(px) = s.display.surfaces.get_mut(&sf.surface) else {
        return Ok(ApiRet::Ret(DDERR_INVALIDOBJECT));
    };
    match flags & 0xf {
        0x2 => px.ckey_dst = key,
        0x8 => px.ckey_src = key,
        0x1 | 0x4 => return Ok(ApiRet::Ret(DDERR_UNSUPPORTED)), // overlay keys
        _ => return Ok(ApiRet::Ret(DDERR_INVALIDPARAMS)),
    }
    px.version += 1;
    Ok(ApiRet::Ret(DD_OK))
}

fn sf_get_color_key(s: &mut Session) -> ApiResult {
    let [_this, flags, pk] = s.args::<3>()?;
    let (_id, sf) = sf_self(s)?;
    let px = &s.display.surfaces[&sf.surface];
    let k = match flags & 0xf {
        0x2 => px.ckey_dst,
        0x8 => px.ckey_src,
        _ => return Ok(ApiRet::Ret(DDERR_INVALIDPARAMS)),
    };
    match k {
        Some(k) => {
            s.mem.write_u32(pk, k.low)?;
            s.mem.write_u32(pk + 4, k.high)?;
            Ok(ApiRet::Ret(DD_OK))
        }
        None => Ok(ApiRet::Ret(DDERR_NOCOLORKEY)),
    }
}

fn sf_set_palette(s: &mut Session) -> ApiResult {
    let [_this, pal] = s.args::<2>()?;
    let (id, sf) = sf_self(s)?;
    let new = if pal == 0 { 0 } else { s.obj_of(pal).unwrap_or(0) };
    if sf.palette != 0 {
        s.com_release(sf.palette);
    }
    let pid = match s.com_obj(new).map(|o| &o.kind) {
        Some(ComKind::Palette(p)) => p.palette,
        _ => 0,
    };
    if new != 0 {
        s.com_addref(new);
    }
    if let Some(x) = s.surf_mut(id) {
        x.palette = new;
    }
    // Palettes attached to a flip chain apply to all its buffers.
    let mut targets = vec![sf.surface];
    for c in &sf.chain {
        if let Some(i) = s.surface_of_obj(*c) {
            targets.push(i);
        }
    }
    for t in targets {
        if let Some(p) = s.display.surfaces.get_mut(&t) {
            p.palette = pid;
            p.version += 1;
        }
    }
    if sf.primary {
        present(s);
    }
    Ok(ApiRet::Ret(DD_OK))
}

fn sf_get_palette(s: &mut Session) -> ApiResult {
    let [_this, pp] = s.args::<2>()?;
    let (_id, sf) = sf_self(s)?;
    if sf.palette == 0 {
        return Ok(ApiRet::Ret(DDERR_NOPALETTEATTACHED));
    }
    s.out_iface(pp, sf.palette, &IDIRECTDRAWPALETTE)?;
    s.com_addref(sf.palette);
    Ok(ApiRet::Ret(DD_OK))
}

fn sf_set_clipper(s: &mut Session) -> ApiResult {
    let [_this, c] = s.args::<2>()?;
    let (id, sf) = sf_self(s)?;
    if sf.clipper != 0 {
        s.com_release(sf.clipper);
    }
    let new = if c == 0 { 0 } else { s.obj_of(c).unwrap_or(0) };
    if new != 0 {
        s.com_addref(new);
    }
    s.surf_mut(id).unwrap().clipper = new;
    Ok(ApiRet::Ret(DD_OK))
}

fn sf_get_clipper(s: &mut Session) -> ApiResult {
    let [_this, pp] = s.args::<2>()?;
    let (_id, sf) = sf_self(s)?;
    if sf.clipper == 0 {
        return Ok(ApiRet::Ret(DDERR_NOCLIPPERATTACHED));
    }
    s.out_iface(pp, sf.clipper, &IDIRECTDRAWCLIPPER)?;
    s.com_addref(sf.clipper);
    Ok(ApiRet::Ret(DD_OK))
}

fn sf_get_dc(s: &mut Session) -> ApiResult {
    let [_this, phdc] = s.args::<2>()?;
    let (id, sf) = sf_self(s)?;
    if sf.dc != 0 {
        return Ok(ApiRet::Ret(DDERR_DCALREADYCREATED));
    }
    let px = &s.display.surfaces[&sf.surface];
    if px.fmt.is_depth() || px.fmt.is_compressed() {
        return Ok(ApiRet::Ret(DDERR_CANTCREATEDC));
    }
    s.surface_cpu_access(sf.surface, true);
    let hdc = crate::dll::gdi32::create_dc(s, crate::dll::gdi32::DcTarget::Surface(sf.surface));
    if let Some(p) = s.display.surfaces.get_mut(&sf.surface) {
        p.dcs += 1;
        p.locks += 1;
    }
    s.surf_mut(id).unwrap().dc = hdc;
    s.mem.write_u32(phdc, hdc)?;
    Ok(ApiRet::Ret(DD_OK))
}

fn sf_release_dc(s: &mut Session) -> ApiResult {
    let [_this, hdc] = s.args::<2>()?;
    let (id, sf) = sf_self(s)?;
    if sf.dc == 0 || sf.dc != hdc {
        return Ok(ApiRet::Ret(DDERR_NODC));
    }
    crate::dll::gdi32::release_dc_handle(s, hdc);
    if let Some(p) = s.display.surfaces.get_mut(&sf.surface) {
        p.locks = p.locks.saturating_sub(1);
        p.cpu_dirty = true;
        p.version += 1;
    }
    s.surf_mut(id).unwrap().dc = 0;
    if sf.primary {
        present(s);
    }
    Ok(ApiRet::Ret(DD_OK))
}

fn sf_get_dd_interface(s: &mut Session) -> ApiResult {
    let [_this, pp] = s.args::<2>()?;
    let (_id, sf) = sf_self(s)?;
    let def = ddraw_def(s);
    s.out_iface(pp, sf.ddraw, def)?;
    s.com_addref(sf.ddraw);
    Ok(ApiRet::Ret(DD_OK))
}

fn sf_page_lock(_s: &mut Session) -> ApiResult {
    Ok(ApiRet::Ret(DD_OK))
}

fn sf_set_priority(s: &mut Session) -> ApiResult {
    let [_this, p] = s.args::<2>()?;
    let (id, _) = sf_self(s)?;
    s.surf_mut(id).unwrap().priority = p;
    Ok(ApiRet::Ret(DD_OK))
}
fn sf_get_priority(s: &mut Session) -> ApiResult {
    let [_this, p] = s.args::<2>()?;
    let (_id, sf) = sf_self(s)?;
    s.mem.write_u32(p, sf.priority)?;
    Ok(ApiRet::Ret(DD_OK))
}
fn sf_set_lod(s: &mut Session) -> ApiResult {
    let [_this, l] = s.args::<2>()?;
    let (id, _) = sf_self(s)?;
    s.surf_mut(id).unwrap().lod = l;
    Ok(ApiRet::Ret(DD_OK))
}
fn sf_get_lod(s: &mut Session) -> ApiResult {
    let [_this, p] = s.args::<2>()?;
    let (_id, sf) = sf_self(s)?;
    s.mem.write_u32(p, sf.lod)?;
    Ok(ApiRet::Ret(DD_OK))
}
fn sf_get_uniqueness_value(s: &mut Session) -> ApiResult {
    let [_this, p] = s.args::<2>()?;
    let (_id, sf) = sf_self(s)?;
    s.mem.write_u32(p, sf.uniqueness)?;
    Ok(ApiRet::Ret(DD_OK))
}
fn sf_change_uniqueness_value(s: &mut Session) -> ApiResult {
    let (id, _) = sf_self(s)?;
    s.surf_mut(id).unwrap().uniqueness += 1;
    Ok(ApiRet::Ret(DD_OK))
}
fn sf_set_private_data(s: &mut Session) -> ApiResult {
    let [_this, g, data, size, flags] = s.args::<5>()?;
    let (id, _) = sf_self(s)?;
    let guid = s.read_guid(g)?;
    if flags & 1 != 0 {
        // DDSPD_IUNKNOWNPOINTER is not supported.
        return Ok(ApiRet::Ret(DDERR_UNSUPPORTED));
    }
    let bytes = s.mem.read_vec(data, size)?;
    let sf = s.surf_mut(id).unwrap();
    sf.private.retain(|(k, _)| *k != guid);
    sf.private.push((guid, bytes));
    Ok(ApiRet::Ret(DD_OK))
}
fn sf_get_private_data(s: &mut Session) -> ApiResult {
    let [_this, g, data, psize] = s.args::<4>()?;
    let (_id, sf) = sf_self(s)?;
    let guid = s.read_guid(g)?;
    let Some((_, v)) = sf.private.iter().find(|(k, _)| *k == guid) else {
        return Ok(ApiRet::Ret(DDERR_NOTFOUND));
    };
    let cap = s.mem.read_u32(psize)?;
    s.mem.write_u32(psize, v.len() as u32)?;
    if (v.len() as u32) > cap {
        return Ok(ApiRet::Ret(dderr(690))); // DDERR_MOREDATA
    }
    s.mem.write(data, v)?;
    Ok(ApiRet::Ret(DD_OK))
}
fn sf_free_private_data(s: &mut Session) -> ApiResult {
    let [_this, g] = s.args::<2>()?;
    let (id, _) = sf_self(s)?;
    let guid = s.read_guid(g)?;
    let sf = s.surf_mut(id).unwrap();
    let n = sf.private.len();
    sf.private.retain(|(k, _)| *k != guid);
    Ok(ApiRet::Ret(if n == sf.private.len() { DDERR_NOTFOUND } else { DD_OK }))
}

// ---------------------------------------------------------------- blits

/// Raw value of a pixel at (x, y).
fn get_px(data: &[u8], pitch: u32, bpp: u32, x: u32, y: u32) -> u32 {
    let o = (y * pitch + x * bpp) as usize;
    let mut b = [0u8; 4];
    b[..bpp as usize].copy_from_slice(&data[o..o + bpp as usize]);
    u32::from_le_bytes(b)
}

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct BltOpts {
    pub src_key: Option<ColorKey>,
    pub dst_key: Option<ColorKey>,
    pub mirror_lr: bool,
    pub mirror_ud: bool,
}

/// Copy a rectangle between surfaces of the same format (no keys, no stretching).
pub fn copy_rect(s: &mut Session, dst: u32, dr: Rect, src: u32, sr: Rect) -> Result<(), ApiError> {
    blit(s, dst, dr, src, sr, BltOpts::default())
}

/// Mip chain of a surface object: the object itself followed by its sublevels.
pub fn mip_chain(s: &Session, obj: u32) -> Vec<u32> {
    let mut out = vec![obj];
    let mut cur = obj;
    while let Some(next) = s.surf(cur).and_then(|sf| {
        sf.attached
            .iter()
            .copied()
            .find(|a| s.surf(*a).is_some_and(|x| x.caps2 & DDSCAPS2_MIPMAPSUBLEVEL != 0))
    }) {
        out.push(next);
        cur = next;
    }
    out
}

/// CPU blit between surfaces with optional stretching (nearest) and colour keys.
pub(crate) fn blit(s: &mut Session, dst: u32, dr: Rect, src: u32, sr: Rect, o: BltOpts) -> Result<(), ApiError> {
    s.surface_cpu_access(src, false);
    s.surface_cpu_access(dst, true);
    let sp = s.display.surfaces[&src].clone();
    let dp = s.display.surfaces[&dst].clone();
    if sp.fmt.is_compressed() || dp.fmt.is_compressed() {
        if sp.fmt == dp.fmt
            && sr.w() == dr.w()
            && sr.h() == dr.h()
            && sr.left == 0
            && sr.top == 0
            && dr.left == 0
            && dr.top == 0
        {
            let n = sp.size.min(dp.size);
            let data = s.mem.read_vec(sp.mem, n)?;
            s.mem.write(dp.mem, &data)?;
            return Ok(());
        }
        return Err(ApiError::Unimplemented("partial blit of compressed surfaces".into()));
    }
    let src_data = s.mem.read_vec(sp.mem, sp.pitch * sp.h)?;
    let convert = sp.fmt != dp.fmt;
    let spal = s.palette_of(src);
    let (sb, db) = (sp.fmt.bytes_pp(), dp.fmt.bytes_pp());
    let dw = dr.w();
    let dh = dr.h();
    if dw <= 0 || dh <= 0 {
        return Ok(());
    }
    let mut row = vec![0u8; (dw as u32 * db) as usize];
    for y in 0..dh {
        let dy = dr.top + y;
        if dy < 0 || dy >= dp.h as i32 {
            continue;
        }
        let mut sy = sr.top + (y as i64 * sr.h() as i64 / dh as i64) as i32;
        if o.mirror_ud {
            sy = sr.bottom - 1 - (sy - sr.top);
        }
        let row_addr = dp.row_addr(dy as u32) + dr.left.max(0) as u32 * db;
        let x0 = (-dr.left).max(0);
        let x1 = dw.min(dp.w as i32 - dr.left);
        if x1 <= x0 {
            continue;
        }
        let need_dst = o.src_key.is_some() || o.dst_key.is_some();
        let span = ((x1 - x0) as u32 * db) as usize;
        if need_dst {
            s.mem.read(row_addr, &mut row[..span])?;
        }
        for x in x0..x1 {
            let mut sx = sr.left + (x as i64 * sr.w() as i64 / dw as i64) as i32;
            if o.mirror_lr {
                sx = sr.right - 1 - (sx - sr.left);
            }
            let srow = if sp.bottom_up { sp.h - 1 - sy as u32 } else { sy as u32 };
            let v = get_px(&src_data, sp.pitch, sb, sx as u32, srow);
            if let Some(k) = o.src_key
                && v >= k.low
                && v <= k.high
            {
                continue;
            }
            let di = ((x - x0) as u32 * db) as usize;
            if let Some(k) = o.dst_key {
                let mut cur = [0u8; 4];
                cur[..db as usize].copy_from_slice(&row[di..di + db as usize]);
                let c = u32::from_le_bytes(cur);
                if c < k.low || c > k.high {
                    continue;
                }
            }
            if convert {
                let mut px = [0u8; 4];
                px[..sb as usize].copy_from_slice(&v.to_le_bytes()[..sb as usize]);
                let argb = sp.fmt.decode(&px[..sb as usize], spal.as_ref());
                let mut out = [0u8; 4];
                dp.fmt.encode(argb, &mut out);
                row[di..di + db as usize].copy_from_slice(&out[..db as usize]);
            } else {
                row[di..di + db as usize].copy_from_slice(&v.to_le_bytes()[..db as usize]);
            }
        }
        s.mem.write(row_addr, &row[..span])?;
    }
    Ok(())
}

fn full_rect(s: &Session, id: u32) -> Rect {
    let p = &s.display.surfaces[&id];
    Rect::new(0, 0, p.w as i32, p.h as i32)
}

fn sf_blt(s: &mut Session) -> ApiResult {
    let [_this, pdr, src_p, psr, flags, pfx] = s.args::<6>()?;
    let (did, dsf) = sf_self(s)?;
    if dsf.lost {
        return Ok(ApiRet::Ret(DDERR_SURFACELOST));
    }
    let dst = dsf.surface;
    let full = full_rect(s, dst);
    let mut dr = if pdr != 0 { s.read_rect(pdr)? } else { full };
    if dr.is_empty() {
        return Ok(ApiRet::Ret(DDERR_INVALIDRECT));
    }
    // A clipper on the destination clips to its window (or clip list).
    if dsf.clipper != 0
        && let Some(ComKind::Clipper(c)) = s.com_obj(dsf.clipper).map(|o| o.kind.clone())
    {
        let clip = if c.hwnd != 0 {
            s.window(c.hwnd).map(|w| w.client).unwrap_or(full)
        } else {
            c.clip.first().copied().unwrap_or(full)
        };
        let nr = dr.intersect(&clip);
        if nr.is_empty() {
            return Ok(ApiRet::Ret(DD_OK));
        }
        dr = nr;
    } else if dr.left < 0 || dr.top < 0 || dr.right > full.right || dr.bottom > full.bottom {
        return Ok(ApiRet::Ret(DDERR_INVALIDRECT));
    }
    if flags & 0x400 != 0 {
        // DDBLT_COLORFILL: dwFillColor at DDBLTFX+80
        let color = s.mem.read_u32(pfx + 80)?;
        if crate::com::d3d::color_fill(s, dst, &dr, color) {
            after_write(s, did, &dsf);
            return Ok(ApiRet::Ret(DD_OK));
        }
        s.surface_cpu_access(dst, true);
        let t = PixelTarget::of(&s.display.surfaces[&dst]);
        t.fill_raw(&mut s.mem, &dr, color & crate::surface::mask_bits(t.fmt.bits()))?;
        after_write(s, did, &dsf);
        return Ok(ApiRet::Ret(DD_OK));
    }
    if flags & 0x200_0000 != 0 {
        // DDBLT_DEPTHFILL: dwFillDepth at +80
        let depth = s.mem.read_u32(pfx + 80)?;
        crate::com::d3d::depth_fill(s, dst, &dr, depth);
        return Ok(ApiRet::Ret(DD_OK));
    }
    if src_p == 0 {
        return Ok(ApiRet::Ret(DDERR_INVALIDPARAMS));
    }
    let Some(sobj) = s.surface_obj_of_ptr(src_p) else {
        return Ok(ApiRet::Ret(DDERR_INVALIDOBJECT));
    };
    let src = s.surface_of_obj(sobj).unwrap();
    let sr = if psr != 0 { s.read_rect(psr)? } else { full_rect(s, src) };
    let sfull = full_rect(s, src);
    if sr.left < 0 || sr.top < 0 || sr.right > sfull.right || sr.bottom > sfull.bottom || sr.is_empty() {
        return Ok(ApiRet::Ret(DDERR_INVALIDRECT));
    }
    let mut o = BltOpts::default();
    if flags & 0x8000 != 0 {
        o.src_key = s.display.surfaces[&src].ckey_src;
        if o.src_key.is_none() {
            return Ok(ApiRet::Ret(DDERR_NOCOLORKEY));
        }
    }
    if flags & 0x10000 != 0 {
        o.src_key = Some(read_ckey(s, pfx + 92)?); // ddckSrcColorkey
    }
    if flags & 0x2000 != 0 {
        o.dst_key = s.display.surfaces[&dst].ckey_dst;
    }
    if flags & 0x4000 != 0 {
        o.dst_key = Some(read_ckey(s, pfx + 84)?); // ddckDestColorkey
    }
    if flags & 0x800 != 0 {
        let fx = s.mem.read_u32(pfx + 4)?;
        o.mirror_lr = fx & 0x2 != 0;
        o.mirror_ud = fx & 0x4 != 0;
    }
    if flags & 0x20000 != 0 {
        let rop = s.mem.read_u32(pfx + 8)?;
        if rop != 0x00cc_0020 {
            s.missing("Blt", format!("ROP {rop:#x}"));
            return Ok(ApiRet::Ret(dderr(280))); // DDERR_NORASTEROPHW
        }
    }
    // Source rect is clipped proportionally if the destination was clipped.
    if crate::com::d3d::gpu_blit(s, dst, dr, src, sr, o.src_key) {
        after_write(s, did, &dsf);
        return Ok(ApiRet::Ret(DD_OK));
    }
    blit(s, dst, dr, src, sr, o)?;
    after_write(s, did, &dsf);
    Ok(ApiRet::Ret(DD_OK))
}

fn after_write(s: &mut Session, _id: u32, sf: &SurfObj) {
    if sf.primary || s.display.front == sf.surface {
        present(s);
    }
}

fn sf_blt_fast(s: &mut Session) -> ApiResult {
    let [_this, x, y, src_p, psr, trans] = s.args::<6>()?;
    let (did, dsf) = sf_self(s)?;
    if dsf.lost {
        return Ok(ApiRet::Ret(DDERR_SURFACELOST));
    }
    if dsf.clipper != 0 {
        return Ok(ApiRet::Ret(dderr(574))); // DDERR_BLTFASTCANTCLIP
    }
    let dst = dsf.surface;
    let Some(sobj) = s.surface_obj_of_ptr(src_p) else {
        return Ok(ApiRet::Ret(DDERR_INVALIDOBJECT));
    };
    let src = s.surface_of_obj(sobj).unwrap();
    let sr = if psr != 0 { s.read_rect(psr)? } else { full_rect(s, src) };
    let dr = Rect::new(x as i32, y as i32, x as i32 + sr.w(), y as i32 + sr.h());
    let full = full_rect(s, dst);
    let sfull = full_rect(s, src);
    if dr.left < 0
        || dr.top < 0
        || dr.right > full.right
        || dr.bottom > full.bottom
        || sr.left < 0
        || sr.top < 0
        || sr.right > sfull.right
        || sr.bottom > sfull.bottom
    {
        return Ok(ApiRet::Ret(DDERR_INVALIDRECT));
    }
    let mut o = BltOpts::default();
    if trans & 1 != 0 {
        o.src_key = s.display.surfaces[&src].ckey_src;
    }
    if trans & 2 != 0 {
        o.dst_key = s.display.surfaces[&dst].ckey_dst;
    }
    if crate::com::d3d::gpu_blit(s, dst, dr, src, sr, o.src_key) {
        after_write(s, did, &dsf);
        return Ok(ApiRet::Ret(DD_OK));
    }
    blit(s, dst, dr, src, sr, o)?;
    after_write(s, did, &dsf);
    Ok(ApiRet::Ret(DD_OK))
}

// ---------------------------------------------------------------- palette / clipper / gamma

fn pal_self(s: &Session) -> Result<u32, ApiError> {
    let id = s.this_obj()?;
    match &s.com_obj(id).unwrap().kind {
        ComKind::Palette(p) => Ok(p.palette),
        _ => Err(ApiError::Fatal("not a palette".into())),
    }
}

fn pal_get_entries(s: &mut Session) -> ApiResult {
    let [_this, _flags, start, n, out] = s.args::<5>()?;
    let pid = pal_self(s)?;
    let e = s.display.palettes[&pid].entries.clone();
    for i in 0..n {
        let c = *e.get((start + i) as usize).unwrap_or(&0);
        let pe = (c >> 16) & 0xff | (c & 0xff00) | (c & 0xff) << 16;
        s.mem.write_u32(out + 4 * i, pe)?;
    }
    Ok(ApiRet::Ret(DD_OK))
}

fn pal_set_entries(s: &mut Session) -> ApiResult {
    let [_this, _flags, start, n, inp] = s.args::<5>()?;
    let pid = pal_self(s)?;
    let mut vals = Vec::new();
    for i in 0..n {
        let pe = s.mem.read_u32(inp + 4 * i)?;
        vals.push((pe & 0xff) << 16 | (pe & 0xff00) | (pe >> 16) & 0xff);
    }
    let p = s.display.palettes.get_mut(&pid).unwrap();
    for (i, v) in vals.into_iter().enumerate() {
        if let Some(slot) = p.entries.get_mut(start as usize + i) {
            *slot = v;
        }
    }
    let users: Vec<u32> = s
        .display
        .surfaces
        .values()
        .filter(|x| x.palette == pid)
        .map(|x| x.id)
        .collect();
    for u in &users {
        if let Some(x) = s.display.surfaces.get_mut(u) {
            x.version += 1;
        }
    }
    if users.contains(&s.display.front) {
        present(s);
    }
    Ok(ApiRet::Ret(DD_OK))
}

fn pal_get_caps(s: &mut Session) -> ApiResult {
    let [_this, p] = s.args::<2>()?;
    let pid = pal_self(s)?;
    let f = s.display.palettes[&pid].flags;
    s.mem.write_u32(p, f)?;
    Ok(ApiRet::Ret(DD_OK))
}

fn clipper_self(s: &Session) -> Result<u32, ApiError> {
    s.this_obj()
}

fn clip_set_hwnd(s: &mut Session) -> ApiResult {
    let [_this, _flags, hwnd] = s.args::<3>()?;
    let id = clipper_self(s)?;
    if let Some(ComKind::Clipper(c)) = s.com_obj_mut(id).map(|o| &mut o.kind) {
        c.hwnd = hwnd;
    }
    Ok(ApiRet::Ret(DD_OK))
}
fn clip_get_hwnd(s: &mut Session) -> ApiResult {
    let [_this, p] = s.args::<2>()?;
    let id = clipper_self(s)?;
    let h = match &s.com_obj(id).unwrap().kind {
        ComKind::Clipper(c) => c.hwnd,
        _ => 0,
    };
    s.mem.write_u32(p, h)?;
    Ok(ApiRet::Ret(DD_OK))
}
fn clip_set_clip_list(s: &mut Session) -> ApiResult {
    let [_this, rgn, _flags] = s.args::<3>()?;
    let id = clipper_self(s)?;
    let mut rects = Vec::new();
    if rgn != 0 {
        // RGNDATA: header (32 bytes: dwSize, iType, nCount, nRgnSize, rcBound) + RECTs.
        let n = s.mem.read_u32(rgn + 8)?;
        for i in 0..n.min(256) {
            rects.push(s.read_rect(rgn + 32 + 16 * i)?);
        }
    }
    if let Some(ComKind::Clipper(c)) = s.com_obj_mut(id).map(|o| &mut o.kind) {
        c.clip = rects;
    }
    Ok(ApiRet::Ret(DD_OK))
}
fn clip_is_clip_list_changed(s: &mut Session) -> ApiResult {
    let [_this, p] = s.args::<2>()?;
    s.mem.write_u32(p, 0)?;
    Ok(ApiRet::Ret(DD_OK))
}

fn gamma_get_ramp(s: &mut Session) -> ApiResult {
    let [_this, _flags, p] = s.args::<3>()?;
    let mut b = vec![0u8; 1536];
    let g = &s.com.ddraw.gamma;
    for i in 0..768usize {
        let v = if g.len() == 768 {
            g[i]
        } else {
            ((i % 256) as u16) << 8 | (i % 256) as u16
        };
        b[i * 2..i * 2 + 2].copy_from_slice(&v.to_le_bytes());
    }
    s.mem.write(p, &b)?;
    Ok(ApiRet::Ret(DD_OK))
}
fn gamma_set_ramp(s: &mut Session) -> ApiResult {
    let [_this, _flags, p] = s.args::<3>()?;
    let b = s.mem.read_vec(p, 1536)?;
    s.com.ddraw.gamma = b
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    present(s);
    Ok(ApiRet::Ret(DD_OK))
}

fn dd_ok(_s: &mut Session) -> ApiResult {
    Ok(ApiRet::Ret(DD_OK))
}

// ---------------------------------------------------------------- scheduling

pub fn next_event(s: &Session) -> Option<u64> {
    s.com.ddraw.flip_pending_until.filter(|&t| t > s.clock.now_ns())
}

pub fn service(s: &mut Session) {
    if let Some(t) = s.com.ddraw.flip_pending_until
        && s.clock.now_ns() >= t
    {
        s.com.ddraw.flip_pending_until = None;
    }
}

// ---------------------------------------------------------------- interface tables

/// IDirectDraw7 methods in vtable order. Earlier versions are prefixes of this list:
/// IDirectDraw2 has the first 24 methods, IDirectDraw4 the first 28. IDirectDraw (1)
/// differs from IDirectDraw2 only in SetDisplayMode's arguments and the missing
/// GetAvailableVidMem.
static DD_METHODS: [ExportDef; 30] = [
    iunknown!()[0],
    iunknown!()[1],
    iunknown!()[2],
    export!("Compact", 1, dd_ok),
    export!("CreateClipper", 4, dd_create_clipper),
    export!("CreatePalette", 5, dd_create_palette),
    export!("CreateSurface", 4, dd_create_surface),
    unimpl_method!("DuplicateSurface", 3),
    export!("EnumDisplayModes", 5, dd_enum_display_modes),
    export!("EnumSurfaces", 5, dd_enum_surfaces),
    export!("FlipToGDISurface", 1, dd_flip_to_gdi_surface),
    export!("GetCaps", 3, dd_get_caps),
    export!("GetDisplayMode", 2, dd_get_display_mode),
    export!("GetFourCCCodes", 3, dd_get_fourcc_codes),
    export!("GetGDISurface", 2, dd_get_gdi_surface),
    export!("GetMonitorFrequency", 2, dd_get_monitor_frequency),
    export!("GetScanLine", 2, dd_get_scan_line),
    export!("GetVerticalBlankStatus", 2, dd_get_vertical_blank_status),
    unimpl_method!("Initialize", 2),
    export!("RestoreDisplayMode", 1, dd_restore_display_mode),
    export!("SetCooperativeLevel", 3, dd_set_cooperative_level),
    export!("SetDisplayMode", 6, dd_set_display_mode),
    export!("WaitForVerticalBlank", 3, dd_wait_for_vertical_blank),
    export!("GetAvailableVidMem", 4, dd_get_available_vid_mem),
    unimpl_method!("GetSurfaceFromDC", 3),
    export!("RestoreAllSurfaces", 1, dd_restore_all_surfaces),
    export!("TestCooperativeLevel", 1, dd_test_cooperative_level),
    export!("GetDeviceIdentifier", 3, dd_get_device_identifier),
    unimpl_method!("StartModeTest", 4),
    unimpl_method!("EvaluateMode", 3),
];

static DD1_METHODS: [ExportDef; 23] = {
    let mut m = [DD_METHODS[0]; 23];
    let mut i = 0;
    while i < 23 {
        m[i] = DD_METHODS[i];
        i += 1;
    }
    m[21] = export!("SetDisplayMode", 4, dd1_set_display_mode);
    m
};

pub static IDIRECTDRAW7: IfaceDef = IfaceDef {
    name: "IDirectDraw7",
    iid: guid("15e65ec0-3b9c-11d2-b92f-00609797ea5b"),
    module: "DDRAW.DLL",
    methods: &DD_METHODS,
};

pub static IDIRECTDRAW4: IfaceDef = IfaceDef {
    name: "IDirectDraw4",
    iid: guid("9c59509a-39bd-11d1-8c4a-00c04fd930c5"),
    module: "DDRAW.DLL",
    methods: DD_METHODS.split_at(28).0,
};

pub static IDIRECTDRAW2: IfaceDef = IfaceDef {
    name: "IDirectDraw2",
    iid: guid("b3a6f3e0-2b43-11cf-a2de-00aa00b93356"),
    module: "DDRAW.DLL",
    methods: DD_METHODS.split_at(24).0,
};

fn dd1_set_display_mode(s: &mut Session) -> ApiResult {
    let [_this, w, h, bpp] = s.args::<4>()?;
    set_display_mode(s, w, h, bpp, 0)
}

pub static IDIRECTDRAW: IfaceDef = IfaceDef {
    name: "IDirectDraw",
    iid: guid("6c14db80-a733-11ce-a521-0020af0be560"),
    module: "DDRAW.DLL",
    methods: &DD1_METHODS,
};

/// IDirectDrawSurface7 methods in vtable order. IDirectDrawSurface has the first 36,
/// IDirectDrawSurface2 39, IDirectDrawSurface3 40 and IDirectDrawSurface4 45.
static SURFACE_METHODS: [ExportDef; 49] = [
    iunknown!()[0],
    iunknown!()[1],
    iunknown!()[2],
    export!("AddAttachedSurface", 2, sf_add_attached_surface),
    unimpl_method!("AddOverlayDirtyRect", 2),
    export!("Blt", 6, sf_blt),
    unimpl_method!("BltBatch", 4),
    export!("BltFast", 6, sf_blt_fast),
    export!("DeleteAttachedSurface", 3, sf_delete_attached_surface),
    export!("EnumAttachedSurfaces", 3, sf_enum_attached_surfaces),
    unimpl_method!("EnumOverlayZOrders", 4),
    export!("Flip", 3, sf_flip),
    export!("GetAttachedSurface", 3, sf_get_attached_surface),
    export!("GetBltStatus", 2, sf_get_blt_status),
    export!("GetCaps", 2, sf_get_caps),
    export!("GetClipper", 2, sf_get_clipper),
    export!("GetColorKey", 3, sf_get_color_key),
    export!("GetDC", 2, sf_get_dc),
    export!("GetFlipStatus", 2, sf_get_flip_status),
    unimpl_method!("GetOverlayPosition", 3),
    export!("GetPalette", 2, sf_get_palette),
    export!("GetPixelFormat", 2, sf_get_pixel_format),
    export!("GetSurfaceDesc", 2, sf_get_surface_desc),
    unimpl_method!("Initialize", 3),
    export!("IsLost", 1, sf_is_lost),
    export!("Lock", 5, sf_lock),
    export!("ReleaseDC", 2, sf_release_dc),
    export!("Restore", 1, sf_restore),
    export!("SetClipper", 2, sf_set_clipper),
    export!("SetColorKey", 3, sf_set_color_key),
    unimpl_method!("SetOverlayPosition", 3),
    export!("SetPalette", 2, sf_set_palette),
    export!("Unlock", 2, sf_unlock),
    unimpl_method!("UpdateOverlay", 6),
    unimpl_method!("UpdateOverlayDisplay", 2),
    unimpl_method!("UpdateOverlayZOrder", 3),
    export!("GetDDInterface", 2, sf_get_dd_interface),
    export!("PageLock", 2, sf_page_lock),
    export!("PageUnlock", 2, sf_page_lock),
    unimpl_method!("SetSurfaceDesc", 3),
    export!("SetPrivateData", 5, sf_set_private_data),
    export!("GetPrivateData", 4, sf_get_private_data),
    export!("FreePrivateData", 2, sf_free_private_data),
    export!("GetUniquenessValue", 2, sf_get_uniqueness_value),
    export!("ChangeUniquenessValue", 1, sf_change_uniqueness_value),
    export!("SetPriority", 2, sf_set_priority),
    export!("GetPriority", 2, sf_get_priority),
    export!("SetLOD", 2, sf_set_lod),
    export!("GetLOD", 2, sf_get_lod),
];

pub static IDIRECTDRAWSURFACE7: IfaceDef = IfaceDef {
    name: "IDirectDrawSurface7",
    iid: guid("06675a80-3b9b-11d2-b92f-00609797ea5b"),
    module: "DDRAW.DLL",
    methods: &SURFACE_METHODS,
};

pub static IDIRECTDRAWSURFACE4: IfaceDef = IfaceDef {
    name: "IDirectDrawSurface4",
    iid: guid("0b2b8630-ad35-11d0-8ea6-00609797ea5b"),
    module: "DDRAW.DLL",
    methods: SURFACE_METHODS.split_at(45).0,
};

pub static IDIRECTDRAWSURFACE3: IfaceDef = IfaceDef {
    name: "IDirectDrawSurface3",
    iid: guid("da044e00-69b2-11d0-a1d5-00aa00b8dfbb"),
    module: "DDRAW.DLL",
    methods: SURFACE_METHODS.split_at(40).0,
};

pub static IDIRECTDRAWSURFACE2: IfaceDef = IfaceDef {
    name: "IDirectDrawSurface2",
    iid: guid("57805885-6eec-11cf-9441-a82303c10e27"),
    module: "DDRAW.DLL",
    methods: SURFACE_METHODS.split_at(39).0,
};

pub static IDIRECTDRAWSURFACE: IfaceDef = IfaceDef {
    name: "IDirectDrawSurface",
    iid: guid("6c14db81-a733-11ce-a521-0020af0be560"),
    module: "DDRAW.DLL",
    methods: SURFACE_METHODS.split_at(36).0,
};

/// Interface generation of the pointer a DirectDraw method was called through.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum DdVersion {
    /// IDirectDraw, IDirectDraw2, IDirectDrawSurface 1–3: DDSURFACEDESC, DDSCAPS.
    Legacy,
    /// IDirectDraw4, IDirectDrawSurface4, Direct3D 6.
    V4,
    V7,
}

pub fn iface_version(name: &str) -> DdVersion {
    match name {
        "IDirectDraw"
        | "IDirectDraw2"
        | "IDirectDrawSurface"
        | "IDirectDrawSurface2"
        | "IDirectDrawSurface3"
        | "IDirect3D"
        | "IDirect3D2"
        | "IDirect3DDevice"
        | "IDirect3DDevice2"
        | "IDirect3DTexture"
        | "IDirect3DTexture2" => DdVersion::Legacy,
        "IDirectDraw4" | "IDirectDrawSurface4" | "IDirect3D3" | "IDirect3DDevice3" | "IDirect3DVertexBuffer" => {
            DdVersion::V4
        }
        _ => DdVersion::V7,
    }
}

impl Session {
    /// Name of the interface `this` (argument 0) points at.
    pub fn caller_iface(&self) -> String {
        let this = self.arg(0).unwrap_or(0);
        self.com.ptrs.get(&this).map(|(_, n)| n.clone()).unwrap_or_default()
    }
    pub fn caller_version(&self) -> DdVersion {
        iface_version(&self.caller_iface())
    }
}

/// The surface interface a method returns: the caller's own surface version, or the
/// surface generation that matches a DirectDraw/Direct3D interface.
pub fn surface_def(s: &Session) -> &'static IfaceDef {
    match s.caller_iface().as_str() {
        "IDirectDrawSurface2" => &IDIRECTDRAWSURFACE2,
        "IDirectDrawSurface3" => &IDIRECTDRAWSURFACE3,
        n => match iface_version(n) {
            DdVersion::Legacy => &IDIRECTDRAWSURFACE,
            DdVersion::V4 => &IDIRECTDRAWSURFACE4,
            DdVersion::V7 => &IDIRECTDRAWSURFACE7,
        },
    }
}

fn ddraw_def(s: &Session) -> &'static IfaceDef {
    match s.caller_version() {
        DdVersion::Legacy => &IDIRECTDRAW,
        DdVersion::V4 => &IDIRECTDRAW4,
        DdVersion::V7 => &IDIRECTDRAW7,
    }
}

/// DDSURFACEDESC (108 bytes) for legacy interfaces, DDSURFACEDESC2 (124) otherwise.
fn ddsd_size(s: &Session) -> u32 {
    if s.caller_version() == DdVersion::Legacy {
        108
    } else {
        124
    }
}

pub static IDIRECTDRAWPALETTE: IfaceDef = IfaceDef {
    name: "IDirectDrawPalette",
    iid: guid("6c14db84-a733-11ce-a521-0020af0be560"),
    module: "DDRAW.DLL",
    methods: &[
        iunknown!()[0],
        iunknown!()[1],
        iunknown!()[2],
        export!("GetCaps", 2, pal_get_caps),
        export!("GetEntries", 5, pal_get_entries),
        unimpl_method!("Initialize", 4),
        export!("SetEntries", 5, pal_set_entries),
    ],
};

pub static IDIRECTDRAWCLIPPER: IfaceDef = IfaceDef {
    name: "IDirectDrawClipper",
    iid: guid("6c14db85-a733-11ce-a521-0020af0be560"),
    module: "DDRAW.DLL",
    methods: &[
        iunknown!()[0],
        iunknown!()[1],
        iunknown!()[2],
        unimpl_method!("GetClipList", 4),
        export!("GetHWnd", 2, clip_get_hwnd),
        unimpl_method!("Initialize", 3),
        export!("IsClipListChanged", 2, clip_is_clip_list_changed),
        export!("SetClipList", 3, clip_set_clip_list),
        export!("SetHWnd", 3, clip_set_hwnd),
    ],
};

pub static IDIRECTDRAWGAMMACONTROL: IfaceDef = IfaceDef {
    name: "IDirectDrawGammaControl",
    iid: guid("69c11c3e-b46b-11d1-ad7a-00c04fc29b4e"),
    module: "DDRAW.DLL",
    methods: &[
        iunknown!()[0],
        iunknown!()[1],
        iunknown!()[2],
        export!("GetGammaRamp", 3, gamma_get_ramp),
        export!("SetGammaRamp", 3, gamma_set_ramp),
    ],
};

fn ddraw_create_clipper_free(s: &mut Session) -> ApiResult {
    let [_flags, pp, outer] = s.args::<3>()?;
    if outer != 0 {
        return Ok(ApiRet::Ret(0x8004_0110));
    }
    let id = s.com_new(ComKind::Clipper(ClipperObj::default()));
    s.out_iface(pp, id, &IDIRECTDRAWCLIPPER)?;
    Ok(ApiRet::Ret(DD_OK))
}

pub static DDRAW_EXPORTS: &[ExportDef] = &[
    export!("DirectDrawCreate", 3, direct_draw_create),
    export!("DirectDrawCreateClipper", 3, ddraw_create_clipper_free),
    export!("DirectDrawCreateEx", 4, direct_draw_create_ex),
    export!("DirectDrawEnumerateA", 2, direct_draw_enumerate_a),
    export!("DirectDrawEnumerateExA", 3, direct_draw_enumerate_ex_a),
];
