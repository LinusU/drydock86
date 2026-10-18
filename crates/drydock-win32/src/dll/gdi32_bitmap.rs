//! gdi32: bitmaps (device-dependent bitmaps and DIB sections), memory DCs, block
//! transfers with raster operations, DC state stacks and logical palettes.

use super::gdi32::{Dc, DcTarget, GdiObj, STOCK_BASE, brush_color, create_dc, dc_target, raw_color};
use crate::api::*;
use crate::com::ddraw::{BltOpts, blit};
use crate::dll::user32::Rect;
use crate::export;
use crate::session::Session;
use crate::surface::Fmt;

const SRCCOPY: u32 = 0x00cc_0020;
const PATCOPY: u32 = 0x00f0_0021;
const BLACKNESS: u32 = 0x0000_0042;
const WHITENESS: u32 = 0x00ff_0062;
const DSTINVERT: u32 = 0x0055_0009;
const PATINVERT: u32 = 0x005a_0049;

const OBJ_PEN: u32 = 1;
const OBJ_BRUSH: u32 = 2;
const OBJ_DC: u32 = 3;
const OBJ_PAL: u32 = 5;
const OBJ_FONT: u32 = 6;
const OBJ_BITMAP: u32 = 7;
const OBJ_MEMDC: u32 = 10;

// ---------------------------------------------------------------- DIB descriptions

/// A parsed BITMAPINFO.
struct DibInfo {
    w: u32,
    h: u32,
    top_down: bool,
    bpp: u16,
    /// Surface format the pixels are stored in (1- and 4-bit DIBs are expanded to P8).
    fmt: Fmt,
    /// Colour table (0x00RRGGBB) for palettized DIBs.
    colors: Vec<u32>,
    /// BITMAPINFOHEADER followed by the colour masks of BI_BITFIELDS DIBs.
    header: Vec<u8>,
    /// Bytes from the start of the BITMAPINFO to the pixels of a packed DIB.
    packed_len: u32,
}

fn parse_bitmap_info(s: &Session, p: u32, usage: u32, hdc: u32) -> Result<Option<DibInfo>, ApiError> {
    let size = s.mem.read_u32(p)?;
    if size < 40 {
        // BITMAPCOREHEADER DIBs are not produced by the applications seen so far.
        return Ok(None);
    }
    let b = s.mem.read_vec(p, 40)?;
    let g32 = |o: usize| u32::from_le_bytes(b[o..o + 4].try_into().unwrap());
    let w = g32(4) as i32;
    let h = g32(8) as i32;
    let bpp = u16::from_le_bytes([b[14], b[15]]);
    let compression = g32(16);
    let used = g32(32);
    if w <= 0 || h == 0 || compression > 3 || compression == 1 || compression == 2 {
        return Ok(None);
    }
    let mut header = b.clone();
    let mut fmt = match bpp {
        1 | 4 | 8 => Fmt::P8,
        16 => Fmt::Xrgb1555,
        24 => Fmt::Rgb888,
        32 => Fmt::Xrgb8888,
        _ => return Ok(None),
    };
    let mut colors = Vec::new();
    let mut packed_len = size;
    if compression == 3 {
        let masks = s.mem.read_vec(p + size, 12)?;
        let m = |i: usize| u32::from_le_bytes(masks[i * 4..i * 4 + 4].try_into().unwrap());
        fmt = match (bpp, m(0), m(1), m(2)) {
            (16, 0xf800, 0x07e0, 0x001f) => Fmt::Rgb565,
            (16, 0x7c00, 0x03e0, 0x001f) => Fmt::Xrgb1555,
            (32, 0xff_0000, 0xff00, 0xff) => Fmt::Xrgb8888,
            _ => return Ok(None),
        };
        header.extend(masks);
        packed_len += 12;
    }
    if bpp <= 8 {
        let n = if used != 0 { used.min(256) } else { 1 << bpp };
        if usage == 1 {
            // DIB_PAL_COLORS: indices into the DC's logical palette.
            let pal = s.gdi.dc(hdc).map(|d| d.palette).unwrap_or(0);
            let entries = s.gdi.palettes.get(&pal).cloned().unwrap_or_default();
            for i in 0..n {
                let k = s.mem.read_u16(p + size + 2 * i)? as usize;
                colors.push(entries.get(k).copied().unwrap_or(0));
            }
            packed_len += 2 * n;
        } else {
            for i in 0..n {
                let q = s.mem.read_u32(p + size + 4 * i)?;
                colors.push(q & 0x00ff_ffff);
            }
            packed_len += 4 * n;
        }
    }
    Ok(Some(DibInfo {
        w: w as u32,
        h: h.unsigned_abs(),
        top_down: h < 0,
        bpp,
        fmt,
        colors,
        header,
        packed_len,
    }))
}

impl Session {
    /// Give a palettized surface its own colour table.
    fn set_surface_colors(&mut self, id: u32, colors: &[u32]) {
        if colors.is_empty() {
            return;
        }
        let pid = self.display.next_id.max(1);
        self.display.next_id = pid + 1;
        let mut entries = colors.to_vec();
        entries.resize(256, 0);
        self.display.palettes.insert(
            pid,
            crate::surface::Palette {
                id: pid,
                entries,
                flags: 0,
            },
        );
        if let Some(sf) = self.display.surfaces.get_mut(&id) {
            sf.palette = pid;
        }
    }
}

/// A description of a BITMAPINFOHEADER for diagnostics.
fn describe_bitmap_info(s: &Session, p: u32) -> Result<String, ApiError> {
    let b = s.mem.read_vec(p, 52)?;
    let g32 = |o: usize| u32::from_le_bytes(b[o..o + 4].try_into().unwrap());
    Ok(format!(
        "biSize {} {}x{} {} bpp, compression {}, masks {:#x}/{:#x}/{:#x}",
        g32(0),
        g32(4) as i32,
        g32(8) as i32,
        u16::from_le_bytes([b[14], b[15]]),
        g32(16),
        g32(40),
        g32(44),
        g32(48)
    ))
}

/// A surface holding a copy of DIB pixels at `bits` (1- and 4-bit pixels expanded).
fn surface_from_dib(s: &mut Session, d: &DibInfo, bits: u32) -> Result<Option<u32>, ApiError> {
    let id = match s.alloc_surface(d.w, d.h, d.fmt, false, 0) {
        Ok(id) => id,
        Err(_) => return Ok(None),
    };
    s.set_surface_colors(id, &d.colors);
    let src_pitch = (d.w * d.bpp as u32).div_ceil(32) * 4;
    let sf = s.display.surfaces[&id].clone();
    for y in 0..d.h {
        let src_row = if d.top_down { y } else { d.h - 1 - y };
        let row = s.mem.read_vec(bits + src_row * src_pitch, src_pitch)?;
        let out: Vec<u8> = match d.bpp {
            1 => (0..d.w).map(|x| (row[(x / 8) as usize] >> (7 - x % 8)) & 1).collect(),
            4 => (0..d.w)
                .map(|x| {
                    let b = row[(x / 2) as usize];
                    if x % 2 == 0 { b >> 4 } else { b & 15 }
                })
                .collect(),
            _ => row[..(d.w * d.fmt.bytes_pp()) as usize].to_vec(),
        };
        s.mem.write(sf.row_addr(y), &out)?;
    }
    Ok(Some(id))
}

fn new_bitmap(s: &mut Session, surface: u32) -> u32 {
    s.gdi.add(GdiObj::Bitmap { surface })
}

// ---------------------------------------------------------------- bitmaps

fn create_dib_section(s: &mut Session) -> ApiResult {
    let [hdc, pbmi, usage, ppbits, section, _offset] = s.args::<6>()?;
    let Some(d) = parse_bitmap_info(s, pbmi, usage, hdc)? else {
        let desc = describe_bitmap_info(s, pbmi)?;
        s.missing("CreateDIBSection", format!("DIB format not supported: {desc}"));
        return Ok(ApiRet::Ret(0));
    };
    if d.bpp < 8 {
        s.missing("CreateDIBSection", format!("{}-bit DIB sections", d.bpp));
        return Ok(ApiRet::Ret(0));
    }
    if section != 0 {
        s.missing("CreateDIBSection", "DIB sections in file mappings".into());
        return Ok(ApiRet::Ret(0));
    }
    let Ok(id) = s.alloc_surface(d.w, d.h, d.fmt, false, 0) else {
        s.set_last_error(8);
        return Ok(ApiRet::Ret(0));
    };
    s.set_surface_colors(id, &d.colors);
    if let Some(sf) = s.display.surfaces.get_mut(&id) {
        sf.bottom_up = !d.top_down;
    }
    let mem = s.display.surfaces[&id].mem;
    if ppbits != 0 {
        s.mem.write_u32(ppbits, mem)?;
    }
    let h = new_bitmap(s, id);
    s.gdi.dibs.insert(h, d.header);
    Ok(ApiRet::Ret(h))
}

/// Format of bitmaps compatible with a DC.
fn dc_format(s: &mut Session, hdc: u32) -> Option<Fmt> {
    let (id, _, _) = dc_target(s, hdc)?;
    s.display.surfaces.get(&id).map(|p| p.fmt)
}

fn create_compatible_bitmap(s: &mut Session) -> ApiResult {
    let [hdc, w, h] = s.args::<3>()?;
    let mono = matches!(
        s.gdi.dc(hdc),
        Some(Dc {
            target: DcTarget::Memory,
            bitmap: 0,
            ..
        })
    );
    let fmt = if mono {
        Fmt::P8
    } else {
        dc_format(s, hdc).unwrap_or(Fmt::Xrgb8888)
    };
    let (w, h) = if w == 0 || h == 0 { (1, 1) } else { (w, h) };
    let Ok(id) = s.alloc_surface(w, h, fmt, false, 0) else {
        s.set_last_error(8);
        return Ok(ApiRet::Ret(0));
    };
    if mono {
        s.set_surface_colors(id, &[0, 0xffffff]);
    } else if fmt == Fmt::P8 {
        let front = s.front_surface();
        let pal = s.display.surfaces.get(&front).map(|f| f.palette).unwrap_or(0);
        if let Some(sf) = s.display.surfaces.get_mut(&id) {
            sf.palette = pal;
        }
    }
    Ok(ApiRet::Ret(new_bitmap(s, id)))
}

fn create_bitmap(s: &mut Session) -> ApiResult {
    let [w, h, planes, bpp, bits] = s.args::<5>()?;
    create_bitmap_common(s, w, h, planes, bpp, bits)
}

fn create_bitmap_indirect(s: &mut Session) -> ApiResult {
    let [p] = s.args::<1>()?;
    let w = s.mem.read_u32(p + 4)?;
    let h = s.mem.read_u32(p + 8)?;
    let planes = s.mem.read_u16(p + 16)? as u32;
    let bpp = s.mem.read_u16(p + 18)? as u32;
    let bits = s.mem.read_u32(p + 20)?;
    create_bitmap_common(s, w, h, planes, bpp, bits)
}

fn create_bitmap_common(s: &mut Session, w: u32, h: u32, planes: u32, bpp: u32, bits: u32) -> ApiResult {
    let fmt = match (planes, bpp) {
        (1, 1) | (1, 8) => Fmt::P8,
        (1, 16) => Fmt::Rgb565,
        (1, 24) => Fmt::Rgb888,
        (1, 32) => Fmt::Xrgb8888,
        _ => {
            s.missing("CreateBitmap", format!("{planes} planes of {bpp} bits"));
            return Ok(ApiRet::Ret(0));
        }
    };
    let (w, h) = if w == 0 || h == 0 { (1, 1) } else { (w, h) };
    let Ok(id) = s.alloc_surface(w, h, fmt, false, 0) else {
        return Ok(ApiRet::Ret(0));
    };
    if bpp == 1 {
        s.set_surface_colors(id, &[0, 0xffffff]);
    }
    if bits != 0 {
        // Device-dependent bitmap rows are padded to 16 bits.
        let src_pitch = (w * bpp).div_ceil(16) * 2;
        let sf = s.display.surfaces[&id].clone();
        for y in 0..h {
            let row = s.mem.read_vec(bits + y * src_pitch, src_pitch)?;
            let out: Vec<u8> = if bpp == 1 {
                (0..w).map(|x| (row[(x / 8) as usize] >> (7 - x % 8)) & 1).collect()
            } else {
                row[..(w * fmt.bytes_pp()) as usize].to_vec()
            };
            s.mem.write(sf.row_addr(y), &out)?;
        }
    }
    Ok(ApiRet::Ret(new_bitmap(s, id)))
}

/// LoadBitmap: RT_BITMAP resources are packed DIBs.
pub fn load_bitmap_a(s: &mut Session) -> ApiResult {
    let [hinst, name] = s.args::<2>()?;
    if hinst == 0 {
        s.missing("LoadBitmap", format!("system bitmap {name:#x}"));
        return Ok(ApiRet::Ret(0));
    }
    let id = crate::dll::kernel32::res_id(s, name)?;
    let Some((ptr, _)) = crate::dll::kernel32::find_resource(s, hinst, drydock_core::pe::ResId::Id(2), id) else {
        s.set_last_error(1814);
        return Ok(ApiRet::Ret(0));
    };
    let Some(d) = parse_bitmap_info(s, ptr, 0, 0)? else {
        s.missing("LoadBitmap", "this DIB format is not supported".into());
        return Ok(ApiRet::Ret(0));
    };
    match surface_from_dib(s, &d, ptr + d.packed_len)? {
        Some(sf) => Ok(ApiRet::Ret(new_bitmap(s, sf))),
        None => Ok(ApiRet::Ret(0)),
    }
}

// ---------------------------------------------------------------- DCs

fn create_compatible_dc(s: &mut Session) -> ApiResult {
    let [_hdc] = s.args::<1>()?;
    Ok(ApiRet::Ret(create_dc(s, DcTarget::Memory)))
}

fn delete_dc(s: &mut Session) -> ApiResult {
    let [hdc] = s.args::<1>()?;
    Ok(ApiRet::Ret(super::gdi32::release_dc_handle(s, hdc) as u32))
}

fn save_dc(s: &mut Session) -> ApiResult {
    let [hdc] = s.args::<1>()?;
    let Some(dc) = s.gdi.dc_mut(hdc) else {
        return Ok(ApiRet::Ret(0));
    };
    let mut snap = dc.clone();
    snap.saved.clear();
    dc.saved.push(snap);
    Ok(ApiRet::Ret(dc.saved.len() as u32))
}

fn restore_dc(s: &mut Session) -> ApiResult {
    let [hdc, level] = s.args::<2>()?;
    let Some(dc) = s.gdi.dc_mut(hdc) else {
        return Ok(ApiRet::Ret(0));
    };
    let n = dc.saved.len() as i32;
    let target = if (level as i32) < 0 {
        n + level as i32 + 1
    } else {
        level as i32
    };
    if target < 1 || target > n {
        return Ok(ApiRet::Ret(0));
    }
    let mut saved = std::mem::take(&mut dc.saved);
    let snap = saved[target as usize - 1].clone();
    saved.truncate(target as usize - 1);
    *dc = snap;
    dc.saved = saved;
    Ok(ApiRet::Ret(1))
}

fn set_stretch_blt_mode(s: &mut Session) -> ApiResult {
    let [hdc, mode] = s.args::<2>()?;
    match s.gdi.dc_mut(hdc) {
        Some(d) => Ok(ApiRet::Ret(std::mem::replace(&mut d.stretch_mode, mode))),
        None => Ok(ApiRet::Ret(0)),
    }
}

fn get_stretch_blt_mode(s: &mut Session) -> ApiResult {
    let [hdc] = s.args::<1>()?;
    Ok(ApiRet::Ret(s.gdi.dc(hdc).map(|d| d.stretch_mode).unwrap_or(0)))
}

fn get_object_type(s: &mut Session) -> ApiResult {
    let [h] = s.args::<1>()?;
    Ok(ApiRet::Ret(match s.gdi.objs.get(&h) {
        Some(GdiObj::Pen { .. }) => OBJ_PEN,
        Some(GdiObj::Brush { .. }) => OBJ_BRUSH,
        Some(GdiObj::Dc(d)) if d.target == DcTarget::Memory => OBJ_MEMDC,
        Some(GdiObj::Dc(_)) => OBJ_DC,
        Some(GdiObj::Palette) => OBJ_PAL,
        Some(GdiObj::Font(_)) => OBJ_FONT,
        Some(GdiObj::Bitmap { .. }) => OBJ_BITMAP,
        None => 0,
    }))
}

fn get_object_a(s: &mut Session) -> ApiResult {
    let [h, n, p] = s.args::<3>()?;
    let out: Vec<u8> = match s.gdi.objs.get(&h).cloned() {
        Some(GdiObj::Bitmap { surface }) => {
            let Some(sf) = s.display.surfaces.get(&surface).cloned() else {
                return Ok(ApiRet::Ret(0));
            };
            let dib = s.gdi.dibs.get(&h).cloned();
            let bpp = sf.fmt.bits();
            let mut b = Vec::with_capacity(84);
            b.extend(0u32.to_le_bytes());
            b.extend(sf.w.to_le_bytes());
            b.extend(sf.h.to_le_bytes());
            let wb = if dib.is_some() {
                sf.pitch
            } else {
                (sf.w * bpp).div_ceil(16) * 2
            };
            b.extend(wb.to_le_bytes());
            b.extend(1u16.to_le_bytes());
            b.extend((bpp as u16).to_le_bytes());
            b.extend((if dib.is_some() { sf.mem } else { 0 }).to_le_bytes());
            if let Some(hdr) = dib
                && n >= 84
            {
                // DIBSECTION: BITMAP, BITMAPINFOHEADER, dsBitfields, dshSection, dsOffset.
                b.extend_from_slice(&hdr[..40]);
                let masks = if hdr.len() >= 52 {
                    hdr[40..52].to_vec()
                } else {
                    vec![0; 12]
                };
                b.extend(masks);
                b.extend([0u8; 8]);
            }
            b
        }
        Some(GdiObj::Font(lf)) => {
            let mut b = vec![0u8; 60];
            b[0..4].copy_from_slice(&lf.height.to_le_bytes());
            b[4..8].copy_from_slice(&lf.width.to_le_bytes());
            b[16..20].copy_from_slice(&lf.weight.to_le_bytes());
            b[20] = lf.italic as u8;
            b[21] = lf.underline as u8;
            b[22] = lf.strikeout as u8;
            b[23] = lf.charset;
            b[26] = lf.quality;
            b[27] = lf.pitch_family;
            let face = crate::strings::encode_ansi(&lf.face);
            let k = face.len().min(31);
            b[28..28 + k].copy_from_slice(&face[..k]);
            b
        }
        Some(GdiObj::Brush { color, null }) => {
            let mut b = vec![0u8; 12];
            b[0..4].copy_from_slice(&(if null { 1u32 } else { 0 }).to_le_bytes());
            b[4..8].copy_from_slice(&color.to_le_bytes());
            b
        }
        Some(GdiObj::Pen { color, width, null }) => {
            let mut b = vec![0u8; 16];
            b[0..4].copy_from_slice(&(if null { 5u32 } else { 0 }).to_le_bytes());
            b[4..8].copy_from_slice(&width.to_le_bytes());
            b[12..16].copy_from_slice(&color.to_le_bytes());
            b
        }
        Some(GdiObj::Palette) => {
            let n = s.gdi.palettes.get(&h).map(|e| e.len()).unwrap_or(20) as u16;
            n.to_le_bytes().to_vec()
        }
        _ => return Ok(ApiRet::Ret(0)),
    };
    if p == 0 {
        return Ok(ApiRet::Ret(out.len() as u32));
    }
    let k = out.len().min(n as usize);
    s.mem.write(p, &out[..k])?;
    Ok(ApiRet::Ret(k as u32))
}

// ---------------------------------------------------------------- block transfers

/// Combine raw pixel values with a binary raster operation (source and destination).
fn rop_combine(rop: u32, src: u32, dst: u32) -> Option<u32> {
    Some(match rop {
        0x0088_00c6 => src & dst,    // SRCAND
        0x00ee_0086 => src | dst,    // SRCPAINT
        0x0066_0046 => src ^ dst,    // SRCINVERT
        0x0044_0328 => src & !dst,   // SRCERASE
        0x0033_0008 => !src,         // NOTSRCCOPY
        0x0011_00a6 => !(src | dst), // NOTSRCERASE
        0x00bb_0226 => !src | dst,   // MERGEPAINT
        _ => return None,
    })
}

/// Map a destination rectangle (in DC coordinates) to surface coordinates and clip it,
/// returning the clipped destination and the matching source rectangle.
fn clip_pair(dr: Rect, clip: &Rect, sr: Rect) -> Option<(Rect, Rect)> {
    let c = dr.intersect(clip);
    if c.is_empty() || dr.w() <= 0 || dr.h() <= 0 {
        return None;
    }
    let sx = |x: i32| sr.left + ((x - dr.left) as i64 * sr.w() as i64 / dr.w() as i64) as i32;
    let sy = |y: i32| sr.top + ((y - dr.top) as i64 * sr.h() as i64 / dr.h() as i64) as i32;
    Some((c, Rect::new(sx(c.left), sy(c.top), sx(c.right), sy(c.bottom))))
}

/// Fill or invert a destination rectangle (raster operations without a source).
fn pattern_op(s: &mut Session, hdc: u32, r: Rect, rop: u32) -> Result<bool, ApiError> {
    let Some((id, (ox, oy), clip)) = dc_target(s, hdc) else {
        return Ok(false);
    };
    let r = Rect::new(r.left + ox, r.top + oy, r.right + ox, r.bottom + oy).intersect(&clip);
    if r.is_empty() {
        return Ok(true);
    }
    let brush = s.gdi.dc(hdc).map(|d| d.brush).unwrap_or(0);
    let color = match rop {
        BLACKNESS => Some(0),
        WHITENESS => Some(0xffffff),
        PATCOPY => brush_color(s, brush),
        _ => None,
    };
    s.surface_cpu_access(id, true);
    let sf = s.display.surfaces[&id].clone();
    if let Some(c) = color {
        let raw = raw_color(s, id, c);
        crate::surface::PixelTarget::of(&sf).fill_raw(&mut s.mem, &r, raw)?;
        return Ok(true);
    }
    let mask = match rop {
        DSTINVERT => crate::surface::mask_bits(sf.fmt.bits()),
        PATINVERT => match brush_color(s, brush) {
            Some(c) => raw_color(s, id, c),
            None => return Ok(true),
        },
        _ => {
            s.missing("PatBlt", format!("raster operation {rop:#x}"));
            return Ok(false);
        }
    };
    let bpp = sf.fmt.bytes_pp();
    for y in r.top..r.bottom {
        let a = sf.row_addr(y as u32) + r.left as u32 * bpp;
        let mut row = s.mem.read_vec(a, r.w() as u32 * bpp)?;
        for px in row.chunks_mut(bpp as usize) {
            let mut v = [0u8; 4];
            v[..px.len()].copy_from_slice(px);
            let x = u32::from_le_bytes(v) ^ mask;
            px.copy_from_slice(&x.to_le_bytes()[..px.len()]);
        }
        s.mem.write(a, &row)?;
    }
    Ok(true)
}

/// Copy between DC surfaces with a raster operation. `dr`/`sr` are in DC coordinates;
/// negative extents mirror.
fn transfer(
    s: &mut Session,
    hdc: u32,
    dr: Rect,
    src: (u32, (i32, i32), Rect),
    sr: Rect,
    rop: u32,
) -> Result<bool, ApiError> {
    let Some((did, (ox, oy), dclip)) = dc_target(s, hdc) else {
        return Ok(false);
    };
    let (sid, (sox, soy), sclip) = src;
    let mirror_lr = (dr.w() < 0) != (sr.w() < 0);
    let mirror_ud = (dr.h() < 0) != (sr.h() < 0);
    let norm = |r: Rect| {
        Rect::new(
            r.left.min(r.right),
            r.top.min(r.bottom),
            r.left.max(r.right),
            r.top.max(r.bottom),
        )
    };
    let d = norm(dr);
    let d = Rect::new(d.left + ox, d.top + oy, d.right + ox, d.bottom + oy);
    let sr0 = norm(sr);
    let sr0 = Rect::new(sr0.left + sox, sr0.top + soy, sr0.right + sox, sr0.bottom + soy);
    // Source pixels outside the source bitmap are not read.
    let Some((sclipped, dsub)) = clip_pair(sr0, &sclip, d) else {
        return Ok(true);
    };
    let Some((dfinal, sfinal)) = clip_pair(dsub, &dclip, sclipped) else {
        return Ok(true);
    };
    if rop == SRCCOPY {
        let o = BltOpts {
            mirror_lr,
            mirror_ud,
            ..Default::default()
        };
        blit(s, did, dfinal, sid, sfinal, o)?;
        return Ok(true);
    }
    if rop_combine(rop, 0, 0).is_none() {
        s.missing("BitBlt", format!("raster operation {rop:#x}"));
        return Ok(false);
    }
    // Other operations: convert the source into the destination format, then combine.
    s.surface_cpu_access(sid, false);
    s.surface_cpu_access(did, true);
    let sp = s.display.surfaces[&sid].clone();
    let dp = s.display.surfaces[&did].clone();
    let spal = s.palette_of(sid);
    let (sb, db) = (sp.fmt.bytes_pp(), dp.fmt.bytes_pp());
    let (dw, dh) = (dfinal.w(), dfinal.h());
    for y in 0..dh {
        let mut sy = sfinal.top + (y as i64 * sfinal.h() as i64 / dh as i64) as i32;
        if mirror_ud {
            sy = sfinal.bottom - 1 - (sy - sfinal.top);
        }
        let srow = s.mem.read_vec(sp.row_addr(sy as u32), sp.pitch)?;
        let a = dp.row_addr((dfinal.top + y) as u32) + dfinal.left as u32 * db;
        let mut row = s.mem.read_vec(a, dw as u32 * db)?;
        for x in 0..dw {
            let mut sx = sfinal.left + (x as i64 * sfinal.w() as i64 / dw as i64) as i32;
            if mirror_lr {
                sx = sfinal.right - 1 - (sx - sfinal.left);
            }
            let so = (sx as u32 * sb) as usize;
            let argb = sp.fmt.decode(&srow[so..so + sb as usize], spal.as_ref());
            let raw_src = if dp.fmt == Fmt::P8 {
                raw_color(s, did, ((argb & 0xff) << 16) | (argb & 0xff00) | ((argb >> 16) & 0xff))
            } else {
                let mut e = [0u8; 4];
                dp.fmt.encode(argb, &mut e);
                u32::from_le_bytes(e)
            };
            let o = (x as u32 * db) as usize;
            let mut cur = [0u8; 4];
            cur[..db as usize].copy_from_slice(&row[o..o + db as usize]);
            let v = rop_combine(rop, raw_src, u32::from_le_bytes(cur)).unwrap();
            row[o..o + db as usize].copy_from_slice(&v.to_le_bytes()[..db as usize]);
        }
        s.mem.write(a, &row)?;
    }
    Ok(true)
}

fn needs_source(rop: u32) -> bool {
    !matches!(rop, BLACKNESS | WHITENESS | PATCOPY | DSTINVERT | PATINVERT)
}

fn bit_blt(s: &mut Session) -> ApiResult {
    let [hdc, x, y, w, h, src, sx, sy, rop] = s.args::<9>()?;
    let (x, y, w, h, sx, sy) = (x as i32, y as i32, w as i32, h as i32, sx as i32, sy as i32);
    let dr = Rect::new(x, y, x + w, y + h);
    if !needs_source(rop) {
        return Ok(ApiRet::Ret(pattern_op(s, hdc, dr, rop)? as u32));
    }
    let Some(st) = dc_target(s, src) else {
        return Ok(ApiRet::Ret(0));
    };
    let ok = transfer(s, hdc, dr, st, Rect::new(sx, sy, sx + w, sy + h), rop)?;
    Ok(ApiRet::Ret(ok as u32))
}

fn stretch_blt(s: &mut Session) -> ApiResult {
    let [hdc, x, y, w, h, src, sx, sy, sw, sh, rop] = s.args::<11>()?;
    let i = |v: u32| v as i32;
    let dr = Rect::new(i(x), i(y), i(x) + i(w), i(y) + i(h));
    if !needs_source(rop) {
        return Ok(ApiRet::Ret(pattern_op(s, hdc, dr, rop)? as u32));
    }
    let Some(st) = dc_target(s, src) else {
        return Ok(ApiRet::Ret(0));
    };
    let sr = Rect::new(i(sx), i(sy), i(sx) + i(sw), i(sy) + i(sh));
    Ok(ApiRet::Ret(transfer(s, hdc, dr, st, sr, rop)? as u32))
}

fn pat_blt(s: &mut Session) -> ApiResult {
    let [hdc, x, y, w, h, rop] = s.args::<6>()?;
    let (x, y, w, h) = (x as i32, y as i32, w as i32, h as i32);
    let r = Rect::new(x.min(x + w), y.min(y + h), x.max(x + w), y.max(y + h));
    Ok(ApiRet::Ret(pattern_op(s, hdc, r, rop)? as u32))
}

/// Draw a packed DIB through a temporary surface. Returns the scan lines drawn.
#[allow(clippy::too_many_arguments)]
fn draw_dib(
    s: &mut Session,
    hdc: u32,
    dr: Rect,
    sx: i32,
    sy: i32,
    sw: i32,
    sh: i32,
    bits: u32,
    pbmi: u32,
    usage: u32,
    rop: u32,
) -> Result<u32, ApiError> {
    let Some(d) = parse_bitmap_info(s, pbmi, usage, hdc)? else {
        s.missing("StretchDIBits", "this DIB format is not supported".into());
        return Ok(0);
    };
    let Some(tmp) = surface_from_dib(s, &d, bits)? else {
        return Ok(0);
    };
    // Source coordinates of bottom-up DIBs count from the bottom row.
    let top = if d.top_down { sy } else { d.h as i32 - sy - sh };
    let sr = Rect::new(sx, top, sx + sw, top + sh);
    let full = Rect::new(0, 0, d.w as i32, d.h as i32);
    let r = if needs_source(rop) {
        transfer(s, hdc, dr, (tmp, (0, 0), full), sr, rop)?
    } else {
        pattern_op(s, hdc, dr, rop)?
    };
    s.free_surface(tmp);
    Ok(if r { sh.unsigned_abs() } else { 0 })
}

fn stretch_di_bits(s: &mut Session) -> ApiResult {
    let [hdc, x, y, w, h, sx, sy, sw, sh, bits, pbmi, usage, rop] = s.args::<13>()?;
    let i = |v: u32| v as i32;
    let dr = Rect::new(i(x), i(y), i(x) + i(w), i(y) + i(h));
    let n = draw_dib(s, hdc, dr, i(sx), i(sy), i(sw), i(sh), bits, pbmi, usage, rop)?;
    Ok(ApiRet::Ret(n))
}

fn set_di_bits_to_device(s: &mut Session) -> ApiResult {
    let [hdc, x, y, w, h, sx, sy, start, lines, bits, pbmi, usage] = s.args::<12>()?;
    if start != 0 {
        s.missing("SetDIBitsToDevice", "banded transfers (uStartScan != 0)".into());
        return Ok(ApiRet::Ret(0));
    }
    let _ = lines;
    let i = |v: u32| v as i32;
    let dr = Rect::new(i(x), i(y), i(x) + i(w), i(y) + i(h));
    let n = draw_dib(s, hdc, dr, i(sx), i(sy), i(w), i(h), bits, pbmi, usage, SRCCOPY)?;
    Ok(ApiRet::Ret(n))
}

fn get_pixel(s: &mut Session) -> ApiResult {
    let [hdc, x, y] = s.args::<3>()?;
    let Some((id, (ox, oy), clip)) = dc_target(s, hdc) else {
        return Ok(ApiRet::Ret(0xffff_ffff));
    };
    let (px, py) = (x as i32 + ox, y as i32 + oy);
    if !clip.contains(px, py) {
        return Ok(ApiRet::Ret(0xffff_ffff)); // CLR_INVALID
    }
    s.surface_cpu_access(id, false);
    let sf = s.display.surfaces[&id].clone();
    let bpp = sf.fmt.bytes_pp();
    let raw = s.mem.read_vec(sf.row_addr(py as u32) + px as u32 * bpp, bpp)?;
    let pal = s.palette_of(id);
    let c = sf.fmt.decode(&raw, pal.as_ref());
    Ok(ApiRet::Ret(((c & 0xff) << 16) | (c & 0xff00) | ((c >> 16) & 0xff)))
}

fn set_pixel(s: &mut Session) -> ApiResult {
    let [hdc, x, y, c] = s.args::<4>()?;
    let Some((id, (ox, oy), clip)) = dc_target(s, hdc) else {
        return Ok(ApiRet::Ret(0xffff_ffff));
    };
    let r = Rect::new(x as i32 + ox, y as i32 + oy, x as i32 + ox + 1, y as i32 + oy + 1);
    if r.intersect(&clip).is_empty() {
        return Ok(ApiRet::Ret(0xffff_ffff));
    }
    let raw = raw_color(s, id, c & 0xffffff);
    s.surface_cpu_access(id, true);
    let t = crate::surface::PixelTarget::of(&s.display.surfaces[&id]);
    t.fill_raw(&mut s.mem, &r, raw)?;
    Ok(ApiRet::Ret(c & 0xffffff))
}

// ---------------------------------------------------------------- palettes

fn create_palette(s: &mut Session) -> ApiResult {
    let [p] = s.args::<1>()?;
    let n = s.mem.read_u16(p + 2)? as u32;
    let mut e = Vec::with_capacity(n as usize);
    for i in 0..n {
        let pe = s.mem.read_u32(p + 4 + 4 * i)?;
        e.push(((pe & 0xff) << 16) | (pe & 0xff00) | ((pe >> 16) & 0xff));
    }
    let h = s.gdi.add(GdiObj::Palette);
    s.gdi.palettes.insert(h, e);
    Ok(ApiRet::Ret(h))
}

fn select_palette(s: &mut Session) -> ApiResult {
    let [hdc, hpal, _bg] = s.args::<3>()?;
    if !matches!(s.gdi.objs.get(&hpal), Some(GdiObj::Palette)) {
        return Ok(ApiRet::Ret(0));
    }
    match s.gdi.dc_mut(hdc) {
        Some(d) => Ok(ApiRet::Ret(std::mem::replace(&mut d.palette, hpal))),
        None => Ok(ApiRet::Ret(0)),
    }
}

fn realize_palette(s: &mut Session) -> ApiResult {
    let [hdc] = s.args::<1>()?;
    // Only an 8-bit display has a hardware palette to realize into.
    let palettized = dc_format(s, hdc) == Some(Fmt::P8);
    if !palettized {
        return Ok(ApiRet::Ret(0));
    }
    let pal = s.gdi.dc(hdc).map(|d| d.palette).unwrap_or(0);
    Ok(ApiRet::Ret(
        s.gdi.palettes.get(&pal).map(|e| e.len() as u32).unwrap_or(0),
    ))
}

fn get_palette_entries(s: &mut Session) -> ApiResult {
    let [h, start, n, out] = s.args::<4>()?;
    let e = if h == STOCK_BASE + 15 {
        default_palette()
    } else {
        match s.gdi.palettes.get(&h) {
            Some(e) => e.clone(),
            None => return Ok(ApiRet::Ret(0)),
        }
    };
    if out == 0 {
        return Ok(ApiRet::Ret(e.len() as u32));
    }
    let mut k = 0;
    for (i, c) in e.iter().enumerate().skip(start as usize).take(n as usize) {
        let pe = ((c & 0xff) << 16) | (c & 0xff00) | ((c >> 16) & 0xff);
        s.mem.write_u32(out + 4 * (i as u32 - start), pe)?;
        k += 1;
    }
    Ok(ApiRet::Ret(k))
}

/// The 20 static colours of the default palette.
fn default_palette() -> Vec<u32> {
    vec![
        0x000000, 0x800000, 0x008000, 0x808000, 0x000080, 0x800080, 0x008080, 0xc0c0c0, 0xc0dcc0, 0xa6caf0, 0xfffbf0,
        0xa0a0a4, 0x808080, 0xff0000, 0x00ff00, 0xffff00, 0x0000ff, 0xff00ff, 0x00ffff, 0xffffff,
    ]
}

fn get_system_palette_entries(s: &mut Session) -> ApiResult {
    let [hdc, start, n, out] = s.args::<4>()?;
    // Displays without a palette have no system palette.
    if dc_format(s, hdc) != Some(Fmt::P8) {
        return Ok(ApiRet::Ret(0));
    }
    let front = s.front_surface();
    let pal = s.palette_of(front).unwrap_or([0; 256]);
    if out == 0 {
        return Ok(ApiRet::Ret(256));
    }
    let n = n.min(256u32.saturating_sub(start));
    for i in 0..n {
        let c = pal[(start + i) as usize];
        s.mem
            .write_u32(out + 4 * i, ((c & 0xff) << 16) | (c & 0xff00) | ((c >> 16) & 0xff))?;
    }
    Ok(ApiRet::Ret(n))
}

pub static GDI32_BITMAP_EXPORTS: &[ExportDef] = &[
    export!("BitBlt", 9, bit_blt),
    export!("CreateBitmap", 5, create_bitmap),
    export!("CreateBitmapIndirect", 1, create_bitmap_indirect),
    export!("CreateCompatibleBitmap", 3, create_compatible_bitmap),
    export!("CreateCompatibleDC", 1, create_compatible_dc),
    export!("CreateDIBSection", 6, create_dib_section),
    export!("CreatePalette", 1, create_palette),
    export!("DeleteDC", 1, delete_dc),
    export!("GetObjectA", 3, get_object_a),
    export!("GetObjectType", 1, get_object_type),
    export!("GetPaletteEntries", 4, get_palette_entries),
    export!("GetPixel", 3, get_pixel),
    export!("GetStretchBltMode", 1, get_stretch_blt_mode),
    export!("GetSystemPaletteEntries", 4, get_system_palette_entries),
    export!("PatBlt", 6, pat_blt),
    export!("RealizePalette", 1, realize_palette),
    export!("RestoreDC", 2, restore_dc),
    export!("SaveDC", 1, save_dc),
    export!("SelectPalette", 3, select_palette),
    export!("SetDIBitsToDevice", 12, set_di_bits_to_device),
    export!("SetPixel", 4, set_pixel),
    export!("SetStretchBltMode", 2, set_stretch_blt_mode),
    export!("StretchBlt", 11, stretch_blt),
    export!("StretchDIBits", 13, stretch_di_bits),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clipping_maps_source() {
        // Destination 0..100 from source 0..50 (2× stretch), clipped to 10..60.
        let (d, sr) = clip_pair(
            Rect::new(0, 0, 100, 100),
            &Rect::new(10, 0, 60, 100),
            Rect::new(0, 0, 50, 50),
        )
        .unwrap();
        assert_eq!(d, Rect::new(10, 0, 60, 100));
        assert_eq!(sr, Rect::new(5, 0, 30, 50));
    }

    #[test]
    fn raster_ops() {
        assert_eq!(rop_combine(0x0088_00c6, 0b1100, 0b1010), Some(0b1000));
        assert_eq!(rop_combine(0x0066_0046, 0b1100, 0b1010), Some(0b0110));
        assert_eq!(rop_combine(0x1234, 0, 0), None);
    }
}
