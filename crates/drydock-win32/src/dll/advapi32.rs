//! advapi32: registry.

use crate::api::*;
use crate::export;
use crate::reg::*;
use crate::session::Session;

const ERROR_FILE_NOT_FOUND: u32 = 2;
const ERROR_INVALID_HANDLE: u32 = 6;
const ERROR_MORE_DATA: u32 = 234;
const ERROR_NO_MORE_ITEMS: u32 = 259;

fn reg_open_key_ex_a(s: &mut Session) -> ApiResult {
    let [hkey, sub, _opts, _sam, out] = s.args::<5>()?;
    let sub = s.read_str_opt(sub)?.unwrap_or_default();
    let Some((root, path)) = s.registry.resolve(hkey, &sub) else {
        return Ok(ApiRet::Ret(ERROR_INVALID_HANDLE));
    };
    if s.registry.key(root, &path).is_none() {
        return Ok(ApiRet::Ret(ERROR_FILE_NOT_FOUND));
    }
    let h = s.registry.open_handle(root, path);
    s.mem.write_u32(out, h)?;
    Ok(ApiRet::Ret(0))
}

fn reg_open_key_a(s: &mut Session) -> ApiResult {
    let [hkey, sub, out] = s.args::<3>()?;
    let sub = s.read_str_opt(sub)?.unwrap_or_default();
    let Some((root, path)) = s.registry.resolve(hkey, &sub) else {
        return Ok(ApiRet::Ret(ERROR_INVALID_HANDLE));
    };
    if s.registry.key(root, &path).is_none() {
        return Ok(ApiRet::Ret(ERROR_FILE_NOT_FOUND));
    }
    let h = s.registry.open_handle(root, path);
    s.mem.write_u32(out, h)?;
    Ok(ApiRet::Ret(0))
}

fn reg_create_key_ex_a(s: &mut Session) -> ApiResult {
    let [hkey, sub, _r, _class, _opts, _sam, _sa, out, disp] = s.args::<9>()?;
    let sub = s.read_str_opt(sub)?.unwrap_or_default();
    let Some((root, path)) = s.registry.resolve(hkey, &sub) else {
        return Ok(ApiRet::Ret(ERROR_INVALID_HANDLE));
    };
    let existed = s.registry.key(root, &path).is_some();
    s.registry.key_mut(root, &path, true);
    let h = s.registry.open_handle(root, path);
    s.mem.write_u32(out, h)?;
    if disp != 0 {
        s.mem.write_u32(disp, if existed { 2 } else { 1 })?;
    }
    Ok(ApiRet::Ret(0))
}

fn reg_create_key_a(s: &mut Session) -> ApiResult {
    let [hkey, sub, out] = s.args::<3>()?;
    let sub = s.read_str_opt(sub)?.unwrap_or_default();
    let Some((root, path)) = s.registry.resolve(hkey, &sub) else {
        return Ok(ApiRet::Ret(ERROR_INVALID_HANDLE));
    };
    s.registry.key_mut(root, &path, true);
    let h = s.registry.open_handle(root, path);
    s.mem.write_u32(out, h)?;
    Ok(ApiRet::Ret(0))
}

fn reg_close_key(s: &mut Session) -> ApiResult {
    let [h] = s.args::<1>()?;
    Ok(ApiRet::Ret(if s.registry.close(h) { 0 } else { ERROR_INVALID_HANDLE }))
}

fn reg_query_value_ex_a(s: &mut Session) -> ApiResult {
    let [hkey, name, _r, ptype, data, pcb] = s.args::<6>()?;
    let name = s.read_str_opt(name)?.unwrap_or_default();
    let Some((root, path)) = s.registry.resolve(hkey, "") else {
        return Ok(ApiRet::Ret(ERROR_INVALID_HANDLE));
    };
    let Some(v) = s
        .registry
        .key(root, &path)
        .and_then(|k| k.values.get(&name.to_uppercase()))
        .cloned()
    else {
        return Ok(ApiRet::Ret(ERROR_FILE_NOT_FOUND));
    };
    if ptype != 0 {
        s.mem.write_u32(ptype, v.ty)?;
    }
    if pcb == 0 {
        return Ok(ApiRet::Ret(0));
    }
    let cap = s.mem.read_u32(pcb)?;
    s.mem.write_u32(pcb, v.data.len() as u32)?;
    if data == 0 {
        return Ok(ApiRet::Ret(0));
    }
    if (v.data.len() as u32) > cap {
        return Ok(ApiRet::Ret(ERROR_MORE_DATA));
    }
    s.mem.write(data, &v.data)?;
    Ok(ApiRet::Ret(0))
}

fn reg_set_value_ex_a(s: &mut Session) -> ApiResult {
    let [hkey, name, _r, ty, data, cb] = s.args::<6>()?;
    let name = s.read_str_opt(name)?.unwrap_or_default();
    let Some((root, path)) = s.registry.resolve(hkey, "") else {
        return Ok(ApiRet::Ret(ERROR_INVALID_HANDLE));
    };
    let mut bytes = s.mem.read_vec(data, cb)?;
    if (ty == REG_SZ || ty == REG_EXPAND_SZ) && bytes.last() != Some(&0) {
        bytes.push(0);
    }
    s.registry.set(root, &path, &name, ty, bytes);
    Ok(ApiRet::Ret(0))
}

fn reg_delete_value_a(s: &mut Session) -> ApiResult {
    let [hkey, name] = s.args::<2>()?;
    let name = s.read_str_opt(name)?.unwrap_or_default();
    let Some((root, path)) = s.registry.resolve(hkey, "") else {
        return Ok(ApiRet::Ret(ERROR_INVALID_HANDLE));
    };
    let removed = s
        .registry
        .key_mut(root, &path, false)
        .and_then(|k| k.values.remove(&name.to_uppercase()));
    Ok(ApiRet::Ret(if removed.is_some() { 0 } else { ERROR_FILE_NOT_FOUND }))
}

fn reg_enum_key_ex_a(s: &mut Session) -> ApiResult {
    let [hkey, idx, name, pcch] = s.args::<4>()?;
    let Some((root, path)) = s.registry.resolve(hkey, "") else {
        return Ok(ApiRet::Ret(ERROR_INVALID_HANDLE));
    };
    let Some(sub) = s
        .registry
        .key(root, &path)
        .and_then(|k| k.subkeys.values().nth(idx as usize))
        .map(|k| k.name.clone())
    else {
        return Ok(ApiRet::Ret(ERROR_NO_MORE_ITEMS));
    };
    let cap = s.mem.read_u32(pcch)?;
    match s.write_str_buf(name, cap, &sub)? {
        Ok(n) => {
            s.mem.write_u32(pcch, n)?;
            Ok(ApiRet::Ret(0))
        }
        Err(_) => Ok(ApiRet::Ret(ERROR_MORE_DATA)),
    }
}

pub static ADVAPI32_EXPORTS: &[ExportDef] = &[
    export!("RegCloseKey", 1, reg_close_key),
    export!("RegCreateKeyA", 3, reg_create_key_a),
    export!("RegCreateKeyExA", 9, reg_create_key_ex_a),
    export!("RegDeleteValueA", 2, reg_delete_value_a),
    export!("RegEnumKeyExA", 8, reg_enum_key_ex_a),
    export!("RegOpenKeyA", 3, reg_open_key_a),
    export!("RegOpenKeyExA", 5, reg_open_key_ex_a),
    export!("RegQueryValueExA", 6, reg_query_value_ex_a),
    export!("RegSetValueExA", 6, reg_set_value_ex_a),
];
