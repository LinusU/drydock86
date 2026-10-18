//! DirectPlay: the lobby object. No lobby application launched the process and no
//! service providers are installed, so the lobby reports "not lobbied" and multiplayer
//! sessions cannot be created.

use super::*;
use crate::{export, iunknown, unimpl_method};

pub const DPERR_UNAVAILABLE: u32 = 0x8877_0050;
pub const DPERR_NOTLOBBIED: u32 = 0x8877_042e;
pub const DPERR_INVALIDPARAMS: u32 = E_INVALIDARG;

fn direct_play_lobby_create_a(s: &mut Session) -> ApiResult {
    let [_sp, pp, outer, _data, _size] = s.args::<5>()?;
    if pp == 0 {
        return Ok(ApiRet::Ret(DPERR_INVALIDPARAMS));
    }
    if outer != 0 {
        s.mem.write_u32(pp, 0)?;
        return Ok(ApiRet::Ret(0x8004_0110)); // CLASS_E_NOAGGREGATION
    }
    let id = s.com_new(ComKind::DPlayLobby);
    s.out_iface(pp, id, &IDIRECTPLAYLOBBYA)?;
    Ok(ApiRet::Ret(S_OK))
}

fn lobby_get_connection_settings(s: &mut Session) -> ApiResult {
    let [_this, _app, _data, psize] = s.args::<4>()?;
    if psize == 0 {
        return Ok(ApiRet::Ret(DPERR_INVALIDPARAMS));
    }
    Ok(ApiRet::Ret(DPERR_NOTLOBBIED))
}

fn lobby_enum_local_applications(_s: &mut Session) -> ApiResult {
    // No DirectPlay applications are registered.
    Ok(ApiRet::Ret(S_OK))
}

fn lobby_connect(s: &mut Session) -> ApiResult {
    let [_this, _flags, pp, _outer] = s.args::<4>()?;
    if pp != 0 {
        s.mem.write_u32(pp, 0)?;
    }
    Ok(ApiRet::Ret(DPERR_NOTLOBBIED))
}

fn lobby_connect_ex(s: &mut Session) -> ApiResult {
    let [_this, _flags, _iid, pp, _outer] = s.args::<5>()?;
    if pp != 0 {
        s.mem.write_u32(pp, 0)?;
    }
    Ok(ApiRet::Ret(DPERR_NOTLOBBIED))
}

fn lobby_set_lobby_message_event(_s: &mut Session) -> ApiResult {
    Ok(ApiRet::Ret(DPERR_NOTLOBBIED))
}

fn lobby_receive_lobby_message(_s: &mut Session) -> ApiResult {
    Ok(ApiRet::Ret(DPERR_NOTLOBBIED))
}

fn lobby_wait_for_connection_settings(_s: &mut Session) -> ApiResult {
    Ok(ApiRet::Ret(DPERR_NOTLOBBIED))
}

/// IDirectPlayLobby3A methods; IDirectPlayLobbyA has the first 14 and
/// IDirectPlayLobby2A the first 15.
static LOBBY_METHODS: [ExportDef; 19] = [
    iunknown!()[0],
    iunknown!()[1],
    iunknown!()[2],
    export!("Connect", 4, lobby_connect),
    unimpl_method!("CreateAddress", 7),
    unimpl_method!("EnumAddress", 5),
    unimpl_method!("EnumAddressTypes", 5),
    export!("EnumLocalApplications", 4, lobby_enum_local_applications),
    export!("GetConnectionSettings", 4, lobby_get_connection_settings),
    export!("ReceiveLobbyMessage", 6, lobby_receive_lobby_message),
    unimpl_method!("RunApplication", 5),
    unimpl_method!("SendLobbyMessage", 5),
    unimpl_method!("SetConnectionSettings", 4),
    export!("SetLobbyMessageEvent", 4, lobby_set_lobby_message_event),
    unimpl_method!("CreateCompoundAddress", 5),
    export!("ConnectEx", 5, lobby_connect_ex),
    unimpl_method!("RegisterApplication", 3),
    unimpl_method!("UnregisterApplication", 3),
    export!("WaitForConnectionSettings", 2, lobby_wait_for_connection_settings),
];

pub static IDIRECTPLAYLOBBYA: IfaceDef = IfaceDef {
    name: "IDirectPlayLobbyA",
    iid: guid("26c66a70-b367-11cf-a024-00aa006157ac"),
    module: "DPLAYX.DLL",
    methods: LOBBY_METHODS.split_at(14).0,
};

pub static IDIRECTPLAYLOBBY2A: IfaceDef = IfaceDef {
    name: "IDirectPlayLobby2A",
    iid: guid("1bb4af80-a303-11d0-9c4f-00a0c905425e"),
    module: "DPLAYX.DLL",
    methods: LOBBY_METHODS.split_at(15).0,
};

pub static IDIRECTPLAYLOBBY3A: IfaceDef = IfaceDef {
    name: "IDirectPlayLobby3A",
    iid: guid("2db72491-652c-11d1-a7a8-0000f803abfc"),
    module: "DPLAYX.DLL",
    methods: &LOBBY_METHODS,
};

pub static LOBBY_IFACES: &[&IfaceDef] = &[&IDIRECTPLAYLOBBYA, &IDIRECTPLAYLOBBY2A, &IDIRECTPLAYLOBBY3A];

pub static DPLAYX_LOBBY_EXPORTS: &[ExportDef] = &[export!("DirectPlayLobbyCreateA" @ 4, 5, direct_play_lobby_create_a)];
