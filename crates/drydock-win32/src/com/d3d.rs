//! Direct3D 7 (immediate mode) on top of the wgpu renderer.

use super::*;
use crate::dll::user32::Rect;
use crate::{iunknown, unimpl_method};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Device {}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VertexBuffer {}

/// Copy GPU-rendered contents of a surface back into guest memory.
pub fn readback(_s: &mut Session, _id: u32) {}
/// A surface was destroyed; drop any GPU resources.
pub fn surface_freed(_s: &mut Session, _id: u32) {}
/// Submit pending GPU work (at presentation).
pub fn flush(_s: &mut Session) {}
/// Fill a depth surface.
pub fn depth_fill(s: &mut Session, id: u32, r: &Rect, depth: u32) {
    s.surface_cpu_access(id, true);
    let t = crate::surface::PixelTarget::of(&s.display.surfaces[&id]);
    let _ = t.fill_raw(&mut s.mem, r, depth & crate::surface::mask_bits(t.fmt.bits()));
}
/// Perform a blit on the GPU when the destination is GPU-owned. Returns false to use
/// the CPU path.
pub fn gpu_blit(_s: &mut Session, _dst: u32, _dr: Rect, _src: u32, _sr: Rect, _key: Option<crate::surface::ColorKey>) -> bool {
    false
}
pub fn device_destroyed(_s: &mut Session, _id: u32, _d: Device) {}

pub fn ddraw_qi(s: &mut Session, id: u32, iid: &Guid, ppv: u32) -> Result<Option<u32>, ApiError> {
    if *iid == IDIRECT3D7.iid {
        let d3d = s.com_new(ComKind::Direct3D);
        let _ = id;
        s.out_iface(ppv, d3d, &IDIRECT3D7)?;
        return Ok(Some(S_OK));
    }
    Ok(None)
}

pub static TEXTURE_IFACES: &[&IfaceDef] = &[];

pub static IDIRECT3D7: IfaceDef = IfaceDef {
    name: "IDirect3D7",
    iid: guid("f5049e77-4861-11d2-a407-00a0c90629a8"),
    module: "DDRAW.DLL",
    methods: &[
        iunknown!()[0],
        iunknown!()[1],
        iunknown!()[2],
        unimpl_method!("EnumDevices", 3),
        unimpl_method!("CreateDevice", 4),
        unimpl_method!("CreateVertexBuffer", 4),
        unimpl_method!("EnumZBufferFormats", 4),
        unimpl_method!("EvictManagedTextures", 1),
    ],
};
pub static IDIRECT3DDEVICE7: IfaceDef = IfaceDef {
    name: "IDirect3DDevice7",
    iid: guid("f5049e79-4861-11d2-a407-00a0c90629a8"),
    module: "DDRAW.DLL",
    methods: &[iunknown!()[0], iunknown!()[1], iunknown!()[2]],
};
pub static IDIRECT3DVERTEXBUFFER7: IfaceDef = IfaceDef {
    name: "IDirect3DVertexBuffer7",
    iid: guid("f5049e7d-4861-11d2-a407-00a0c90629a8"),
    module: "DDRAW.DLL",
    methods: &[iunknown!()[0], iunknown!()[1], iunknown!()[2]],
};
