//! gdi32: device contexts, brushes, pens, fonts, text and simple drawing.
//!
//! Drawing happens on the CPU into surface memory (window DCs draw into the surface
//! currently scanned out, at the window's position). TrueType text uses bundled
//! Liberation fonts, which are metric-compatible with Arial, Times New Roman and
//! Courier New; glyph rasterisation is not bit-identical to Windows' hinted renderer.

use crate::api::*;
use crate::dll::user32::Rect;
use crate::export;
use crate::session::Session;
use crate::surface::{Fmt, PixelTarget};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DcTarget {
    /// Client area (or whole window for GetWindowDC) of a window, on the front surface.
    Window {
        hwnd: u32,
        whole: bool,
    },
    Surface(u32),
    /// Memory DC with a selected bitmap (or the default 1x1 monochrome bitmap).
    Memory,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Dc {
    pub target: DcTarget,
    pub brush: u32,
    pub pen: u32,
    pub font: u32,
    pub bitmap: u32,
    pub text_color: u32,
    pub bk_color: u32,
    /// 1 = TRANSPARENT, 2 = OPAQUE
    pub bk_mode: u32,
    pub text_align: u32,
    pub pos: (i32, i32),
    #[serde(default)]
    pub stretch_mode: u32,
    #[serde(default)]
    pub palette: u32,
    /// States saved by SaveDC, innermost last.
    #[serde(default)]
    pub saved: Vec<Dc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogFont {
    pub height: i32,
    pub width: i32,
    pub weight: i32,
    pub italic: bool,
    pub underline: bool,
    pub strikeout: bool,
    pub charset: u8,
    pub quality: u8,
    pub pitch_family: u8,
    pub face: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum GdiObj {
    Dc(Dc),
    Brush {
        color: u32,
        null: bool,
    },
    Pen {
        color: u32,
        width: i32,
        null: bool,
    },
    Font(LogFont),
    /// Bitmap backed by a surface.
    Bitmap {
        surface: u32,
    },
    Palette,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GdiState {
    pub objs: BTreeMap<u32, GdiObj>,
    pub next: u32,
    /// BITMAPINFOHEADER (and colour masks) of bitmaps created as DIB sections.
    #[serde(default)]
    pub dibs: BTreeMap<u32, Vec<u8>>,
    /// Entries (0x00RRGGBB) of logical palettes.
    #[serde(default)]
    pub palettes: BTreeMap<u32, Vec<u32>>,
}

impl Default for GdiState {
    fn default() -> Self {
        let mut objs = BTreeMap::new();
        // Stock objects at fixed handles.
        let stock: [(u32, GdiObj); 11] = [
            (
                0,
                GdiObj::Brush {
                    color: 0xffffff,
                    null: false,
                },
            ),
            (
                1,
                GdiObj::Brush {
                    color: 0xc0c0c0,
                    null: false,
                },
            ),
            (
                2,
                GdiObj::Brush {
                    color: 0x808080,
                    null: false,
                },
            ),
            (
                3,
                GdiObj::Brush {
                    color: 0x404040,
                    null: false,
                },
            ),
            (
                4,
                GdiObj::Brush {
                    color: 0x000000,
                    null: false,
                },
            ),
            (5, GdiObj::Brush { color: 0, null: true }),
            (
                6,
                GdiObj::Pen {
                    color: 0xffffff,
                    width: 1,
                    null: false,
                },
            ),
            (
                7,
                GdiObj::Pen {
                    color: 0,
                    width: 1,
                    null: false,
                },
            ),
            (
                8,
                GdiObj::Pen {
                    color: 0,
                    width: 1,
                    null: true,
                },
            ),
            (13, GdiObj::Font(system_font("System", 16, 700))),
            (17, GdiObj::Font(system_font("MS Shell Dlg", -11, 400))),
        ];
        for (i, o) in stock {
            objs.insert(STOCK_BASE + i, o);
        }
        for (i, face) in [
            (10u32, "Terminal"),
            (11, "Courier"),
            (12, "MS Sans Serif"),
            (16, "Fixedsys"),
        ] {
            objs.insert(STOCK_BASE + i, GdiObj::Font(system_font(face, 16, 400)));
        }
        objs.insert(STOCK_BASE + 15, GdiObj::Palette);
        GdiState {
            objs,
            next: 0x0101_0000,
            dibs: BTreeMap::new(),
            palettes: BTreeMap::new(),
        }
    }
}

pub(crate) const STOCK_BASE: u32 = 0x0180_0000;
const SYS_BRUSH_BASE: u32 = 0x0181_0000;

/// The shared brush of system colour `i` (GetSysColorBrush). Deleting it has no effect.
pub fn sys_color_brush(s: &mut Session, i: u32) -> u32 {
    let h = SYS_BRUSH_BASE + i;
    s.gdi.objs.entry(h).or_insert(GdiObj::Brush {
        color: crate::dll::user32::sys_color(i),
        null: false,
    });
    h
}

pub fn system_font(face: &str, height: i32, weight: i32) -> LogFont {
    LogFont {
        height,
        width: 0,
        weight,
        italic: false,
        underline: false,
        strikeout: false,
        charset: 0,
        quality: 0,
        pitch_family: 0,
        face: face.into(),
    }
}

impl GdiState {
    pub fn add(&mut self, o: GdiObj) -> u32 {
        let h = self.next;
        self.next += 4;
        self.objs.insert(h, o);
        h
    }
    pub fn dc(&self, h: u32) -> Option<&Dc> {
        match self.objs.get(&h) {
            Some(GdiObj::Dc(d)) => Some(d),
            _ => None,
        }
    }
    pub fn dc_mut(&mut self, h: u32) -> Option<&mut Dc> {
        match self.objs.get_mut(&h) {
            Some(GdiObj::Dc(d)) => Some(d),
            _ => None,
        }
    }
}

fn new_dc(target: DcTarget) -> Dc {
    Dc {
        target,
        brush: STOCK_BASE,
        pen: STOCK_BASE + 7,
        font: STOCK_BASE + 13,
        bitmap: 0,
        text_color: 0,
        bk_color: 0xffffff,
        bk_mode: 2,
        text_align: 0,
        pos: (0, 0),
        stretch_mode: 1,
        palette: STOCK_BASE + 15,
        saved: Vec::new(),
    }
}

pub fn create_dc(s: &mut Session, target: DcTarget) -> u32 {
    s.gdi.add(GdiObj::Dc(new_dc(target)))
}

pub fn window_dc(s: &mut Session, hwnd: u32, whole: bool) -> u32 {
    create_dc(s, DcTarget::Window { hwnd, whole })
}

pub fn release_dc_handle(s: &mut Session, hdc: u32) -> bool {
    if let Some(GdiObj::Dc(d)) = s.gdi.objs.get(&hdc) {
        if let DcTarget::Surface(id) = d.target
            && let Some(sf) = s.display.surfaces.get_mut(&id)
        {
            sf.dcs = sf.dcs.saturating_sub(1);
        }
        s.gdi.objs.remove(&hdc);
        return true;
    }
    false
}

pub fn get_dc(s: &mut Session) -> ApiResult {
    let [hwnd] = s.args::<1>()?;
    Ok(ApiRet::Ret(window_dc(s, hwnd, false)))
}
pub fn get_window_dc(s: &mut Session) -> ApiResult {
    let [hwnd] = s.args::<1>()?;
    Ok(ApiRet::Ret(window_dc(s, hwnd, true)))
}
pub fn release_dc(s: &mut Session) -> ApiResult {
    let [_hwnd, hdc] = s.args::<2>()?;
    Ok(ApiRet::Ret(release_dc_handle(s, hdc) as u32))
}

/// Resolve a DC to (surface id, origin, clip rectangle in target coordinates).
pub(crate) fn dc_target(s: &mut Session, hdc: u32) -> Option<(u32, (i32, i32), Rect)> {
    let dc = s.gdi.dc(hdc)?.clone();
    match dc.target {
        DcTarget::Surface(id) => {
            let sf = s.display.surfaces.get(&id)?;
            Some((id, (0, 0), Rect::new(0, 0, sf.w as i32, sf.h as i32)))
        }
        DcTarget::Window { hwnd, whole } => {
            let front = s.front_surface();
            let sf = s.display.surfaces.get(&front)?;
            let full = Rect::new(0, 0, sf.w as i32, sf.h as i32);
            if hwnd == 0 || hwnd == crate::dll::user32::HWND_DESKTOP {
                return Some((front, (0, 0), full));
            }
            let w = s.window(hwnd)?;
            let r = if whole { w.rect } else { w.client };
            Some((front, (r.left, r.top), r.intersect(&full)))
        }
        DcTarget::Memory => {
            let GdiObj::Bitmap { surface } = s.gdi.objs.get(&dc.bitmap)? else {
                return None;
            };
            let sf = s.display.surfaces.get(surface)?;
            Some((*surface, (0, 0), Rect::new(0, 0, sf.w as i32, sf.h as i32)))
        }
    }
}

fn colorref_to_argb(c: u32) -> u32 {
    0xff00_0000 | (c & 0xff) << 16 | (c & 0xff00) | (c >> 16) & 0xff
}

/// Raw pixel value for a COLORREF on a surface (nearest palette entry for P8).
pub(crate) fn raw_color(s: &Session, id: u32, colorref: u32) -> u32 {
    let sf = &s.display.surfaces[&id];
    let argb = colorref_to_argb(colorref);
    if sf.fmt == Fmt::P8 {
        let pal = s.palette_of(id).unwrap_or([0; 256]);
        let (r, g, b) = ((argb >> 16) & 0xff, (argb >> 8) & 0xff, argb & 0xff);
        let mut best = (u32::MAX, 0u32);
        for (i, p) in pal.iter().enumerate() {
            let (pr, pg, pb) = ((p >> 16) & 0xff, (p >> 8) & 0xff, p & 0xff);
            let d = (pr.abs_diff(r)).pow(2) + (pg.abs_diff(g)).pow(2) + (pb.abs_diff(b)).pow(2);
            if d < best.0 {
                best = (d, i as u32);
            }
        }
        return best.1;
    }
    let mut b = [0u8; 4];
    sf.fmt.encode(argb, &mut b);
    u32::from_le_bytes(b)
}

fn fill_on(s: &mut Session, id: u32, clip: &Rect, r: &Rect, colorref: u32) -> Result<(), ApiError> {
    let raw = raw_color(s, id, colorref);
    s.surface_cpu_access(id, true);
    let t = PixelTarget::of(&s.display.surfaces[&id]);
    t.fill_raw(&mut s.mem, &r.intersect(clip), raw)?;
    Ok(())
}

pub fn fill_rect_hdc(s: &mut Session, hdc: u32, r: &Rect, brush: u32) -> Result<(), ApiError> {
    let color = match brush_color(s, brush) {
        Some(c) => c,
        None => return Ok(()),
    };
    let Some((id, (ox, oy), clip)) = dc_target(s, hdc) else {
        return Ok(());
    };
    let rr = Rect::new(r.left + ox, r.top + oy, r.right + ox, r.bottom + oy);
    fill_on(s, id, &clip, &rr, color)
}

/// Brush colour (COLORREF); system colour brushes are `COLOR_x + 1`.
pub(crate) fn brush_color(s: &Session, brush: u32) -> Option<u32> {
    if brush > 0 && brush <= 31 {
        // (HBRUSH)(COLOR_WINDOW + 1) style class brushes.
        let c: u32 = match brush - 1 {
            5 => 0xffffff,
            15 => 0xc8d0d4,
            1 => 0xa56e3a,
            _ => 0xc8d0d4,
        };
        return Some(c);
    }
    match s.gdi.objs.get(&brush) {
        Some(GdiObj::Brush { color, null: false }) => Some(*color),
        _ => None,
    }
}

/// Fill with a COLORREF on a DC (used by control painting).
pub fn fill_solid(s: &mut Session, hdc: u32, r: &Rect, colorref: u32) -> Result<(), ApiError> {
    let Some((id, (ox, oy), clip)) = dc_target(s, hdc) else {
        return Ok(());
    };
    let rr = Rect::new(r.left + ox, r.top + oy, r.right + ox, r.bottom + oy);
    fill_on(s, id, &clip, &rr, colorref)
}

pub fn create_font(s: &mut Session, height: i32, face: &str) -> u32 {
    let bold = face.to_uppercase().contains("BOLD");
    s.gdi
        .add(GdiObj::Font(system_font(face, height, if bold { 700 } else { 400 })))
}

pub fn default_gui_font() -> u32 {
    STOCK_BASE + 17
}

/// DrawText with an explicit font and colour, transparent background.
pub fn draw_label(
    s: &mut Session,
    hdc: u32,
    font: u32,
    text: &str,
    rc: &Rect,
    fmt: u32,
    color: u32,
) -> Result<(), ApiError> {
    let lf = match s.gdi.objs.get(&font) {
        Some(GdiObj::Font(lf)) => lf.clone(),
        _ => system_font("MS Sans Serif", -11, 400),
    };
    let fi = font_inst(s, &lf);
    let Some((id, (ox, oy), clip)) = dc_target(s, hdc) else {
        return Ok(());
    };
    let lines: Vec<String> = layout_lines(&fi, text, rc.w(), fmt).into_iter().map(|l| l.0).collect();
    let lh = fi.ascent + fi.descent;
    let clip = clip.intersect(&Rect::new(rc.left + ox, rc.top + oy, rc.right + ox, rc.bottom + oy));
    let mut y = rc.top;
    if fmt & 0x20 != 0 && fmt & 0x4 != 0 {
        y = rc.top + (rc.h() - lh) / 2;
    }
    for l in &lines {
        let w = text_width(&fi, l);
        let x = match fmt & 3 {
            1 => rc.left + (rc.w() - w) / 2,
            2 => rc.right - w,
            _ => rc.left,
        };
        draw_text_run(s, id, &clip, &fi, &lf, x + ox, y + oy, l, color, None)?;
        y += lh;
        if y > rc.bottom {
            break;
        }
    }
    Ok(())
}

/// Size of `text` drawn with `font` and DrawText flags `fmt`, wrapped at `width`
/// when `fmt` has DT_WORDBREAK.
pub fn measure_label(s: &mut Session, font: u32, text: &str, width: i32, fmt: u32) -> (i32, i32) {
    let lf = match s.gdi.objs.get(&font) {
        Some(GdiObj::Font(lf)) => lf.clone(),
        _ => system_font("MS Sans Serif", -11, 400),
    };
    let fi = font_inst(s, &lf);
    let lines = layout_lines(&fi, text, width, fmt);
    let w = lines.iter().map(|l| text_width(&fi, &l.0)).max().unwrap_or(0);
    (w, lines.len() as i32 * (fi.ascent + fi.descent))
}

pub fn fill_rect(s: &mut Session) -> ApiResult {
    let [hdc, p, brush] = s.args::<3>()?;
    let r = s.read_rect(p)?;
    fill_rect_hdc(s, hdc, &r, brush)?;
    Ok(ApiRet::Ret(1))
}

fn rectangle(s: &mut Session) -> ApiResult {
    let [hdc, l, t, r, b] = s.args::<5>()?;
    let Some(dc) = s.gdi.dc(hdc).cloned() else {
        return Ok(ApiRet::Ret(0));
    };
    let Some((id, (ox, oy), clip)) = dc_target(s, hdc) else {
        return Ok(ApiRet::Ret(1));
    };
    let rc = Rect::new(l as i32 + ox, t as i32 + oy, r as i32 + ox, b as i32 + oy);
    if let Some(c) = brush_color(s, dc.brush) {
        fill_on(
            s,
            id,
            &clip,
            &Rect::new(rc.left + 1, rc.top + 1, rc.right - 1, rc.bottom - 1),
            c,
        )?;
    }
    if let Some(GdiObj::Pen { color, null: false, .. }) = s.gdi.objs.get(&dc.pen).cloned() {
        for edge in [
            Rect::new(rc.left, rc.top, rc.right, rc.top + 1),
            Rect::new(rc.left, rc.bottom - 1, rc.right, rc.bottom),
            Rect::new(rc.left, rc.top, rc.left + 1, rc.bottom),
            Rect::new(rc.right - 1, rc.top, rc.right, rc.bottom),
        ] {
            fill_on(s, id, &clip, &edge, color)?;
        }
    }
    Ok(ApiRet::Ret(1))
}

fn get_stock_object(s: &mut Session) -> ApiResult {
    let [i] = s.args::<1>()?;
    let h = STOCK_BASE + i;
    Ok(ApiRet::Ret(if s.gdi.objs.contains_key(&h) { h } else { 0 }))
}

fn create_solid_brush(s: &mut Session) -> ApiResult {
    let [c] = s.args::<1>()?;
    Ok(ApiRet::Ret(s.gdi.add(GdiObj::Brush {
        color: c & 0xffffff,
        null: false,
    })))
}

fn create_pen(s: &mut Session) -> ApiResult {
    let [style, width, c] = s.args::<3>()?;
    Ok(ApiRet::Ret(s.gdi.add(GdiObj::Pen {
        color: c & 0xffffff,
        width: width as i32,
        null: style == 5,
    })))
}

fn read_logfont(s: &Session, p: u32) -> Result<LogFont, ApiError> {
    let b = s.mem.read_vec(p, 60)?;
    let g = |o: usize| i32::from_le_bytes(b[o..o + 4].try_into().unwrap());
    let face_end = b[28..60].iter().position(|&c| c == 0).unwrap_or(32);
    Ok(LogFont {
        height: g(0),
        width: g(4),
        weight: g(16),
        italic: b[20] != 0,
        underline: b[21] != 0,
        strikeout: b[22] != 0,
        charset: b[23],
        quality: b[26],
        pitch_family: b[27],
        face: crate::strings::decode_ansi(&b[28..28 + face_end]),
    })
}

fn create_font_a(s: &mut Session) -> ApiResult {
    let a = s.args::<14>()?;
    let face = s.read_str_opt(a[13])?.unwrap_or_default();
    let lf = LogFont {
        height: a[0] as i32,
        width: a[1] as i32,
        weight: a[4] as i32,
        italic: a[5] != 0,
        underline: a[6] != 0,
        strikeout: a[7] != 0,
        charset: a[8] as u8,
        quality: a[11] as u8,
        pitch_family: a[12] as u8,
        face,
    };
    Ok(ApiRet::Ret(s.gdi.add(GdiObj::Font(lf))))
}

fn create_font_indirect_a(s: &mut Session) -> ApiResult {
    let [p] = s.args::<1>()?;
    let lf = read_logfont(s, p)?;
    Ok(ApiRet::Ret(s.gdi.add(GdiObj::Font(lf))))
}

fn select_object(s: &mut Session) -> ApiResult {
    let [hdc, h] = s.args::<2>()?;
    let kind = match s.gdi.objs.get(&h) {
        Some(GdiObj::Brush { .. }) => 0,
        Some(GdiObj::Pen { .. }) => 1,
        Some(GdiObj::Font(_)) => 2,
        Some(GdiObj::Bitmap { .. }) => 3,
        _ => return Ok(ApiRet::Ret(0)),
    };
    let Some(dc) = s.gdi.dc_mut(hdc) else {
        return Ok(ApiRet::Ret(0));
    };
    let old = match kind {
        0 => std::mem::replace(&mut dc.brush, h),
        1 => std::mem::replace(&mut dc.pen, h),
        2 => std::mem::replace(&mut dc.font, h),
        _ => std::mem::replace(&mut dc.bitmap, h),
    };
    Ok(ApiRet::Ret(old))
}

fn delete_object(s: &mut Session) -> ApiResult {
    let [h] = s.args::<1>()?;
    if (STOCK_BASE..STOCK_BASE + 32).contains(&h) || (SYS_BRUSH_BASE..SYS_BRUSH_BASE + 32).contains(&h) {
        return Ok(ApiRet::Ret(1));
    }
    s.gdi.dibs.remove(&h);
    s.gdi.palettes.remove(&h);
    match s.gdi.objs.remove(&h) {
        Some(GdiObj::Bitmap { surface }) => {
            s.free_surface(surface);
            Ok(ApiRet::Ret(1))
        }
        Some(_) => Ok(ApiRet::Ret(1)),
        None => Ok(ApiRet::Ret(0)),
    }
}

fn set_text_color(s: &mut Session) -> ApiResult {
    let [hdc, c] = s.args::<2>()?;
    match s.gdi.dc_mut(hdc) {
        Some(d) => Ok(ApiRet::Ret(std::mem::replace(&mut d.text_color, c))),
        None => Ok(ApiRet::Ret(0xffff_ffff)),
    }
}
fn set_bk_color(s: &mut Session) -> ApiResult {
    let [hdc, c] = s.args::<2>()?;
    match s.gdi.dc_mut(hdc) {
        Some(d) => Ok(ApiRet::Ret(std::mem::replace(&mut d.bk_color, c))),
        None => Ok(ApiRet::Ret(0xffff_ffff)),
    }
}
fn set_bk_mode(s: &mut Session) -> ApiResult {
    let [hdc, m] = s.args::<2>()?;
    match s.gdi.dc_mut(hdc) {
        Some(d) => Ok(ApiRet::Ret(std::mem::replace(&mut d.bk_mode, m))),
        None => Ok(ApiRet::Ret(0)),
    }
}
fn set_text_align(s: &mut Session) -> ApiResult {
    let [hdc, a] = s.args::<2>()?;
    match s.gdi.dc_mut(hdc) {
        Some(d) => Ok(ApiRet::Ret(std::mem::replace(&mut d.text_align, a))),
        None => Ok(ApiRet::Ret(0xffff_ffff)),
    }
}

fn get_device_caps(s: &mut Session) -> ApiResult {
    let [hdc, idx] = s.args::<2>()?;
    let (w, h, bpp) = match dc_target(s, hdc) {
        Some((id, _, _)) => {
            let sf = &s.display.surfaces[&id];
            (sf.w, sf.h, sf.fmt.bits())
        }
        None => (s.user.mode.width, s.user.mode.height, s.user.mode.bpp),
    };
    let v = match idx {
        0 => 0x300,         // DRIVERVERSION
        2 => 1,             // TECHNOLOGY: DT_RASDISPLAY
        4 => w * 254 / 960, // HORZSIZE (mm at 96 dpi)
        6 => h * 254 / 960,
        8 => w,    // HORZRES
        10 => h,   // VERTRES
        12 => bpp, // BITSPIXEL
        14 => 1,   // PLANES
        24 => {
            if bpp <= 8 {
                20
            } else {
                0xffff_ffff
            }
        } // NUMCOLORS
        38 => 0x7e99, // RASTERCAPS
        40 => 36,  // ASPECTX
        42 => 36,
        44 => 51,
        88 | 90 => 96, // LOGPIXELSX/Y
        104 => {
            if bpp <= 8 {
                256
            } else {
                0
            }
        } // SIZEPALETTE
        106 => {
            if bpp <= 8 {
                20
            } else {
                0
            }
        } // NUMRESERVED
        108 => {
            if bpp <= 8 {
                18
            } else {
                24
            }
        } // COLORRES
        116 => s.user.mode.refresh, // VREFRESH
        117 => w,      // DESKTOPVERTRES (approx)
        118 => h,
        _ => {
            s.missing("GetDeviceCaps", format!("index {idx}"));
            0
        }
    };
    Ok(ApiRet::Ret(v))
}

// ---------------------------------------------------------------- fonts & text

struct Fonts {
    sans: fontdue::Font,
    sans_bold: fontdue::Font,
    serif: fontdue::Font,
    mono: fontdue::Font,
}

fn fonts() -> &'static Fonts {
    static F: OnceLock<Fonts> = OnceLock::new();
    F.get_or_init(|| {
        let load = |b: &[u8]| fontdue::Font::from_bytes(b, fontdue::FontSettings::default()).unwrap();
        Fonts {
            sans: load(include_bytes!("../../../../assets/fonts/LiberationSans-Regular.ttf")),
            sans_bold: load(include_bytes!("../../../../assets/fonts/LiberationSans-Bold.ttf")),
            serif: load(include_bytes!("../../../../assets/fonts/LiberationSerif-Regular.ttf")),
            mono: load(include_bytes!("../../../../assets/fonts/LiberationMono-Regular.ttf")),
        }
    })
}

struct FontInst {
    font: &'static fontdue::Font,
    px: f32,
    ascent: i32,
    descent: i32,
    internal_leading: i32,
}

/// Vertical metrics (usWinAscent, usWinDescent per 2048 units/em) of the TrueType fonts
/// installed with Windows 2000. Text in these faces is drawn with the bundled
/// Liberation fonts but laid out with the line height Windows gives the real face.
const WINDOWS_FACES: &[(&str, u32, u32)] = &[
    ("ARIAL BLACK", 2254, 634),
    ("ARIAL", 1854, 434),
    ("COMIC SANS MS", 2257, 597),
    ("COURIER NEW", 1705, 615),
    ("GEORGIA", 1878, 449),
    ("IMPACT", 2066, 432),
    ("LUCIDA CONSOLE", 1616, 432),
    ("MICROSOFT SANS SERIF", 1888, 430),
    ("TAHOMA", 2049, 423),
    ("TIMES NEW ROMAN", 1825, 443),
    ("TREBUCHET MS", 1923, 455),
    ("VERDANA", 2059, 430),
];

/// The font a LOGFONT selects: an installed guest font of that face, else a bundled one.
fn font_inst(s: &mut Session, lf: &LogFont) -> FontInst {
    match s.find_guest_font(&lf.face, lf.weight, lf.italic) {
        Some(g) => scaled(g.font, lf, g.ascent, g.descent),
        None => fallback_font_inst(lf),
    }
}

fn fallback_font_inst(lf: &LogFont) -> FontInst {
    let f = fonts();
    let face = lf.face.to_uppercase();
    let font = if face.contains("COURIER")
        || face.contains("MONO")
        || face.contains("FIXED")
        || face.contains("TERMINAL")
        || lf.pitch_family & 3 == 1
    {
        &f.mono
    } else if face.contains("TIMES") || face.contains("SERIF") && !face.contains("SANS") || face.contains("GEORGIA") {
        &f.serif
    } else if lf.weight >= 600 {
        &f.sans_bold
    } else {
        &f.sans
    };
    let m = font.horizontal_line_metrics(1.0).unwrap();
    // Ascent and descent per em: the requested Windows face's, else the drawing font's.
    // (Face names may carry a style suffix, such as "Trebuchet MS Bold".)
    let (asc, desc) = WINDOWS_FACES
        .iter()
        .find(|(n, _, _)| face == *n || face.strip_prefix(n).is_some_and(|r| r.starts_with(' ')))
        .map(|&(_, a, d)| (a as f32 / 2048.0, d as f32 / 2048.0))
        .unwrap_or((m.ascent, -m.descent));
    scaled(font, lf, asc, desc)
}

/// Pixel metrics for the LOGFONT height, given ascent and descent per em.
fn scaled(font: &'static fontdue::Font, lf: &LogFont, asc: f32, desc: f32) -> FontInst {
    let h = if lf.height == 0 { -12 } else { lf.height };
    let px = if h < 0 { -h as f32 } else { h as f32 / (asc + desc) };
    let ascent = (asc * px).round() as i32;
    let descent = (desc * px).round() as i32;
    let internal_leading = (ascent + descent) - px.round() as i32;
    FontInst {
        font,
        px,
        ascent,
        descent,
        internal_leading: internal_leading.max(0),
    }
}

fn dc_font(s: &Session, hdc: u32) -> LogFont {
    let f = s.gdi.dc(hdc).map(|d| d.font).unwrap_or(STOCK_BASE + 13);
    match s.gdi.objs.get(&f) {
        Some(GdiObj::Font(lf)) => lf.clone(),
        _ => system_font("System", 16, 700),
    }
}

fn char_advance(fi: &FontInst, c: char) -> i32 {
    fi.font.metrics(c, fi.px).advance_width.round() as i32
}

fn text_width(fi: &FontInst, text: &str) -> i32 {
    text.chars().map(|c| char_advance(fi, c)).sum()
}

/// Draw `text` with its cell's top-left at (x, y) in target coordinates.
fn draw_text_run(
    s: &mut Session,
    id: u32,
    clip: &Rect,
    fi: &FontInst,
    lf: &LogFont,
    x: i32,
    y: i32,
    text: &str,
    color: u32,
    bk: Option<u32>,
) -> Result<i32, ApiError> {
    let width = text_width(fi, text);
    if let Some(bk) = bk {
        fill_on(s, id, clip, &Rect::new(x, y, x + width, y + fi.ascent + fi.descent), bk)?;
    }
    s.surface_cpu_access(id, true);
    let sf = s.display.surfaces[&id].clone();
    let raw = raw_color(s, id, color);
    let argb = colorref_to_argb(color);
    let aa = lf.quality == 4 || lf.quality == 5;
    let bpp = sf.fmt.bytes_pp() as usize;
    let pal = s.palette_of(id);
    let mut pen_x = x;
    let baseline = y + fi.ascent;
    for c in text.chars() {
        let (m, bitmap) = fi.font.rasterize(c, fi.px);
        let gx = pen_x + m.xmin;
        let gy = baseline - m.height as i32 - m.ymin;
        for row in 0..m.height {
            let py = gy + row as i32;
            if py < clip.top || py >= clip.bottom {
                continue;
            }
            for col in 0..m.width {
                let px_ = gx + col as i32;
                if px_ < clip.left || px_ >= clip.right {
                    continue;
                }
                let cov = bitmap[row * m.width + col] as u32;
                let addr = sf.row_addr(py as u32) + px_ as u32 * bpp as u32;
                if aa && sf.fmt != Fmt::P8 {
                    if cov == 0 {
                        continue;
                    }
                    let mut cur = [0u8; 4];
                    s.mem.read(addr, &mut cur[..bpp])?;
                    let d = sf.fmt.decode(&cur[..bpp], pal.as_ref());
                    let mix = |sh: u32| {
                        let a = (argb >> sh) & 0xff;
                        let b = (d >> sh) & 0xff;
                        ((a * cov + b * (255 - cov)) / 255) << sh
                    };
                    let out = 0xff00_0000 | mix(16) | mix(8) | mix(0);
                    let mut ob = [0u8; 4];
                    sf.fmt.encode(out, &mut ob);
                    s.mem.write(addr, &ob[..bpp])?;
                } else if cov >= 128 {
                    s.mem.write(addr, &raw.to_le_bytes()[..bpp])?;
                }
            }
        }
        pen_x += char_advance(fi, c);
    }
    if lf.underline {
        let uy = baseline + 1;
        fill_on(s, id, clip, &Rect::new(x, uy, x + width, uy + 1), color)?;
    }
    if lf.strikeout {
        let sy = baseline - fi.ascent / 3;
        fill_on(s, id, clip, &Rect::new(x, sy, x + width, sy + 1), color)?;
    }
    Ok(width)
}

fn text_out_a(s: &mut Session) -> ApiResult {
    let [hdc, x, y, p, n] = s.args::<5>()?;
    let text = crate::strings::decode_ansi(&s.mem.read_vec(p, n)?);
    let Some(dc) = s.gdi.dc(hdc).cloned() else {
        return Ok(ApiRet::Ret(0));
    };
    let lf = dc_font(s, hdc);
    let fi = font_inst(s, &lf);
    let Some((id, (ox, oy), clip)) = dc_target(s, hdc) else {
        return Ok(ApiRet::Ret(1));
    };
    let (mut x, mut y) = if dc.text_align & 1 != 0 {
        dc.pos
    } else {
        (x as i32, y as i32)
    };
    let w = text_width(&fi, &text);
    match dc.text_align & 6 {
        2 => x -= w,     // TA_RIGHT
        6 => x -= w / 2, // TA_CENTER
        _ => {}
    }
    match dc.text_align & 0x18 {
        8 => y -= fi.ascent + fi.descent, // TA_BOTTOM
        0x18 => y -= fi.ascent,           // TA_BASELINE
        _ => {}
    }
    let bk = (dc.bk_mode == 2).then_some(dc.bk_color);
    draw_text_run(s, id, &clip, &fi, &lf, x + ox, y + oy, &text, dc.text_color, bk)?;
    if dc.text_align & 1 != 0
        && let Some(d) = s.gdi.dc_mut(hdc)
    {
        d.pos.0 += w;
    }
    Ok(ApiRet::Ret(1))
}

fn get_text_extent_point32_a(s: &mut Session) -> ApiResult {
    let [hdc, p, n, sz] = s.args::<4>()?;
    let text = crate::strings::decode_ansi(&s.mem.read_vec(p, n)?);
    let lf = dc_font(s, hdc);
    let fi = font_inst(s, &lf);
    s.mem.write_u32(sz, text_width(&fi, &text) as u32)?;
    s.mem.write_u32(sz + 4, (fi.ascent + fi.descent) as u32)?;
    Ok(ApiRet::Ret(1))
}

fn get_text_metrics_a(s: &mut Session) -> ApiResult {
    let [hdc, p] = s.args::<2>()?;
    let lf = dc_font(s, hdc);
    let fi = font_inst(s, &lf);
    let avg = char_advance(&fi, 'x');
    let max = ('A'..='Z')
        .map(|c| char_advance(&fi, c))
        .max()
        .unwrap_or(avg)
        .max(char_advance(&fi, 'W'));
    let mut b = vec![0u8; 56];
    let vals: [i32; 11] = [
        fi.ascent + fi.descent,
        fi.ascent,
        fi.descent,
        fi.internal_leading,
        0,
        avg,
        max,
        if lf.weight == 0 { 400 } else { lf.weight },
        0,
        96,
        96,
    ];
    for (i, v) in vals.iter().enumerate() {
        b[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
    }
    b[44] = 0x20;
    b[45] = 0xff;
    b[46] = 0x1f;
    b[47] = 0x20;
    b[48] = lf.italic as u8;
    b[49] = lf.underline as u8;
    b[50] = lf.strikeout as u8;
    b[51] = 0x26; // TMPF_VECTOR | TMPF_TRUETYPE | variable pitch, FF_SWISS
    b[52] = lf.charset;
    s.mem.write(p, &b)?;
    Ok(ApiRet::Ret(1))
}

/// Lines of `text` as DrawText lays them out, each with the number of characters of
/// `text` it consumes (including its line break, or the spaces at a word wrap).
fn layout_lines(fi: &FontInst, text: &str, width: i32, fmt: u32) -> Vec<(String, usize)> {
    // Prefix characters (&) are removed unless DT_NOPREFIX; DT_EXPANDTABS expands tabs.
    let disp = |src: &str| -> String {
        let mut out = String::new();
        let mut chars = src.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '&' && fmt & 0x800 == 0 {
                if chars.peek() == Some(&'&') {
                    out.push('&');
                    chars.next();
                }
                continue;
            }
            if c == '\t' && fmt & 0x40 != 0 {
                out.push_str("        ");
                continue;
            }
            out.push(c);
        }
        out
    };
    let total = text.chars().count();
    if fmt & 0x20 != 0 {
        // DT_SINGLELINE: line breaks are ignored.
        return vec![(disp(&text.replace(['\r', '\n'], "")), total)];
    }
    // Paragraphs with their source length including the terminating line break.
    let mut paragraphs: Vec<(String, usize)> = Vec::new();
    let mut cur = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' | '\n' => {
                let mut n = cur.chars().count() + 1;
                if c == '\r' && chars.peek() == Some(&'\n') {
                    chars.next();
                    n += 1;
                }
                paragraphs.push((std::mem::take(&mut cur), n));
            }
            c => cur.push(c),
        }
    }
    let n = cur.chars().count();
    paragraphs.push((cur, n));
    if fmt & 0x10 == 0 {
        return paragraphs.into_iter().map(|(p, n)| (disp(&p), n)).collect();
    }
    // DT_WORDBREAK: wrap at spaces.
    let mut out = Vec::new();
    for (p, n) in paragraphs {
        let mut line = String::new();
        let mut used = 0;
        for word in p.split_inclusive(' ') {
            let cand = format!("{line}{word}");
            if !line.is_empty() && text_width(fi, disp(&cand).trim_end()) > width {
                let k = line.chars().count();
                out.push((disp(&line).trim_end().to_string(), k));
                used += k;
                line = word.to_string();
            } else {
                line = cand;
            }
        }
        out.push((disp(&line).trim_end().to_string(), n - used));
    }
    out
}

pub fn draw_text_a(s: &mut Session) -> ApiResult {
    let [hdc, p, n, prc, fmt] = s.args::<5>()?;
    draw_text(s, hdc, p, n, prc, fmt, 0)
}

/// DrawText/DrawTextEx. `params` is a DRAWTEXTPARAMS (margins in, characters
/// processed out) or 0.
pub fn draw_text(s: &mut Session, hdc: u32, p: u32, n: u32, prc: u32, fmt: u32, params: u32) -> ApiResult {
    let bytes = if n as i32 == -1 {
        s.mem.read_cstr(p, 0x10000)?
    } else {
        s.mem.read_vec(p, n)?
    };
    let text = crate::strings::decode_ansi(&bytes);
    let Some(dc) = s.gdi.dc(hdc).cloned() else {
        return Ok(ApiRet::Ret(0));
    };
    let (lm, rm) = if params != 0 {
        (s.mem.read_u32(params + 8)? as i32, s.mem.read_u32(params + 12)? as i32)
    } else {
        (0, 0)
    };
    let lf = dc_font(s, hdc);
    let fi = font_inst(s, &lf);
    let full = s.read_rect(prc)?;
    let rc = Rect::new(full.left + lm, full.top, full.right - rm, full.bottom);
    let lines = layout_lines(&fi, &text, rc.w(), fmt);
    let lh = fi.ascent + fi.descent;
    let total_h = lh * lines.len() as i32;
    if params != 0 {
        // Characters of the lines that start inside the rectangle (all of them when
        // measuring or not clipping).
        let fits = if fmt & (0x400 | 0x100 | 0x20) != 0 {
            lines.len()
        } else {
            (((full.h() + lh - 1) / lh.max(1)).max(1) as usize).min(lines.len())
        };
        let drawn: usize = lines[..fits].iter().map(|l| l.1).sum();
        s.mem.write_u32(params + 16, drawn as u32)?;
    }
    if fmt & 0x400 != 0 {
        // DT_CALCRECT
        let w = lines.iter().map(|l| text_width(&fi, &l.0)).max().unwrap_or(0);
        let mut r = full;
        r.right = r.left
            + lm
            + rm
            + if fmt & 0x10 != 0 {
                w.max(0).min(rc.w().max(w))
            } else {
                w
            };
        r.bottom = r.top + total_h;
        s.write_rect(prc, &r)?;
        return Ok(ApiRet::Ret(total_h as u32));
    }
    let Some((id, (ox, oy), clip)) = dc_target(s, hdc) else {
        return Ok(ApiRet::Ret(total_h as u32));
    };
    let clip = if fmt & 0x100 != 0 {
        clip
    } else {
        clip.intersect(&Rect::new(
            full.left + ox,
            full.top + oy,
            full.right + ox,
            full.bottom + oy,
        ))
    };
    let mut y = rc.top;
    if fmt & 0x20 != 0 {
        if fmt & 0x4 != 0 {
            y = rc.top + (rc.h() - lh) / 2;
        } else if fmt & 0x8 != 0 {
            y = rc.bottom - lh;
        }
    }
    let bk = (dc.bk_mode == 2).then_some(dc.bk_color);
    for (l, _) in &lines {
        let w = text_width(&fi, l);
        let x = match fmt & 3 {
            1 => rc.left + (rc.w() - w) / 2,
            2 => rc.right - w,
            _ => rc.left,
        };
        draw_text_run(s, id, &clip, &fi, &lf, x + ox, y + oy, l, dc.text_color, bk)?;
        y += lh;
    }
    Ok(ApiRet::Ret(total_h as u32))
}

pub static GDI32_EXPORTS: &[ExportDef] = &[
    export!("CreateFontA", 14, create_font_a),
    export!("CreateFontIndirectA", 1, create_font_indirect_a),
    export!("CreatePen", 3, create_pen),
    export!("CreateSolidBrush", 1, create_solid_brush),
    export!("DeleteObject", 1, delete_object),
    export!("GetDeviceCaps", 2, get_device_caps),
    export!("GetStockObject", 1, get_stock_object),
    export!("GetTextExtentPoint32A", 4, get_text_extent_point32_a),
    export!("GetTextMetricsA", 2, get_text_metrics_a),
    export!("Rectangle", 5, rectangle),
    export!("SelectObject", 2, select_object),
    export!("SetBkColor", 2, set_bk_color),
    export!("SetBkMode", 2, set_bk_mode),
    export!("SetTextAlign", 2, set_text_align),
    export!("SetTextColor", 2, set_text_color),
    export!("TextOutA", 5, text_out_a),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_counts_consumed_characters() {
        let fi = fallback_font_inst(&system_font("MS Sans Serif", -11, 400));
        // Line breaks count towards the line they end.
        let l = layout_lines(&fi, "ab\r\ncd\nef", 1000, 0);
        let counts: Vec<_> = l.iter().map(|x| (x.0.as_str(), x.1)).collect();
        assert_eq!(counts, vec![("ab", 4), ("cd", 3), ("ef", 2)]);
        // Word wrap: the space at the break belongs to the first line.
        let w = text_width(&fi, "hello");
        let l = layout_lines(&fi, "hello world", w + 2, 0x10);
        let counts: Vec<_> = l.iter().map(|x| (x.0.as_str(), x.1)).collect();
        assert_eq!(counts, vec![("hello", 6), ("world", 5)]);
        // Prefix characters are hidden but counted.
        let l = layout_lines(&fi, "&File", 1000, 0x20);
        assert_eq!((l[0].0.as_str(), l[0].1), ("File", 5));
    }
}
