//! user32: atoms, hooks, accelerators and window/class queries.

use crate::api::*;
use crate::dll::user32::{HWND_DESKTOP, WS_CHILD, run_seq, send};
use crate::export;
use crate::session::Session;
use crate::strings::StrOrId;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

// ---------------------------------------------------------------- atoms

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Atom {
    pub atom: u32,
    pub name: String,
    pub refs: u32,
}

/// Find or add a string atom; integer atoms (`#123` or values below 0xC000) map to
/// themselves.
fn add_atom(s: &mut Session, name: &str) -> u32 {
    if let Some(n) = name.strip_prefix('#')
        && let Ok(v) = n.parse::<u32>()
        && (1..0xc000).contains(&v)
    {
        return v;
    }
    let key = name.to_uppercase();
    if let Some(a) = s.user.atoms.get_mut(&key) {
        a.refs += 1;
        return a.atom;
    }
    let atom = s.user.next_atom;
    s.user.next_atom += 1;
    s.user.atoms.insert(
        key,
        Atom {
            atom,
            name: name.to_string(),
            refs: 1,
        },
    );
    atom
}

fn register_window_message_a(s: &mut Session) -> ApiResult {
    let [p] = s.args::<1>()?;
    let name = s.read_str(p)?;
    if name.is_empty() {
        s.set_last_error(87);
        return Ok(ApiRet::Ret(0));
    }
    Ok(ApiRet::Ret(add_atom(s, &name)))
}

pub fn global_add_atom_a(s: &mut Session) -> ApiResult {
    let [p] = s.args::<1>()?;
    match s.read_str_or_id(p)? {
        StrOrId::Id(v) => Ok(ApiRet::Ret(if (1..0xc000).contains(&v) { v } else { 0 })),
        StrOrId::Str(n) => Ok(ApiRet::Ret(add_atom(s, &n))),
    }
}

pub fn global_find_atom_a(s: &mut Session) -> ApiResult {
    let [p] = s.args::<1>()?;
    match s.read_str_or_id(p)? {
        StrOrId::Id(v) => Ok(ApiRet::Ret(v)),
        StrOrId::Str(n) => {
            let a = s.user.atoms.get(&n.to_uppercase()).map(|a| a.atom).unwrap_or(0);
            if a == 0 {
                s.set_last_error(2); // ERROR_FILE_NOT_FOUND
            }
            Ok(ApiRet::Ret(a))
        }
    }
}

pub fn global_delete_atom(s: &mut Session) -> ApiResult {
    let [atom] = s.args::<1>()?;
    let key = s
        .user
        .atoms
        .iter()
        .find(|(_, a)| a.atom == atom & 0xffff)
        .map(|(k, _)| k.clone());
    if let Some(k) = key {
        let a = s.user.atoms.get_mut(&k).unwrap();
        a.refs = a.refs.saturating_sub(1);
        if a.refs == 0 {
            s.user.atoms.remove(&k);
        }
    }
    Ok(ApiRet::Ret(0))
}

pub fn global_get_atom_name_a(s: &mut Session) -> ApiResult {
    let [atom, buf, n] = s.args::<3>()?;
    let atom = atom & 0xffff;
    let name = if atom < 0xc000 {
        Some(format!("#{atom}"))
    } else {
        s.user.atoms.values().find(|a| a.atom == atom).map(|a| a.name.clone())
    };
    match name {
        Some(name) => Ok(ApiRet::Ret(s.write_str_trunc(buf, n, &name)?)),
        None => {
            s.set_last_error(6); // ERROR_INVALID_HANDLE
            Ok(ApiRet::Ret(0))
        }
    }
}

// ---------------------------------------------------------------- hooks

pub const WH_CBT: i32 = 5;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hook {
    pub handle: u32,
    pub kind: i32,
    pub proc_: u32,
    /// Thread the hook applies to (0 = all threads of the process).
    pub tid: u32,
}

const HHOOK_BASE: u32 = 0x0002_0000;

/// The procedure of the first hook of `kind` that applies to the current thread.
pub fn first_hook(s: &Session, kind: i32) -> Option<u32> {
    let tid = s.tid();
    s.user
        .hooks
        .iter()
        .find(|h| h.kind == kind && (h.tid == 0 || h.tid == tid))
        .map(|h| h.proc_)
}

fn set_windows_hook_ex_a(s: &mut Session) -> ApiResult {
    let [kind, proc_, _hmod, tid] = s.args::<4>()?;
    let kind = kind as i32;
    if proc_ == 0 || !(-1..=14).contains(&kind) {
        s.set_last_error(1426); // ERROR_INVALID_HOOK_FILTER
        return Ok(ApiRet::Ret(0));
    }
    // Hooks other than WH_CBT are recorded but their events are not generated.
    if kind != WH_CBT {
        s.event(
            crate::diag::Severity::Info,
            "SetWindowsHookEx",
            format!("hook type {kind} is installed but never called"),
        );
    }
    s.user.next_hook += 1;
    let handle = HHOOK_BASE + s.user.next_hook;
    s.user.hooks.insert(
        0,
        Hook {
            handle,
            kind,
            proc_,
            tid,
        },
    );
    Ok(ApiRet::Ret(handle))
}

fn unhook_windows_hook_ex(s: &mut Session) -> ApiResult {
    let [h] = s.args::<1>()?;
    let before = s.user.hooks.len();
    s.user.hooks.retain(|x| x.handle != h);
    if s.user.hooks.len() == before {
        s.set_last_error(1404); // ERROR_INVALID_HOOK_HANDLE
        return Ok(ApiRet::Ret(0));
    }
    Ok(ApiRet::Ret(1))
}

fn call_next_hook_ex(s: &mut Session) -> ApiResult {
    let [h, code, wp, lp] = s.args::<4>()?;
    let tid = s.tid();
    let Some(i) = s.user.hooks.iter().position(|x| x.handle == h) else {
        return Ok(ApiRet::Ret(0));
    };
    let kind = s.user.hooks[i].kind;
    let next = s.user.hooks[i + 1..]
        .iter()
        .find(|x| x.kind == kind && (x.tid == 0 || x.tid == tid))
        .map(|x| x.proc_);
    match next {
        Some(p) => Ok(ApiRet::call(
            p,
            vec![code.into(), wp.into(), lp.into()],
            Cont::ReturnValue,
        )),
        None => Ok(ApiRet::Ret(0)),
    }
}

// ---------------------------------------------------------------- accelerators

/// An accelerator table handle is the address of its resource data (ACCELTABLEENTRY
/// records: flags, key, command, padding; the last has flag 0x80) or of a copy of it.
fn load_accelerators_a(s: &mut Session) -> ApiResult {
    let [hinst, name] = s.args::<2>()?;
    let name = crate::dll::kernel32::res_id(s, name)?;
    match crate::dll::kernel32::find_resource(s, hinst, drydock_core::pe::ResId::Id(9), name) {
        Some((ptr, _)) => Ok(ApiRet::Ret(ptr)),
        None => {
            s.set_last_error(1814); // ERROR_RESOURCE_NAME_NOT_FOUND
            Ok(ApiRet::Ret(0))
        }
    }
}

fn accel_entries(s: &Session, h: u32) -> Result<Vec<(u16, u16, u16)>, ApiError> {
    let mut v = Vec::new();
    for i in 0..1024 {
        let p = h + i * 8;
        let flags = s.mem.read_u16(p)?;
        v.push((flags & 0x7f, s.mem.read_u16(p + 2)?, s.mem.read_u16(p + 4)?));
        if flags & 0x80 != 0 {
            break;
        }
    }
    Ok(v)
}

fn copy_accelerator_table_a(s: &mut Session) -> ApiResult {
    let [h, out, n] = s.args::<3>()?;
    let e = accel_entries(s, h)?;
    if out == 0 {
        return Ok(ApiRet::Ret(e.len() as u32));
    }
    let k = e.len().min(n as usize);
    for (i, (f, key, cmd)) in e.iter().take(k).enumerate() {
        let p = out + i as u32 * 6;
        s.mem.write_u8(p, *f as u8)?;
        s.mem.write_u8(p + 1, 0)?;
        s.mem.write_u16(p + 2, *key)?;
        s.mem.write_u16(p + 4, *cmd)?;
    }
    Ok(ApiRet::Ret(k as u32))
}

fn translate_accelerator_a(s: &mut Session) -> ApiResult {
    let [hwnd, h, msgp] = s.args::<3>()?;
    if h == 0 || msgp == 0 {
        return Ok(ApiRet::Ret(0));
    }
    let msg = s.mem.read_u32(msgp + 4)?;
    let wp = s.mem.read_u32(msgp + 8)?;
    let virt = match msg {
        0x100 | 0x104 => true,  // WM_KEYDOWN, WM_SYSKEYDOWN
        0x102 | 0x106 => false, // WM_CHAR, WM_SYSCHAR
        _ => return Ok(ApiRet::Ret(0)),
    };
    let keys = &s.user.input.keys;
    let mods = ((keys[0x10] as u16) * 0x04) | ((keys[0x11] as u16) * 0x08) | ((keys[0x12] as u16) * 0x10);
    for (f, key, cmd) in accel_entries(s, h)? {
        let is_virt = f & 1 != 0;
        if is_virt != virt {
            continue;
        }
        let hit = if virt {
            key as u32 == wp & 0xff && f & 0x1c == mods
        } else {
            key as u32 == wp & 0xffff && (f & 0x10 != 0) == (msg == 0x106)
        };
        if hit {
            let steps = VecDeque::from(vec![send(hwnd, 0x111, cmd as u32 | (1 << 16), 0)]);
            return run_seq(s, steps, 1);
        }
    }
    Ok(ApiRet::Ret(0))
}

// ---------------------------------------------------------------- windows and classes

fn get_class_name_a(s: &mut Session) -> ApiResult {
    let [h, buf, n] = s.args::<3>()?;
    let Some(w) = s.window(h) else {
        s.set_last_error(1400);
        return Ok(ApiRet::Ret(0));
    };
    let name = s
        .user
        .classes
        .get(&w.class)
        .map(|c| c.name.clone())
        .unwrap_or_else(|| w.class.clone());
    Ok(ApiRet::Ret(s.write_str_trunc(buf, n, &name)?))
}

fn get_class_long_a(s: &mut Session) -> ApiResult {
    let [h, idx] = s.args::<2>()?;
    let Some(c) = s.window(h).and_then(|w| s.user.classes.get(&w.class)) else {
        s.set_last_error(1400);
        return Ok(ApiRet::Ret(0));
    };
    let v = match idx as i32 {
        -8 => c.menu,
        -10 => c.brush,
        -12 => c.cursor,
        -14 | -34 => c.icon,
        -16 => c.hinst,
        -18 => c.wnd_extra,
        -20 => c.cls_extra,
        -24 => c.wndproc,
        -26 => c.style,
        -32 => c.atom,
        _ => {
            s.set_last_error(1413);
            return Ok(ApiRet::Ret(0));
        }
    };
    Ok(ApiRet::Ret(v))
}

/// Siblings of `h` (windows with the same parent; top-level windows share parent 0)
/// in z-order, which Drydock keeps as creation order.
fn siblings(s: &Session, parent: u32, child: bool) -> Vec<u32> {
    s.user
        .windows
        .values()
        .filter(|w| {
            if child {
                w.style & WS_CHILD != 0 && w.parent == parent
            } else {
                w.style & WS_CHILD == 0
            }
        })
        .map(|w| w.hwnd)
        .collect()
}

fn get_window(s: &mut Session) -> ApiResult {
    let [h, cmd] = s.args::<2>()?;
    let r = if h == HWND_DESKTOP {
        match cmd {
            5 => siblings(s, 0, false).first().copied().unwrap_or(0),
            _ => 0,
        }
    } else {
        let Some(w) = s.window(h).cloned() else {
            s.set_last_error(1400);
            return Ok(ApiRet::Ret(0));
        };
        let child = w.style & WS_CHILD != 0;
        let sib = siblings(s, w.parent, child);
        let pos = sib.iter().position(|&x| x == h);
        match cmd {
            0 => sib.first().copied().unwrap_or(0),
            1 => sib.last().copied().unwrap_or(0),
            2 => pos.and_then(|i| sib.get(i + 1)).copied().unwrap_or(0),
            3 => pos.and_then(|i| i.checked_sub(1)).map(|i| sib[i]).unwrap_or(0),
            4 => {
                if child {
                    0
                } else {
                    w.owner
                }
            }
            5 => siblings(s, h, true).first().copied().unwrap_or(0),
            6 => s
                .user
                .windows
                .values()
                .find(|x| x.owner == h && x.visible && x.style & WS_CHILD == 0)
                .map(|x| x.hwnd)
                .unwrap_or(0),
            _ => {
                s.set_last_error(87);
                0
            }
        }
    };
    Ok(ApiRet::Ret(r))
}

fn get_top_window(s: &mut Session) -> ApiResult {
    let [h] = s.args::<1>()?;
    let r = if h == 0 || h == HWND_DESKTOP {
        siblings(s, 0, false).first().copied()
    } else {
        siblings(s, h, true).first().copied()
    };
    Ok(ApiRet::Ret(r.unwrap_or(0)))
}

fn is_child(s: &mut Session) -> ApiResult {
    let [parent, h] = s.args::<2>()?;
    // Only WS_CHILD links count: an owned popup is not a child.
    let mut cur = h;
    while let Some(w) = s.window(cur) {
        if w.style & WS_CHILD == 0 {
            return Ok(ApiRet::Ret(0));
        }
        if w.parent == parent {
            return Ok(ApiRet::Ret(1));
        }
        cur = w.parent;
    }
    Ok(ApiRet::Ret(0))
}

fn get_window_thread_process_id(s: &mut Session) -> ApiResult {
    let [h, pid] = s.args::<2>()?;
    let tid = if h == HWND_DESKTOP {
        Some(0)
    } else {
        s.window(h).map(|w| w.tid)
    };
    let Some(tid) = tid else {
        s.set_last_error(1400);
        return Ok(ApiRet::Ret(0));
    };
    if pid != 0 {
        let p = s.process.pid;
        s.mem.write_u32(pid, p)?;
    }
    Ok(ApiRet::Ret(tid))
}

fn get_last_active_popup(s: &mut Session) -> ApiResult {
    let [h] = s.args::<1>()?;
    let a = s.user.active;
    let owned = s.window(a).map(|w| w.owner == h).unwrap_or(false);
    Ok(ApiRet::Ret(if owned { a } else { h }))
}

fn get_window_text_length_a(s: &mut Session) -> ApiResult {
    let [h] = s.args::<1>()?;
    let n = s
        .window(h)
        .map(|w| crate::strings::encode_ansi(&w.title).len() as u32)
        .unwrap_or(0);
    Ok(ApiRet::Ret(n))
}

fn get_sys_color_brush(s: &mut Session) -> ApiResult {
    let [i] = s.args::<1>()?;
    if i > 30 {
        return Ok(ApiRet::Ret(0));
    }
    Ok(ApiRet::Ret(crate::dll::gdi32::sys_color_brush(s, i)))
}

fn in_send_message(_s: &mut Session) -> ApiResult {
    // Sent messages are always delivered on the sending thread.
    Ok(ApiRet::Ret(0))
}

fn message_beep(_s: &mut Session) -> ApiResult {
    Ok(ApiRet::Ret(1))
}

fn get_keyboard_type(s: &mut Session) -> ApiResult {
    let [which] = s.args::<1>()?;
    // IBM enhanced (101/102-key) keyboard.
    Ok(ApiRet::Ret(match which {
        0 => 4,
        1 => 0,
        2 => 12,
        _ => 0,
    }))
}

fn get_keyboard_layout(_s: &mut Session) -> ApiResult {
    Ok(ApiRet::Ret(0x0409_0409)) // US English
}

fn is_char_alpha_numeric_a(s: &mut Session) -> ApiResult {
    let [c] = s.args::<1>()?;
    let ch = crate::strings::decode_ansi(&[c as u8]).chars().next().unwrap_or('\0');
    Ok(ApiRet::Ret(ch.is_alphanumeric() as u32))
}

/// CharToOem/OemToChar: the OEM code page (437) agrees with cp1252 on ASCII; other
/// characters are converted through Unicode.
fn char_oem(s: &mut Session, to_oem: bool) -> ApiResult {
    let [src, dst] = s.args::<2>()?;
    if src == 0 || dst == 0 {
        return Ok(ApiRet::Ret(0));
    }
    let b = s.mem.read_cstr(src, 0x10000)?;
    let out: Vec<u8> = b
        .iter()
        .map(|&c| {
            if c < 0x80 {
                c
            } else if to_oem {
                let ch = crate::strings::decode_ansi(&[c]).chars().next().unwrap_or('?');
                CP437_HI
                    .iter()
                    .position(|&x| x == ch as u16)
                    .map(|i| 0x80 + i as u8)
                    .unwrap_or(b'?')
            } else {
                let w = CP437_HI[(c - 0x80) as usize];
                crate::strings::utf16_to_ansi(&[w])[0]
            }
        })
        .chain([0])
        .collect();
    s.mem.write(dst, &out)?;
    Ok(ApiRet::Ret(1))
}
fn char_to_oem_a(s: &mut Session) -> ApiResult {
    char_oem(s, true)
}
fn oem_to_char_a(s: &mut Session) -> ApiResult {
    char_oem(s, false)
}

const CP437_HI: [u16; 128] = [
    0xc7, 0xfc, 0xe9, 0xe2, 0xe4, 0xe0, 0xe5, 0xe7, 0xea, 0xeb, 0xe8, 0xef, 0xee, 0xec, 0xc4, 0xc5, 0xc9, 0xe6, 0xc6,
    0xf4, 0xf6, 0xf2, 0xfb, 0xf9, 0xff, 0xd6, 0xdc, 0xa2, 0xa3, 0xa5, 0x20a7, 0x192, 0xe1, 0xed, 0xf3, 0xfa, 0xf1,
    0xd1, 0xaa, 0xba, 0xbf, 0x2310, 0xac, 0xbd, 0xbc, 0xa1, 0xab, 0xbb, 0x2591, 0x2592, 0x2593, 0x2502, 0x2524, 0x2561,
    0x2562, 0x2556, 0x2555, 0x2563, 0x2551, 0x2557, 0x255d, 0x255c, 0x255b, 0x2510, 0x2514, 0x2534, 0x252c, 0x251c,
    0x2500, 0x253c, 0x255e, 0x255f, 0x255a, 0x2554, 0x2569, 0x2566, 0x2560, 0x2550, 0x256c, 0x2567, 0x2568, 0x2564,
    0x2565, 0x2559, 0x2558, 0x2552, 0x2553, 0x256b, 0x256a, 0x2518, 0x250c, 0x2588, 0x2584, 0x258c, 0x2590, 0x2580,
    0x3b1, 0xdf, 0x393, 0x3c0, 0x3a3, 0x3c3, 0xb5, 0x3c4, 0x3a6, 0x398, 0x3a9, 0x3b4, 0x221e, 0x3c6, 0x3b5, 0x2229,
    0x2261, 0xb1, 0x2265, 0x2264, 0x2320, 0x2321, 0xf7, 0x2248, 0xb0, 0x2219, 0xb7, 0x221a, 0x207f, 0xb2, 0x25a0, 0xa0,
];

fn win_help_a(s: &mut Session) -> ApiResult {
    let [_h, file, cmd, _data] = s.args::<4>()?;
    let file = s.read_str_opt(file)?;
    s.missing(
        "WinHelp",
        format!("no help viewer is available (file={file:?} cmd={cmd:#x})"),
    );
    Ok(ApiRet::Ret(0))
}

// ---------------------------------------------------------------- deferred positioning

const HDWP_BASE: u32 = 0x0003_0000;

fn begin_defer_window_pos(s: &mut Session) -> ApiResult {
    let [_n] = s.args::<1>()?;
    let h = HDWP_BASE + s.user.defer.keys().next_back().map(|k| k - HDWP_BASE + 1).unwrap_or(1);
    s.user.defer.insert(h, Vec::new());
    Ok(ApiRet::Ret(h))
}

fn defer_window_pos(s: &mut Session) -> ApiResult {
    let [h, hwnd, _after, x, y, cx, cy, flags] = s.args::<8>()?;
    if s.window(hwnd).is_none() {
        s.user.defer.remove(&h);
        s.set_last_error(1400);
        return Ok(ApiRet::Ret(0));
    }
    match s.user.defer.get_mut(&h) {
        Some(list) => {
            list.push((hwnd, x as i32, y as i32, cx as i32, cy as i32, flags));
            Ok(ApiRet::Ret(h))
        }
        None => {
            s.set_last_error(6);
            Ok(ApiRet::Ret(0))
        }
    }
}

fn end_defer_window_pos(s: &mut Session) -> ApiResult {
    let [h] = s.args::<1>()?;
    let Some(list) = s.user.defer.remove(&h) else {
        s.set_last_error(6);
        return Ok(ApiRet::Ret(0));
    };
    let mut steps = VecDeque::new();
    for (hwnd, x, y, cx, cy, flags) in list {
        steps.extend(crate::dll::user32::window_pos_steps(s, hwnd, x, y, cx, cy, flags)?);
    }
    run_seq(s, steps, 1)
}

fn to_ascii(s: &mut Session) -> ApiResult {
    let [vk, scan, keys, out, _flags] = s.args::<5>()?;
    if scan & 0x8000 != 0 {
        return Ok(ApiRet::Ret(0)); // key release
    }
    let state = if keys != 0 {
        s.mem.read_vec(keys, 256)?
    } else {
        vec![0; 256]
    };
    let shift = state[0x10] & 0x80 != 0;
    let caps = state[0x14] & 1 != 0;
    let ctrl = state[0x11] & 0x80 != 0;
    let Some(mut c) = crate::input::vk_to_char(vk as u8, shift, caps) else {
        return Ok(ApiRet::Ret(0));
    };
    if ctrl && c.is_ascii_alphabetic() {
        c = c.to_ascii_uppercase() - b'A' + 1;
    }
    s.mem.write_u16(out, c as u16)?;
    Ok(ApiRet::Ret(1))
}

/// MessageBoxExA = MessageBoxA plus a language id (the first four arguments match).
fn message_box_ex_a(s: &mut Session) -> ApiResult {
    crate::dll::user32::message_box_a(s)
}

/// DrawTextExA: DrawText with DRAWTEXTPARAMS (margins; the characters processed are
/// reported back). Tab stops use the DT_EXPANDTABS default of eight characters.
fn draw_text_ex_a(s: &mut Session) -> ApiResult {
    let [hdc, p, n, prc, fmt, params] = s.args::<6>()?;
    crate::dll::gdi32::draw_text(s, hdc, p, n, prc, fmt, params)
}

fn open_icon(s: &mut Session) -> ApiResult {
    let [h] = s.args::<1>()?;
    let Some(w) = s.user.windows.get_mut(&h) else {
        return Ok(ApiRet::Ret(0));
    };
    w.style &= !crate::dll::user32::WS_MINIMIZE;
    Ok(ApiRet::Ret(1))
}

pub static USER32_MISC_EXPORTS: &[ExportDef] = &[
    export!("DrawTextExA", 6, draw_text_ex_a),
    export!("LoadBitmapA", 2, crate::dll::gdi32_bitmap::load_bitmap_a),
    export!("MessageBoxExA", 5, message_box_ex_a),
    export!("OpenIcon", 1, open_icon),
    export!("ToAscii", 5, to_ascii),
    export!("BeginDeferWindowPos", 1, begin_defer_window_pos),
    export!("DeferWindowPos", 8, defer_window_pos),
    export!("EndDeferWindowPos", 1, end_defer_window_pos),
    export!("CallNextHookEx", 4, call_next_hook_ex),
    export!("CharToOemA", 2, char_to_oem_a),
    export!("CopyAcceleratorTableA", 3, copy_accelerator_table_a),
    export!("GetClassLongA", 2, get_class_long_a),
    export!("GetClassNameA", 3, get_class_name_a),
    export!("GetKeyboardLayout", 1, get_keyboard_layout),
    export!("GetKeyboardType", 1, get_keyboard_type),
    export!("GetLastActivePopup", 1, get_last_active_popup),
    export!("GetSysColorBrush", 1, get_sys_color_brush),
    export!("GetTopWindow", 1, get_top_window),
    export!("GetWindow", 2, get_window),
    export!("GetWindowTextLengthA", 1, get_window_text_length_a),
    export!("GetWindowThreadProcessId", 2, get_window_thread_process_id),
    export!("InSendMessage", 0, in_send_message),
    export!("IsCharAlphaNumericA", 1, is_char_alpha_numeric_a),
    export!("IsChild", 2, is_child),
    export!("LoadAcceleratorsA", 2, load_accelerators_a),
    export!("MessageBeep", 1, message_beep),
    export!("OemToCharA", 2, oem_to_char_a),
    export!("RegisterClipboardFormatA", 1, register_window_message_a),
    export!("RegisterWindowMessageA", 1, register_window_message_a),
    export!("SetWindowsHookExA", 4, set_windows_hook_ex_a),
    export!("TranslateAcceleratorA", 3, translate_accelerator_a),
    export!("UnhookWindowsHookEx", 1, unhook_windows_hook_ex),
    export!("WinHelpA", 4, win_help_a),
];
