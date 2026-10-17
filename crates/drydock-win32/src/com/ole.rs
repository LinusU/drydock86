//! ole32 (COM activation) and DirectPlay entry points.

use super::*;
use crate::{export, iunknown, unimpl_method};

pub const REGDB_E_CLASSNOTREG: u32 = 0x8004_0154;
pub const CO_E_NOTINITIALIZED: u32 = 0x8004_01f0;

fn co_initialize(s: &mut Session) -> ApiResult {
    s.com.initialized += 1;
    Ok(ApiRet::Ret(if s.com.initialized == 1 { S_OK } else { S_FALSE }))
}
fn co_initialize_ex(s: &mut Session) -> ApiResult {
    co_initialize(s)
}
fn co_uninitialize(s: &mut Session) -> ApiResult {
    s.com.initialized = s.com.initialized.saturating_sub(1);
    Ok(ApiRet::Ret(0))
}

/// In-process classes provided by the emulated system.
fn class_object(clsid: &Guid) -> Option<&'static str> {
    let known: &[(&str, Guid)] = &[
        ("DirectSound", guid("47d4d946-62e8-11cf-93bc-444553540000")),
        ("DirectInput", guid("25e609e0-b259-11cf-bfc7-444553540000")),
        ("DirectDraw7", guid("3c305196-50db-11d3-9cfe-00c04fd930c5")),
    ];
    known.iter().find(|(_, g)| g == clsid).map(|(n, _)| *n)
}

fn co_create_instance(s: &mut Session) -> ApiResult {
    let [clsid_p, outer, _ctx, iid_p, ppv] = s.args::<5>()?;
    let clsid = s.read_guid(clsid_p)?;
    let iid = s.read_guid(iid_p)?;
    if ppv != 0 {
        s.mem.write_u32(ppv, 0)?;
    }
    if s.com.initialized == 0 {
        return Ok(ApiRet::Ret(CO_E_NOTINITIALIZED));
    }
    if outer != 0 {
        return Ok(ApiRet::Ret(0x8004_0110));
    }
    match class_object(&clsid) {
        Some("DirectSound") => super::dsound::create_via_com(s, &iid, ppv),
        Some(other) => {
            s.missing("CoCreateInstance", format!("class {other} via COM activation"));
            Ok(ApiRet::Ret(REGDB_E_CLASSNOTREG))
        }
        None => {
            // The class is not registered in the emulated system (e.g. components that
            // are not installed). This is a genuine COM result, reported for diagnosis.
            s.event(
                crate::diag::Severity::Warning,
                "CoCreateInstance",
                format!("class {} not registered (iid {})", guid_str(&clsid), guid_str(&iid)),
            );
            Ok(ApiRet::Ret(REGDB_E_CLASSNOTREG))
        }
    }
}

pub static ICLASSFACTORY: IfaceDef = IfaceDef {
    name: "IClassFactory",
    iid: guid("00000001-0000-0000-c000-000000000046"),
    module: "OLE32.DLL",
    methods: &[
        iunknown!()[0],
        iunknown!()[1],
        iunknown!()[2],
        unimpl_method!("CreateInstance", 4),
        unimpl_method!("LockServer", 2),
    ],
};

pub static OLE32_EXPORTS: &[ExportDef] = &[
    export!("CoCreateInstance", 5, co_create_instance),
    export!("CoInitialize", 1, co_initialize),
    export!("CoInitializeEx", 2, co_initialize_ex),
    export!("CoUninitialize", 0, co_uninitialize),
];

// DirectPlay: no service providers are installed on the emulated machine, so creation
// fails the way it does on a system without DirectPlay service providers.
fn direct_play_create(s: &mut Session) -> ApiResult {
    let [_guid, pp, _outer] = s.args::<3>()?;
    if pp != 0 {
        s.mem.write_u32(pp, 0)?;
    }
    s.missing("DirectPlayCreate", "multiplayer (DirectPlay) is not implemented".into());
    Ok(ApiRet::Ret(0x8877_0050)) // DPERR_UNAVAILABLE
}

pub static DPLAYX_EXPORTS: &[ExportDef] = &[export!("DirectPlayCreate" @ 1, 3, direct_play_create)];
