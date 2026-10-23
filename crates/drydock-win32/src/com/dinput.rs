//! DirectInput 7: keyboard, mouse and (optional) joystick devices.
//!
//! Devices read the shared virtual input state ([`crate::input`]); buffered data is fed
//! from the same timestamped events that produce window messages, so human and scripted
//! input reach DirectInput identically.

use super::*;
use crate::input::InputEvent;
use crate::{export, iunknown, unimpl_method};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

pub const DI_OK: u32 = 0;
pub const DI_BUFFEROVERFLOW: u32 = 1;
pub const DI_POLLEDDEVICE: u32 = 2;
pub const DIERR_NOTACQUIRED: u32 = 0x8007_000c;
pub const DIERR_INPUTLOST: u32 = 0x8007_001e;
pub const DIERR_INVALIDPARAM: u32 = E_INVALIDARG;
pub const DIERR_NOTINITIALIZED: u32 = 0x8007_0015;
pub const DIERR_DEVICENOTREG: u32 = 0x8004_0154;
pub const DIERR_ACQUIRED: u32 = 0x8007_00aa;
pub const DIERR_OBJECTNOTFOUND: u32 = 0x8007_0002;
pub const DIERR_UNSUPPORTED: u32 = E_NOTIMPL;

pub const GUID_SYSMOUSE: Guid = guid("6f1d2b60-d5a0-11cf-bfc7-444553540000");
pub const GUID_SYSKEYBOARD: Guid = guid("6f1d2b61-d5a0-11cf-bfc7-444553540000");
pub const GUID_JOYSTICK: Guid = guid("6f1d2b70-d5a0-11cf-bfc7-444553540000");
pub const GUID_XAXIS: Guid = guid("a36d02e0-c9f3-11cf-bfc7-444553540000");
pub const GUID_YAXIS: Guid = guid("a36d02e1-c9f3-11cf-bfc7-444553540000");
pub const GUID_ZAXIS: Guid = guid("a36d02e2-c9f3-11cf-bfc7-444553540000");
pub const GUID_RZAXIS: Guid = guid("a36d02e3-c9f3-11cf-bfc7-444553540000");
pub const GUID_BUTTON: Guid = guid("a36d02f0-c9f3-11cf-bfc7-444553540000");
pub const GUID_KEY: Guid = guid("55728220-d33c-11cf-bfc7-444553540000");
pub const GUID_POV: Guid = guid("a36d02f2-c9f3-11cf-bfc7-444553540000");

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DevKind {
    Keyboard,
    Mouse,
    Joystick,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DInputObj {
    pub version: u32,
}

/// One object of a device: an axis, button or key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Obj {
    Axis(u32),
    Button(u32),
    Key(u32),
    Pov(u32),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FormatEntry {
    pub obj: Obj,
    pub ofs: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiDevice {
    pub kind: DevKind,
    pub acquired: bool,
    pub coop: u32,
    pub hwnd: u32,
    pub format: Vec<FormatEntry>,
    pub data_size: u32,
    pub rel_axes: bool,
    pub buffer_size: u32,
    /// (ofs, data, timestamp ms, sequence)
    pub buffer: VecDeque<(u32, u32, u32, u32)>,
    pub overflow: bool,
    pub seq: u32,
    pub event: u32,
    /// Accumulated relative mouse motion since the last GetDeviceState.
    pub accum: [i32; 3],
    pub last_abs: Option<(i32, i32)>,
    pub ranges: [(i32, i32); 4],
    pub deadzone: [u32; 4],
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DiCont {}

pub fn resume(_s: &mut Session, c: DiCont, _v: Resume) -> ApiResult {
    match c {}
}

pub fn device_destroyed(_s: &mut Session, _id: u32, _d: DiDevice) {}

fn create(s: &mut Session, version: u32, pp: u32, def: &'static IfaceDef) -> ApiResult {
    let id = s.com_new(ComKind::DInput(DInputObj { version }));
    s.out_iface(pp, id, def)?;
    Ok(ApiRet::Ret(DI_OK))
}

fn direct_input_create_a(s: &mut Session) -> ApiResult {
    let [_hinst, ver, pp, outer] = s.args::<4>()?;
    if outer != 0 {
        return Ok(ApiRet::Ret(0x8004_0110));
    }
    if ver > 0x0700 {
        return Ok(ApiRet::Ret(0x8878_0001)); // DIERR_BETADIRECTINPUTVERSION-like: newer than installed
    }
    create(s, ver, pp, &IDIRECTINPUTA)
}

fn direct_input_create_ex(s: &mut Session) -> ApiResult {
    let [_hinst, ver, riid, pp, outer] = s.args::<5>()?;
    if outer != 0 {
        return Ok(ApiRet::Ret(0x8004_0110));
    }
    let iid = s.read_guid(riid)?;
    let def = DINPUT_IFACES.iter().copied().find(|d| d.iid == iid);
    match def {
        Some(d) => create(s, ver, pp, d),
        None => Ok(ApiRet::Ret(E_NOINTERFACE)),
    }
}

/// CoCreateInstance(CLSID_DirectInput): the program calls Initialize afterwards.
pub fn create_via_com(s: &mut Session, iid: &Guid, ppv: u32) -> ApiResult {
    let def = if *iid == IID_IUNKNOWN {
        Some(&IDIRECTINPUTA)
    } else {
        DINPUT_IFACES.iter().copied().find(|d| d.iid == *iid)
    };
    match def {
        Some(d) => create(s, 0, ppv, d),
        None => Ok(ApiRet::Ret(E_NOINTERFACE)),
    }
}

fn kind_of(g: &Guid) -> Option<DevKind> {
    match *g {
        GUID_SYSKEYBOARD => Some(DevKind::Keyboard),
        GUID_SYSMOUSE => Some(DevKind::Mouse),
        GUID_JOYSTICK => Some(DevKind::Joystick),
        _ => None,
    }
}

fn instance_guid(k: DevKind) -> Guid {
    match k {
        DevKind::Keyboard => GUID_SYSKEYBOARD,
        DevKind::Mouse => GUID_SYSMOUSE,
        DevKind::Joystick => GUID_JOYSTICK,
    }
}

fn dev_type(k: DevKind) -> u32 {
    match k {
        DevKind::Mouse => 2 | (1 << 8),
        DevKind::Keyboard => 3 | (4 << 8),
        DevKind::Joystick => 4 | (1 << 8),
    }
}

fn device_instance(k: DevKind) -> Vec<u8> {
    let mut b = vec![0u8; 580];
    b[0..4].copy_from_slice(&580u32.to_le_bytes());
    b[4..20].copy_from_slice(&instance_guid(k));
    b[20..36].copy_from_slice(&instance_guid(k));
    b[36..40].copy_from_slice(&dev_type(k).to_le_bytes());
    let name: &[u8] = match k {
        DevKind::Keyboard => b"Keyboard",
        DevKind::Mouse => b"Mouse",
        DevKind::Joystick => b"Drydock Virtual Joystick",
    };
    b[40..40 + name.len()].copy_from_slice(name);
    b[300..300 + name.len()].copy_from_slice(name);
    b
}

fn di_create_device(s: &mut Session) -> ApiResult {
    let [_this, rguid, pp, outer] = s.args::<4>()?;
    if outer != 0 {
        return Ok(ApiRet::Ret(0x8004_0110));
    }
    let g = s.read_guid(rguid)?;
    let Some(kind) = kind_of(&g) else {
        return Ok(ApiRet::Ret(DIERR_DEVICENOTREG));
    };
    if kind == DevKind::Joystick && !s.user.input.joy_present {
        return Ok(ApiRet::Ret(DIERR_DEVICENOTREG));
    }
    let dev = DiDevice {
        kind,
        acquired: false,
        coop: 0,
        hwnd: 0,
        format: Vec::new(),
        data_size: 0,
        rel_axes: kind == DevKind::Mouse,
        buffer_size: 0,
        buffer: VecDeque::new(),
        overflow: false,
        seq: 0,
        event: 0,
        accum: [0; 3],
        last_abs: None,
        ranges: [(0, 65535); 4],
        deadzone: [0; 4],
    };
    let id = s.com_new(ComKind::DInputDevice(Box::new(dev)));
    s.out_iface(pp, id, &IDIRECTINPUTDEVICE7A)?;
    Ok(ApiRet::Ret(DI_OK))
}

fn di_enum_devices(s: &mut Session) -> ApiResult {
    let [_this, ty, cb, ctx, _flags] = s.args::<5>()?;
    let mut items = Vec::new();
    let mut kinds = vec![DevKind::Mouse, DevKind::Keyboard];
    if s.user.input.joy_present {
        kinds.push(DevKind::Joystick);
    }
    for k in kinds {
        let t = dev_type(k) & 0xff;
        if ty == 0 || ty == t || (ty == 1 && false) {
            items.push(vec![CallArg::Buf(device_instance(k)), CallArg::Val(ctx)]);
        }
    }
    enum_callbacks(cb, items, 0, DI_OK)
}

fn di_get_device_status(s: &mut Session) -> ApiResult {
    let [_this, rguid] = s.args::<2>()?;
    let g = s.read_guid(rguid)?;
    Ok(ApiRet::Ret(match kind_of(&g) {
        Some(DevKind::Joystick) if !s.user.input.joy_present => 1, // DI_NOTATTACHED
        Some(_) => DI_OK,
        None => DIERR_DEVICENOTREG,
    }))
}

fn di_initialize(s: &mut Session) -> ApiResult {
    let [_this, _hinst, ver] = s.args::<3>()?;
    if ver > 0x0700 {
        return Ok(ApiRet::Ret(0x8878_0001));
    }
    let id = s.this_obj()?;
    if let Some(ComKind::DInput(o)) = s.com_obj_mut(id).map(|o| &mut o.kind) {
        o.version = ver;
    }
    Ok(ApiRet::Ret(DI_OK))
}

// ---------------------------------------------------------------- device

fn dev_self(s: &Session) -> Result<(u32, DiDevice), ApiError> {
    let id = s.this_obj()?;
    match &s.com_obj(id).unwrap().kind {
        ComKind::DInputDevice(d) => Ok((id, (**d).clone())),
        _ => Err(ApiError::Fatal("not a DirectInput device".into())),
    }
}

fn dev_mut(s: &mut Session, id: u32) -> &mut DiDevice {
    match &mut s.com_obj_mut(id).unwrap().kind {
        ComKind::DInputDevice(d) => d,
        _ => unreachable!(),
    }
}

/// Objects a device exposes (in enumeration order).
fn objects(k: DevKind) -> Vec<Obj> {
    match k {
        DevKind::Keyboard => (1..256).filter(|&i| key_exists(i)).map(Obj::Key).collect(),
        DevKind::Mouse => vec![
            Obj::Axis(0),
            Obj::Axis(1),
            Obj::Axis(2),
            Obj::Button(0),
            Obj::Button(1),
            Obj::Button(2),
        ],
        DevKind::Joystick => {
            let mut v = vec![Obj::Axis(0), Obj::Axis(1), Obj::Axis(2), Obj::Axis(5)];
            v.extend((0..8).map(Obj::Button));
            v.push(Obj::Pov(0));
            v
        }
    }
}

fn key_exists(dik: u32) -> bool {
    matches!(dik, 0x01..=0x58 | 0x9c | 0x9d | 0xb5 | 0xb7 | 0xb8 | 0xc5 | 0xc7..=0xc9 | 0xcb | 0xcd | 0xcf..=0xd3 | 0xdb..=0xdd)
}

fn obj_guid(o: Obj) -> Guid {
    match o {
        Obj::Axis(0) => GUID_XAXIS,
        Obj::Axis(1) => GUID_YAXIS,
        Obj::Axis(2) => GUID_ZAXIS,
        Obj::Axis(_) => GUID_RZAXIS,
        Obj::Button(_) => GUID_BUTTON,
        Obj::Key(_) => GUID_KEY,
        Obj::Pov(_) => GUID_POV,
    }
}

fn obj_type(o: Obj, rel: bool) -> u32 {
    // DIDFT_MAKEINSTANCE(n) | type bits
    match o {
        Obj::Axis(n) => (n << 8) | if rel { 1 } else { 2 },
        Obj::Button(n) => (n << 8) | 4,
        Obj::Key(n) => (n << 8) | 4,
        Obj::Pov(n) => (n << 8) | 0x10,
    }
}

fn dev_set_data_format(s: &mut Session) -> ApiResult {
    let [_this, pf] = s.args::<2>()?;
    let (id, d) = dev_self(s)?;
    if d.acquired {
        return Ok(ApiRet::Ret(DIERR_ACQUIRED));
    }
    let flags = s.mem.read_u32(pf + 8)?;
    let data_size = s.mem.read_u32(pf + 12)?;
    let n = s.mem.read_u32(pf + 16)?;
    let objs_p = s.mem.read_u32(pf + 20)?;
    let all = objects(d.kind);
    let mut used = vec![false; all.len()];
    let mut fmt = Vec::new();
    for i in 0..n {
        let o = objs_p + 16 * i;
        let pguid = s.mem.read_u32(o)?;
        let ofs = s.mem.read_u32(o + 4)?;
        let ty = s.mem.read_u32(o + 8)?;
        let want_guid = if pguid != 0 { Some(s.read_guid(pguid)?) } else { None };
        let inst = (ty >> 8) & 0xffff;
        let any = inst == 0xffff;
        let tybits = ty & 0xff;
        // Pick the first unused device object matching the GUID, type and instance.
        let found = all.iter().enumerate().find(|(k, ob)| {
            if used[*k] {
                return false;
            }
            if let Some(g) = want_guid
                && obj_guid(**ob) != g
            {
                return false;
            }
            let ot = obj_type(**ob, d.rel_axes);
            let type_ok = (tybits & 0x3 != 0 && ot & 0x3 != 0)
                || (tybits & 0xc != 0 && ot & 0xc != 0)
                || (tybits & 0x10 != 0 && ot & 0x10 != 0)
                || tybits == 0;
            type_ok && (any || (ot >> 8) & 0xffff == inst)
        });
        match found {
            Some((k, ob)) => {
                used[k] = true;
                fmt.push(FormatEntry { obj: *ob, ofs });
            }
            None => {
                let optional = s.mem.read_u32(o + 8)? & 0x8000_0000 != 0; // DIDFT_OPTIONAL
                if !optional {
                    return Ok(ApiRet::Ret(DIERR_INVALIDPARAM));
                }
            }
        }
    }
    let dm = dev_mut(s, id);
    dm.format = fmt;
    dm.data_size = data_size;
    dm.rel_axes = if dm.kind == DevKind::Mouse {
        flags & 1 == 0
    } else {
        flags & 2 != 0
    };
    Ok(ApiRet::Ret(DI_OK))
}

fn dev_set_cooperative_level(s: &mut Session) -> ApiResult {
    let [_this, hwnd, flags] = s.args::<3>()?;
    let (id, _d) = dev_self(s)?;
    let dm = dev_mut(s, id);
    dm.coop = flags;
    dm.hwnd = hwnd;
    Ok(ApiRet::Ret(DI_OK))
}

fn focus_ok(s: &Session, d: &DiDevice) -> bool {
    // Foreground devices need the application to be active.
    d.coop & 0x8 != 0 || s.user.app_active
}

fn dev_acquire(s: &mut Session) -> ApiResult {
    let (id, d) = dev_self(s)?;
    if d.format.is_empty() && d.data_size == 0 {
        return Ok(ApiRet::Ret(DIERR_INVALIDPARAM));
    }
    if !focus_ok(s, &d) {
        return Ok(ApiRet::Ret(0x8007_0005)); // DIERR_OTHERAPPHASPRIO
    }
    if d.acquired {
        return Ok(ApiRet::Ret(S_FALSE));
    }
    let (mx, my) = (s.user.input.mouse_x, s.user.input.mouse_y);
    let dm = dev_mut(s, id);
    dm.acquired = true;
    dm.accum = [0; 3];
    dm.last_abs = Some((mx, my));
    dm.buffer.clear();
    Ok(ApiRet::Ret(DI_OK))
}

fn dev_unacquire(s: &mut Session) -> ApiResult {
    let (id, d) = dev_self(s)?;
    if !d.acquired {
        return Ok(ApiRet::Ret(S_FALSE));
    }
    dev_mut(s, id).acquired = false;
    Ok(ApiRet::Ret(DI_OK))
}

fn check_acquired(s: &mut Session, id: u32, d: &DiDevice) -> Option<u32> {
    if !d.acquired {
        return Some(DIERR_NOTACQUIRED);
    }
    if !focus_ok(s, d) {
        dev_mut(s, id).acquired = false;
        return Some(DIERR_INPUTLOST);
    }
    None
}

fn obj_value(s: &Session, d: &mut DiDevice, o: Obj) -> u32 {
    let inp = &s.user.input;
    match (d.kind, o) {
        (DevKind::Keyboard, Obj::Key(k)) => {
            let (scan, ext) = ((k & 0x7f) as u16, k & 0x80 != 0);
            let vk = vk_for_dik(scan, ext);
            if vk != 0 && inp.keys[vk as usize] { 0x80 } else { 0 }
        }
        (DevKind::Mouse, Obj::Axis(a)) => {
            if d.rel_axes {
                let v = d.accum[a as usize];
                d.accum[a as usize] = 0;
                v as u32
            } else {
                match a {
                    0 => inp.mouse_x as u32,
                    1 => inp.mouse_y as u32,
                    _ => 0,
                }
            }
        }
        (DevKind::Mouse, Obj::Button(b)) => {
            if inp.buttons & (1 << b) != 0 {
                0x80
            } else {
                0
            }
        }
        (DevKind::Joystick, Obj::Axis(a)) => {
            let raw = inp.joy_axes[(a as usize).min(3)] as i64;
            let (lo, hi) = d.ranges[(a as usize).min(3)];
            (lo as i64 + raw * (hi as i64 - lo as i64) / 65535) as u32
        }
        (DevKind::Joystick, Obj::Button(b)) => {
            if inp.joy_buttons & (1 << b) != 0 {
                0x80
            } else {
                0
            }
        }
        (DevKind::Joystick, Obj::Pov(_)) => 0xffff_ffff,
        _ => 0,
    }
}

fn vk_for_dik(scan: u16, ext: bool) -> u8 {
    // Inverse of the US keyboard scan table, preferring left/right-specific modifiers.
    match (scan, ext) {
        (0x2a, false) => 0xa0,
        (0x36, false) => 0xa1,
        (0x1d, false) => 0xa2,
        (0x1d, true) => 0xa3,
        (0x38, false) => 0xa4,
        (0x38, true) => 0xa5,
        (0x1c, true) => 0x0d,
        (0x35, true) => 0x6f,
        (0x47, true) => 0x24,
        (0x48, true) => 0x26,
        (0x49, true) => 0x21,
        (0x4b, true) => 0x25,
        (0x4d, true) => 0x27,
        (0x4f, true) => 0x23,
        (0x50, true) => 0x28,
        (0x51, true) => 0x22,
        (0x52, true) => 0x2d,
        (0x53, true) => 0x2e,
        (0x47, false) => 0x67,
        (0x48, false) => 0x68,
        (0x49, false) => 0x69,
        (0x4b, false) => 0x64,
        (0x4c, false) => 0x65,
        (0x4d, false) => 0x66,
        (0x4f, false) => 0x61,
        (0x50, false) => 0x62,
        (0x51, false) => 0x63,
        (0x52, false) => 0x60,
        (0x53, false) => 0x6e,
        _ => (1..=255u8)
            .find(|&v| {
                crate::input::vk_to_scan(v) == scan
                    && !matches!(v, 0x10..=0x12 | 0x60..=0x6f | 0x21..=0x28 | 0x2d | 0x2e)
            })
            .unwrap_or(0),
    }
}

fn dev_get_device_state(s: &mut Session) -> ApiResult {
    let [_this, cb, data] = s.args::<3>()?;
    let (id, mut d) = dev_self(s)?;
    if let Some(e) = check_acquired(s, id, &d) {
        return Ok(ApiRet::Ret(e));
    }
    if cb != d.data_size {
        return Ok(ApiRet::Ret(DIERR_INVALIDPARAM));
    }
    let mut buf = vec![0u8; cb as usize];
    for f in d.format.clone() {
        let v = obj_value(s, &mut d, f.obj);
        let o = f.ofs as usize;
        match f.obj {
            Obj::Key(_) | Obj::Button(_) => {
                if o < buf.len() {
                    buf[o] = v as u8;
                }
            }
            _ => {
                if o + 4 <= buf.len() {
                    buf[o..o + 4].copy_from_slice(&v.to_le_bytes());
                }
            }
        }
    }
    *dev_mut(s, id) = d;
    s.mem.write(data, &buf)?;
    Ok(ApiRet::Ret(DI_OK))
}

fn dev_get_device_data(s: &mut Session) -> ApiResult {
    let [_this, cbod, rgdod, pinout, flags] = s.args::<5>()?;
    let (id, d) = dev_self(s)?;
    if let Some(e) = check_acquired(s, id, &d) {
        return Ok(ApiRet::Ret(e));
    }
    if d.buffer_size == 0 {
        return Ok(ApiRet::Ret(DIERR_NOTINITIALIZED));
    }
    let want = s.mem.read_u32(pinout)?;
    let peek = flags & 1 != 0; // DIGDD_PEEK
    let dm = dev_mut(s, id);
    let n = (want as usize).min(dm.buffer.len());
    let items: Vec<_> = dm.buffer.iter().take(n).copied().collect();
    if !peek {
        dm.buffer.drain(..n);
    }
    let overflow = std::mem::replace(&mut dm.overflow, false);
    if rgdod != 0 {
        for (i, (ofs, val, ts, seq)) in items.iter().enumerate() {
            let p = rgdod + cbod * i as u32;
            s.mem.write_u32(p, *ofs)?;
            s.mem.write_u32(p + 4, *val)?;
            s.mem.write_u32(p + 8, *ts)?;
            s.mem.write_u32(p + 12, *seq)?;
        }
    }
    s.mem.write_u32(pinout, n as u32)?;
    Ok(ApiRet::Ret(if overflow { DI_BUFFEROVERFLOW } else { DI_OK }))
}

fn dev_poll(s: &mut Session) -> ApiResult {
    let (id, d) = dev_self(s)?;
    if let Some(e) = check_acquired(s, id, &d) {
        return Ok(ApiRet::Ret(e));
    }
    // Keyboard and mouse are interrupt-driven: Poll has no effect.
    Ok(ApiRet::Ret(1)) // DI_NOEFFECT
}

fn prop_obj(d: &DiDevice, how: u32, obj: u32) -> Option<usize> {
    match how {
        0 => None,
        1 => d.format.iter().find(|f| f.ofs == obj).map(|f| match f.obj {
            Obj::Axis(a) => a.min(3) as usize,
            _ => 0,
        }),
        _ => Some(((obj >> 8) & 0xffff).min(3) as usize),
    }
}

fn dev_set_property(s: &mut Session) -> ApiResult {
    let [_this, rguid, ph] = s.args::<3>()?;
    let (id, d) = dev_self(s)?;
    let prop = if rguid < 0x10000 { rguid } else { 0 };
    let how = s.mem.read_u32(ph + 12)?;
    let obj = s.mem.read_u32(ph + 8)?;
    match prop {
        1 => {
            // DIPROP_BUFFERSIZE
            if d.acquired {
                return Ok(ApiRet::Ret(DIERR_ACQUIRED));
            }
            let v = s.mem.read_u32(ph + 16)?;
            dev_mut(s, id).buffer_size = v;
        }
        2 => {
            // DIPROP_AXISMODE: 0 abs, 1 rel
            let v = s.mem.read_u32(ph + 16)?;
            dev_mut(s, id).rel_axes = v == 1;
        }
        4 => {
            let lo = s.mem.read_i32(ph + 16)?;
            let hi = s.mem.read_i32(ph + 20)?;
            let dm = dev_mut(s, id);
            match prop_obj(&d, how, obj) {
                Some(a) => dm.ranges[a] = (lo, hi),
                None => dm.ranges = [(lo, hi); 4],
            }
        }
        5 | 6 => {
            let v = s.mem.read_u32(ph + 16)?;
            let dm = dev_mut(s, id);
            match prop_obj(&d, how, obj) {
                Some(a) => dm.deadzone[a] = v,
                None => dm.deadzone = [v; 4],
            }
        }
        7 | 9 => {
            // FFGAIN / AUTOCENTER: no force feedback hardware.
            return Ok(ApiRet::Ret(DIERR_UNSUPPORTED));
        }
        _ => {
            s.missing("DirectInput", format!("SetProperty {prop}"));
            return Ok(ApiRet::Ret(DIERR_UNSUPPORTED));
        }
    }
    Ok(ApiRet::Ret(DI_OK))
}

fn dev_get_property(s: &mut Session) -> ApiResult {
    let [_this, rguid, ph] = s.args::<3>()?;
    let (_id, d) = dev_self(s)?;
    let prop = if rguid < 0x10000 { rguid } else { 0 };
    let how = s.mem.read_u32(ph + 12)?;
    let obj = s.mem.read_u32(ph + 8)?;
    match prop {
        1 => s.mem.write_u32(ph + 16, d.buffer_size)?,
        2 => s.mem.write_u32(ph + 16, d.rel_axes as u32)?,
        3 => s.mem.write_u32(ph + 16, 1)?, // DIPROP_GRANULARITY
        4 => {
            let (lo, hi) = d.ranges[prop_obj(&d, how, obj).unwrap_or(0)];
            s.mem.write_u32(ph + 16, lo as u32)?;
            s.mem.write_u32(ph + 20, hi as u32)?;
        }
        5 | 6 => s
            .mem
            .write_u32(ph + 16, d.deadzone[prop_obj(&d, how, obj).unwrap_or(0)])?,
        _ => return Ok(ApiRet::Ret(DIERR_UNSUPPORTED)),
    }
    Ok(ApiRet::Ret(DI_OK))
}

fn dev_get_capabilities(s: &mut Session) -> ApiResult {
    let [_this, p] = s.args::<2>()?;
    let (_id, d) = dev_self(s)?;
    let objs = objects(d.kind);
    let axes = objs.iter().filter(|o| matches!(o, Obj::Axis(_))).count() as u32;
    let buttons = objs
        .iter()
        .filter(|o| matches!(o, Obj::Button(_) | Obj::Key(_)))
        .count() as u32;
    let povs = objs.iter().filter(|o| matches!(o, Obj::Pov(_))).count() as u32;
    let size = s.mem.read_u32(p)?.clamp(4, 44);
    let mut b = vec![0u8; size as usize];
    for (i, v) in [size, 1, dev_type(d.kind), axes, buttons, povs].iter().enumerate() {
        if i * 4 + 4 <= b.len() {
            b[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
        }
    }
    s.mem.write(p, &b)?;
    Ok(ApiRet::Ret(DI_OK))
}

fn object_instance(o: Obj, rel: bool, ofs: u32, size: u32) -> Vec<u8> {
    let mut b = vec![0u8; size.max(296) as usize];
    b[0..4].copy_from_slice(&size.to_le_bytes());
    b[4..20].copy_from_slice(&obj_guid(o));
    b[20..24].copy_from_slice(&ofs.to_le_bytes());
    b[24..28].copy_from_slice(&obj_type(o, rel).to_le_bytes());
    let name = match o {
        Obj::Axis(0) => "X-axis".to_string(),
        Obj::Axis(1) => "Y-axis".to_string(),
        Obj::Axis(2) => "Wheel".to_string(),
        Obj::Axis(_) => "Rz-axis".to_string(),
        Obj::Button(n) => format!("Button {n}"),
        Obj::Key(k) => crate::input::scan_name((k & 0x7f) as u16, k & 0x80 != 0)
            .unwrap_or("Key")
            .to_string(),
        Obj::Pov(_) => "POV".to_string(),
    };
    let n = name.as_bytes();
    b[32..32 + n.len().min(259)].copy_from_slice(&n[..n.len().min(259)]);
    b.truncate(size as usize);
    b
}

fn dev_enum_objects(s: &mut Session) -> ApiResult {
    let [_this, cb, ctx, flags] = s.args::<4>()?;
    let (_id, d) = dev_self(s)?;
    let mut items = Vec::new();
    for o in objects(d.kind) {
        let t = obj_type(o, d.rel_axes);
        // DIDFT_ALL = 0; otherwise filter by type bits.
        if flags != 0 && flags & t & 0x1f == 0 {
            continue;
        }
        let ofs = d.format.iter().find(|f| f.obj == o).map(|f| f.ofs).unwrap_or(0);
        items.push(vec![
            CallArg::Buf(object_instance(o, d.rel_axes, ofs, 316)),
            CallArg::Val(ctx),
        ]);
    }
    enum_callbacks(cb, items, 0, DI_OK)
}

fn dev_get_object_info(s: &mut Session) -> ApiResult {
    let [_this, p, obj, how] = s.args::<4>()?;
    let (_id, d) = dev_self(s)?;
    let size = s.mem.read_u32(p)?;
    let o = match how {
        1 => d.format.iter().find(|f| f.ofs == obj).map(|f| f.obj),
        2 => objects(d.kind).into_iter().find(|o| obj_type(*o, d.rel_axes) == obj),
        _ => None,
    };
    match o {
        Some(o) => {
            let b = object_instance(o, d.rel_axes, obj, size);
            s.mem.write(p, &b)?;
            Ok(ApiRet::Ret(DI_OK))
        }
        None => Ok(ApiRet::Ret(DIERR_OBJECTNOTFOUND)),
    }
}

fn dev_get_device_info(s: &mut Session) -> ApiResult {
    let [_this, p] = s.args::<2>()?;
    let (_id, d) = dev_self(s)?;
    let size = s.mem.read_u32(p)?;
    let mut b = device_instance(d.kind);
    b.truncate(size.clamp(4, 580) as usize);
    b[0..4].copy_from_slice(&size.to_le_bytes());
    s.mem.write(p, &b)?;
    Ok(ApiRet::Ret(DI_OK))
}

fn dev_set_event_notification(s: &mut Session) -> ApiResult {
    let [_this, h] = s.args::<2>()?;
    let (id, _d) = dev_self(s)?;
    dev_mut(s, id).event = h;
    Ok(ApiRet::Ret(DI_OK))
}

fn dev_ff_none(_s: &mut Session) -> ApiResult {
    // The virtual devices have no force-feedback hardware.
    Ok(ApiRet::Ret(DIERR_UNSUPPORTED))
}

fn dev_enum_effects(s: &mut Session) -> ApiResult {
    let [_this, _cb, _ctx, _ty] = s.args::<4>()?;
    Ok(ApiRet::Ret(DI_OK))
}

fn dev_initialize(_s: &mut Session) -> ApiResult {
    Ok(ApiRet::Ret(DI_OK))
}

/// Feed an applied input event into acquired devices' buffers.
pub fn on_input(s: &mut Session, at_ns: u64, ev: &InputEvent) {
    let ts = ((at_ns + crate::dll::kernel32::UPTIME_AT_START_NS) / 1_000_000) as u32;
    let ids: Vec<u32> = s
        .com
        .objs
        .iter()
        .filter(|(_, o)| matches!(o.kind, ComKind::DInputDevice(_)))
        .map(|(k, _)| *k)
        .collect();
    let (mx, my) = (s.user.input.mouse_x, s.user.input.mouse_y);
    let mut events = Vec::new();
    for id in ids {
        let d = dev_mut(s, id);
        if !d.acquired {
            continue;
        }
        let mut push = |d: &mut DiDevice, obj: Obj, val: u32| {
            if let Some(f) = d.format.iter().find(|f| f.obj == obj) {
                if d.buffer_size > 0 {
                    if d.buffer.len() as u32 >= d.buffer_size {
                        d.buffer.pop_front();
                        d.overflow = true;
                    }
                    d.seq += 1;
                    d.buffer.push_back((f.ofs, val, ts, d.seq));
                }
                if d.event != 0 {
                    events.push(d.event);
                }
            }
        };
        match (d.kind, ev) {
            (
                DevKind::Keyboard,
                InputEvent::Key {
                    scan, extended, down, ..
                },
            ) => {
                let dik = (*scan as u32 & 0x7f) | if *extended { 0x80 } else { 0 };
                push(d, Obj::Key(dik), if *down { 0x80 } else { 0 });
            }
            (DevKind::Mouse, InputEvent::MouseRel { dx, dy }) => {
                d.accum[0] += dx;
                d.accum[1] += dy;
                if *dx != 0 {
                    push(d, Obj::Axis(0), *dx as u32);
                }
                if *dy != 0 {
                    push(d, Obj::Axis(1), *dy as u32);
                }
                d.last_abs = Some((mx, my));
            }
            (DevKind::Mouse, InputEvent::MouseMove { x, y }) => {
                let (lx, ly) = d.last_abs.unwrap_or((*x, *y));
                let (dx, dy) = (x - lx, y - ly);
                d.last_abs = Some((*x, *y));
                d.accum[0] += dx;
                d.accum[1] += dy;
                if dx != 0 {
                    push(d, Obj::Axis(0), dx as u32);
                }
                if dy != 0 {
                    push(d, Obj::Axis(1), dy as u32);
                }
            }
            (DevKind::Mouse, InputEvent::Wheel { delta }) => {
                d.accum[2] += delta;
                push(d, Obj::Axis(2), *delta as u32);
            }
            (DevKind::Mouse, InputEvent::MouseButton { button, down }) => {
                push(d, Obj::Button(*button as u32), if *down { 0x80 } else { 0 });
            }
            (DevKind::Joystick, InputEvent::JoyButton { button, down }) => {
                push(d, Obj::Button(*button as u32), if *down { 0x80 } else { 0 });
            }
            _ => {}
        }
    }
    for h in events {
        if let Some(crate::objects::KObj::Event { signaled, .. }) = s.objects.by_handle_mut(h) {
            *signaled = true;
        }
    }
}

// ---------------------------------------------------------------- tables

pub static IDIRECTINPUTA: IfaceDef = IfaceDef {
    name: "IDirectInputA",
    iid: guid("89521360-aa8a-11cf-bfc7-444553540000"),
    module: "DINPUT.DLL",
    methods: &[
        iunknown!()[0],
        iunknown!()[1],
        iunknown!()[2],
        export!("CreateDevice", 4, di_create_device),
        export!("EnumDevices", 5, di_enum_devices),
        export!("GetDeviceStatus", 2, di_get_device_status),
        unimpl_method!("RunControlPanel", 3),
        export!("Initialize", 3, di_initialize),
    ],
};

pub static IDIRECTINPUT2A: IfaceDef = IfaceDef {
    name: "IDirectInput2A",
    iid: guid("5944e662-aa8a-11cf-bfc7-444553540000"),
    module: "DINPUT.DLL",
    methods: &[
        iunknown!()[0],
        iunknown!()[1],
        iunknown!()[2],
        export!("CreateDevice", 4, di_create_device),
        export!("EnumDevices", 5, di_enum_devices),
        export!("GetDeviceStatus", 2, di_get_device_status),
        unimpl_method!("RunControlPanel", 3),
        export!("Initialize", 3, di_initialize),
        unimpl_method!("FindDevice", 4),
    ],
};

pub static IDIRECTINPUT7A: IfaceDef = IfaceDef {
    name: "IDirectInput7A",
    iid: guid("9a4cb684-236d-11d3-8e9d-00c04f6844ae"),
    module: "DINPUT.DLL",
    methods: &[
        iunknown!()[0],
        iunknown!()[1],
        iunknown!()[2],
        export!("CreateDevice", 4, di_create_device),
        export!("EnumDevices", 5, di_enum_devices),
        export!("GetDeviceStatus", 2, di_get_device_status),
        unimpl_method!("RunControlPanel", 3),
        export!("Initialize", 3, di_initialize),
        unimpl_method!("FindDevice", 4),
        unimpl_method!("CreateDeviceEx", 5),
    ],
};

pub static DINPUT_IFACES: &[&IfaceDef] = &[&IDIRECTINPUTA, &IDIRECTINPUT2A, &IDIRECTINPUT7A];

macro_rules! device_methods {
    ($($extra:expr),*) => {
        &[
            iunknown!()[0],
            iunknown!()[1],
            iunknown!()[2],
            export!("GetCapabilities", 2, dev_get_capabilities),
            export!("EnumObjects", 4, dev_enum_objects),
            export!("GetProperty", 3, dev_get_property),
            export!("SetProperty", 3, dev_set_property),
            export!("Acquire", 1, dev_acquire),
            export!("Unacquire", 1, dev_unacquire),
            export!("GetDeviceState", 3, dev_get_device_state),
            export!("GetDeviceData", 5, dev_get_device_data),
            export!("SetDataFormat", 2, dev_set_data_format),
            export!("SetEventNotification", 2, dev_set_event_notification),
            export!("SetCooperativeLevel", 3, dev_set_cooperative_level),
            export!("GetObjectInfo", 4, dev_get_object_info),
            export!("GetDeviceInfo", 2, dev_get_device_info),
            unimpl_method!("RunControlPanel", 3),
            export!("Initialize", 4, dev_initialize),
            $($extra),*
        ]
    };
}

pub static IDIRECTINPUTDEVICEA: IfaceDef = IfaceDef {
    name: "IDirectInputDeviceA",
    iid: guid("5944e680-c92e-11cf-bfc7-444553540000"),
    module: "DINPUT.DLL",
    methods: device_methods!(),
};

pub static IDIRECTINPUTDEVICE2A: IfaceDef = IfaceDef {
    name: "IDirectInputDevice2A",
    iid: guid("5944e682-c92e-11cf-bfc7-444553540000"),
    module: "DINPUT.DLL",
    methods: device_methods!(
        export!("CreateEffect", 5, dev_ff_none),
        export!("EnumEffects", 4, dev_enum_effects),
        export!("GetEffectInfo", 3, dev_ff_none),
        export!("GetForceFeedbackState", 2, dev_ff_none),
        export!("SendForceFeedbackCommand", 2, dev_ff_none),
        unimpl_method!("EnumCreatedEffectObjects", 4),
        export!("Escape", 2, dev_ff_none),
        export!("Poll", 1, dev_poll),
        unimpl_method!("SendDeviceData", 5)
    ),
};

pub static IDIRECTINPUTDEVICE7A: IfaceDef = IfaceDef {
    name: "IDirectInputDevice7A",
    iid: guid("57d7c6bc-2356-11d3-8e9d-00c04f6844ae"),
    module: "DINPUT.DLL",
    methods: device_methods!(
        export!("CreateEffect", 5, dev_ff_none),
        export!("EnumEffects", 4, dev_enum_effects),
        export!("GetEffectInfo", 3, dev_ff_none),
        export!("GetForceFeedbackState", 2, dev_ff_none),
        export!("SendForceFeedbackCommand", 2, dev_ff_none),
        unimpl_method!("EnumCreatedEffectObjects", 4),
        export!("Escape", 2, dev_ff_none),
        export!("Poll", 1, dev_poll),
        unimpl_method!("SendDeviceData", 5),
        unimpl_method!("EnumEffectsInFile", 5),
        unimpl_method!("WriteEffectToFile", 5)
    ),
};

pub static DEVICE_IFACES: &[&IfaceDef] = &[&IDIRECTINPUTDEVICEA, &IDIRECTINPUTDEVICE2A, &IDIRECTINPUTDEVICE7A];

pub static DINPUT_EXPORTS: &[ExportDef] = &[
    export!("DirectInputCreateA", 4, direct_input_create_a),
    export!("DirectInputCreateEx", 5, direct_input_create_ex),
];
