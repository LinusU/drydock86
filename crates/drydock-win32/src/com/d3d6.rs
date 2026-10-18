//! Direct3D 6: IDirect3D3, IDirect3DDevice3, IDirect3DViewport3, IDirect3DMaterial3,
//! IDirect3DLight, IDirect3DTexture2 and IDirect3DVertexBuffer.
//!
//! These are layered on the Direct3D 7 device: a device created through IDirect3D3 is
//! an ordinary [`Device`] that additionally carries [`Dev3`]. State that Direct3D 6
//! keeps in separate objects (viewports with their lights, materials referenced by
//! handle, the light states and the legacy render states) is folded into the device
//! state before each draw.

use super::d3d::*;
use super::*;
use crate::dll::user32::Rect;
use crate::{export, iunknown, unimpl_method};
use std::collections::BTreeMap;

const RS_TEXTUREHANDLE: u32 = 1;
const RS_TEXTUREADDRESS: u32 = 3;
const RS_TEXTUREMAG: u32 = 17;
const RS_TEXTUREMIN: u32 = 18;
const RS_TEXTUREMAPBLEND: u32 = 21;
const RS_TEXTUREADDRESSU: u32 = 44;
const RS_TEXTUREADDRESSV: u32 = 45;
const RS_LIGHTING: usize = 137;

const D3DLIGHT_ACTIVE: u32 = 1;
const D3DLIGHT_NO_SPECULAR: u32 = 2;
const D3DDP_DONOTLIGHT: u32 = 0x10;

/// Direct3D 6 state of a device.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Dev3 {
    /// Viewports added to the device (object ids), in order.
    pub viewports: Vec<u32>,
    pub current: u32,
    /// D3DLIGHTSTATETYPE values 1..=8 (index 0 unused).
    pub light_state: [u32; 9],
    /// D3DRENDERSTATE_TEXTUREMAPBLEND, once the application has set it.
    pub map_blend: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Viewport3 {
    /// Device the viewport was added to (0 if none).
    pub device: u32,
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
    /// Clip volume: x, y (top), width, height.
    pub clip: [f32; 4],
    pub min_z: f32,
    pub max_z: f32,
    pub lights: Vec<u32>,
    /// Background material handle.
    pub background: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Material3 {
    pub mat: Material7,
    pub texture: u32,
    pub ramp: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Light3 {
    pub light: Light7,
    /// D3DLIGHT2 flags.
    pub flags: u32,
    /// The D3DLIGHT structure as last set (returned by GetLight).
    pub raw: Vec<u8>,
}

// ---------------------------------------------------------------- device descriptions

/// D3DDEVICEDESC (the Direct3D 6 layout, 252 bytes), derived from the Direct3D 7
/// description of the same device. `filled` false gives the empty description that
/// marks a device without that implementation (no hardware for the RGB device).
fn device_desc6(hal: bool, filled: bool) -> Vec<u8> {
    let mut b = vec![0u8; 252];
    b[0..4].copy_from_slice(&252u32.to_le_bytes());
    if !filled {
        return b;
    }
    let d7 = device_desc(false, hal);
    let g = |o: usize| u32::from_le_bytes(d7[o..o + 4].try_into().unwrap());
    let p = |b: &mut Vec<u8>, o: usize, v: u32| b[o..o + 4].copy_from_slice(&v.to_le_bytes());
    p(&mut b, 4, 0x7ff); // every D3DDD_* field is valid
    p(&mut b, 8, 2); // D3DCOLOR_RGB
    p(&mut b, 12, g(0));
    p(&mut b, 16, 8);
    p(&mut b, 20, 1); // D3DTRANSFORMCAPS_CLIP
    p(&mut b, 24, 1); // bClipping
    p(&mut b, 28, 16);
    p(&mut b, 32, 0x1 | 0x2 | 0x4); // point, spot, directional lights
    p(&mut b, 36, 1); // D3DLIGHTINGMODEL_RGB
    p(&mut b, 40, 8);
    b[44..100].copy_from_slice(&d7[4..60]);
    b[100..156].copy_from_slice(&d7[60..116]);
    // Direct3D 6 era hardware needs power-of-two texture sizes (D3DPTEXTURECAPS_POW2),
    // and games of that era rely on it: GOS splits wide bitmaps into tiles of limited
    // aspect ratio only when the device reports it.
    for o in [44 + 32, 100 + 32] {
        let caps = u32::from_le_bytes(b[o..o + 4].try_into().unwrap());
        p(&mut b, o, caps | 0x2);
    }
    p(&mut b, 156, g(116));
    p(&mut b, 160, g(120));
    p(&mut b, 164, 0);
    p(&mut b, 168, 65535);
    for (o6, o7) in [(172, 124), (176, 128), (180, 132), (184, 136)] {
        p(&mut b, o6, g(o7));
    }
    for (o6, o7) in [
        (204, 140),
        (208, 144),
        (212, 148),
        (216, 152),
        (220, 156),
        (224, 160),
        (228, 164),
    ] {
        p(&mut b, o6, g(o7));
    }
    for (o6, o7) in [(232, 168), (236, 172), (240, 176), (244, 180)] {
        p(&mut b, o6, g(o7));
    }
    b[248..252].copy_from_slice(&d7[184..188]);
    b
}

fn write_desc6(s: &mut Session, p: u32, desc: &[u8]) -> Result<(), ApiError> {
    if p == 0 {
        return Ok(());
    }
    let size = s.mem.read_u32(p)?.clamp(4, 252) as usize;
    s.mem.write(p + 4, &desc[4..size])?;
    Ok(())
}

// ---------------------------------------------------------------- IDirect3D3

fn d3_enum_devices(s: &mut Session) -> ApiResult {
    let [_this, cb, ctx] = s.args::<3>()?;
    let dev = |guid: Guid, desc: &str, name: &str, hal: bool| {
        vec![
            CallArg::Buf(guid.to_vec()),
            cstr(desc),
            cstr(name),
            CallArg::Buf(device_desc6(true, hal)),
            CallArg::Buf(device_desc6(hal, true)),
            CallArg::Val(ctx),
        ]
    };
    let items = vec![
        dev(
            IID_RGB_DEVICE,
            "Microsoft Direct3D RGB Software Emulation",
            "RGB Emulation",
            false,
        ),
        dev(
            IID_HAL_DEVICE,
            "Microsoft Direct3D Hardware acceleration through Direct3D HAL",
            "Direct3D HAL",
            true,
        ),
    ];
    enum_callbacks(cb, items, 0, D3D_OK)
}

fn no_aggregation(s: &mut Session, pp: u32, outer: u32) -> Result<Option<u32>, ApiError> {
    if outer != 0 {
        if pp != 0 {
            s.mem.write_u32(pp, 0)?;
        }
        return Ok(Some(0x8004_0110)); // CLASS_E_NOAGGREGATION
    }
    Ok(None)
}

fn d3_create_light(s: &mut Session) -> ApiResult {
    let [_this, pp, outer] = s.args::<3>()?;
    if let Some(e) = no_aggregation(s, pp, outer)? {
        return Ok(ApiRet::Ret(e));
    }
    let id = s.com_new(ComKind::Light3(Box::default()));
    s.out_iface(pp, id, &IDIRECT3DLIGHT)?;
    Ok(ApiRet::Ret(D3D_OK))
}

fn d3_create_material(s: &mut Session) -> ApiResult {
    let [_this, pp, outer] = s.args::<3>()?;
    if let Some(e) = no_aggregation(s, pp, outer)? {
        return Ok(ApiRet::Ret(e));
    }
    let id = s.com_new(ComKind::Material3(Material3::default()));
    s.out_iface(pp, id, &IDIRECT3DMATERIAL3)?;
    Ok(ApiRet::Ret(D3D_OK))
}

fn d3_create_viewport(s: &mut Session) -> ApiResult {
    let [_this, pp, outer] = s.args::<3>()?;
    if let Some(e) = no_aggregation(s, pp, outer)? {
        return Ok(ApiRet::Ret(e));
    }
    let id = s.com_new(ComKind::Viewport3(Box::new(Viewport3 {
        device: 0,
        x: 0,
        y: 0,
        w: 0,
        h: 0,
        clip: [-1.0, 1.0, 2.0, 2.0],
        min_z: 0.0,
        max_z: 1.0,
        lights: Vec::new(),
        background: 0,
    })));
    s.out_iface(pp, id, &IDIRECT3DVIEWPORT3)?;
    Ok(ApiRet::Ret(D3D_OK))
}

fn d3_create_device(s: &mut Session) -> ApiResult {
    let [_this, clsid_p, target_p, pp, outer] = s.args::<5>()?;
    if let Some(e) = no_aggregation(s, pp, outer)? {
        return Ok(ApiRet::Ret(e));
    }
    let clsid = s.read_guid(clsid_p)?;
    if clsid != IID_RGB_DEVICE && clsid != IID_HAL_DEVICE {
        return Ok(ApiRet::Ret(D3DERR_INVALID_DEVICE));
    }
    let v3 = Dev3 {
        // D3DLIGHTSTATE_COLORVERTEX defaults to TRUE.
        light_state: [0, 0, 0, 2, 0, 0, 0, 0, 1],
        ..Default::default()
    };
    create_device(s, clsid, target_p, pp, &IDIRECT3DDEVICE3, Some(Box::new(v3)))
}

fn d3_create_vertex_buffer(s: &mut Session) -> ApiResult {
    let [_this, pdesc, pp, _flags, outer] = s.args::<5>()?;
    if let Some(e) = no_aggregation(s, pp, outer)? {
        return Ok(ApiRet::Ret(e));
    }
    create_vertex_buffer(s, pdesc, pp, &IDIRECT3DVERTEXBUFFER)
}

// ---------------------------------------------------------------- device state folding

fn viewport_of(s: &Session, id: u32) -> Option<Viewport3> {
    match &s.com_obj(id)?.kind {
        ComKind::Viewport3(v) => Some((**v).clone()),
        _ => None,
    }
}

fn material_of(s: &Session, handle: u32) -> Option<Material3> {
    match &s.com_obj(handle)?.kind {
        ComKind::Material3(m) => Some(m.clone()),
        _ => None,
    }
}

fn v3_of(d: &Device) -> Dev3 {
    d.v3.as_deref().cloned().unwrap_or_default()
}

/// Matrix applied after the projection that maps a viewport's clip volume onto the
/// standard one (x in [-1, 1], y in [-1, 1]), or None for the standard volume.
fn clip_adjust(clip: [f32; 4]) -> Option<[f32; 16]> {
    let [cx, cy, cw, ch] = clip;
    if (cx, cy, cw, ch) == (-1.0, 1.0, 2.0, 2.0) || cw == 0.0 || ch == 0.0 {
        return None;
    }
    let mut m = IDENTITY;
    m[0] = 2.0 / cw;
    m[12] = -(2.0 * cx / cw + 1.0);
    m[5] = 2.0 / ch;
    m[13] = -(2.0 * (cy - ch) / ch + 1.0);
    Some(m)
}

/// Fold the Direct3D 6 objects into the device before a draw with `fvf`/`flags`.
fn sync3(s: &mut Session, fvf: u32, flags: u32) -> Result<(), ApiError> {
    let d = dev_clone(s)?;
    let v3 = v3_of(&d);
    let vp = viewport_of(s, v3.current);
    let mut lights = BTreeMap::new();
    let mut enable = BTreeMap::new();
    if let Some(v) = &vp {
        for (i, l) in v.lights.iter().enumerate() {
            if let Some(ComKind::Light3(x)) = s.com_obj(*l).map(|o| &o.kind)
                && x.flags & D3DLIGHT_ACTIVE != 0
                && (1..=3).contains(&x.light.kind)
            {
                lights.insert(i as u32, x.light);
                enable.insert(i as u32, true);
            }
        }
    }
    let mat = material_of(s, v3.light_state[1]).map(|m| m.mat);
    // MODULATE takes the texture's alpha when it has one, else the vertex alpha.
    let tex_alpha = d.textures[0] != 0
        && s.surface_of_obj(d.textures[0])
            .and_then(|id| s.display.surfaces.get(&id))
            .is_some_and(|p| p.fmt.has_alpha());
    with_dev(s, |d| {
        if let Some(v) = &vp {
            d.viewport = Viewport7 {
                x: v.x,
                y: v.y,
                w: v.w,
                h: v.h,
                min_z: v.min_z,
                max_z: v.max_z,
            };
            d.proj_adjust = clip_adjust(v.clip);
        }
        d.lights = lights;
        d.light_enable = enable;
        if let Some(m) = mat {
            d.material = m;
        }
        // Direct3D 6 lights vertices that carry normals unless told not to.
        d.rs[RS_LIGHTING] = (fvf & 0x10 != 0 && flags & D3DDP_DONOTLIGHT == 0) as u32;
        if matches!(v3.map_blend, Some(2)) {
            d.tss[0][4] = if tex_alpha { 2 } else { 3 };
        }
    })
}

// ---------------------------------------------------------------- IDirect3DDevice3

fn dev3_get_caps(s: &mut Session) -> ApiResult {
    let [_this, hal, hel] = s.args::<3>()?;
    let d = dev_clone(s)?;
    let is_hal = d.kind == IID_HAL_DEVICE;
    write_desc6(s, hal, &device_desc6(true, is_hal))?;
    write_desc6(s, hel, &device_desc6(is_hal, true))?;
    Ok(ApiRet::Ret(D3D_OK))
}

fn dev3_get_stats(s: &mut Session) -> ApiResult {
    let [_this, p] = s.args::<2>()?;
    // D3DSTATS: no counters are kept.
    let size = s.mem.read_u32(p)?.clamp(4, 28);
    s.mem.write(p + 4, &vec![0u8; size as usize - 4])?;
    Ok(ApiRet::Ret(D3D_OK))
}

fn dev3_add_viewport(s: &mut Session) -> ApiResult {
    let [_this, vp] = s.args::<2>()?;
    let dev = dev_id(s)?;
    let Some(v) = s.obj_of(vp).filter(|o| viewport_of(s, *o).is_some()) else {
        return Ok(ApiRet::Ret(D3DERR_INVALIDCALL));
    };
    if let Some(ComKind::Viewport3(x)) = s.com_obj_mut(v).map(|o| &mut o.kind) {
        x.device = dev;
    }
    let added = with_dev(s, |d| {
        let v3 = d.v3.get_or_insert_with(Default::default);
        if v3.viewports.contains(&v) {
            false
        } else {
            v3.viewports.push(v);
            true
        }
    })?;
    if !added {
        return Ok(ApiRet::Ret(d3derr(762))); // D3DERR_VIEWPORTHASNODEVICE family
    }
    s.com_addref(v);
    Ok(ApiRet::Ret(D3D_OK))
}

fn dev3_delete_viewport(s: &mut Session) -> ApiResult {
    let [_this, vp] = s.args::<2>()?;
    let Some(v) = s.obj_of(vp) else {
        return Ok(ApiRet::Ret(D3DERR_INVALIDCALL));
    };
    let removed = with_dev(s, |d| {
        let v3 = d.v3.get_or_insert_with(Default::default);
        let n = v3.viewports.len();
        v3.viewports.retain(|&x| x != v);
        if v3.current == v {
            v3.current = 0;
        }
        v3.viewports.len() != n
    })?;
    if !removed {
        return Ok(ApiRet::Ret(D3DERR_INVALIDCALL));
    }
    if let Some(ComKind::Viewport3(x)) = s.com_obj_mut(v).map(|o| &mut o.kind) {
        x.device = 0;
    }
    s.com_release(v);
    Ok(ApiRet::Ret(D3D_OK))
}

fn dev3_next_viewport(s: &mut Session) -> ApiResult {
    let [_this, vp, pp, flags] = s.args::<4>()?;
    let list = with_dev(s, |d| v3_of(d).viewports)?;
    let next = match flags {
        2 => list.first().copied(),
        4 => list.last().copied(),
        _ => {
            let cur = s.obj_of(vp);
            list.iter()
                .position(|&x| Some(x) == cur)
                .and_then(|i| list.get(i + 1))
                .copied()
        }
    };
    match next {
        Some(v) => {
            s.out_iface(pp, v, &IDIRECT3DVIEWPORT3)?;
            s.com_addref(v);
            Ok(ApiRet::Ret(D3D_OK))
        }
        None => {
            s.mem.write_u32(pp, 0)?;
            Ok(ApiRet::Ret(D3DERR_INVALIDCALL))
        }
    }
}

fn dev3_get_direct3d(s: &mut Session) -> ApiResult {
    let [_this, pp] = s.args::<2>()?;
    let d = dev_clone(s)?;
    s.out_iface(pp, d.ddraw, &IDIRECT3D3)?;
    s.com_addref(d.ddraw);
    Ok(ApiRet::Ret(D3D_OK))
}

fn dev3_set_current_viewport(s: &mut Session) -> ApiResult {
    let [_this, vp] = s.args::<2>()?;
    let Some(v) = s.obj_of(vp) else {
        return Ok(ApiRet::Ret(D3DERR_INVALIDCALL));
    };
    let ok = with_dev(s, |d| {
        let v3 = d.v3.get_or_insert_with(Default::default);
        if v3.viewports.contains(&v) {
            v3.current = v;
            true
        } else {
            false
        }
    })?;
    if !ok {
        return Ok(ApiRet::Ret(D3DERR_INVALIDCALL));
    }
    sync3(s, 0, 0)?;
    Ok(ApiRet::Ret(D3D_OK))
}

fn dev3_get_current_viewport(s: &mut Session) -> ApiResult {
    let [_this, pp] = s.args::<2>()?;
    let cur = with_dev(s, |d| v3_of(d).current)?;
    if cur == 0 || s.com_obj(cur).is_none() {
        s.mem.write_u32(pp, 0)?;
        return Ok(ApiRet::Ret(d3derr(761))); // D3DERR_NOCURRENTVIEWPORT
    }
    s.out_iface(pp, cur, &IDIRECT3DVIEWPORT3)?;
    s.com_addref(cur);
    Ok(ApiRet::Ret(D3D_OK))
}

fn dev3_get_render_target(s: &mut Session) -> ApiResult {
    let [_this, pp] = s.args::<2>()?;
    let d = dev_clone(s)?;
    s.out_iface(pp, d.target, &super::ddraw::IDIRECTDRAWSURFACE4)?;
    s.com_addref(d.target);
    Ok(ApiRet::Ret(D3D_OK))
}

/// Set texture stage `stage`'s colour/alpha operations for a legacy
/// D3DRENDERSTATE_TEXTUREMAPBLEND mode.
fn map_blend_ops(mode: u32) -> Option<(u32, u32, u32, u32, u32, u32)> {
    // (colorop, colorarg1, colorarg2, alphaop, alphaarg1, alphaarg2); D3DTA_DIFFUSE 0,
    // D3DTA_TEXTURE 2.
    Some(match mode {
        1 | 5 | 7 => (2, 2, 0, 2, 2, 0), // DECAL, DECALMASK, COPY
        2 | 6 => (4, 2, 0, 2, 2, 0),     // MODULATE, MODULATEMASK (alpha resolved per draw)
        3 => (13, 2, 0, 3, 2, 0),        // DECALALPHA
        4 => (4, 2, 0, 4, 2, 0),         // MODULATEALPHA
        8 => (7, 2, 0, 3, 2, 0),         // ADD
        _ => return None,
    })
}

fn dev3_set_render_state(s: &mut Session) -> ApiResult {
    let [_this, st, v] = s.args::<3>()?;
    match st {
        RS_TEXTUREHANDLE => {
            let new = if v == 0 {
                0
            } else {
                match s.com_obj(v).map(|o| &o.kind) {
                    Some(ComKind::Surface(_)) => v,
                    _ => return Ok(ApiRet::Ret(D3DERR_INVALIDCALL)),
                }
            };
            set_stage_texture(s, 0, new)?;
        }
        RS_TEXTUREADDRESS | RS_TEXTUREADDRESSU | RS_TEXTUREADDRESSV => {
            with_dev(s, |d| {
                if st != RS_TEXTUREADDRESSV {
                    d.tss[0][13] = v;
                }
                if st != RS_TEXTUREADDRESSU {
                    d.tss[0][14] = v;
                }
            })?;
        }
        RS_TEXTUREMAG => {
            with_dev(s, |d| d.tss[0][16] = if v == 2 { 2 } else { 1 })?;
        }
        RS_TEXTUREMIN => {
            // D3DFILTER_*: NEAREST, LINEAR, MIPNEAREST, MIPLINEAR, LINEARMIPNEAREST,
            // LINEARMIPLINEAR → (minification filter, mip filter).
            let (min, mip) = match v {
                2 => (2, 1),
                3 => (1, 2),
                4 => (2, 2),
                5 => (1, 3),
                6 => (2, 3),
                _ => (1, 1),
            };
            with_dev(s, |d| {
                d.tss[0][17] = min;
                d.tss[0][18] = mip;
            })?;
        }
        RS_TEXTUREMAPBLEND => {
            let Some((cop, ca1, ca2, aop, aa1, aa2)) = map_blend_ops(v) else {
                s.missing("SetRenderState", format!("TEXTUREMAPBLEND {v}"));
                return Ok(ApiRet::Ret(D3DERR_INVALIDCALL));
            };
            with_dev(s, |d| {
                let t = &mut d.tss[0];
                t[1] = cop;
                t[2] = ca1;
                t[3] = ca2;
                t[4] = aop;
                t[5] = aa1;
                t[6] = aa2;
                d.v3.get_or_insert_with(Default::default).map_blend = Some(v);
            })?;
        }
        _ => return dev_set_render_state(s),
    }
    with_dev(s, |d| {
        if (st as usize) < d.rs.len() {
            d.rs[st as usize] = v;
        }
    })?;
    Ok(ApiRet::Ret(D3D_OK))
}

fn set_stage_texture(s: &mut Session, stage: usize, new: u32) -> Result<(), ApiError> {
    if new != 0 {
        s.com_addref(new);
    }
    let old = with_dev(s, |d| std::mem::replace(&mut d.textures[stage], new))?;
    if old != 0 {
        s.com_release(old);
    }
    Ok(())
}

fn dev3_get_light_state(s: &mut Session) -> ApiResult {
    let [_this, ty, p] = s.args::<3>()?;
    if !(1..=8).contains(&ty) {
        return Ok(ApiRet::Ret(D3DERR_INVALIDCALL));
    }
    let v = with_dev(s, |d| v3_of(d).light_state[ty as usize])?;
    s.mem.write_u32(p, v)?;
    Ok(ApiRet::Ret(D3D_OK))
}

fn dev3_set_light_state(s: &mut Session) -> ApiResult {
    let [_this, ty, v] = s.args::<3>()?;
    if !(1..=8).contains(&ty) {
        return Ok(ApiRet::Ret(D3DERR_INVALIDCALL));
    }
    if ty == 1 && v != 0 && material_of(s, v).is_none() {
        return Ok(ApiRet::Ret(D3DERR_INVALIDCALL));
    }
    with_dev(s, |d| {
        d.v3.get_or_insert_with(Default::default).light_state[ty as usize] = v;
        // The light states that Direct3D 7 made render states.
        match ty {
            2 => d.rs[139] = v, // AMBIENT
            4 => d.rs[140] = v, // FOGMODE → FOGVERTEXMODE
            5 => d.rs[36] = v,  // FOGSTART
            6 => d.rs[37] = v,  // FOGEND
            7 => d.rs[38] = v,  // FOGDENSITY
            8 => d.rs[141] = v, // COLORVERTEX
            _ => {}
        }
    })?;
    Ok(ApiRet::Ret(D3D_OK))
}

fn dev3_draw_primitive(s: &mut Session) -> ApiResult {
    let [_this, _prim, fvf, _verts, _count, flags] = s.args::<6>()?;
    sync3(s, fvf, flags)?;
    dev_draw_primitive(s)
}

fn dev3_draw_indexed_primitive(s: &mut Session) -> ApiResult {
    let [_this, _prim, fvf, _verts, _vcount, _idx, _icount, flags] = s.args::<8>()?;
    sync3(s, fvf, flags)?;
    dev_draw_indexed_primitive(s)
}

fn dev3_draw_primitive_strided(s: &mut Session) -> ApiResult {
    let [_this, _prim, fvf, _data, _count, flags] = s.args::<6>()?;
    sync3(s, fvf, flags)?;
    dev_draw_primitive_strided(s)
}

fn dev3_draw_indexed_primitive_strided(s: &mut Session) -> ApiResult {
    let [_this, _prim, fvf, _data, _vcount, _idx, _icount, flags] = s.args::<8>()?;
    sync3(s, fvf, flags)?;
    dev_draw_indexed_primitive_strided(s)
}

fn dev3_draw_primitive_vb(s: &mut Session) -> ApiResult {
    let [_this, _prim, vb, _start, _count, flags] = s.args::<6>()?;
    let fvf = vb_of(s, vb).map(|v| v.fvf).unwrap_or(0);
    sync3(s, fvf, flags)?;
    dev_draw_primitive_vb(s)
}

fn dev3_draw_indexed_primitive_vb(s: &mut Session) -> ApiResult {
    let [_this, prim, vb, idx, icount, flags] = s.args::<6>()?;
    let Some(vb) = vb_of(s, vb) else {
        return Ok(ApiRet::Ret(D3DERR_INVALIDCALL));
    };
    if vb.locks > 0 {
        return Ok(ApiRet::Ret(D3DERR_VERTEXBUFFERLOCKED));
    }
    sync3(s, vb.fvf, flags)?;
    let st = streams_from_fvf(vb.mem, vb.fvf);
    let v = read_vertices(s, &st, vb.fvf, 0, vb.count)?;
    let i = read_indices16(s, idx, icount)?;
    Ok(ApiRet::Ret(submit_draw(s, prim, vb.fvf, v, Some(i))?))
}

fn dev3_get_texture(s: &mut Session) -> ApiResult {
    let [_this, stage, pp] = s.args::<3>()?;
    if stage >= 8 {
        return Ok(ApiRet::Ret(D3DERR_INVALIDCALL));
    }
    let t = with_dev(s, |d| d.textures[stage as usize])?;
    if t == 0 {
        s.mem.write_u32(pp, 0)?;
    } else {
        s.out_iface(pp, t, &IDIRECT3DTEXTURE2)?;
        s.com_addref(t);
    }
    Ok(ApiRet::Ret(D3D_OK))
}

// ---------------------------------------------------------------- IDirect3DViewport3

fn vp_self(s: &Session) -> Result<(u32, Viewport3), ApiError> {
    let id = s.this_obj()?;
    let v = viewport_of(s, id).ok_or_else(|| ApiError::Fatal("not a viewport".into()))?;
    Ok((id, v))
}

fn vp_update(s: &mut Session, id: u32, f: impl FnOnce(&mut Viewport3)) {
    if let Some(ComKind::Viewport3(v)) = s.com_obj_mut(id).map(|o| &mut o.kind) {
        f(v);
    }
}

/// Re-apply a viewport that is current on its device.
fn vp_changed(s: &mut Session, id: u32) -> Result<(), ApiError> {
    let Some(v) = viewport_of(s, id) else { return Ok(()) };
    let current = match s.com_obj(v.device).map(|o| &o.kind) {
        Some(ComKind::Device(d)) => d.v3.as_ref().is_some_and(|x| x.current == id),
        _ => false,
    };
    if current && let Some(ComKind::Device(d)) = s.com_obj_mut(v.device).map(|o| &mut o.kind) {
        d.viewport = Viewport7 {
            x: v.x,
            y: v.y,
            w: v.w,
            h: v.h,
            min_z: v.min_z,
            max_z: v.max_z,
        };
        d.proj_adjust = clip_adjust(v.clip);
    }
    Ok(())
}

fn vp_initialize(_s: &mut Session) -> ApiResult {
    Ok(ApiRet::Ret(super::ddraw::dderr(5))) // DDERR_ALREADYINITIALIZED
}

fn vp_get_viewport(s: &mut Session) -> ApiResult {
    let [_this, p] = s.args::<2>()?;
    let (_id, v) = vp_self(s)?;
    let max_x = v.clip[2] / 2.0;
    let max_y = v.clip[3] / 2.0;
    let vals: [u32; 10] = [
        v.x,
        v.y,
        v.w,
        v.h,
        (v.w as f32 / v.clip[2].max(1e-6)).to_bits(),
        (v.h as f32 / v.clip[3].max(1e-6)).to_bits(),
        max_x.to_bits(),
        max_y.to_bits(),
        v.min_z.to_bits(),
        v.max_z.to_bits(),
    ];
    for (i, x) in vals.iter().enumerate() {
        s.mem.write_u32(p + 4 + 4 * i as u32, *x)?;
    }
    Ok(ApiRet::Ret(D3D_OK))
}

fn vp_set_viewport(s: &mut Session) -> ApiResult {
    let [_this, p] = s.args::<2>()?;
    let (id, _) = vp_self(s)?;
    let g = |o: u32| s.mem.read_u32(p + o);
    let f = |o: u32| s.mem.read_f32(p + o);
    // D3DVIEWPORT: the clip volume is [-MaxX, MaxX] × [-MaxY, MaxY].
    let (x, y, w, h) = (g(4)?, g(8)?, g(12)?, g(16)?);
    let (max_x, max_y, min_z, max_z) = (f(28)?, f(32)?, f(36)?, f(40)?);
    vp_update(s, id, |v| {
        v.x = x;
        v.y = y;
        v.w = w;
        v.h = h;
        v.clip = [-max_x, max_y, 2.0 * max_x, 2.0 * max_y];
        v.min_z = min_z;
        v.max_z = max_z;
    });
    vp_changed(s, id)?;
    Ok(ApiRet::Ret(D3D_OK))
}

fn vp_get_viewport2(s: &mut Session) -> ApiResult {
    let [_this, p] = s.args::<2>()?;
    let (_id, v) = vp_self(s)?;
    let vals: [u32; 10] = [
        v.x,
        v.y,
        v.w,
        v.h,
        v.clip[0].to_bits(),
        v.clip[1].to_bits(),
        v.clip[2].to_bits(),
        v.clip[3].to_bits(),
        v.min_z.to_bits(),
        v.max_z.to_bits(),
    ];
    for (i, x) in vals.iter().enumerate() {
        s.mem.write_u32(p + 4 + 4 * i as u32, *x)?;
    }
    Ok(ApiRet::Ret(D3D_OK))
}

fn vp_set_viewport2(s: &mut Session) -> ApiResult {
    let [_this, p] = s.args::<2>()?;
    let (id, _) = vp_self(s)?;
    let g = |o: u32| s.mem.read_u32(p + o);
    let f = |o: u32| s.mem.read_f32(p + o);
    let (x, y, w, h) = (g(4)?, g(8)?, g(12)?, g(16)?);
    let clip = [f(20)?, f(24)?, f(28)?, f(32)?];
    let (min_z, max_z) = (f(36)?, f(40)?);
    vp_update(s, id, |v| {
        v.x = x;
        v.y = y;
        v.w = w;
        v.h = h;
        v.clip = clip;
        v.min_z = min_z;
        v.max_z = max_z;
    });
    vp_changed(s, id)?;
    Ok(ApiRet::Ret(D3D_OK))
}

fn vp_set_background(s: &mut Session) -> ApiResult {
    let [_this, h] = s.args::<2>()?;
    let (id, _) = vp_self(s)?;
    if h != 0 && material_of(s, h).is_none() {
        return Ok(ApiRet::Ret(D3DERR_INVALIDCALL));
    }
    vp_update(s, id, |v| v.background = h);
    Ok(ApiRet::Ret(D3D_OK))
}

fn vp_get_background(s: &mut Session) -> ApiResult {
    let [_this, ph, pvalid] = s.args::<3>()?;
    let (_id, v) = vp_self(s)?;
    s.mem.write_u32(ph, v.background)?;
    if pvalid != 0 {
        s.mem.write_u32(pvalid, (v.background != 0) as u32)?;
    }
    Ok(ApiRet::Ret(D3D_OK))
}

fn vp_clear_common(s: &mut Session, count: u32, rects: u32, flags: u32, color: u32, z: u32) -> ApiResult {
    let (_id, v) = vp_self(s)?;
    let target = match s.com_obj(v.device).map(|o| &o.kind) {
        Some(ComKind::Device(d)) => d.target,
        _ => return Ok(ApiRet::Ret(d3derr(762))), // D3DERR_VIEWPORTHASNODEVICE
    };
    let vp = Rect::new(v.x as i32, v.y as i32, (v.x + v.w) as i32, (v.y + v.h) as i32);
    Ok(ApiRet::Ret(clear_rects(s, target, vp, count, rects, flags, color, z)?))
}

fn vp_clear(s: &mut Session) -> ApiResult {
    let [_this, count, rects, flags] = s.args::<4>()?;
    let (_id, v) = vp_self(s)?;
    // The target is cleared to the background material's diffuse colour.
    let color = material_of(s, v.background)
        .map(|m| {
            let c = m.mat.diffuse;
            let b = |x: f32| (x.clamp(0.0, 1.0) * 255.0).round() as u32;
            (b(c[3]) << 24) | (b(c[0]) << 16) | (b(c[1]) << 8) | b(c[2])
        })
        .unwrap_or(0);
    vp_clear_common(s, count, rects, flags, color, 1.0f32.to_bits())
}

fn vp_clear2(s: &mut Session) -> ApiResult {
    let [_this, count, rects, flags, color, z, _stencil] = s.args::<7>()?;
    vp_clear_common(s, count, rects, flags, color, z)
}

fn vp_add_light(s: &mut Session) -> ApiResult {
    let [_this, lp] = s.args::<2>()?;
    let (id, v) = vp_self(s)?;
    let Some(l) = s
        .obj_of(lp)
        .filter(|o| matches!(s.com_obj(*o).map(|x| &x.kind), Some(ComKind::Light3(_))))
    else {
        return Ok(ApiRet::Ret(D3DERR_INVALIDCALL));
    };
    if v.lights.contains(&l) {
        return Ok(ApiRet::Ret(D3DERR_INVALIDCALL));
    }
    vp_update(s, id, |v| v.lights.push(l));
    s.com_addref(l);
    Ok(ApiRet::Ret(D3D_OK))
}

fn vp_delete_light(s: &mut Session) -> ApiResult {
    let [_this, lp] = s.args::<2>()?;
    let (id, v) = vp_self(s)?;
    let Some(l) = s.obj_of(lp).filter(|o| v.lights.contains(o)) else {
        return Ok(ApiRet::Ret(D3DERR_INVALIDCALL));
    };
    vp_update(s, id, |v| v.lights.retain(|&x| x != l));
    s.com_release(l);
    Ok(ApiRet::Ret(D3D_OK))
}

fn vp_next_light(s: &mut Session) -> ApiResult {
    let [_this, lp, pp, flags] = s.args::<4>()?;
    let (_id, v) = vp_self(s)?;
    let next = match flags {
        2 => v.lights.first().copied(),
        4 => v.lights.last().copied(),
        _ => {
            let cur = s.obj_of(lp);
            v.lights
                .iter()
                .position(|&x| Some(x) == cur)
                .and_then(|i| v.lights.get(i + 1))
                .copied()
        }
    };
    match next {
        Some(l) => {
            s.out_iface(pp, l, &IDIRECT3DLIGHT)?;
            s.com_addref(l);
            Ok(ApiRet::Ret(D3D_OK))
        }
        None => {
            s.mem.write_u32(pp, 0)?;
            Ok(ApiRet::Ret(D3DERR_INVALIDCALL))
        }
    }
}

// ---------------------------------------------------------------- IDirect3DMaterial3

fn mat_self(s: &Session) -> Result<(u32, Material3), ApiError> {
    let id = s.this_obj()?;
    let m = material_of(s, id).ok_or_else(|| ApiError::Fatal("not a material".into()))?;
    Ok((id, m))
}

fn mat_set_material(s: &mut Session) -> ApiResult {
    let [_this, p] = s.args::<2>()?;
    let (id, _) = mat_self(s)?;
    // D3DMATERIAL: dwSize, diffuse, ambient, specular, emissive, power, hTexture,
    // dwRampSize.
    let m = Material3 {
        mat: Material7 {
            diffuse: read_color4(s, p + 4)?,
            ambient: read_color4(s, p + 20)?,
            specular: read_color4(s, p + 36)?,
            emissive: read_color4(s, p + 52)?,
            power: s.mem.read_f32(p + 68)?,
        },
        texture: s.mem.read_u32(p + 72)?,
        ramp: s.mem.read_u32(p + 76)?,
    };
    if let Some(o) = s.com_obj_mut(id) {
        o.kind = ComKind::Material3(m);
    }
    Ok(ApiRet::Ret(D3D_OK))
}

fn mat_get_material(s: &mut Session) -> ApiResult {
    let [_this, p] = s.args::<2>()?;
    let (_id, m) = mat_self(s)?;
    let c = &m.mat;
    for (k, col) in [c.diffuse, c.ambient, c.specular, c.emissive].iter().enumerate() {
        for (i, v) in col.iter().enumerate() {
            s.mem.write_f32(p + 4 + 16 * k as u32 + 4 * i as u32, *v)?;
        }
    }
    s.mem.write_f32(p + 68, c.power)?;
    s.mem.write_u32(p + 72, m.texture)?;
    s.mem.write_u32(p + 76, m.ramp)?;
    Ok(ApiRet::Ret(D3D_OK))
}

/// Material and texture handles are the object ids.
fn get_handle(s: &mut Session) -> ApiResult {
    let [_this, _dev, ph] = s.args::<3>()?;
    let id = s.this_obj()?;
    s.mem.write_u32(ph, id)?;
    Ok(ApiRet::Ret(D3D_OK))
}

// ---------------------------------------------------------------- IDirect3DLight

fn light_self(s: &Session) -> Result<(u32, Light3), ApiError> {
    let id = s.this_obj()?;
    match &s.com_obj(id).map(|o| &o.kind) {
        Some(ComKind::Light3(l)) => Ok((id, (**l).clone())),
        _ => Err(ApiError::Fatal("not a light".into())),
    }
}

fn light_set_light(s: &mut Session) -> ApiResult {
    let [_this, p] = s.args::<2>()?;
    let (id, _) = light_self(s)?;
    let size = s.mem.read_u32(p)?;
    if size != 76 && size != 80 {
        return Ok(ApiRet::Ret(D3DERR_INVALIDCALL));
    }
    // D3DLIGHT / D3DLIGHT2: dwSize, type, colour, position, direction, range,
    // falloff, attenuation 0–2, theta, phi, [dwFlags].
    let kind = s.mem.read_u32(p + 4)?;
    if kind == 4 {
        s.missing("IDirect3DLight::SetLight", "D3DLIGHT_PARALLELPOINT lights".into());
    }
    let f = |o: u32| s.mem.read_f32(p + o);
    let color = read_color4(s, p + 8)?;
    let flags = if size == 80 {
        s.mem.read_u32(p + 76)?
    } else {
        D3DLIGHT_ACTIVE
    };
    let light = Light7 {
        kind: if kind == 5 { 2 } else { kind }, // GLSPOT is a spotlight
        diffuse: color,
        specular: if flags & D3DLIGHT_NO_SPECULAR != 0 {
            [0.0; 4]
        } else {
            color
        },
        ambient: [0.0; 4],
        position: [f(24)?, f(28)?, f(32)?],
        direction: [f(36)?, f(40)?, f(44)?],
        range: f(48)?,
        falloff: f(52)?,
        att: [f(56)?, f(60)?, f(64)?],
        theta: f(68)?,
        phi: f(72)?,
    };
    let raw = s.mem.read_vec(p, size)?;
    if let Some(o) = s.com_obj_mut(id) {
        o.kind = ComKind::Light3(Box::new(Light3 { light, flags, raw }));
    }
    Ok(ApiRet::Ret(D3D_OK))
}

fn light_get_light(s: &mut Session) -> ApiResult {
    let [_this, p] = s.args::<2>()?;
    let (_id, l) = light_self(s)?;
    let size = s.mem.read_u32(p)?;
    if size != 76 && size != 80 {
        return Ok(ApiRet::Ret(D3DERR_INVALIDCALL));
    }
    let mut b = l.raw.clone();
    b.resize(80, 0);
    if l.raw.len() == 76 {
        b[76..80].copy_from_slice(&D3DLIGHT_ACTIVE.to_le_bytes());
    }
    b[0..4].copy_from_slice(&size.to_le_bytes());
    s.mem.write(p, &b[..size as usize])?;
    Ok(ApiRet::Ret(D3D_OK))
}

fn light_initialize(_s: &mut Session) -> ApiResult {
    Ok(ApiRet::Ret(super::ddraw::dderr(5))) // DDERR_ALREADYINITIALIZED
}

// ---------------------------------------------------------------- IDirect3DTexture2

fn tex_palette_changed(_s: &mut Session) -> ApiResult {
    Ok(ApiRet::Ret(D3D_OK))
}

fn tex_load(s: &mut Session) -> ApiResult {
    let [_this, src] = s.args::<2>()?;
    let dobj = s.this_obj()?;
    let Some(sobj) = s.surface_obj_of_ptr(src) else {
        return Ok(ApiRet::Ret(D3DERR_INVALIDCALL));
    };
    let dchain = super::ddraw::mip_chain(s, dobj);
    let schain = super::ddraw::mip_chain(s, sobj);
    for (d, so) in dchain.iter().zip(schain.iter()) {
        let (Some(did), Some(sid)) = (s.surface_of_obj(*d), s.surface_of_obj(*so)) else {
            continue;
        };
        let (dw, dh, dfmt) = {
            let p = &s.display.surfaces[&did];
            (p.w, p.h, p.fmt)
        };
        let (sw, sh, sfmt) = {
            let p = &s.display.surfaces[&sid];
            (p.w, p.h, p.fmt)
        };
        if dfmt != sfmt {
            return Ok(ApiRet::Ret(super::ddraw::DDERR_INVALIDPIXELFORMAT));
        }
        if (dw, dh) != (sw, sh) {
            return Ok(ApiRet::Ret(D3DERR_INVALIDCALL));
        }
        let r = Rect::new(0, 0, dw as i32, dh as i32);
        super::ddraw::copy_rect(s, did, r, sid, r)?;
    }
    // Palettes are loaded with the texture.
    let pal = match s.com_obj(sobj).map(|o| &o.kind) {
        Some(ComKind::Surface(x)) => x.palette,
        _ => 0,
    };
    if pal != 0 {
        let sp = s
            .surface_of_obj(sobj)
            .and_then(|id| s.display.surfaces.get(&id))
            .map(|p| p.palette);
        if let (Some(sp), Some(did)) = (sp, s.surface_of_obj(dobj))
            && let Some(dp) = s.display.surfaces.get_mut(&did)
        {
            dp.palette = sp;
        }
    }
    Ok(ApiRet::Ret(D3D_OK))
}

// ---------------------------------------------------------------- interface tables

pub static IDIRECT3D3: IfaceDef = IfaceDef {
    name: "IDirect3D3",
    iid: guid("bb223240-e72b-11d0-a9b4-00aa00c0993e"),
    module: "DDRAW.DLL",
    methods: &[
        iunknown!()[0],
        iunknown!()[1],
        iunknown!()[2],
        export!("EnumDevices", 3, d3_enum_devices),
        export!("CreateLight", 3, d3_create_light),
        export!("CreateMaterial", 3, d3_create_material),
        export!("CreateViewport", 3, d3_create_viewport),
        unimpl_method!("FindDevice", 3),
        export!("CreateDevice", 5, d3_create_device),
        export!("CreateVertexBuffer", 5, d3_create_vertex_buffer),
        export!("EnumZBufferFormats", 4, d3d_enum_zbuffer_formats),
        export!("EvictManagedTextures", 1, d3d_evict_managed_textures),
    ],
};

pub static IDIRECT3DDEVICE3: IfaceDef = IfaceDef {
    name: "IDirect3DDevice3",
    iid: guid("b0ab3b60-33d7-11d1-a981-00c04fd7b174"),
    module: "DDRAW.DLL",
    methods: &[
        iunknown!()[0],
        iunknown!()[1],
        iunknown!()[2],
        export!("GetCaps", 3, dev3_get_caps),
        export!("GetStats", 2, dev3_get_stats),
        export!("AddViewport", 2, dev3_add_viewport),
        export!("DeleteViewport", 2, dev3_delete_viewport),
        export!("NextViewport", 4, dev3_next_viewport),
        export!("EnumTextureFormats", 3, dev_enum_texture_formats),
        export!("BeginScene", 1, dev_begin_scene),
        export!("EndScene", 1, dev_end_scene),
        export!("GetDirect3D", 2, dev3_get_direct3d),
        export!("SetCurrentViewport", 2, dev3_set_current_viewport),
        export!("GetCurrentViewport", 2, dev3_get_current_viewport),
        export!("SetRenderTarget", 3, dev_set_render_target),
        export!("GetRenderTarget", 2, dev3_get_render_target),
        unimpl_method!("Begin", 4),
        unimpl_method!("BeginIndexed", 6),
        unimpl_method!("Vertex", 2),
        unimpl_method!("Index", 2),
        unimpl_method!("End", 2),
        export!("GetRenderState", 3, dev_get_render_state),
        export!("SetRenderState", 3, dev3_set_render_state),
        export!("GetLightState", 3, dev3_get_light_state),
        export!("SetLightState", 3, dev3_set_light_state),
        export!("SetTransform", 3, dev_set_transform),
        export!("GetTransform", 3, dev_get_transform),
        export!("MultiplyTransform", 3, dev_multiply_transform),
        export!("DrawPrimitive", 6, dev3_draw_primitive),
        export!("DrawIndexedPrimitive", 8, dev3_draw_indexed_primitive),
        export!("SetClipStatus", 2, dev_set_clip_status),
        export!("GetClipStatus", 2, dev_get_clip_status),
        export!("DrawPrimitiveStrided", 6, dev3_draw_primitive_strided),
        export!("DrawIndexedPrimitiveStrided", 8, dev3_draw_indexed_primitive_strided),
        export!("DrawPrimitiveVB", 6, dev3_draw_primitive_vb),
        export!("DrawIndexedPrimitiveVB", 6, dev3_draw_indexed_primitive_vb),
        export!("ComputeSphereVisibility", 6, dev_compute_sphere_visibility),
        export!("GetTexture", 3, dev3_get_texture),
        export!("SetTexture", 3, dev_set_texture),
        export!("GetTextureStageState", 4, dev_get_texture_stage_state),
        export!("SetTextureStageState", 4, dev_set_texture_stage_state),
        export!("ValidateDevice", 2, dev_validate_device),
    ],
};

pub static IDIRECT3DVIEWPORT3: IfaceDef = IfaceDef {
    name: "IDirect3DViewport3",
    iid: guid("b0ab3b61-33d7-11d1-a981-00c04fd7b174"),
    module: "DDRAW.DLL",
    methods: &[
        iunknown!()[0],
        iunknown!()[1],
        iunknown!()[2],
        export!("Initialize", 2, vp_initialize),
        export!("GetViewport", 2, vp_get_viewport),
        export!("SetViewport", 2, vp_set_viewport),
        unimpl_method!("TransformVertices", 5),
        unimpl_method!("LightElements", 3),
        export!("SetBackground", 2, vp_set_background),
        export!("GetBackground", 3, vp_get_background),
        unimpl_method!("SetBackgroundDepth", 2),
        unimpl_method!("GetBackgroundDepth", 3),
        export!("Clear", 4, vp_clear),
        export!("AddLight", 2, vp_add_light),
        export!("DeleteLight", 2, vp_delete_light),
        export!("NextLight", 4, vp_next_light),
        export!("GetViewport2", 2, vp_get_viewport2),
        export!("SetViewport2", 2, vp_set_viewport2),
        unimpl_method!("SetBackgroundDepth2", 2),
        unimpl_method!("GetBackgroundDepth2", 3),
        export!("Clear2", 7, vp_clear2),
    ],
};

pub static IDIRECT3DMATERIAL3: IfaceDef = IfaceDef {
    name: "IDirect3DMaterial3",
    iid: guid("ca9c46f4-d3c5-11d1-b75a-00600852b312"),
    module: "DDRAW.DLL",
    methods: &[
        iunknown!()[0],
        iunknown!()[1],
        iunknown!()[2],
        export!("SetMaterial", 2, mat_set_material),
        export!("GetMaterial", 2, mat_get_material),
        export!("GetHandle", 3, get_handle),
    ],
};

pub static IDIRECT3DLIGHT: IfaceDef = IfaceDef {
    name: "IDirect3DLight",
    iid: guid("4417c142-33ad-11cf-816f-0000c020156e"),
    module: "DDRAW.DLL",
    methods: &[
        iunknown!()[0],
        iunknown!()[1],
        iunknown!()[2],
        export!("Initialize", 2, light_initialize),
        export!("SetLight", 2, light_set_light),
        export!("GetLight", 2, light_get_light),
    ],
};

pub static IDIRECT3DTEXTURE2: IfaceDef = IfaceDef {
    name: "IDirect3DTexture2",
    iid: guid("93281502-8cf8-11d0-89ab-00a0c9054129"),
    module: "DDRAW.DLL",
    methods: &[
        iunknown!()[0],
        iunknown!()[1],
        iunknown!()[2],
        export!("GetHandle", 3, get_handle),
        export!("PaletteChanged", 3, tex_palette_changed),
        export!("Load", 2, tex_load),
    ],
};

pub static IDIRECT3DVERTEXBUFFER: IfaceDef = IfaceDef {
    name: "IDirect3DVertexBuffer",
    iid: guid("7a503555-4a83-11d1-a5db-00a0c90367f8"),
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
    ],
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desc6_layout() {
        let d = device_desc6(true, true);
        assert_eq!(d.len(), 252);
        let g = |o: usize| u32::from_le_bytes(d[o..o + 4].try_into().unwrap());
        assert_eq!(g(0), 252);
        assert_eq!(g(8), 2);
        assert_eq!(g(44), 56); // line caps dwSize
        assert_eq!(g(100), 56); // triangle caps dwSize
        assert_eq!(g(180), 2048); // max texture width
        let empty = device_desc6(true, false);
        assert_eq!(u32::from_le_bytes(empty[8..12].try_into().unwrap()), 0);
    }

    #[test]
    fn clip_volume_mapping() {
        assert!(clip_adjust([-1.0, 1.0, 2.0, 2.0]).is_none());
        // A volume of [-2, 2] × [-1, 1] maps x = 2 to 1 and x = -2 to -1.
        let m = clip_adjust([-2.0, 1.0, 4.0, 2.0]).unwrap();
        let x = |v: f32| v * m[0] + m[12];
        assert!((x(2.0) - 1.0).abs() < 1e-6 && (x(-2.0) + 1.0).abs() < 1e-6);
    }
}
