//! ole32 (COM activation) and DirectPlay entry points.

use super::*;
use crate::{export, iunknown, unimpl_method};

pub const REGDB_E_CLASSNOTREG: u32 = 0x8004_0154;
pub const CO_E_NOTINITIALIZED: u32 = 0x8004_01f0;
pub const CO_E_CLASSSTRING: u32 = 0x8004_01f3;

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
    if let Some(r) = super::dmusic::create_via_com(s, &clsid, &iid, ppv) {
        return r;
    }
    if clsid == super::amstream::CLSID_AMMULTIMEDIASTREAM {
        return super::amstream::create_via_com(s, &iid, ppv);
    }
    match class_object(&clsid) {
        Some("DirectSound") => super::dsound::create_via_com(s, &iid, ppv),
        Some("DirectInput") => super::dinput::create_via_com(s, &iid, ppv),
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

/// Parse "{xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx}".
fn parse_guid(s: &str) -> Option<Guid> {
    let inner = s.strip_prefix('{')?.strip_suffix('}')?;
    let ok = inner.len() == 36
        && inner.char_indices().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => c == '-',
            _ => c.is_ascii_hexdigit(),
        });
    ok.then(|| guid(inner))
}

fn clsid_from_string(s: &mut Session) -> ApiResult {
    let [p, out] = s.args::<2>()?;
    let text = s.read_wstr(p)?;
    // Program ids (names rather than GUIDs) would be looked up in the registry; none
    // are registered on the emulated machine.
    match parse_guid(&text) {
        Some(g) => {
            s.mem.write(out, &g)?;
            Ok(ApiRet::Ret(S_OK))
        }
        None => {
            s.mem.write(out, &[0u8; 16])?;
            Ok(ApiRet::Ret(CO_E_CLASSSTRING))
        }
    }
}

fn task_alloc(s: &mut Session, size: u32) -> u32 {
    let h = s.process.sys_heap;
    s.heaps.alloc(&mut s.mem, h, size, false).unwrap_or(0)
}

fn string_from_clsid(s: &mut Session) -> ApiResult {
    let [p, out] = s.args::<2>()?;
    let g = s.read_guid(p)?;
    let text = format!("{{{}}}", guid_str(&g).to_uppercase());
    let buf = task_alloc(s, 2 * (text.len() as u32 + 1));
    if buf == 0 {
        s.mem.write_u32(out, 0)?;
        return Ok(ApiRet::Ret(E_OUTOFMEMORY));
    }
    let _ = s.write_wstr_buf(buf, text.len() as u32 + 1, &text)?;
    s.mem.write_u32(out, buf)?;
    Ok(ApiRet::Ret(S_OK))
}

/// Version 4 GUIDs from a counter, so runs that make GUIDs stay reproducible.
fn co_create_guid(s: &mut Session) -> ApiResult {
    let [out] = s.args::<1>()?;
    s.com.guids_made += 1;
    let mut x = 0x6472_7964_6f63_6b00u64 ^ s.com.guids_made as u64;
    let mut g = [0u8; 16];
    for chunk in g.chunks_mut(8) {
        // splitmix64
        x = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = x;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        chunk.copy_from_slice(&(z ^ (z >> 31)).to_le_bytes());
    }
    g[7] = (g[7] & 0x0f) | 0x40;
    g[8] = (g[8] & 0x3f) | 0x80;
    s.mem.write(out, &g)?;
    Ok(ApiRet::Ret(S_OK))
}

fn co_task_mem_alloc(s: &mut Session) -> ApiResult {
    let [size] = s.args::<1>()?;
    Ok(ApiRet::Ret(task_alloc(s, size)))
}

fn co_task_mem_realloc(s: &mut Session) -> ApiResult {
    let [p, size] = s.args::<2>()?;
    if p == 0 {
        return Ok(ApiRet::Ret(task_alloc(s, size)));
    }
    let h = s.process.sys_heap;
    if size == 0 {
        s.heaps.free(h, p);
        return Ok(ApiRet::Ret(0));
    }
    Ok(ApiRet::Ret(
        s.heaps.realloc(&mut s.mem, h, p, size, false, false).unwrap_or(0),
    ))
}

fn co_task_mem_free(s: &mut Session) -> ApiResult {
    let [p] = s.args::<1>()?;
    if p != 0 {
        let h = s.process.sys_heap;
        s.heaps.free(h, p);
    }
    Ok(ApiRet::Ret(0))
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
    export!("CLSIDFromString", 2, clsid_from_string),
    export!("CoCreateGuid", 1, co_create_guid),
    export!("CoCreateInstance", 5, co_create_instance),
    export!("CoInitialize", 1, co_initialize),
    export!("CoInitializeEx", 2, co_initialize_ex),
    export!("CoTaskMemAlloc", 1, co_task_mem_alloc),
    export!("CoTaskMemFree", 1, co_task_mem_free),
    export!("CoTaskMemRealloc", 2, co_task_mem_realloc),
    export!("CoUninitialize", 0, co_uninitialize),
    export!("StringFromCLSID", 2, string_from_clsid),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guid_strings() {
        let g = parse_guid("{DD56A202-F1F5-11D3-91E0-005004D1445F}").unwrap();
        assert_eq!(g, guid("dd56a202-f1f5-11d3-91e0-005004d1445f"));
        assert_eq!(parse_guid("DD56A202-F1F5-11D3-91E0-005004D1445F"), None);
        assert_eq!(parse_guid("{DD56A202-F1F5-11D3-91E0-005004D1445}"), None);
        assert_eq!(parse_guid("{DD56A202+F1F5-11D3-91E0-005004D1445F}"), None);
    }
}
