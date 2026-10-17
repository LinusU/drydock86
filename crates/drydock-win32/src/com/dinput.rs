//! DirectInput: keyboard, mouse and joystick devices over the virtual input state.

use super::*;
use crate::{export, iunknown, unimpl_method};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DInputObj {}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiDevice {}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DiCont {}

pub fn resume(_s: &mut Session, c: DiCont, _v: Resume) -> ApiResult {
    match c {}
}
pub fn device_destroyed(_s: &mut Session, _id: u32, _d: DiDevice) {}

pub static DINPUT_IFACES: &[&IfaceDef] = &[];
pub static DEVICE_IFACES: &[&IfaceDef] = &[];

pub static DINPUT_EXPORTS: &[ExportDef] = &[unimpl_method!("DirectInputCreateA", 4)];
#[allow(unused)]
fn _keep() -> ExportDef {
    let _ = iunknown!();
    export!("x", 0, |_s| Ok(ApiRet::Ret(0)))
}
