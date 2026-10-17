//! DirectSound: virtual mixing device driven by guest time.

use super::*;
use crate::{export, iunknown, unimpl_method};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DSoundGlobal {}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DSoundObj {}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DsBuffer {}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DsCont {}

pub fn resume(_s: &mut Session, c: DsCont, _v: Resume) -> ApiResult {
    match c {}
}

pub fn next_event(_s: &Session) -> Option<u64> {
    None
}
pub fn service(_s: &mut Session) {}
pub fn buffer_destroyed(_s: &mut Session, _id: u32, _b: DsBuffer) {}
pub fn buffer_qi(_s: &mut Session, _id: u32, _iid: &Guid, _ppv: u32) -> Result<Option<u32>, ApiError> {
    Ok(None)
}
pub fn create_via_com(_s: &mut Session, _iid: &Guid, _ppv: u32) -> ApiResult {
    unimpl("DirectSound via CoCreateInstance")
}

pub static IDIRECTSOUND: IfaceDef = IfaceDef {
    name: "IDirectSound",
    iid: guid("279afa83-4981-11ce-a521-0020af0be560"),
    module: "DSOUND.DLL",
    methods: &[iunknown!()[0], iunknown!()[1], iunknown!()[2]],
};
pub static IDIRECTSOUND3DLISTENER: IfaceDef = IfaceDef {
    name: "IDirectSound3DListener",
    iid: guid("279afa84-4981-11ce-a521-0020af0be560"),
    module: "DSOUND.DLL",
    methods: &[iunknown!()[0], iunknown!()[1], iunknown!()[2]],
};
pub static IDIRECTSOUND3DBUFFER: IfaceDef = IfaceDef {
    name: "IDirectSound3DBuffer",
    iid: guid("279afa86-4981-11ce-a521-0020af0be560"),
    module: "DSOUND.DLL",
    methods: &[iunknown!()[0], iunknown!()[1], iunknown!()[2]],
};
pub static BUFFER_IFACES: &[&IfaceDef] = &[];

pub static DSOUND_EXPORTS: &[ExportDef] = &[
    unimpl_method!("DirectSoundCreate", 3),
    unimpl_method!("DirectSoundEnumerateA", 2),
];
#[allow(unused)]
fn _keep() -> ExportDef {
    export!("x", 0, |_s| Ok(ApiRet::Ret(0)))
}
