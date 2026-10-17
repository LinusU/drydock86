//! Direct3D device and GPU-side resource ownership.

use crate::session::Session;

/// Copy GPU-rendered contents of a surface back into guest memory.
pub fn readback(_s: &mut Session, _id: u32) {}

/// A surface was destroyed; drop any GPU resources.
pub fn surface_freed(_s: &mut Session, _id: u32) {}
