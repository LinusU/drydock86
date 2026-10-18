//! Pixel surfaces shared by DirectDraw, Direct3D and GDI.
//!
//! Every surface's pixels live in guest memory (system memory surfaces in the process
//! heap, "video memory" surfaces in a dedicated VRAM region), so `Lock` returns a real
//! guest pointer with a real pitch. When a GPU copy exists, ownership is tracked with
//! `cpu_dirty` (guest wrote; upload before GPU use) and `gpu_dirty` (GPU rendered; read
//! back before CPU access).

use crate::dll::user32::Rect;
use crate::session::Session;
use drydock_core::mem::{AddressSpace, MemResult};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Fmt {
    P8,
    Rgb565,
    Xrgb1555,
    Argb1555,
    Argb4444,
    Rgb888,
    Xrgb8888,
    Argb8888,
    /// 16-bit depth.
    D16,
    /// 32-bit container with 24-bit depth (and optional 8-bit stencil).
    D24S8,
    D32,
    Dxt1,
    Dxt3,
    Dxt5,
}

impl Fmt {
    pub fn bits(self) -> u32 {
        match self {
            Fmt::P8 => 8,
            Fmt::Rgb565 | Fmt::Xrgb1555 | Fmt::Argb1555 | Fmt::Argb4444 | Fmt::D16 => 16,
            Fmt::Rgb888 => 24,
            Fmt::Xrgb8888 | Fmt::Argb8888 | Fmt::D24S8 | Fmt::D32 => 32,
            Fmt::Dxt1 => 4,
            Fmt::Dxt3 | Fmt::Dxt5 => 8,
        }
    }
    pub fn bytes_pp(self) -> u32 {
        self.bits().div_ceil(8)
    }
    pub fn is_depth(self) -> bool {
        matches!(self, Fmt::D16 | Fmt::D24S8 | Fmt::D32)
    }
    pub fn is_compressed(self) -> bool {
        matches!(self, Fmt::Dxt1 | Fmt::Dxt3 | Fmt::Dxt5)
    }
    pub fn has_alpha(self) -> bool {
        matches!(
            self,
            Fmt::Argb1555 | Fmt::Argb4444 | Fmt::Argb8888 | Fmt::Dxt3 | Fmt::Dxt5
        )
    }
    pub fn pitch_for(self, w: u32) -> u32 {
        if self.is_compressed() {
            w.div_ceil(4) * if self == Fmt::Dxt1 { 8 } else { 16 }
        } else {
            (w * self.bytes_pp()).div_ceil(4) * 4
        }
    }
    pub fn rows_for(self, h: u32) -> u32 {
        if self.is_compressed() { h.div_ceil(4) } else { h }
    }

    /// DDPIXELFORMAT (32 bytes).
    pub fn ddpf(self) -> [u8; 32] {
        let (flags, fourcc, bits, r, g, b, a): (u32, u32, u32, u32, u32, u32, u32) = match self {
            Fmt::P8 => (0x20 | 0x40, 0, 8, 0, 0, 0, 0),
            Fmt::Rgb565 => (0x40, 0, 16, 0xf800, 0x07e0, 0x001f, 0),
            Fmt::Xrgb1555 => (0x40, 0, 16, 0x7c00, 0x03e0, 0x001f, 0),
            Fmt::Argb1555 => (0x41, 0, 16, 0x7c00, 0x03e0, 0x001f, 0x8000),
            Fmt::Argb4444 => (0x41, 0, 16, 0x0f00, 0x00f0, 0x000f, 0xf000),
            Fmt::Rgb888 => (0x40, 0, 24, 0xff0000, 0xff00, 0xff, 0),
            Fmt::Xrgb8888 => (0x40, 0, 32, 0xff0000, 0xff00, 0xff, 0),
            Fmt::Argb8888 => (0x41, 0, 32, 0xff0000, 0xff00, 0xff, 0xff00_0000),
            // DDPF_ZBUFFER: dwZBufferBitDepth, dwStencilBitDepth, dwZBitMask, dwStencilBitMask
            Fmt::D16 => (0x400, 0, 16, 0, 0xffff, 0, 0),
            Fmt::D24S8 => (0x400 | 0x4000, 0, 32, 8, 0xffff_ff00, 0xff, 0),
            Fmt::D32 => (0x400, 0, 32, 0, 0xffff_ffff, 0, 0),
            Fmt::Dxt1 => (0x4, u32::from_le_bytes(*b"DXT1"), 0, 0, 0, 0, 0),
            Fmt::Dxt3 => (0x4, u32::from_le_bytes(*b"DXT3"), 0, 0, 0, 0, 0),
            Fmt::Dxt5 => (0x4, u32::from_le_bytes(*b"DXT5"), 0, 0, 0, 0, 0),
        };
        let mut o = [0u8; 32];
        for (i, v) in [32u32, flags, fourcc, bits, r, g, b, a].iter().enumerate() {
            o[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
        }
        o
    }

    pub fn from_ddpf(p: &[u8]) -> Option<Fmt> {
        let g = |o: usize| u32::from_le_bytes(p[o..o + 4].try_into().unwrap());
        let (flags, fourcc, bits, r, gm, b, a) = (g(4), g(8), g(12), g(16), g(20), g(24), g(28));
        if flags & 0x4 != 0 {
            return match &fourcc.to_le_bytes() {
                b"DXT1" => Some(Fmt::Dxt1),
                b"DXT2" | b"DXT3" => Some(Fmt::Dxt3),
                b"DXT4" | b"DXT5" => Some(Fmt::Dxt5),
                _ => None,
            };
        }
        if flags & 0x400 != 0 {
            return match (bits, r) {
                (16, _) => Some(Fmt::D16),
                (32, 8) => Some(Fmt::D24S8),
                (32, _) if gm == 0xffff_ff00 => Some(Fmt::D24S8),
                (32, _) => Some(Fmt::D32),
                (24, _) => Some(Fmt::D24S8),
                _ => None,
            };
        }
        if flags & 0x20 != 0 {
            return Some(Fmt::P8);
        }
        let alpha = flags & 1 != 0;
        match (bits, r, gm, b) {
            (16, 0xf800, 0x07e0, 0x001f) => Some(Fmt::Rgb565),
            (16, 0x7c00, 0x03e0, 0x001f) if alpha && a == 0x8000 => Some(Fmt::Argb1555),
            (16, 0x7c00, 0x03e0, 0x001f) => Some(Fmt::Xrgb1555),
            (16, 0x0f00, 0x00f0, 0x000f) => Some(Fmt::Argb4444),
            (24, 0xff0000, 0xff00, 0xff) => Some(Fmt::Rgb888),
            (32, 0xff0000, 0xff00, 0xff) if alpha && a == 0xff00_0000 => Some(Fmt::Argb8888),
            (32, 0xff0000, 0xff00, 0xff) => Some(Fmt::Xrgb8888),
            _ => None,
        }
    }

    /// Decode one pixel to 0xAARRGGBB.
    pub fn decode(self, px: &[u8], pal: Option<&[u32; 256]>) -> u32 {
        let u16v = || u16::from_le_bytes([px[0], px[1]]) as u32;
        let e5 = |v: u32| (v << 3) | (v >> 2);
        let e6 = |v: u32| (v << 2) | (v >> 4);
        let e4 = |v: u32| v * 17;
        match self {
            Fmt::P8 => pal.map(|p| p[px[0] as usize] | 0xff00_0000).unwrap_or(0xff00_0000),
            Fmt::Rgb565 => {
                let v = u16v();
                0xff00_0000 | e5(v >> 11) << 16 | e6((v >> 5) & 63) << 8 | e5(v & 31)
            }
            Fmt::Xrgb1555 | Fmt::Argb1555 => {
                let v = u16v();
                let a = if self == Fmt::Argb1555 && v & 0x8000 == 0 {
                    0
                } else {
                    0xff
                };
                a << 24 | e5((v >> 10) & 31) << 16 | e5((v >> 5) & 31) << 8 | e5(v & 31)
            }
            Fmt::Argb4444 => {
                let v = u16v();
                e4(v >> 12) << 24 | e4((v >> 8) & 15) << 16 | e4((v >> 4) & 15) << 8 | e4(v & 15)
            }
            Fmt::Rgb888 => 0xff00_0000 | (px[2] as u32) << 16 | (px[1] as u32) << 8 | px[0] as u32,
            Fmt::Xrgb8888 => 0xff00_0000 | u32::from_le_bytes([px[0], px[1], px[2], 0]),
            Fmt::Argb8888 => u32::from_le_bytes([px[0], px[1], px[2], px[3]]),
            _ => 0,
        }
    }

    /// Encode 0xAARRGGBB into this format (not for P8/depth/compressed).
    pub fn encode(self, c: u32, out: &mut [u8]) {
        let (a, r, g, b) = (c >> 24, (c >> 16) & 0xff, (c >> 8) & 0xff, c & 0xff);
        match self {
            Fmt::Rgb565 => {
                let v = ((r >> 3) << 11 | (g >> 2) << 5 | b >> 3) as u16;
                out[..2].copy_from_slice(&v.to_le_bytes());
            }
            Fmt::Xrgb1555 | Fmt::Argb1555 => {
                let av = if self == Fmt::Argb1555 && a < 128 { 0 } else { 0x8000 };
                let v = (av | (r >> 3) << 10 | (g >> 3) << 5 | b >> 3) as u16;
                out[..2].copy_from_slice(&v.to_le_bytes());
            }
            Fmt::Argb4444 => {
                let v = ((a >> 4) << 12 | (r >> 4) << 8 | (g >> 4) << 4 | b >> 4) as u16;
                out[..2].copy_from_slice(&v.to_le_bytes());
            }
            Fmt::Rgb888 => {
                out[0] = b as u8;
                out[1] = g as u8;
                out[2] = r as u8;
            }
            Fmt::Xrgb8888 | Fmt::Argb8888 => out[..4].copy_from_slice(&c.to_le_bytes()),
            _ => {}
        }
    }

    /// Convert a 0x00RRGGBB colour to the raw pixel value of this format (for fills and
    /// colour keys).
    pub fn raw_from_rgb(self, c: u32) -> u32 {
        let mut b = [0u8; 4];
        self.encode(c | 0xff00_0000, &mut b);
        u32::from_le_bytes(b) & mask_bits(self.bits())
    }
}

pub fn mask_bits(bits: u32) -> u32 {
    if bits >= 32 { u32::MAX } else { (1u32 << bits) - 1 }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct ColorKey {
    pub low: u32,
    pub high: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Surface {
    pub id: u32,
    pub w: u32,
    pub h: u32,
    pub fmt: Fmt,
    pub pitch: u32,
    pub mem: u32,
    pub size: u32,
    pub vram: bool,
    pub caps: u32,
    pub caps2: u32,
    pub palette: u32,
    pub ckey_src: Option<ColorKey>,
    pub ckey_dst: Option<ColorKey>,
    /// GPU copy is older than guest memory.
    pub cpu_dirty: bool,
    /// Guest memory is older than the GPU copy.
    pub gpu_dirty: bool,
    pub locks: u32,
    pub dcs: u32,
    /// Bumped whenever the CPU-side contents change (for texture caches).
    pub version: u64,
    /// Rows are stored bottom row first (GDI DIB sections with a positive height).
    #[serde(default)]
    pub bottom_up: bool,
}

impl Surface {
    /// Guest address of row `y` (0 = top row).
    pub fn row_addr(&self, y: u32) -> u32 {
        if self.bottom_up {
            self.mem + (self.h - 1 - y) * self.pitch
        } else {
            self.mem + y * self.pitch
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Palette {
    pub id: u32,
    /// 0x00RRGGBB entries.
    pub entries: Vec<u32>,
    pub flags: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Display {
    pub surfaces: BTreeMap<u32, Surface>,
    pub palettes: BTreeMap<u32, Palette>,
    pub next_id: u32,
    /// Heap handle used for video-memory surfaces.
    pub vram_heap: u32,
    pub vram_size: u32,
    /// Surface currently scanned out (primary front buffer, or the desktop).
    pub front: u32,
    /// Desktop framebuffer used when no exclusive-mode primary exists.
    pub desktop: u32,
}

pub const VRAM_SIZE: u32 = 32 << 20;

impl Session {
    pub fn alloc_surface(&mut self, w: u32, h: u32, fmt: Fmt, vram: bool, caps: u32) -> Result<u32, String> {
        if self.display.vram_heap == 0 {
            self.display.vram_heap = self
                .heaps
                .create(&mut self.mem, 0, VRAM_SIZE, VRAM_SIZE)
                .map_err(|e| e.to_string())?;
            self.display.vram_size = VRAM_SIZE;
            if self.display.next_id == 0 {
                self.display.next_id = 1;
            }
        }
        let pitch = fmt.pitch_for(w.max(1));
        let size = (pitch * fmt.rows_for(h.max(1))).max(4);
        let heap = if vram {
            self.display.vram_heap
        } else {
            self.heaps.process_heap
        };
        let mem = self.heaps.alloc(&mut self.mem, heap, size, true).ok_or_else(|| {
            if vram {
                "out of video memory".to_string()
            } else {
                "out of memory".to_string()
            }
        })?;
        let id = self.display.next_id.max(1);
        self.display.next_id = id + 1;
        self.display.surfaces.insert(
            id,
            Surface {
                id,
                w,
                h,
                fmt,
                pitch,
                mem,
                size,
                vram,
                caps,
                caps2: 0,
                palette: 0,
                ckey_src: None,
                ckey_dst: None,
                cpu_dirty: true,
                gpu_dirty: false,
                locks: 0,
                dcs: 0,
                version: 0,
                bottom_up: false,
            },
        );
        Ok(id)
    }

    pub fn free_surface(&mut self, id: u32) {
        if let Some(sf) = self.display.surfaces.remove(&id) {
            let heap = if sf.vram {
                self.display.vram_heap
            } else {
                self.heaps.process_heap
            };
            self.heaps.free(heap, sf.mem);
            crate::com::d3d::surface_freed(self, id);
        }
    }

    pub fn vram_free(&self) -> u32 {
        let used: u64 = self
            .heaps
            .heaps
            .get(&self.display.vram_heap)
            .map(|h| h.total_allocated())
            .unwrap_or(0);
        self.display.vram_size.saturating_sub(used as u32)
    }

    /// Make guest memory authoritative before the CPU reads or writes a surface.
    pub fn surface_cpu_access(&mut self, id: u32, write: bool) {
        let dirty = self.display.surfaces.get(&id).map(|s| s.gpu_dirty).unwrap_or(false);
        if dirty {
            crate::com::d3d::readback(self, id);
            if let Some(sf) = self.display.surfaces.get_mut(&id) {
                sf.gpu_dirty = false;
            }
        }
        if write && let Some(sf) = self.display.surfaces.get_mut(&id) {
            sf.cpu_dirty = true;
            sf.version += 1;
        }
    }

    pub fn palette_of(&self, id: u32) -> Option<[u32; 256]> {
        let sf = self.display.surfaces.get(&id)?;
        let p = self.display.palettes.get(&sf.palette)?;
        let mut out = [0u32; 256];
        for (i, e) in p.entries.iter().take(256).enumerate() {
            out[i] = *e;
        }
        Some(out)
    }

    /// Decode a surface (or a sub-rectangle) to 0xAARRGGBB pixels.
    pub fn surface_rgba(&mut self, id: u32) -> Option<(u32, u32, Vec<u32>)> {
        self.surface_cpu_access(id, false);
        let sf = self.display.surfaces.get(&id)?.clone();
        if sf.fmt.is_compressed() || sf.fmt.is_depth() {
            return None;
        }
        let pal = self.palette_of(id);
        let data = self.mem.slice(sf.mem, sf.pitch * sf.h).ok()?;
        let bpp = sf.fmt.bytes_pp() as usize;
        let mut out = Vec::with_capacity((sf.w * sf.h) as usize);
        for y in 0..sf.h as usize {
            let row = &data[(sf.row_addr(y as u32) - sf.mem) as usize..];
            for x in 0..sf.w as usize {
                out.push(sf.fmt.decode(&row[x * bpp..x * bpp + bpp], pal.as_ref()));
            }
        }
        Some((sf.w, sf.h, out))
    }

    /// The surface currently presented to the (virtual) monitor.
    pub fn front_surface(&mut self) -> u32 {
        if self.display.front != 0 && self.display.surfaces.contains_key(&self.display.front) {
            return self.display.front;
        }
        self.desktop_surface()
    }

    pub fn desktop_surface(&mut self) -> u32 {
        let m = self.user.mode;
        if let Some(d) = self.display.surfaces.get(&self.display.desktop)
            && d.w == m.width
            && d.h == m.height
            && d.fmt.bits() == m.bpp
        {
            return d.id;
        }
        if self.display.desktop != 0 {
            let old = self.display.desktop;
            self.free_surface(old);
        }
        let fmt = match m.bpp {
            8 => Fmt::P8,
            16 => Fmt::Rgb565,
            24 => Fmt::Rgb888,
            _ => Fmt::Xrgb8888,
        };
        let id = self.alloc_surface(m.width, m.height, fmt, true, 0x200).unwrap_or(0);
        self.display.desktop = id;
        id
    }
}

/// A CPU-side view of a surface region for blits and GDI drawing.
#[derive(Debug, Clone, Copy)]
pub struct PixelTarget {
    pub mem: u32,
    pub w: u32,
    pub h: u32,
    pub pitch: u32,
    pub fmt: Fmt,
    pub bottom_up: bool,
}

impl PixelTarget {
    pub fn of(sf: &Surface) -> PixelTarget {
        PixelTarget {
            mem: sf.mem,
            w: sf.w,
            h: sf.h,
            pitch: sf.pitch,
            fmt: sf.fmt,
            bottom_up: sf.bottom_up,
        }
    }

    /// Fill a rectangle with a raw pixel value.
    pub fn fill_raw(&self, m: &mut AddressSpace, r: &Rect, raw: u32) -> MemResult<()> {
        let r = r.intersect(&Rect::new(0, 0, self.w as i32, self.h as i32));
        if r.is_empty() {
            return Ok(());
        }
        let bpp = self.fmt.bytes_pp() as usize;
        let bytes = raw.to_le_bytes();
        let row_len = r.w() as usize * bpp;
        let mut row = vec![0u8; row_len];
        for x in 0..r.w() as usize {
            row[x * bpp..x * bpp + bpp].copy_from_slice(&bytes[..bpp]);
        }
        for y in r.top..r.bottom {
            let ry = if self.bottom_up {
                self.h - 1 - y as u32
            } else {
                y as u32
            };
            let a = self.mem + ry * self.pitch + r.left as u32 * bpp as u32;
            m.write(a, &row)?;
        }
        Ok(())
    }
}
