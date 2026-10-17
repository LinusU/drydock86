//! COM infrastructure and DirectX components.

pub mod d3d;

use crate::api::*;
use crate::session::Session;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ComState {}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ComCont {}

pub fn resume(_s: &mut Session, c: ComCont, _v: Resume) -> ApiResult {
    match c {}
}

pub fn next_event(_s: &Session) -> Option<u64> {
    None
}

pub fn service(_s: &mut Session) {}

pub static OLE32_EXPORTS: &[ExportDef] = &[];
pub static DDRAW_EXPORTS: &[ExportDef] = &[];
pub static DINPUT_EXPORTS: &[ExportDef] = &[];
pub static DSOUND_EXPORTS: &[ExportDef] = &[];
pub static DPLAYX_EXPORTS: &[ExportDef] = &[];
