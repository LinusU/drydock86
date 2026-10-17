//! Direct3D 7 immediate mode on the wgpu fixed-function renderer.
//!
//! Surfaces stay authoritative in guest memory until the GPU renders into them; then
//! `gpu_dirty` marks the GPU copy as newer and any CPU access (Lock, GetDC, blits,
//! screenshots) reads it back first. Textures are uploaded when their guest-memory
//! version changes.

use super::*;
use crate::dll::user32::Rect;
use crate::surface::Fmt;
use crate::{export, iunknown, unimpl_method};
use drydock_gfx as gfx;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const D3D_OK: u32 = 0;
pub const fn d3derr(code: u32) -> u32 {
    0x8876_0000 | code
}
pub const D3DERR_INVALIDPRIMITIVETYPE: u32 = d3derr(721);
pub const D3DERR_INVALIDVERTEXTYPE: u32 = d3derr(722);
pub const D3DERR_SCENE_IN_SCENE: u32 = d3derr(732);
pub const D3DERR_SCENE_NOT_IN_SCENE: u32 = d3derr(733);
pub const D3DERR_INVALIDCALL: u32 = E_INVALIDARG;
pub const D3DERR_VERTEXBUFFERLOCKED: u32 = d3derr(2097);
pub const D3DERR_LIGHT_SET_FAILED: u32 = d3derr(750);
pub const D3DERR_LIGHTNOTENABLED: u32 = d3derr(2133);
pub const D3DERR_INVALID_DEVICE: u32 = d3derr(705);

pub const IID_RGB_DEVICE: Guid = guid("a4665c60-2673-11cf-a31a-00aa00b93356");
pub const IID_HAL_DEVICE: Guid = guid("84e63de0-46aa-11cf-816f-0000c020156e");
pub const IID_TNL_HAL_DEVICE: Guid = guid("f5049e78-4861-11d2-a407-00a0c90629a8");

// Render state indices.
const RS_ZENABLE: usize = 7;
const RS_ZWRITEENABLE: usize = 14;
const RS_ALPHATESTENABLE: usize = 15;
const RS_SRCBLEND: usize = 19;
const RS_DESTBLEND: usize = 20;
const RS_CULLMODE: usize = 22;
const RS_ZFUNC: usize = 23;
const RS_ALPHAREF: usize = 24;
const RS_ALPHAFUNC: usize = 25;
const RS_ALPHABLENDENABLE: usize = 27;
const RS_FOGENABLE: usize = 28;
const RS_SPECULARENABLE: usize = 29;
const RS_FOGCOLOR: usize = 34;
const RS_FOGTABLEMODE: usize = 35;
const RS_FOGSTART: usize = 36;
const RS_FOGEND: usize = 37;
const RS_FOGDENSITY: usize = 38;
const RS_COLORKEYENABLE: usize = 41;
const RS_ZBIAS: usize = 47;
const RS_TEXTUREFACTOR: usize = 60;
const RS_LIGHTING: usize = 137;
const RS_AMBIENT: usize = 139;
const RS_FOGVERTEXMODE: usize = 140;
const RS_COLORVERTEX: usize = 141;
const RS_LOCALVIEWER: usize = 142;
const RS_NORMALIZENORMALS: usize = 143;
const RS_DIFFUSEMATERIALSOURCE: usize = 145;
const RS_SPECULARMATERIALSOURCE: usize = 146;
const RS_AMBIENTMATERIALSOURCE: usize = 147;
const RS_EMISSIVEMATERIALSOURCE: usize = 148;

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
pub struct Light7 {
    pub kind: u32,
    pub diffuse: [f32; 4],
    pub specular: [f32; 4],
    pub ambient: [f32; 4],
    pub position: [f32; 3],
    pub direction: [f32; 3],
    pub range: f32,
    pub falloff: f32,
    pub att: [f32; 3],
    pub theta: f32,
    pub phi: f32,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
pub struct Material7 {
    pub diffuse: [f32; 4],
    pub ambient: [f32; 4],
    pub specular: [f32; 4],
    pub emissive: [f32; 4],
    pub power: f32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct Viewport7 {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
    pub min_z: f32,
    pub max_z: f32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StateBlock {
    pub rs: BTreeMap<usize, u32>,
    pub tss: BTreeMap<(usize, usize), u32>,
    pub textures: BTreeMap<usize, u32>,
    pub transforms: BTreeMap<u32, [f32; 16]>,
    pub viewport: Option<Viewport7>,
    pub material: Option<Material7>,
    pub lights: BTreeMap<u32, Light7>,
    pub light_enable: BTreeMap<u32, bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Device {
    pub ddraw: u32,
    pub target: u32,
    pub kind: Guid,
    pub rs: Vec<u32>,
    pub tss: Vec<[u32; 32]>,
    pub textures: [u32; 8],
    pub transforms: BTreeMap<u32, [f32; 16]>,
    pub viewport: Viewport7,
    pub material: Material7,
    pub lights: BTreeMap<u32, Light7>,
    pub light_enable: BTreeMap<u32, bool>,
    pub in_scene: bool,
    pub clip_status: [u8; 24],
    pub blocks: BTreeMap<u32, StateBlock>,
    pub recording: Option<StateBlock>,
    pub next_block: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VertexBuffer {
    pub mem: u32,
    pub size: u32,
    pub fvf: u32,
    pub count: u32,
    pub caps: u32,
    pub locks: u32,
}

const IDENTITY: [f32; 16] = [1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1.];

fn default_rs(zenable: bool) -> Vec<u32> {
    let mut rs = vec![0u32; 256];
    let f = |x: f32| x.to_bits();
    rs[RS_ZENABLE] = zenable as u32;
    rs[8] = 3; // FILLMODE solid
    rs[9] = 2; // SHADEMODE gouraud
    rs[RS_ZWRITEENABLE] = 1;
    rs[16] = 1; // LASTPIXEL
    rs[RS_SRCBLEND] = 2;
    rs[RS_DESTBLEND] = 1;
    rs[RS_CULLMODE] = 3;
    rs[RS_ZFUNC] = 4;
    rs[RS_ALPHAFUNC] = 8;
    rs[RS_FOGSTART] = f(0.0);
    rs[RS_FOGEND] = f(1.0);
    rs[RS_FOGDENSITY] = f(1.0);
    rs[56] = 8; // STENCILFUNC always
    rs[53] = 1;
    rs[54] = 1;
    rs[55] = 1;
    rs[58] = 0xffff_ffff;
    rs[59] = 0xffff_ffff;
    rs[RS_TEXTUREFACTOR] = 0xffff_ffff;
    rs[136] = 1; // CLIPPING
    rs[RS_LIGHTING] = 1;
    rs[RS_COLORVERTEX] = 1;
    rs[RS_LOCALVIEWER] = 1;
    rs[RS_DIFFUSEMATERIALSOURCE] = 1;
    rs[RS_SPECULARMATERIALSOURCE] = 2;
    rs
}

fn default_tss() -> Vec<[u32; 32]> {
    let mut v = vec![[0u32; 32]; 8];
    for (i, st) in v.iter_mut().enumerate() {
        st[1] = if i == 0 { 4 } else { 1 }; // COLOROP
        st[2] = 2; // COLORARG1 texture
        st[3] = 1; // COLORARG2 current
        st[4] = if i == 0 { 2 } else { 1 }; // ALPHAOP
        st[5] = 2;
        st[6] = 1;
        st[11] = i as u32; // TEXCOORDINDEX
        st[12] = 1;
        st[13] = 1;
        st[14] = 1;
        st[16] = 1; // MAGFILTER point
        st[17] = 1; // MINFILTER point
        st[18] = 1; // MIPFILTER none
        st[21] = 1;
    }
    v
}

// ---------------------------------------------------------------- GPU glue

impl Session {
    /// The renderer, created on first use. Failure is reported, never hidden.
    pub fn renderer(&mut self) -> Result<&mut gfx::Renderer, String> {
        if self.host.renderer.is_none() {
            if let Some(e) = &self.host.gpu_error {
                return Err(e.clone());
            }
            match gfx::Gpu::new(&self.host.gpu_options) {
                Ok(g) => {
                    self.event(
                        crate::diag::Severity::Info,
                        "gpu",
                        format!("adapter {:?}", g.report),
                    );
                    self.host.renderer = Some(gfx::Renderer::new(g));
                }
                Err(e) => {
                    self.host.gpu_error = Some(e.clone());
                    self.event(crate::diag::Severity::Error, "gpu", e.clone());
                    return Err(e);
                }
            }
        }
        Ok(self.host.renderer.as_mut().unwrap())
    }
}

fn decode_rgba(s: &mut Session, id: u32) -> Option<(u32, u32, Vec<u8>)> {
    let sf = s.display.surfaces.get(&id)?.clone();
    if sf.fmt.is_compressed() {
        let data = s.mem.read_vec(sf.mem, sf.size).ok()?;
        return Some((sf.w, sf.h, crate::surface_dxt::decode(sf.fmt, sf.w, sf.h, &data)));
    }
    let (w, h, px) = s.surface_rgba(id)?;
    let key = sf.ckey_src;
    let mut out = Vec::with_capacity(px.len() * 4);
    let bpp = sf.fmt.bytes_pp() as usize;
    let raw = s.mem.slice(sf.mem, sf.pitch * sf.h).ok()?.to_vec();
    for y in 0..h as usize {
        for x in 0..w as usize {
            let p = px[y * w as usize + x];
            let mut a = (p >> 24) as u8;
            if let Some(k) = key {
                let o = y * sf.pitch as usize + x * bpp;
                let mut b = [0u8; 4];
                b[..bpp].copy_from_slice(&raw[o..o + bpp]);
                let v = u32::from_le_bytes(b);
                if v >= k.low && v <= k.high {
                    a = 0;
                }
            }
            out.extend([(p >> 16) as u8, (p >> 8) as u8, p as u8, a]);
        }
    }
    Some((w, h, out))
}

/// Upload a render target's guest contents if they are newer than the GPU copy.
fn sync_target(s: &mut Session, id: u32) -> Result<(), String> {
    let Some(sf) = s.display.surfaces.get(&id).cloned() else {
        return Ok(());
    };
    let fmt = if sf.fmt.is_depth() { gfx::TexFormat::Depth } else { gfx::TexFormat::Color };
    let r = s.renderer()?;
    let existed = r.has_texture(id);
    r.ensure_texture(id, sf.w, sf.h, 1, fmt);
    if sf.cpu_dirty || !existed {
        if fmt == gfx::TexFormat::Depth {
            let z = depth_value_of(s, id);
            clear_depth_full(s, id, z)?;
        } else if let Some((w, h, px)) = decode_rgba(s, id) {
            s.renderer()?.upload(id, 0, w, h, &px);
        }
        if let Some(p) = s.display.surfaces.get_mut(&id) {
            p.cpu_dirty = false;
        }
        s.host.gpu_versions.insert(id, sf.version);
    }
    Ok(())
}

/// The value a CPU-side depth buffer holds (uniform contents are representable).
fn depth_value_of(s: &mut Session, id: u32) -> f32 {
    let sf = s.display.surfaces[&id].clone();
    let raw = s.mem.read_vec(sf.mem, sf.fmt.bytes_pp().min(sf.size)).unwrap_or_default();
    let mut b = [0u8; 4];
    b[..raw.len()].copy_from_slice(&raw);
    let v = u32::from_le_bytes(b);
    let z = match sf.fmt {
        Fmt::D16 => (v & 0xffff) as f32 / 65535.0,
        Fmt::D24S8 => (v >> 8) as f32 / 16_777_215.0,
        _ => v as f32 / u32::MAX as f32,
    };
    // Non-uniform CPU-written depth contents cannot be uploaded; report it.
    let all = s.mem.read_vec(sf.mem, sf.pitch * sf.h).unwrap_or_default();
    if all.chunks(sf.fmt.bytes_pp() as usize).any(|c| c != &raw[..]) {
        s.missing("depth", "guest-written non-uniform depth buffer contents are not uploaded to the GPU".into());
    }
    z
}

fn clear_depth_full(s: &mut Session, id: u32, z: f32) -> Result<(), String> {
    let sf = s.display.surfaces[&id].clone();
    // Clearing a depth target needs a color attachment of the same size.
    let scratch = 0x8000_0000 | id;
    let r = s.renderer()?;
    r.ensure_texture(scratch, sf.w, sf.h, 1, gfx::TexFormat::Color);
    r.clear(scratch, Some(id), [0, 0, sf.w, sf.h], None, Some(z));
    Ok(())
}

pub fn readback(s: &mut Session, id: u32) {
    let Some(sf) = s.display.surfaces.get(&id).cloned() else {
        return;
    };
    let Ok(r) = s.renderer() else {
        return;
    };
    let Some((w, h, data)) = r.readback(id) else {
        return;
    };
    let bpp = sf.fmt.bytes_pp() as usize;
    let mut row = vec![0u8; sf.pitch as usize];
    for y in 0..h.min(sf.h) as usize {
        for x in 0..w.min(sf.w) as usize {
            let o = (y * w as usize + x) * 4;
            let px = &data[o..o + 4];
            let mut out = [0u8; 4];
            if sf.fmt.is_depth() {
                let z = f32::from_le_bytes(px.try_into().unwrap()).clamp(0.0, 1.0);
                let v = match sf.fmt {
                    Fmt::D16 => (z * 65535.0).round() as u32,
                    Fmt::D24S8 => ((z * 16_777_215.0).round() as u32) << 8,
                    _ => (z as f64 * u32::MAX as f64) as u32,
                };
                out = v.to_le_bytes();
            } else {
                let argb = (px[3] as u32) << 24 | (px[0] as u32) << 16 | (px[1] as u32) << 8 | px[2] as u32;
                sf.fmt.encode(argb, &mut out);
            }
            row[x * bpp..x * bpp + bpp].copy_from_slice(&out[..bpp]);
        }
        let _ = s.mem.write_force(sf.mem + y as u32 * sf.pitch, &row[..sf.w as usize * bpp]);
    }
    // The readback itself is not a guest write: contents now match the GPU copy.
    if let Some(p) = s.display.surfaces.get_mut(&id) {
        p.cpu_dirty = false;
    }
}

pub fn surface_freed(s: &mut Session, id: u32) {
    if let Some(r) = s.host.renderer.as_mut() {
        r.drop_texture(id);
        r.drop_texture(0x8000_0000 | id);
    }
    s.host.gpu_versions.remove(&id);
}

pub fn flush(s: &mut Session) {
    if let Some(r) = s.host.renderer.as_mut() {
        r.flush();
        let errs = r.gpu.take_errors();
        if let Some(e) = errs.first() {
            s.halt("gpu", format!("renderer validation error: {e}"), None);
        }
    }
}

pub fn depth_fill(s: &mut Session, id: u32, r: &Rect, depth: u32) {
    let gpu = s.display.surfaces.get(&id).map(|x| x.gpu_dirty).unwrap_or(false);
    if gpu || s.host.renderer.as_ref().is_some_and(|rr| rr.has_texture(id)) {
        let sf = s.display.surfaces[&id].clone();
        let z = match sf.fmt {
            Fmt::D16 => (depth & 0xffff) as f32 / 65535.0,
            Fmt::D24S8 => (depth >> 8) as f32 / 16_777_215.0,
            _ => depth as f32 / u32::MAX as f32,
        };
        let scratch = 0x8000_0000 | id;
        if let Ok(rr) = s.renderer() {
            rr.ensure_texture(scratch, sf.w, sf.h, 1, gfx::TexFormat::Color);
            rr.clear(scratch, Some(id), [r.left as u32, r.top as u32, r.w() as u32, r.h() as u32], None, Some(z));
        }
        if let Some(p) = s.display.surfaces.get_mut(&id) {
            p.gpu_dirty = true;
            p.cpu_dirty = false;
        }
        return;
    }
    s.surface_cpu_access(id, true);
    let t = crate::surface::PixelTarget::of(&s.display.surfaces[&id]);
    let _ = t.fill_raw(&mut s.mem, r, depth & crate::surface::mask_bits(t.fmt.bits()));
}

/// GPU path for blits onto GPU-owned surfaces (avoids read-back/upload round trips).
pub fn gpu_blit(s: &mut Session, dst: u32, dr: Rect, src: u32, sr: Rect, key: Option<crate::surface::ColorKey>) -> bool {
    let dst_gpu = s.display.surfaces.get(&dst).map(|x| x.gpu_dirty).unwrap_or(false);
    if !dst_gpu {
        return false;
    }
    let Some(sp) = s.display.surfaces.get(&src).cloned() else {
        return false;
    };
    if sp.fmt.is_depth() || s.display.surfaces[&dst].fmt.is_depth() {
        return false;
    }
    // Source: its own GPU copy if newer, otherwise upload its guest contents.
    if !sp.gpu_dirty {
        let stamp = s.host.gpu_versions.get(&src).copied();
        let need = stamp != Some(sp.version) || !s.host.renderer.as_ref().is_some_and(|r| r.has_texture(src));
        if need {
            let Some((w, h, mut px)) = decode_rgba(s, src) else {
                return false;
            };
            if key.is_none() {
                for a in px.iter_mut().skip(3).step_by(4) {
                    *a = 255;
                }
            }
            let Ok(r) = s.renderer() else {
                return false;
            };
            r.ensure_texture(src, w, h, 1, gfx::TexFormat::Color);
            r.upload(src, 0, w, h, &px);
            s.host.gpu_versions.insert(src, sp.version);
        }
    }
    let dp = s.display.surfaces[&dst].clone();
    let (tw, th) = (dp.w as f32, dp.h as f32);
    let mut u = gfx::Uniforms::default();
    u.rt = [tw, th, tw, th];
    u.flags[0] = gfx::F_PRETRANSFORMED | gfx::F_HAS_DIFFUSE | if key.is_some() { gfx::F_COLORKEY } else { 0 };
    u.stages[0].ops = [2, 2, 1, 2];
    u.stages[0].ops2 = [2, 1, 0, 1];
    let (sw, sh) = (sp.w as f32, sp.h as f32);
    let v = |x: f32, y: f32, uu: f32, vv: f32| gfx::Vertex {
        pos: [x - 0.5, y - 0.5, 0.0, 1.0],
        diffuse: [255; 4],
        uv0: [uu, vv],
        ..Default::default()
    };
    let verts = [
        v(dr.left as f32, dr.top as f32, sr.left as f32 / sw, sr.top as f32 / sh),
        v(dr.right as f32, dr.top as f32, sr.right as f32 / sw, sr.top as f32 / sh),
        v(dr.left as f32, dr.bottom as f32, sr.left as f32 / sw, sr.bottom as f32 / sh),
        v(dr.right as f32, dr.bottom as f32, sr.right as f32 / sw, sr.bottom as f32 / sh),
    ];
    let pipe = gfx::PipeKey {
        topology: gfx::Topology::Triangles,
        blend: None,
        depth: None,
        cull: 1,
        color_write: true,
    };
    let samp = gfx::SamplerKey { mag: 1, min: 1, mip: 1, addr_u: 3, addr_v: 3 };
    let vp = gfx::Viewport { x: 0.0, y: 0.0, w: tw, h: th, min_z: 0.0, max_z: 1.0 };
    let Ok(r) = s.renderer() else {
        return false;
    };
    let full = Rect::new(0, 0, dp.w as i32, dp.h as i32);
    let c = dr.intersect(&full);
    if c.is_empty() {
        return true;
    }
    r.draw(dst, None, pipe, &u, &verts, &[0, 1, 2, 2, 1, 3], [Some(src), None], [samp, samp], vp, [c.left as u32, c.top as u32, c.w() as u32, c.h() as u32]);
    if let Some(p) = s.display.surfaces.get_mut(&dst) {
        p.version += 1;
    }
    true
}

pub fn device_destroyed(_s: &mut Session, _id: u32, _d: Device) {}

// ---------------------------------------------------------------- IDirect3D7

pub fn ddraw_qi(s: &mut Session, id: u32, iid: &Guid, ppv: u32) -> Result<Option<u32>, ApiError> {
    if *iid == IDIRECT3D7.iid {
        // IDirect3D7 is an interface of the DirectDraw object.
        s.out_iface(ppv, id, &IDIRECT3D7)?;
        s.com_addref(id);
        return Ok(Some(S_OK));
    }
    Ok(None)
}

fn device_desc(tnl: bool, hal: bool) -> Vec<u8> {
    let mut b = vec![0u8; 236];
    let p = |b: &mut Vec<u8>, o: usize, v: u32| b[o..o + 4].copy_from_slice(&v.to_le_bytes());
    let mut dev = 0x1 | 0x10 | 0x20 | 0x40 | 0x80 | 0x200 | 0x400 | 0x800 | 0x2000 | 0x8000;
    if hal {
        dev |= 0x80000;
    }
    if tnl {
        dev |= 0x10000;
    }
    p(&mut b, 0, dev);
    for base in [4usize, 60] {
        p(&mut b, base, 56);
        p(&mut b, base + 4, 0x2 | 0x10 | 0x20 | 0x40); // misc caps
        p(&mut b, base + 8, 0x1 | 0x10 | 0x20 | 0x80 | 0x100 | 0x4000 | 0x10000 | 0x100000 | 0x200000);
        p(&mut b, base + 12, 0xff);
        p(&mut b, base + 16, 0x1fff);
        p(&mut b, base + 20, 0x1fff);
        p(&mut b, base + 24, 0xff);
        p(&mut b, base + 28, 0x8 | 0x200 | 0x4000 | 0x80000);
        p(&mut b, base + 32, 0x1 | 0x4 | 0x8 | 0x80 | 0x1000);
        p(&mut b, base + 36, 0x303_033f);
        p(&mut b, base + 40, 0xff);
        p(&mut b, base + 44, 0x1 | 0x2 | 0x4 | 0x10);
    }
    p(&mut b, 116, 0x400 | 0x100); // render depths 16, 32
    p(&mut b, 120, 0x400 | 0x200 | 0x100); // z depths 16, 24, 32
    p(&mut b, 124, 1);
    p(&mut b, 128, 1);
    p(&mut b, 132, 2048);
    p(&mut b, 136, 2048);
    p(&mut b, 140, 8192);
    p(&mut b, 144, 2048);
    p(&mut b, 148, 1);
    for (o, v) in [(152usize, -8192.0f32), (156, -8192.0), (160, 8192.0), (164, 8192.0)] {
        b[o..o + 4].copy_from_slice(&v.to_le_bytes());
    }
    p(&mut b, 172, 0);
    p(&mut b, 176, 0x8_0008); // 8 texture coordinate sets, DONOTSTRIPELEMENTS
    p(&mut b, 180, 0x1f_ffff | 0x80_0000);
    b[184..186].copy_from_slice(&2u16.to_le_bytes());
    b[186..188].copy_from_slice(&2u16.to_le_bytes());
    p(&mut b, 188, if tnl { 8 } else { 0 });
    b[192..196].copy_from_slice(&1e10f32.to_le_bytes());
    let g = if tnl {
        IID_TNL_HAL_DEVICE
    } else if hal {
        IID_HAL_DEVICE
    } else {
        IID_RGB_DEVICE
    };
    b[196..212].copy_from_slice(&g);
    p(&mut b, 216, if tnl { 0x3e } else { 0 });
    b
}

fn cstr(t: &str) -> CallArg {
    let mut b = crate::strings::encode_ansi(t);
    b.push(0);
    CallArg::Buf(b)
}

fn d3d_enum_devices(s: &mut Session) -> ApiResult {
    let [_this, cb, ctx] = s.args::<3>()?;
    let items = vec![
        vec![cstr("Microsoft Direct3D RGB Software Emulation"), cstr("RGB Emulation"), CallArg::Buf(device_desc(false, false)), CallArg::Val(ctx)],
        vec![cstr("Microsoft Direct3D Hardware acceleration through Direct3D HAL"), cstr("Direct3D HAL"), CallArg::Buf(device_desc(false, true)), CallArg::Val(ctx)],
        vec![cstr("Microsoft Direct3D Hardware Transform and Lighting acceleration capable device"), cstr("Direct3D T&L HAL"), CallArg::Buf(device_desc(true, true)), CallArg::Val(ctx)],
    ];
    enum_callbacks(cb, items, 0, D3D_OK)
}

fn d3d_enum_zbuffer_formats(s: &mut Session) -> ApiResult {
    let [_this, _riid, cb, ctx] = s.args::<4>()?;
    let items = [Fmt::D16, Fmt::D32]
        .iter()
        .map(|f| {
            let mut pf = f.ddpf();
            if *f == Fmt::D32 {
                // 24-bit depth in a 32-bit container, no stencil.
                pf[12..16].copy_from_slice(&32u32.to_le_bytes());
                pf[20..24].copy_from_slice(&0x00ff_ffffu32.to_le_bytes());
            }
            vec![CallArg::Buf(pf.to_vec()), CallArg::Val(ctx)]
        })
        .collect();
    enum_callbacks(cb, items, 0, D3D_OK)
}

fn d3d_create_device(s: &mut Session) -> ApiResult {
    let [_this, clsid_p, target_p, pp] = s.args::<4>()?;
    let dd = s.this_obj()?;
    let clsid = s.read_guid(clsid_p)?;
    if clsid != IID_RGB_DEVICE && clsid != IID_HAL_DEVICE && clsid != IID_TNL_HAL_DEVICE {
        return Ok(ApiRet::Ret(D3DERR_INVALID_DEVICE));
    }
    let Some(target) = s.surface_obj_of_ptr(target_p) else {
        return Ok(ApiRet::Ret(D3DERR_INVALIDCALL));
    };
    let caps = match &s.com_obj(target).unwrap().kind {
        ComKind::Surface(x) => x.caps,
        _ => 0,
    };
    if caps & super::ddraw::DDSCAPS_3DDEVICE == 0 {
        return Ok(ApiRet::Ret(d3derr(694))); // DDERR_D3DNOTINITIALIZED-like failure
    }
    if let Err(e) = s.renderer() {
        s.event(crate::diag::Severity::Error, "CreateDevice", format!("no GPU renderer: {e}"));
        return Ok(ApiRet::Ret(E_OUTOFMEMORY));
    }
    let zbuf = depth_of(s, target).is_some();
    let sid = s.surface_of_obj(target).unwrap();
    let (w, h) = {
        let p = &s.display.surfaces[&sid];
        (p.w, p.h)
    };
    let mut transforms = BTreeMap::new();
    for t in [1u32, 2, 3] {
        transforms.insert(t, IDENTITY);
    }
    let dev = Device {
        ddraw: dd,
        target,
        kind: clsid,
        rs: default_rs(zbuf),
        tss: default_tss(),
        textures: [0; 8],
        transforms,
        viewport: Viewport7 { x: 0, y: 0, w, h, min_z: 0.0, max_z: 1.0 },
        material: Material7::default(),
        lights: BTreeMap::new(),
        light_enable: BTreeMap::new(),
        in_scene: false,
        clip_status: [0; 24],
        blocks: BTreeMap::new(),
        recording: None,
        next_block: 1,
    };
    s.com_addref(target);
    let id = s.com_new(ComKind::Device(Box::new(dev)));
    s.out_iface(pp, id, &IDIRECT3DDEVICE7)?;
    // Direct3D switches the x87 unit to single precision unless asked to preserve it.
    if s.com.ddraw.coop_flags & super::ddraw::DDSCL_FPUPRESERVE == 0 {
        let mut r = s.cpu.read_regs();
        r.fpcw &= !0x300;
        s.cpu.write_regs(&mut s.mem, &r).map_err(ApiError::Fatal)?;
    }
    Ok(ApiRet::Ret(D3D_OK))
}

fn depth_of(s: &Session, target: u32) -> Option<u32> {
    let ComKind::Surface(sf) = &s.com_obj(target)?.kind else {
        return None;
    };
    // The z-buffer may be attached to the render target or to its flip-chain root.
    let mut cands = sf.attached.clone();
    if sf.parent != 0
        && let Some(o) = s.com_obj(sf.parent)
        && let ComKind::Surface(p) = &o.kind
    {
        cands.extend(p.attached.iter().copied());
    }
    for a in cands {
        if let Some(o) = s.com_obj(a)
            && let ComKind::Surface(z) = &o.kind
            && z.caps & super::ddraw::DDSCAPS_ZBUFFER != 0
        {
            return Some(z.surface);
        }
    }
    None
}

fn d3d_create_vertex_buffer(s: &mut Session) -> ApiResult {
    let [_this, pdesc, pp, _flags] = s.args::<4>()?;
    let caps = s.mem.read_u32(pdesc + 4)?;
    let fvf = s.mem.read_u32(pdesc + 8)?;
    let count = s.mem.read_u32(pdesc + 12)?;
    let stride = fvf_stride(fvf);
    if stride == 0 || count == 0 {
        return Ok(ApiRet::Ret(D3DERR_INVALIDVERTEXTYPE));
    }
    let size = stride * count;
    let mem = s
        .heaps
        .alloc(&mut s.mem, s.heaps.process_heap, size, true)
        .ok_or(ApiError::Fatal("vertex buffer allocation".into()))?;
    let id = s.com_new(ComKind::VertexBuffer(VertexBuffer { mem, size, fvf, count, caps, locks: 0 }));
    s.out_iface(pp, id, &IDIRECT3DVERTEXBUFFER7)?;
    Ok(ApiRet::Ret(D3D_OK))
}

fn d3d_evict_managed_textures(_s: &mut Session) -> ApiResult {
    Ok(ApiRet::Ret(D3D_OK))
}

// ---------------------------------------------------------------- FVF

fn fvf_stride(fvf: u32) -> u32 {
    let mut n = match fvf & 0xe {
        0x2 => 12,
        0x4 => 16,
        0x6 => 16,
        0x8 => 20,
        0xa => 24,
        0xc => 28,
        0xe => 32,
        _ => return 0,
    };
    if fvf & 0x10 != 0 {
        n += 12;
    }
    if fvf & 0x20 != 0 {
        n += 4;
    }
    if fvf & 0x40 != 0 {
        n += 4;
    }
    if fvf & 0x80 != 0 {
        n += 4;
    }
    let tex = (fvf >> 8) & 0xf;
    for i in 0..tex {
        n += match (fvf >> (16 + 2 * i)) & 3 {
            0 => 8,
            1 => 12,
            2 => 16,
            _ => 4,
        };
    }
    n
}

fn color_rgba(c: u32) -> [u8; 4] {
    [(c >> 16) as u8, (c >> 8) as u8, c as u8, (c >> 24) as u8]
}

/// Strided source description: (address, stride) per component.
#[derive(Default, Clone, Copy)]
struct Streams {
    pos: (u32, u32),
    normal: (u32, u32),
    diffuse: (u32, u32),
    specular: (u32, u32),
    tex: [(u32, u32); 8],
}

fn streams_from_fvf(base: u32, fvf: u32) -> Streams {
    let stride = fvf_stride(fvf);
    let mut o = match fvf & 0xe {
        0x2 => 12,
        0x4 => 16,
        0x6 => 16,
        0x8 => 20,
        0xa => 24,
        0xc => 28,
        _ => 32,
    };
    let mut st = Streams {
        pos: (base, stride),
        ..Default::default()
    };
    if fvf & 0x10 != 0 {
        st.normal = (base + o, stride);
        o += 12;
    }
    if fvf & 0x20 != 0 {
        o += 4;
    }
    if fvf & 0x40 != 0 {
        st.diffuse = (base + o, stride);
        o += 4;
    }
    if fvf & 0x80 != 0 {
        st.specular = (base + o, stride);
        o += 4;
    }
    for i in 0..((fvf >> 8) & 0xf).min(8) {
        st.tex[i as usize] = (base + o, stride);
        o += match (fvf >> (16 + 2 * i)) & 3 {
            0 => 8,
            1 => 12,
            2 => 16,
            _ => 4,
        };
    }
    st
}

fn read_vertices(s: &Session, st: &Streams, fvf: u32, first: u32, count: u32) -> Result<Vec<gfx::Vertex>, ApiError> {
    let mut out = Vec::with_capacity(count as usize);
    let rhw = fvf & 0xe == 0x4;
    let f = |a: u32| -> Result<f32, ApiError> { Ok(s.mem.read_f32(a)?) };
    for i in first..first + count {
        let mut v = gfx::Vertex::default();
        let p = st.pos.0 + i * st.pos.1;
        v.pos = [f(p)?, f(p + 4)?, f(p + 8)?, if rhw { f(p + 12)? } else { 1.0 }];
        if st.normal.0 != 0 {
            let n = st.normal.0 + i * st.normal.1;
            v.normal = [f(n)?, f(n + 4)?, f(n + 8)?];
        }
        if st.diffuse.0 != 0 {
            v.diffuse = color_rgba(s.mem.read_u32(st.diffuse.0 + i * st.diffuse.1)?);
        } else {
            v.diffuse = [255; 4];
        }
        if st.specular.0 != 0 {
            v.specular = color_rgba(s.mem.read_u32(st.specular.0 + i * st.specular.1)?);
        }
        if st.tex[0].0 != 0 {
            let t = st.tex[0].0 + i * st.tex[0].1;
            v.uv0 = [f(t)?, f(t + 4)?];
        }
        if st.tex[1].0 != 0 {
            let t = st.tex[1].0 + i * st.tex[1].1;
            v.uv1 = [f(t)?, f(t + 4)?];
        }
        out.push(v);
    }
    let _ = fvf;
    Ok(out)
}

/// Convert a primitive type + vertex/index count into (topology, list indices).
fn primitive_indices(prim: u32, n: u32, indices: Option<&[u32]>) -> Option<(gfx::Topology, Vec<u32>)> {
    let idx = |i: u32| indices.map(|x| x[i as usize]).unwrap_or(i);
    let mut out = Vec::new();
    let topo = match prim {
        1 => {
            for i in 0..n {
                out.push(idx(i));
            }
            gfx::Topology::Points
        }
        2 => {
            for i in 0..n / 2 * 2 {
                out.push(idx(i));
            }
            gfx::Topology::Lines
        }
        3 => {
            for i in 1..n {
                out.push(idx(i - 1));
                out.push(idx(i));
            }
            gfx::Topology::Lines
        }
        4 => {
            for i in 0..n / 3 * 3 {
                out.push(idx(i));
            }
            gfx::Topology::Triangles
        }
        5 => {
            for i in 2..n {
                if i % 2 == 0 {
                    out.extend([idx(i - 2), idx(i - 1), idx(i)]);
                } else {
                    out.extend([idx(i - 1), idx(i - 2), idx(i)]);
                }
            }
            gfx::Topology::Triangles
        }
        6 => {
            for i in 2..n {
                out.extend([idx(0), idx(i - 1), idx(i)]);
            }
            gfx::Topology::Triangles
        }
        _ => return None,
    };
    Some((topo, out))
}

fn verts_for(prim: u32, count: u32) -> u32 {
    // Number of vertices consumed by `count` primitives.
    match prim {
        1 => count,
        2 => count * 2,
        3 => count + 1,
        4 => count * 3,
        5 | 6 => count + 2,
        _ => 0,
    }
}

// ---------------------------------------------------------------- device helpers

fn dev_id(s: &Session) -> Result<u32, ApiError> {
    s.this_obj()
}

fn with_dev<T>(s: &mut Session, f: impl FnOnce(&mut Device) -> T) -> Result<T, ApiError> {
    let id = dev_id(s)?;
    match &mut s.com_obj_mut(id).unwrap().kind {
        ComKind::Device(d) => Ok(f(d)),
        _ => Err(ApiError::Fatal("not a Direct3D device".into())),
    }
}

fn dev_clone(s: &Session) -> Result<Device, ApiError> {
    let id = dev_id(s)?;
    match &s.com_obj(id).unwrap().kind {
        ComKind::Device(d) => Ok((**d).clone()),
        _ => Err(ApiError::Fatal("not a Direct3D device".into())),
    }
}

fn read_matrix(s: &Session, p: u32) -> Result<[f32; 16], ApiError> {
    let mut m = [0f32; 16];
    for (i, v) in m.iter_mut().enumerate() {
        *v = s.mem.read_f32(p + 4 * i as u32)?;
    }
    Ok(m)
}

fn mat_mul(a: &[f32; 16], b: &[f32; 16]) -> [f32; 16] {
    // Row-major (D3D) product a * b.
    let mut r = [0f32; 16];
    for i in 0..4 {
        for j in 0..4 {
            let mut acc = 0.0;
            for k in 0..4 {
                acc += a[i * 4 + k] * b[k * 4 + j];
            }
            r[i * 4 + j] = acc;
        }
    }
    r
}

/// Inverse-transpose of the upper 3x3 (row-vector convention), as a 4x4.
fn normal_matrix(m: &[f32; 16]) -> [f32; 16] {
    let a = [m[0], m[1], m[2], m[4], m[5], m[6], m[8], m[9], m[10]];
    let det = a[0] * (a[4] * a[8] - a[5] * a[7]) - a[1] * (a[3] * a[8] - a[5] * a[6]) + a[2] * (a[3] * a[7] - a[4] * a[6]);
    if det.abs() < 1e-20 {
        return IDENTITY;
    }
    let inv = [
        (a[4] * a[8] - a[5] * a[7]) / det,
        (a[2] * a[7] - a[1] * a[8]) / det,
        (a[1] * a[5] - a[2] * a[4]) / det,
        (a[5] * a[6] - a[3] * a[8]) / det,
        (a[0] * a[8] - a[2] * a[6]) / det,
        (a[2] * a[3] - a[0] * a[5]) / det,
        (a[3] * a[7] - a[4] * a[6]) / det,
        (a[1] * a[6] - a[0] * a[7]) / det,
        (a[0] * a[4] - a[1] * a[3]) / det,
    ];
    // transpose(inv)
    [
        inv[0], inv[3], inv[6], 0.0, inv[1], inv[4], inv[7], 0.0, inv[2], inv[5], inv[8], 0.0, 0.0, 0.0, 0.0, 1.0,
    ]
}

fn xform_point(m: &[f32; 16], p: [f32; 3]) -> [f32; 3] {
    [
        p[0] * m[0] + p[1] * m[4] + p[2] * m[8] + m[12],
        p[0] * m[1] + p[1] * m[5] + p[2] * m[9] + m[13],
        p[0] * m[2] + p[1] * m[6] + p[2] * m[10] + m[14],
    ]
}

fn xform_dir(m: &[f32; 16], p: [f32; 3]) -> [f32; 3] {
    let v = [
        p[0] * m[0] + p[1] * m[4] + p[2] * m[8],
        p[0] * m[1] + p[1] * m[5] + p[2] * m[9],
        p[0] * m[2] + p[1] * m[6] + p[2] * m[10],
    ];
    let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt().max(1e-20);
    [v[0] / l, v[1] / l, v[2] / l]
}

fn argb_f(c: u32) -> [f32; 4] {
    [
        ((c >> 16) & 0xff) as f32 / 255.0,
        ((c >> 8) & 0xff) as f32 / 255.0,
        (c & 0xff) as f32 / 255.0,
        (c >> 24) as f32 / 255.0,
    ]
}

/// Ensure a texture (with its mip chain) is resident; returns its renderer key.
fn sync_texture(s: &mut Session, obj: u32) -> Option<u32> {
    let ComKind::Surface(sf) = &s.com_obj(obj)?.kind else {
        return None;
    };
    let sf = sf.clone();
    let top = sf.surface;
    // Mip chain: follow attached sublevels.
    let mut levels = vec![top];
    let mut cur = sf.clone();
    while let Some(next) = cur.attached.iter().find_map(|a| match &s.com_obj(*a)?.kind {
        ComKind::Surface(x) if x.caps2 & super::ddraw::DDSCAPS2_MIPMAPSUBLEVEL != 0 => Some(x.clone()),
        _ => None,
    }) {
        levels.push(next.surface);
        cur = next;
    }
    let px = s.display.surfaces.get(&top)?.clone();
    if px.gpu_dirty {
        // Rendered-to texture: the GPU copy is current.
        return Some(top);
    }
    let stamp: u64 = levels
        .iter()
        .map(|l| s.display.surfaces.get(l).map(|x| x.version + 1).unwrap_or(0))
        .fold(0u64, |a, v| a.wrapping_mul(1_000_003).wrapping_add(v));
    let ok_size = s.host.renderer.as_ref().is_some_and(|r| r.has_texture(top));
    if ok_size && s.host.gpu_versions.get(&top) == Some(&stamp) {
        return Some(top);
    }
    let r = s.renderer().ok()?;
    r.ensure_texture(top, px.w, px.h, levels.len() as u32, gfx::TexFormat::Color);
    for (i, l) in levels.iter().enumerate() {
        if let Some((w, h, data)) = decode_rgba(s, *l) {
            s.renderer().ok()?.upload(top, i as u32, w, h, &data);
        }
    }
    s.host.gpu_versions.insert(top, stamp);
    if let Some(p) = s.display.surfaces.get_mut(&top) {
        p.cpu_dirty = false;
    }
    Some(top)
}

fn build_uniforms(s: &Session, d: &Device, pretransformed: bool, fvf: u32, tw: f32, th: f32) -> gfx::Uniforms {
    let mut u = gfx::Uniforms::default();
    let world = d.transforms.get(&1).copied().unwrap_or(IDENTITY);
    let view = d.transforms.get(&2).copied().unwrap_or(IDENTITY);
    let proj = d.transforms.get(&3).copied().unwrap_or(IDENTITY);
    u.world = world;
    u.view = view;
    u.proj = proj;
    u.normal_mat = normal_matrix(&mat_mul(&world, &view));
    u.rt = [tw, th, d.viewport.w.max(1) as f32, d.viewport.h.max(1) as f32];
    let rs = &d.rs;
    let mut bits = 0;
    if pretransformed {
        bits |= gfx::F_PRETRANSFORMED;
    }
    if !pretransformed && rs[RS_LIGHTING] != 0 {
        bits |= gfx::F_LIGHTING;
    }
    if fvf & 0x10 != 0 {
        bits |= gfx::F_HAS_NORMAL;
    }
    if fvf & 0x40 != 0 {
        bits |= gfx::F_HAS_DIFFUSE;
    }
    if fvf & 0x80 != 0 {
        bits |= gfx::F_HAS_SPECULAR;
    }
    if rs[RS_SPECULARENABLE] != 0 {
        bits |= gfx::F_SPECULAR_ENABLE;
    }
    if rs[RS_LOCALVIEWER] != 0 {
        bits |= gfx::F_LOCAL_VIEWER;
    }
    if rs[RS_NORMALIZENORMALS] != 0 {
        bits |= gfx::F_NORMALIZE;
    }
    if rs[RS_COLORKEYENABLE] != 0 {
        bits |= gfx::F_COLORKEY;
    }
    if rs[RS_ALPHATESTENABLE] != 0 {
        bits |= gfx::F_ALPHATEST;
    }
    if rs[RS_COLORVERTEX] != 0 {
        bits |= gfx::F_COLOR_VERTEX;
    }
    let table = rs[RS_FOGTABLEMODE];
    let vmode = rs[RS_FOGVERTEXMODE];
    if rs[RS_FOGENABLE] != 0 {
        bits |= gfx::F_FOG;
    }
    let fog_mode = if table != 0 {
        table
    } else if vmode != 0 && !pretransformed {
        vmode
    } else {
        0
    };
    u.flags = [bits, rs[RS_ALPHAFUNC], fog_mode, 0];
    u.alpha_fog = [
        (rs[RS_ALPHAREF] & 0xff) as f32,
        f32::from_bits(rs[RS_FOGSTART]),
        f32::from_bits(rs[RS_FOGEND]),
        f32::from_bits(rs[RS_FOGDENSITY]),
    ];
    u.fog_color = argb_f(rs[RS_FOGCOLOR]);
    u.ambient = argb_f(rs[RS_AMBIENT]);
    u.tfactor = argb_f(rs[RS_TEXTUREFACTOR]);
    u.mat_diffuse = d.material.diffuse;
    u.mat_ambient = d.material.ambient;
    u.mat_specular = d.material.specular;
    u.mat_emissive = d.material.emissive;
    u.mat_power = [d.material.power, 0.0, 0.0, 0.0];
    let src = |v: u32| v.min(2);
    u.color_src = [
        src(rs[RS_DIFFUSEMATERIALSOURCE]),
        src(rs[RS_AMBIENTMATERIALSOURCE]),
        src(rs[RS_SPECULARMATERIALSOURCE]),
        src(rs[RS_EMISSIVEMATERIALSOURCE]),
    ];
    let mut li = 0;
    for (idx, l) in &d.lights {
        if li >= 8 {
            break;
        }
        if !d.light_enable.get(idx).copied().unwrap_or(false) {
            continue;
        }
        let pos = xform_point(&view, l.position);
        let dir = xform_dir(&view, l.direction);
        u.lights[li] = gfx::GpuLight {
            kind: [l.kind, 0, 0, 0],
            diffuse: l.diffuse,
            specular: l.specular,
            ambient: l.ambient,
            position: [pos[0], pos[1], pos[2], 1.0],
            direction: [dir[0], dir[1], dir[2], 0.0],
            params: [if l.range <= 0.0 { f32::MAX } else { l.range }, l.falloff, l.att[0], l.att[1]],
            params2: [l.att[2], (l.theta / 2.0).cos(), (l.phi / 2.0).cos(), 0.0],
        };
        li += 1;
    }
    for st in 0..2 {
        let t = &d.tss[st];
        let has_tex = d.textures[st] != 0;
        u.stages[st] = gfx::GpuStage {
            ops: [t[1], t[2], t[3], t[4]],
            ops2: [t[5], t[6], t[11], has_tex as u32],
            tt: [t[24] & 0xff, (t[24] & 0x100 != 0) as u32, 0, 0],
            texmat: d.transforms.get(&(16 + st as u32)).copied().unwrap_or(IDENTITY),
        };
    }
    // A disabled stage 0 colour op ends the cascade at the diffuse colour.
    if d.tss[0][1] == 1 {
        u.stages[0].ops = [2, 0, 0, 2];
        u.stages[0].ops2 = [0, 0, 0, 0];
        u.stages[0].tt = [0; 4];
        u.stages[1].ops[0] = 1;
    }
    let _ = s;
    u
}

fn sampler_for(d: &Device, st: usize) -> gfx::SamplerKey {
    let t = &d.tss[st];
    let filt = |v: u32| if v >= 2 { 2 } else { 1 };
    gfx::SamplerKey {
        mag: filt(t[16]),
        min: filt(t[17]),
        mip: t[18].clamp(1, 3),
        addr_u: t[13],
        addr_v: t[14],
    }
}

fn pipe_for(d: &Device, topology: gfx::Topology, has_depth: bool) -> gfx::PipeKey {
    let rs = &d.rs;
    gfx::PipeKey {
        topology,
        blend: (rs[RS_ALPHABLENDENABLE] != 0).then_some(gfx::BlendState {
            src: rs[RS_SRCBLEND],
            dst: rs[RS_DESTBLEND],
        }),
        depth: (has_depth && rs[RS_ZENABLE] != 0).then_some(gfx::DepthState {
            func: rs[RS_ZFUNC],
            write: rs[RS_ZWRITEENABLE] != 0,
            bias: rs[RS_ZBIAS] as i32,
        }),
        cull: rs[RS_CULLMODE],
        color_write: true,
    }
}

/// Issue a draw with vertices already decoded.
fn submit_draw(s: &mut Session, prim: u32, fvf: u32, verts: Vec<gfx::Vertex>, indices: Option<Vec<u32>>) -> Result<u32, ApiError> {
    let d = dev_clone(s)?;
    let n = indices.as_ref().map(|i| i.len() as u32).unwrap_or(verts.len() as u32);
    let Some((topo, idx)) = primitive_indices(prim, n, indices.as_deref()) else {
        return Ok(D3DERR_INVALIDPRIMITIVETYPE);
    };
    if idx.is_empty() {
        return Ok(D3D_OK);
    }
    if idx.iter().any(|&i| i as usize >= verts.len()) {
        return Ok(D3DERR_INVALIDCALL);
    }
    let Some(target) = s.surface_of_obj(d.target) else {
        return Ok(D3DERR_INVALIDCALL);
    };
    let depth = depth_of(s, d.target);
    sync_target(s, target).map_err(ApiError::Fatal)?;
    if let Some(z) = depth {
        sync_target(s, z).map_err(ApiError::Fatal)?;
    }
    let tex0 = if d.textures[0] != 0 { sync_texture(s, d.textures[0]) } else { None };
    let tex1 = if d.textures[1] != 0 { sync_texture(s, d.textures[1]) } else { None };
    let (tw, th) = {
        let p = &s.display.surfaces[&target];
        (p.w as f32, p.h as f32)
    };
    let pre = fvf & 0xe == 0x4;
    let u = build_uniforms(s, &d, pre, fvf, tw, th);
    let pipe = pipe_for(&d, topo, depth.is_some());
    // Transformed vertices carry absolute screen coordinates and depth; the
    // viewport only bounds them (through the scissor below).
    let vp = if pre {
        gfx::Viewport { x: 0.0, y: 0.0, w: tw, h: th, min_z: 0.0, max_z: 1.0 }
    } else {
        gfx::Viewport {
            x: d.viewport.x as f32,
            y: d.viewport.y as f32,
            w: d.viewport.w as f32,
            h: d.viewport.h as f32,
            min_z: d.viewport.min_z,
            max_z: d.viewport.max_z,
        }
    };
    let scissor = [d.viewport.x, d.viewport.y, d.viewport.w, d.viewport.h];
    let samplers = [sampler_for(&d, 0), sampler_for(&d, 1)];
    let r = s.renderer().map_err(ApiError::Fatal)?;
    r.draw(target, depth, pipe, &u, &verts, &idx, [tex0, tex1], samplers, vp, scissor);
    for id in [Some(target), depth].into_iter().flatten() {
        if let Some(p) = s.display.surfaces.get_mut(&id) {
            p.gpu_dirty = true;
            p.cpu_dirty = false;
        }
    }
    Ok(D3D_OK)
}

// ---------------------------------------------------------------- IDirect3DDevice7 methods

fn dev_get_caps(s: &mut Session) -> ApiResult {
    let [_this, p] = s.args::<2>()?;
    let d = dev_clone(s)?;
    let desc = device_desc(d.kind == IID_TNL_HAL_DEVICE, d.kind != IID_RGB_DEVICE);
    s.mem.write(p, &desc)?;
    Ok(ApiRet::Ret(D3D_OK))
}

fn dev_enum_texture_formats(s: &mut Session) -> ApiResult {
    let [_this, cb, ctx] = s.args::<3>()?;
    let fmts = [
        Fmt::Rgb565,
        Fmt::Xrgb1555,
        Fmt::Argb1555,
        Fmt::Argb4444,
        Fmt::Xrgb8888,
        Fmt::Argb8888,
        Fmt::Dxt1,
        Fmt::Dxt3,
        Fmt::Dxt5,
    ];
    let items = fmts
        .iter()
        .map(|f| vec![CallArg::Buf(f.ddpf().to_vec()), CallArg::Val(ctx)])
        .collect();
    enum_callbacks(cb, items, 0, D3D_OK)
}

fn dev_begin_scene(s: &mut Session) -> ApiResult {
    let r = with_dev(s, |d| {
        if d.in_scene {
            D3DERR_SCENE_IN_SCENE
        } else {
            d.in_scene = true;
            D3D_OK
        }
    })?;
    Ok(ApiRet::Ret(r))
}

fn dev_end_scene(s: &mut Session) -> ApiResult {
    let r = with_dev(s, |d| {
        if !d.in_scene {
            D3DERR_SCENE_NOT_IN_SCENE
        } else {
            d.in_scene = false;
            D3D_OK
        }
    })?;
    Ok(ApiRet::Ret(r))
}

fn dev_get_direct3d(s: &mut Session) -> ApiResult {
    let [_this, pp] = s.args::<2>()?;
    let d = dev_clone(s)?;
    s.out_iface(pp, d.ddraw, &IDIRECT3D7)?;
    s.com_addref(d.ddraw);
    Ok(ApiRet::Ret(D3D_OK))
}

fn dev_set_render_target(s: &mut Session) -> ApiResult {
    let [_this, surf, _flags] = s.args::<3>()?;
    let Some(t) = s.surface_obj_of_ptr(surf) else {
        return Ok(ApiRet::Ret(D3DERR_INVALIDCALL));
    };
    let old = with_dev(s, |d| std::mem::replace(&mut d.target, t))?;
    s.com_addref(t);
    s.com_release(old);
    let sid = s.surface_of_obj(t).unwrap();
    let (w, h) = (s.display.surfaces[&sid].w, s.display.surfaces[&sid].h);
    with_dev(s, |d| d.viewport = Viewport7 { x: 0, y: 0, w, h, min_z: 0.0, max_z: 1.0 })?;
    Ok(ApiRet::Ret(D3D_OK))
}

fn dev_get_render_target(s: &mut Session) -> ApiResult {
    let [_this, pp] = s.args::<2>()?;
    let d = dev_clone(s)?;
    s.out_iface(pp, d.target, &super::ddraw::IDIRECTDRAWSURFACE7)?;
    s.com_addref(d.target);
    Ok(ApiRet::Ret(D3D_OK))
}

fn dev_clear(s: &mut Session) -> ApiResult {
    let [_this, count, rects, flags, color, z, _stencil] = s.args::<7>()?;
    let d = dev_clone(s)?;
    let Some(target) = s.surface_of_obj(d.target) else {
        return Ok(ApiRet::Ret(D3DERR_INVALIDCALL));
    };
    let depth = depth_of(s, d.target);
    let vp = Rect::new(
        d.viewport.x as i32,
        d.viewport.y as i32,
        (d.viewport.x + d.viewport.w) as i32,
        (d.viewport.y + d.viewport.h) as i32,
    );
    let mut list = Vec::new();
    if count == 0 || rects == 0 {
        list.push(vp);
    } else {
        for i in 0..count.min(256) {
            // D3DRECT: x1, y1, x2, y2
            let r = s.read_rect(rects + 16 * i)?;
            list.push(r.intersect(&vp));
        }
    }
    sync_target(s, target).map_err(ApiError::Fatal)?;
    if let Some(zz) = depth {
        sync_target(s, zz).map_err(ApiError::Fatal)?;
    }
    let zf = f32::from_bits(z);
    let c = color_rgba(color);
    let r = s.renderer().map_err(ApiError::Fatal)?;
    for rc in list {
        if rc.is_empty() {
            continue;
        }
        let rr = [rc.left as u32, rc.top as u32, rc.w() as u32, rc.h() as u32];
        r.clear(
            target,
            depth,
            rr,
            (flags & 1 != 0).then_some(c),
            (flags & 2 != 0 && depth.is_some()).then_some(zf),
        );
    }
    for id in [Some(target), depth].into_iter().flatten() {
        if let Some(p) = s.display.surfaces.get_mut(&id) {
            p.gpu_dirty = true;
            p.cpu_dirty = false;
        }
    }
    Ok(ApiRet::Ret(D3D_OK))
}

fn record<F: FnOnce(&mut StateBlock)>(d: &mut Device, f: F) -> bool {
    if let Some(b) = d.recording.as_mut() {
        f(b);
        true
    } else {
        false
    }
}

fn dev_set_transform(s: &mut Session) -> ApiResult {
    let [_this, ty, pm] = s.args::<3>()?;
    let m = read_matrix(s, pm)?;
    with_dev(s, |d| {
        if !record(d, |b| {
            b.transforms.insert(ty, m);
        }) {
            d.transforms.insert(ty, m);
        }
    })?;
    Ok(ApiRet::Ret(D3D_OK))
}

fn dev_get_transform(s: &mut Session) -> ApiResult {
    let [_this, ty, pm] = s.args::<3>()?;
    let m = with_dev(s, |d| d.transforms.get(&ty).copied().unwrap_or(IDENTITY))?;
    for (i, v) in m.iter().enumerate() {
        s.mem.write_f32(pm + 4 * i as u32, *v)?;
    }
    Ok(ApiRet::Ret(D3D_OK))
}

fn dev_multiply_transform(s: &mut Session) -> ApiResult {
    let [_this, ty, pm] = s.args::<3>()?;
    let m = read_matrix(s, pm)?;
    with_dev(s, |d| {
        let cur = d.transforms.get(&ty).copied().unwrap_or(IDENTITY);
        d.transforms.insert(ty, mat_mul(&m, &cur));
    })?;
    Ok(ApiRet::Ret(D3D_OK))
}

fn dev_set_viewport(s: &mut Session) -> ApiResult {
    let [_this, p] = s.args::<2>()?;
    let v = Viewport7 {
        x: s.mem.read_u32(p)?,
        y: s.mem.read_u32(p + 4)?,
        w: s.mem.read_u32(p + 8)?,
        h: s.mem.read_u32(p + 12)?,
        min_z: s.mem.read_f32(p + 16)?,
        max_z: s.mem.read_f32(p + 20)?,
    };
    with_dev(s, |d| {
        if !record(d, |b| b.viewport = Some(v)) {
            d.viewport = v;
        }
    })?;
    Ok(ApiRet::Ret(D3D_OK))
}

fn dev_get_viewport(s: &mut Session) -> ApiResult {
    let [_this, p] = s.args::<2>()?;
    let v = with_dev(s, |d| d.viewport)?;
    s.mem.write_u32(p, v.x)?;
    s.mem.write_u32(p + 4, v.y)?;
    s.mem.write_u32(p + 8, v.w)?;
    s.mem.write_u32(p + 12, v.h)?;
    s.mem.write_f32(p + 16, v.min_z)?;
    s.mem.write_f32(p + 20, v.max_z)?;
    Ok(ApiRet::Ret(D3D_OK))
}

fn read_color4(s: &Session, p: u32) -> Result<[f32; 4], ApiError> {
    Ok([s.mem.read_f32(p)?, s.mem.read_f32(p + 4)?, s.mem.read_f32(p + 8)?, s.mem.read_f32(p + 12)?])
}

fn dev_set_material(s: &mut Session) -> ApiResult {
    let [_this, p] = s.args::<2>()?;
    let m = Material7 {
        diffuse: read_color4(s, p)?,
        ambient: read_color4(s, p + 16)?,
        specular: read_color4(s, p + 32)?,
        emissive: read_color4(s, p + 48)?,
        power: s.mem.read_f32(p + 64)?,
    };
    with_dev(s, |d| {
        if !record(d, |b| b.material = Some(m)) {
            d.material = m;
        }
    })?;
    Ok(ApiRet::Ret(D3D_OK))
}

fn dev_get_material(s: &mut Session) -> ApiResult {
    let [_this, p] = s.args::<2>()?;
    let m = with_dev(s, |d| d.material)?;
    for (k, c) in [m.diffuse, m.ambient, m.specular, m.emissive].iter().enumerate() {
        for (i, v) in c.iter().enumerate() {
            s.mem.write_f32(p + 16 * k as u32 + 4 * i as u32, *v)?;
        }
    }
    s.mem.write_f32(p + 64, m.power)?;
    Ok(ApiRet::Ret(D3D_OK))
}

fn dev_set_light(s: &mut Session) -> ApiResult {
    let [_this, idx, p] = s.args::<3>()?;
    let kind = s.mem.read_u32(p)?;
    if !(1..=3).contains(&kind) {
        return Ok(ApiRet::Ret(D3DERR_INVALIDCALL));
    }
    let f = |o: u32| s.mem.read_f32(p + o);
    let l = Light7 {
        kind,
        diffuse: read_color4(s, p + 4)?,
        specular: read_color4(s, p + 20)?,
        ambient: read_color4(s, p + 36)?,
        position: [f(52)?, f(56)?, f(60)?],
        direction: [f(64)?, f(68)?, f(72)?],
        range: f(76)?,
        falloff: f(80)?,
        att: [f(84)?, f(88)?, f(92)?],
        theta: f(96)?,
        phi: f(100)?,
    };
    with_dev(s, |d| {
        if !record(d, |b| {
            b.lights.insert(idx, l);
        }) {
            d.lights.insert(idx, l);
        }
    })?;
    Ok(ApiRet::Ret(D3D_OK))
}

fn dev_get_light(s: &mut Session) -> ApiResult {
    let [_this, idx, p] = s.args::<3>()?;
    let Some(l) = with_dev(s, |d| d.lights.get(&idx).copied())? else {
        return Ok(ApiRet::Ret(D3DERR_INVALIDCALL));
    };
    s.mem.write_u32(p, l.kind)?;
    for (k, c) in [l.diffuse, l.specular, l.ambient].iter().enumerate() {
        for (i, v) in c.iter().enumerate() {
            s.mem.write_f32(p + 4 + 16 * k as u32 + 4 * i as u32, *v)?;
        }
    }
    let rest = [
        l.position[0], l.position[1], l.position[2], l.direction[0], l.direction[1], l.direction[2], l.range, l.falloff, l.att[0], l.att[1], l.att[2], l.theta, l.phi,
    ];
    for (i, v) in rest.iter().enumerate() {
        s.mem.write_f32(p + 52 + 4 * i as u32, *v)?;
    }
    Ok(ApiRet::Ret(D3D_OK))
}

fn dev_light_enable(s: &mut Session) -> ApiResult {
    let [_this, idx, en] = s.args::<3>()?;
    with_dev(s, |d| {
        if !record(d, |b| {
            b.light_enable.insert(idx, en != 0);
        }) {
            // Enabling a light that was never set creates a default directional light.
            d.lights.entry(idx).or_insert(Light7 {
                kind: 3,
                diffuse: [1.0; 4],
                direction: [0.0, 0.0, 1.0],
                range: f32::MAX,
                ..Default::default()
            });
            d.light_enable.insert(idx, en != 0);
        }
    })?;
    Ok(ApiRet::Ret(D3D_OK))
}

fn dev_get_light_enable(s: &mut Session) -> ApiResult {
    let [_this, idx, p] = s.args::<3>()?;
    let e = with_dev(s, |d| d.light_enable.get(&idx).copied())?;
    match e {
        Some(v) => {
            s.mem.write_u32(p, v as u32)?;
            Ok(ApiRet::Ret(D3D_OK))
        }
        None => Ok(ApiRet::Ret(D3DERR_INVALIDCALL)),
    }
}

fn dev_set_render_state(s: &mut Session) -> ApiResult {
    let [_this, st, v] = s.args::<3>()?;
    if st >= 256 {
        return Ok(ApiRet::Ret(D3DERR_INVALIDCALL));
    }
    let st = st as usize;
    if matches!(st, 8) && v != 3 {
        s.missing("SetRenderState", format!("FILLMODE {v} (only solid fill is implemented)"));
    }
    if (52..=59).contains(&st) && st == 52 && v != 0 {
        s.missing("SetRenderState", "stencil buffering".into());
    }
    with_dev(s, |d| {
        if !record(d, |b| {
            b.rs.insert(st, v);
        }) {
            d.rs[st] = v;
        }
    })?;
    Ok(ApiRet::Ret(D3D_OK))
}

fn dev_get_render_state(s: &mut Session) -> ApiResult {
    let [_this, st, p] = s.args::<3>()?;
    if st >= 256 {
        return Ok(ApiRet::Ret(D3DERR_INVALIDCALL));
    }
    let v = with_dev(s, |d| d.rs[st as usize])?;
    s.mem.write_u32(p, v)?;
    Ok(ApiRet::Ret(D3D_OK))
}

fn dev_set_texture(s: &mut Session) -> ApiResult {
    let [_this, stage, tex] = s.args::<3>()?;
    if stage >= 8 {
        return Ok(ApiRet::Ret(D3DERR_INVALIDCALL));
    }
    let new = if tex == 0 {
        0
    } else {
        match s.surface_obj_of_ptr(tex) {
            Some(o) => o,
            None => return Ok(ApiRet::Ret(D3DERR_INVALIDCALL)),
        }
    };
    if stage >= 2 && new != 0 {
        s.missing("SetTexture", format!("texture stage {stage} (2 stages are implemented)"));
    }
    let recorded = with_dev(s, |d| {
        record(d, |b| {
            b.textures.insert(stage as usize, new);
        })
    })?;
    if recorded {
        return Ok(ApiRet::Ret(D3D_OK));
    }
    if new != 0 {
        s.com_addref(new);
    }
    let old = with_dev(s, |d| std::mem::replace(&mut d.textures[stage as usize], new))?;
    if old != 0 {
        s.com_release(old);
    }
    Ok(ApiRet::Ret(D3D_OK))
}

fn dev_get_texture(s: &mut Session) -> ApiResult {
    let [_this, stage, pp] = s.args::<3>()?;
    if stage >= 8 {
        return Ok(ApiRet::Ret(D3DERR_INVALIDCALL));
    }
    let t = with_dev(s, |d| d.textures[stage as usize])?;
    if t == 0 {
        s.mem.write_u32(pp, 0)?;
    } else {
        s.out_iface(pp, t, &super::ddraw::IDIRECTDRAWSURFACE7)?;
        s.com_addref(t);
    }
    Ok(ApiRet::Ret(D3D_OK))
}

fn dev_set_texture_stage_state(s: &mut Session) -> ApiResult {
    let [_this, stage, ty, v] = s.args::<4>()?;
    if stage >= 8 || ty >= 32 {
        return Ok(ApiRet::Ret(D3DERR_INVALIDCALL));
    }
    if stage < 2 && ty == 11 && (v >> 16) > 3 {
        s.missing("SetTextureStageState", format!("texture coordinate generation {:#x}", v >> 16));
    }
    if stage >= 2 && ty == 1 && v != 1 {
        s.missing("SetTextureStageState", format!("stage {stage} colour op {v}"));
    }
    with_dev(s, |d| {
        let (st, t) = (stage as usize, ty as usize);
        if ty == 12 {
            // D3DTSS_ADDRESS sets both U and V.
            if !record(d, |b| {
                b.tss.insert((st, 13), v);
                b.tss.insert((st, 14), v);
            }) {
                d.tss[st][13] = v;
                d.tss[st][14] = v;
            }
        }
        if !record(d, |b| {
            b.tss.insert((st, t), v);
        }) {
            d.tss[st][t] = v;
        }
    })?;
    Ok(ApiRet::Ret(D3D_OK))
}

fn dev_get_texture_stage_state(s: &mut Session) -> ApiResult {
    let [_this, stage, ty, p] = s.args::<4>()?;
    if stage >= 8 || ty >= 32 {
        return Ok(ApiRet::Ret(D3DERR_INVALIDCALL));
    }
    let v = with_dev(s, |d| d.tss[stage as usize][ty as usize])?;
    s.mem.write_u32(p, v)?;
    Ok(ApiRet::Ret(D3D_OK))
}

fn dev_validate_device(s: &mut Session) -> ApiResult {
    let [_this, p] = s.args::<2>()?;
    if p != 0 {
        s.mem.write_u32(p, 1)?;
    }
    Ok(ApiRet::Ret(D3D_OK))
}

fn dev_draw_primitive(s: &mut Session) -> ApiResult {
    let [_this, prim, fvf, verts, count, _flags] = s.args::<6>()?;
    if fvf_stride(fvf) == 0 {
        return Ok(ApiRet::Ret(D3DERR_INVALIDVERTEXTYPE));
    }
    let st = streams_from_fvf(verts, fvf);
    let v = read_vertices(s, &st, fvf, 0, count)?;
    Ok(ApiRet::Ret(submit_draw(s, prim, fvf, v, None)?))
}

fn read_indices16(s: &Session, p: u32, n: u32) -> Result<Vec<u32>, ApiError> {
    let raw = s.mem.read_vec(p, n * 2)?;
    Ok(raw.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]]) as u32).collect())
}

fn dev_draw_indexed_primitive(s: &mut Session) -> ApiResult {
    let [_this, prim, fvf, verts, vcount, idx, icount, _flags] = s.args::<8>()?;
    if fvf_stride(fvf) == 0 {
        return Ok(ApiRet::Ret(D3DERR_INVALIDVERTEXTYPE));
    }
    let st = streams_from_fvf(verts, fvf);
    let v = read_vertices(s, &st, fvf, 0, vcount)?;
    let i = read_indices16(s, idx, icount)?;
    Ok(ApiRet::Ret(submit_draw(s, prim, fvf, v, Some(i))?))
}

fn read_strided(s: &Session, p: u32) -> Result<Streams, ApiError> {
    let g = |o: u32| -> Result<(u32, u32), ApiError> { Ok((s.mem.read_u32(p + o)?, s.mem.read_u32(p + o + 4)?)) };
    let mut st = Streams {
        pos: g(0)?,
        normal: g(8)?,
        diffuse: g(16)?,
        specular: g(24)?,
        ..Default::default()
    };
    for i in 0..8 {
        st.tex[i] = g(32 + 8 * i as u32)?;
    }
    Ok(st)
}

fn mask_streams(mut st: Streams, fvf: u32) -> Streams {
    if fvf & 0x10 == 0 {
        st.normal = (0, 0);
    }
    if fvf & 0x40 == 0 {
        st.diffuse = (0, 0);
    }
    if fvf & 0x80 == 0 {
        st.specular = (0, 0);
    }
    let n = ((fvf >> 8) & 0xf) as usize;
    for i in n..8 {
        st.tex[i] = (0, 0);
    }
    st
}

fn dev_draw_primitive_strided(s: &mut Session) -> ApiResult {
    let [_this, prim, fvf, data, count, _flags] = s.args::<6>()?;
    let st = mask_streams(read_strided(s, data)?, fvf);
    let v = read_vertices(s, &st, fvf, 0, count)?;
    Ok(ApiRet::Ret(submit_draw(s, prim, fvf, v, None)?))
}

fn dev_draw_indexed_primitive_strided(s: &mut Session) -> ApiResult {
    let [_this, prim, fvf, data, vcount, idx, icount, _flags] = s.args::<8>()?;
    let st = mask_streams(read_strided(s, data)?, fvf);
    let v = read_vertices(s, &st, fvf, 0, vcount)?;
    let i = read_indices16(s, idx, icount)?;
    Ok(ApiRet::Ret(submit_draw(s, prim, fvf, v, Some(i))?))
}

fn vb_of(s: &Session, ptr: u32) -> Option<VertexBuffer> {
    match &s.com_obj(s.obj_of(ptr)?)?.kind {
        ComKind::VertexBuffer(v) => Some(v.clone()),
        _ => None,
    }
}

fn dev_draw_primitive_vb(s: &mut Session) -> ApiResult {
    let [_this, prim, vb, start, count, _flags] = s.args::<6>()?;
    let Some(vb) = vb_of(s, vb) else {
        return Ok(ApiRet::Ret(D3DERR_INVALIDCALL));
    };
    if vb.locks > 0 {
        return Ok(ApiRet::Ret(D3DERR_VERTEXBUFFERLOCKED));
    }
    if start + count > vb.count {
        return Ok(ApiRet::Ret(D3DERR_INVALIDCALL));
    }
    let st = streams_from_fvf(vb.mem, vb.fvf);
    let v = read_vertices(s, &st, vb.fvf, start, count)?;
    Ok(ApiRet::Ret(submit_draw(s, prim, vb.fvf, v, None)?))
}

fn dev_draw_indexed_primitive_vb(s: &mut Session) -> ApiResult {
    let [_this, prim, vb, start, nverts, idx, icount, _flags] = s.args::<8>()?;
    let Some(vb) = vb_of(s, vb) else {
        return Ok(ApiRet::Ret(D3DERR_INVALIDCALL));
    };
    if vb.locks > 0 {
        return Ok(ApiRet::Ret(D3DERR_VERTEXBUFFERLOCKED));
    }
    if start + nverts > vb.count {
        return Ok(ApiRet::Ret(D3DERR_INVALIDCALL));
    }
    let st = streams_from_fvf(vb.mem, vb.fvf);
    let v = read_vertices(s, &st, vb.fvf, start, nverts)?;
    let i = read_indices16(s, idx, icount)?;
    Ok(ApiRet::Ret(submit_draw(s, prim, vb.fvf, v, Some(i))?))
}

fn dev_set_clip_status(s: &mut Session) -> ApiResult {
    let [_this, p] = s.args::<2>()?;
    let b = s.mem.read_vec(p, 24)?;
    with_dev(s, |d| d.clip_status.copy_from_slice(&b))?;
    Ok(ApiRet::Ret(D3D_OK))
}
fn dev_get_clip_status(s: &mut Session) -> ApiResult {
    let [_this, p] = s.args::<2>()?;
    let b = with_dev(s, |d| d.clip_status)?;
    s.mem.write(p, &b)?;
    Ok(ApiRet::Ret(D3D_OK))
}

fn dev_begin_state_block(s: &mut Session) -> ApiResult {
    with_dev(s, |d| d.recording = Some(StateBlock::default()))?;
    Ok(ApiRet::Ret(D3D_OK))
}
fn dev_end_state_block(s: &mut Session) -> ApiResult {
    let [_this, p] = s.args::<2>()?;
    let h = with_dev(s, |d| {
        let b = d.recording.take().unwrap_or_default();
        let h = d.next_block;
        d.next_block += 1;
        d.blocks.insert(h, b);
        h
    })?;
    s.mem.write_u32(p, h)?;
    Ok(ApiRet::Ret(D3D_OK))
}
fn dev_apply_state_block(s: &mut Session) -> ApiResult {
    let [_this, h] = s.args::<2>()?;
    let Some(b) = with_dev(s, |d| d.blocks.get(&h).cloned())? else {
        return Ok(ApiRet::Ret(D3DERR_INVALIDCALL));
    };
    for (stage, t) in &b.textures {
        if *t != 0 {
            s.com_addref(*t);
        }
        let old = with_dev(s, |d| std::mem::replace(&mut d.textures[*stage], *t))?;
        if old != 0 {
            s.com_release(old);
        }
    }
    with_dev(s, |d| {
        for (k, v) in &b.rs {
            d.rs[*k] = *v;
        }
        for ((st, t), v) in &b.tss {
            d.tss[*st][*t] = *v;
        }
        for (k, m) in &b.transforms {
            d.transforms.insert(*k, *m);
        }
        if let Some(v) = b.viewport {
            d.viewport = v;
        }
        if let Some(m) = b.material {
            d.material = m;
        }
        for (k, l) in &b.lights {
            d.lights.insert(*k, *l);
        }
        for (k, e) in &b.light_enable {
            d.light_enable.insert(*k, *e);
        }
    })?;
    Ok(ApiRet::Ret(D3D_OK))
}
fn dev_capture_state_block(s: &mut Session) -> ApiResult {
    let [_this, h] = s.args::<2>()?;
    let ok = with_dev(s, |d| {
        let snapshot = d.clone();
        let Some(b) = d.blocks.get_mut(&h) else {
            return false;
        };
        for (k, v) in b.rs.iter_mut() {
            *v = snapshot.rs[*k];
        }
        for ((st, t), v) in b.tss.iter_mut() {
            *v = snapshot.tss[*st][*t];
        }
        for (k, v) in b.textures.iter_mut() {
            *v = snapshot.textures[*k];
        }
        for (k, m) in b.transforms.iter_mut() {
            *m = snapshot.transforms.get(k).copied().unwrap_or(IDENTITY);
        }
        if b.viewport.is_some() {
            b.viewport = Some(snapshot.viewport);
        }
        if b.material.is_some() {
            b.material = Some(snapshot.material);
        }
        for (k, l) in b.lights.iter_mut() {
            if let Some(x) = snapshot.lights.get(k) {
                *l = *x;
            }
        }
        for (k, e) in b.light_enable.iter_mut() {
            *e = snapshot.light_enable.get(k).copied().unwrap_or(false);
        }
        true
    })?;
    Ok(ApiRet::Ret(if ok { D3D_OK } else { D3DERR_INVALIDCALL }))
}
fn dev_delete_state_block(s: &mut Session) -> ApiResult {
    let [_this, h] = s.args::<2>()?;
    let ok = with_dev(s, |d| d.blocks.remove(&h).is_some())?;
    Ok(ApiRet::Ret(if ok { D3D_OK } else { D3DERR_INVALIDCALL }))
}
fn dev_create_state_block(s: &mut Session) -> ApiResult {
    let [_this, ty, p] = s.args::<3>()?;
    let h = with_dev(s, |d| {
        let mut b = StateBlock::default();
        // D3DSBT_ALL = 1, PIXELSTATE = 2, VERTEXSTATE = 3
        for k in 0..256usize {
            if ty == 1 || ty == 2 || ty == 3 {
                b.rs.insert(k, d.rs[k]);
            }
        }
        if ty == 1 || ty == 2 {
            for st in 0..8 {
                for t in 0..32 {
                    b.tss.insert((st, t), d.tss[st][t]);
                }
                b.textures.insert(st, d.textures[st]);
            }
        }
        if ty == 1 || ty == 3 {
            for (k, m) in &d.transforms {
                b.transforms.insert(*k, *m);
            }
            b.viewport = Some(d.viewport);
            b.material = Some(d.material);
            b.lights = d.lights.clone();
            b.light_enable = d.light_enable.clone();
        }
        let h = d.next_block;
        d.next_block += 1;
        d.blocks.insert(h, b);
        h
    })?;
    s.mem.write_u32(p, h)?;
    Ok(ApiRet::Ret(D3D_OK))
}

/// IDirect3DDevice7::Load: copy texture contents (whole mip chain for full loads).
fn dev_load(s: &mut Session) -> ApiResult {
    let [_this, dst_p, ppoint, src_p, prect, _flags] = s.args::<6>()?;
    let (Some(dobj), Some(sobj)) = (s.surface_obj_of_ptr(dst_p), s.surface_obj_of_ptr(src_p)) else {
        return Ok(ApiRet::Ret(D3DERR_INVALIDCALL));
    };
    let dchain = super::ddraw::mip_chain(s, dobj);
    let schain = super::ddraw::mip_chain(s, sobj);
    let (px, py) = if ppoint != 0 { (s.mem.read_i32(ppoint)?, s.mem.read_i32(ppoint + 4)?) } else { (0, 0) };
    let srect = if prect != 0 { Some(s.read_rect(prect)?) } else { None };
    // Source levels are matched to destination levels of the same size.
    for (k, d) in dchain.iter().enumerate() {
        let Some(did) = s.surface_of_obj(*d) else { continue };
        let (dw, dh, dfmt) = {
            let p = &s.display.surfaces[&did];
            (p.w, p.h, p.fmt)
        };
        let Some(sobj_l) = schain.iter().copied().find(|o| {
            s.surface_of_obj(*o).is_some_and(|id| {
                let p = &s.display.surfaces[&id];
                p.w == dw && p.h == dh
            })
        }) else {
            continue;
        };
        let sid = s.surface_of_obj(sobj_l).unwrap();
        if s.display.surfaces[&sid].fmt != dfmt {
            return Ok(ApiRet::Ret(super::ddraw::DDERR_INVALIDPIXELFORMAT));
        }
        let scale = 1i32 << k;
        let sr = match srect {
            Some(r) => Rect::new(r.left / scale, r.top / scale, (r.right / scale).max(r.left / scale + 1), (r.bottom / scale).max(r.top / scale + 1)),
            None => Rect::new(0, 0, dw as i32, dh as i32),
        };
        let dr = Rect::new(px / scale, py / scale, px / scale + sr.w(), py / scale + sr.h());
        super::ddraw::copy_rect(s, did, dr, sid, sr)?;
        if srect.is_some() && ppoint != 0 && k > 0 && prect != 0 {
            // Partial loads apply to the top level only.
            break;
        }
    }
    Ok(ApiRet::Ret(D3D_OK))
}

fn dev_preload(_s: &mut Session) -> ApiResult {
    Ok(ApiRet::Ret(D3D_OK))
}

fn dev_get_info(_s: &mut Session) -> ApiResult {
    // No driver statistics are exposed (S_FALSE as for drivers without the info).
    Ok(ApiRet::Ret(S_FALSE))
}

fn dev_compute_sphere_visibility(s: &mut Session) -> ApiResult {
    let [_this, centers, radii, n, _flags, results] = s.args::<6>()?;
    let d = dev_clone(s)?;
    let world = d.transforms.get(&1).copied().unwrap_or(IDENTITY);
    let view = d.transforms.get(&2).copied().unwrap_or(IDENTITY);
    let proj = d.transforms.get(&3).copied().unwrap_or(IDENTITY);
    let m = mat_mul(&mat_mul(&world, &view), &proj);
    for i in 0..n {
        let c = [s.mem.read_f32(centers + 12 * i)?, s.mem.read_f32(centers + 12 * i + 4)?, s.mem.read_f32(centers + 12 * i + 8)?];
        let r = s.mem.read_f32(radii + 4 * i)?;
        // Clip-space test of the sphere against the six frustum planes.
        let col = |j: usize| [m[j], m[4 + j], m[8 + j], m[12 + j]];
        let (c0, c1, c2, c3) = (col(0), col(1), col(2), col(3));
        let planes = [
            [c3[0] + c0[0], c3[1] + c0[1], c3[2] + c0[2], c3[3] + c0[3]],
            [c3[0] - c0[0], c3[1] - c0[1], c3[2] - c0[2], c3[3] - c0[3]],
            [c3[0] + c1[0], c3[1] + c1[1], c3[2] + c1[2], c3[3] + c1[3]],
            [c3[0] - c1[0], c3[1] - c1[1], c3[2] - c1[2], c3[3] - c1[3]],
            [c2[0], c2[1], c2[2], c2[3]],
            [c3[0] - c2[0], c3[1] - c2[1], c3[2] - c2[2], c3[3] - c2[3]],
        ];
        let mut res = 0u32;
        let mut all_in = true;
        for (k, p) in planes.iter().enumerate() {
            let len = (p[0] * p[0] + p[1] * p[1] + p[2] * p[2]).sqrt().max(1e-20);
            let dist = (p[0] * c[0] + p[1] * c[1] + p[2] * c[2] + p[3]) / len;
            if dist < -r {
                res |= 1 << (k + 12); // D3DSTATUS_CLIPUNION bits per plane
                all_in = false;
            } else if dist < r {
                all_in = false;
                res |= 1 << k;
            }
        }
        let _ = all_in;
        s.mem.write_u32(results + 4 * i, res)?;
    }
    Ok(ApiRet::Ret(D3D_OK))
}

// ---------------------------------------------------------------- vertex buffers

fn vb_self(s: &Session) -> Result<(u32, VertexBuffer), ApiError> {
    let id = s.this_obj()?;
    match &s.com_obj(id).unwrap().kind {
        ComKind::VertexBuffer(v) => Ok((id, v.clone())),
        _ => Err(ApiError::Fatal("not a vertex buffer".into())),
    }
}

fn vb_set(s: &mut Session, id: u32, v: VertexBuffer) {
    if let Some(o) = s.com_obj_mut(id) {
        o.kind = ComKind::VertexBuffer(v);
    }
}

fn vb_lock(s: &mut Session) -> ApiResult {
    let [_this, _flags, pp, psize] = s.args::<4>()?;
    let (id, mut v) = vb_self(s)?;
    s.mem.write_u32(pp, v.mem)?;
    if psize != 0 {
        s.mem.write_u32(psize, v.size)?;
    }
    v.locks += 1;
    vb_set(s, id, v);
    Ok(ApiRet::Ret(D3D_OK))
}

fn vb_unlock(s: &mut Session) -> ApiResult {
    let (id, mut v) = vb_self(s)?;
    v.locks = v.locks.saturating_sub(1);
    vb_set(s, id, v);
    Ok(ApiRet::Ret(D3D_OK))
}

fn vb_get_desc(s: &mut Session) -> ApiResult {
    let [_this, p] = s.args::<2>()?;
    let (_id, v) = vb_self(s)?;
    s.mem.write_u32(p + 4, v.caps)?;
    s.mem.write_u32(p + 8, v.fvf)?;
    s.mem.write_u32(p + 12, v.count)?;
    Ok(ApiRet::Ret(D3D_OK))
}

fn vb_optimize(s: &mut Session) -> ApiResult {
    let (id, mut v) = vb_self(s)?;
    v.caps |= 0x8000_0000; // D3DVBCAPS_OPTIMIZED
    vb_set(s, id, v);
    Ok(ApiRet::Ret(D3D_OK))
}

// ---------------------------------------------------------------- interface tables

pub static TEXTURE_IFACES: &[&IfaceDef] = &[];

pub static IDIRECT3D7: IfaceDef = IfaceDef {
    name: "IDirect3D7",
    iid: guid("f5049e77-4861-11d2-a407-00a0c90629a8"),
    module: "DDRAW.DLL",
    methods: &[
        iunknown!()[0],
        iunknown!()[1],
        iunknown!()[2],
        export!("EnumDevices", 3, d3d_enum_devices),
        export!("CreateDevice", 4, d3d_create_device),
        export!("CreateVertexBuffer", 4, d3d_create_vertex_buffer),
        export!("EnumZBufferFormats", 4, d3d_enum_zbuffer_formats),
        export!("EvictManagedTextures", 1, d3d_evict_managed_textures),
    ],
};

pub static IDIRECT3DDEVICE7: IfaceDef = IfaceDef {
    name: "IDirect3DDevice7",
    iid: guid("f5049e79-4861-11d2-a407-00a0c90629a8"),
    module: "DDRAW.DLL",
    methods: &[
        iunknown!()[0],
        iunknown!()[1],
        iunknown!()[2],
        export!("GetCaps", 2, dev_get_caps),
        export!("EnumTextureFormats", 3, dev_enum_texture_formats),
        export!("BeginScene", 1, dev_begin_scene),
        export!("EndScene", 1, dev_end_scene),
        export!("GetDirect3D", 2, dev_get_direct3d),
        export!("SetRenderTarget", 3, dev_set_render_target),
        export!("GetRenderTarget", 2, dev_get_render_target),
        export!("Clear", 7, dev_clear),
        export!("SetTransform", 3, dev_set_transform),
        export!("GetTransform", 3, dev_get_transform),
        export!("SetViewport", 2, dev_set_viewport),
        export!("MultiplyTransform", 3, dev_multiply_transform),
        export!("GetViewport", 2, dev_get_viewport),
        export!("SetMaterial", 2, dev_set_material),
        export!("GetMaterial", 2, dev_get_material),
        export!("SetLight", 3, dev_set_light),
        export!("GetLight", 3, dev_get_light),
        export!("SetRenderState", 3, dev_set_render_state),
        export!("GetRenderState", 3, dev_get_render_state),
        export!("BeginStateBlock", 1, dev_begin_state_block),
        export!("EndStateBlock", 2, dev_end_state_block),
        export!("PreLoad", 2, dev_preload),
        export!("DrawPrimitive", 6, dev_draw_primitive),
        export!("DrawIndexedPrimitive", 8, dev_draw_indexed_primitive),
        export!("SetClipStatus", 2, dev_set_clip_status),
        export!("GetClipStatus", 2, dev_get_clip_status),
        export!("DrawPrimitiveStrided", 6, dev_draw_primitive_strided),
        export!("DrawIndexedPrimitiveStrided", 8, dev_draw_indexed_primitive_strided),
        export!("DrawPrimitiveVB", 6, dev_draw_primitive_vb),
        export!("DrawIndexedPrimitiveVB", 8, dev_draw_indexed_primitive_vb),
        export!("ComputeSphereVisibility", 6, dev_compute_sphere_visibility),
        export!("GetTexture", 3, dev_get_texture),
        export!("SetTexture", 3, dev_set_texture),
        export!("GetTextureStageState", 4, dev_get_texture_stage_state),
        export!("SetTextureStageState", 4, dev_set_texture_stage_state),
        export!("ValidateDevice", 2, dev_validate_device),
        export!("ApplyStateBlock", 2, dev_apply_state_block),
        export!("CaptureStateBlock", 2, dev_capture_state_block),
        export!("DeleteStateBlock", 2, dev_delete_state_block),
        export!("CreateStateBlock", 3, dev_create_state_block),
        export!("Load", 6, dev_load),
        export!("LightEnable", 3, dev_light_enable),
        export!("GetLightEnable", 3, dev_get_light_enable),
        unimpl_method!("SetClipPlane", 3),
        unimpl_method!("GetClipPlane", 3),
        export!("GetInfo", 4, dev_get_info),
    ],
};

pub static IDIRECT3DVERTEXBUFFER7: IfaceDef = IfaceDef {
    name: "IDirect3DVertexBuffer7",
    iid: guid("f5049e7d-4861-11d2-a407-00a0c90629a8"),
    module: "DDRAW.DLL",
    methods: &[
        iunknown!()[0],
        iunknown!()[1],
        iunknown!()[2],
        export!("Lock", 4, vb_lock),
        export!("Unlock", 1, vb_unlock),
        unimpl_method!("ProcessVertices", 8),
        export!("GetVertexBufferDesc", 2, vb_get_desc),
        export!("Optimize", 3, vb_optimize),
        unimpl_method!("ProcessVerticesStrided", 8),
    ],
};

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fvf_strides() {
        assert_eq!(fvf_stride(0x1c4), 28); // XYZRHW|DIFFUSE|SPECULAR|TEX1
        assert_eq!(fvf_stride(0x112), 32); // XYZ|NORMAL|TEX1
        assert_eq!(fvf_stride(0x252), 32 + 8); // XYZ|NORMAL|DIFFUSE|TEX2
    }
    #[test]
    fn strip_and_fan_indices() {
        let (_, s) = primitive_indices(5, 5, None).unwrap();
        assert_eq!(s, vec![0, 1, 2, 2, 1, 3, 2, 3, 4]);
        let (_, f) = primitive_indices(6, 4, None).unwrap();
        assert_eq!(f, vec![0, 1, 2, 0, 2, 3]);
    }
    #[test]
    fn normal_matrix_of_uniform_scale() {
        let m = [2., 0., 0., 0., 0., 2., 0., 0., 0., 0., 2., 0., 0., 0., 0., 1.];
        let n = normal_matrix(&m);
        assert!((n[0] - 0.5).abs() < 1e-6);
    }
}
