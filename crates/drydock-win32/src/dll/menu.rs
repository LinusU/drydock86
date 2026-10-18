//! user32: menus (resources, menu bars, popup menus and their items).
//!
//! Menus are data kept by the session; a window's menu bar is drawn with its frame.
//! Interactive menu tracking (opening menus with the mouse or keyboard) is not
//! implemented and is reported when an application asks for it.

use crate::api::*;
use crate::dll::user32::{Rect, WS_CHILD};
use crate::export;
use crate::session::Session;
use serde::{Deserialize, Serialize};

const MF_GRAYED: u32 = 0x1;
const MF_DISABLED: u32 = 0x2;
const MF_BITMAP: u32 = 0x4;
const MF_CHECKED: u32 = 0x8;
const MF_POPUP: u32 = 0x10;
const MF_END: u32 = 0x80;
const MF_OWNERDRAW: u32 = 0x100;
const MF_BYPOSITION: u32 = 0x400;
const MF_SEPARATOR: u32 = 0x800;

/// Flags stored per item (everything except how the call addresses the item).
const ITEM_FLAGS: u32 = !(MF_BYPOSITION | MF_END);

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MenuItem {
    pub flags: u32,
    pub id: u32,
    pub text: String,
    /// Submenu handle for MF_POPUP items.
    pub sub: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Menu {
    pub items: Vec<MenuItem>,
}

const HMENU_BASE: u32 = 0x0004_0000;

impl Session {
    fn new_menu(&mut self, items: Vec<MenuItem>) -> u32 {
        let h = HMENU_BASE
            + self
                .user
                .menus
                .keys()
                .next_back()
                .map(|k| k - HMENU_BASE + 1)
                .unwrap_or(1);
        self.user.menus.insert(h, Menu { items });
        h
    }

    /// The menu and position of an item addressed by position or (recursively) by
    /// command id.
    fn find_item(&self, h: u32, item: u32, flags: u32) -> Option<(u32, usize)> {
        let m = self.user.menus.get(&h)?;
        if flags & MF_BYPOSITION != 0 {
            return ((item as usize) < m.items.len()).then_some((h, item as usize));
        }
        for (i, it) in m.items.iter().enumerate() {
            if it.flags & MF_POPUP == 0 && it.id == item & 0xffff {
                return Some((h, i));
            }
        }
        for it in &m.items {
            if it.flags & MF_POPUP != 0
                && let Some(r) = self.find_item(it.sub, item, flags)
            {
                return Some(r);
            }
        }
        // A popup item can also be addressed by its submenu handle.
        m.items
            .iter()
            .position(|it| it.flags & MF_POPUP != 0 && it.sub == item)
            .map(|i| (h, i))
    }

    fn destroy_menu_tree(&mut self, h: u32) {
        if let Some(m) = self.user.menus.remove(&h) {
            for it in m.items {
                if it.flags & MF_POPUP != 0 {
                    self.destroy_menu_tree(it.sub);
                }
            }
        }
    }

    /// Text of a window's menu bar items (for drawing the frame).
    pub fn menu_bar_items(&self, hwnd: u32) -> Option<Vec<String>> {
        let w = self.window(hwnd)?;
        if w.style & WS_CHILD != 0 || w.id == 0 {
            return None;
        }
        let m = self.user.menus.get(&w.id)?;
        Some(m.items.iter().map(|i| i.text.replace('&', "")).collect())
    }
}

fn read_utf16z(b: &[u8], o: &mut usize) -> Option<String> {
    let mut w = Vec::new();
    loop {
        let c = u16::from_le_bytes([*b.get(*o)?, *b.get(*o + 1)?]);
        *o += 2;
        if c == 0 {
            break;
        }
        w.push(c);
    }
    Some(String::from_utf16_lossy(&w))
}

/// Parse a standard (version 0) menu template's item list, creating submenus.
fn parse_items(s: &mut Session, b: &[u8], o: &mut usize) -> Option<Vec<MenuItem>> {
    let mut items = Vec::new();
    loop {
        let flags = u16::from_le_bytes([*b.get(*o)?, *b.get(*o + 1)?]) as u32;
        *o += 2;
        let mut it = MenuItem {
            flags: flags & ITEM_FLAGS,
            ..Default::default()
        };
        if flags & MF_POPUP != 0 {
            it.text = read_utf16z(b, o)?;
            let sub = parse_items(s, b, o)?;
            it.sub = s.new_menu(sub);
        } else {
            it.id = u16::from_le_bytes([*b.get(*o)?, *b.get(*o + 1)?]) as u32;
            *o += 2;
            it.text = read_utf16z(b, o)?;
            if it.id == 0 && it.text.is_empty() {
                it.flags |= MF_SEPARATOR;
            }
        }
        items.push(it);
        if flags & MF_END != 0 {
            return Some(items);
        }
    }
}

fn menu_from_template(s: &mut Session, b: &[u8]) -> Result<u32, ApiError> {
    let version = u16::from_le_bytes([b[0], b[1]]);
    if version != 0 {
        s.missing("LoadMenu", format!("extended menu templates (version {version})"));
        return Ok(0);
    }
    let header = u16::from_le_bytes([b[2], b[3]]) as usize;
    let mut o = 4 + header;
    match parse_items(s, b, &mut o) {
        Some(items) => Ok(s.new_menu(items)),
        None => Err(ApiError::Fatal("malformed menu template".into())),
    }
}

fn load_menu_a(s: &mut Session) -> ApiResult {
    let [hinst, name] = s.args::<2>()?;
    let id = crate::dll::kernel32::res_id(s, name)?;
    let Some((ptr, size)) = crate::dll::kernel32::find_resource(s, hinst, drydock_core::pe::ResId::Id(4), id) else {
        s.set_last_error(1814); // ERROR_RESOURCE_NAME_NOT_FOUND
        return Ok(ApiRet::Ret(0));
    };
    let b = s.mem.read_vec(ptr, size.max(4))?;
    Ok(ApiRet::Ret(menu_from_template(s, &b)?))
}

fn load_menu_indirect_a(s: &mut Session) -> ApiResult {
    let [p] = s.args::<1>()?;
    let b = s.mem.read_vec(p, 0x4000).or_else(|_| s.mem.read_vec(p, 0x400))?;
    Ok(ApiRet::Ret(menu_from_template(s, &b)?))
}

fn create_menu(s: &mut Session) -> ApiResult {
    Ok(ApiRet::Ret(s.new_menu(Vec::new())))
}

fn destroy_menu(s: &mut Session) -> ApiResult {
    let [h] = s.args::<1>()?;
    if !s.user.menus.contains_key(&h) {
        s.set_last_error(1401); // ERROR_INVALID_MENU_HANDLE
        return Ok(ApiRet::Ret(0));
    }
    s.destroy_menu_tree(h);
    Ok(ApiRet::Ret(1))
}

fn is_menu(s: &mut Session) -> ApiResult {
    let [h] = s.args::<1>()?;
    Ok(ApiRet::Ret(s.user.menus.contains_key(&h) as u32))
}

fn get_menu(s: &mut Session) -> ApiResult {
    let [hwnd] = s.args::<1>()?;
    Ok(ApiRet::Ret(
        s.window(hwnd)
            .filter(|w| w.style & WS_CHILD == 0)
            .map(|w| w.id)
            .unwrap_or(0),
    ))
}

fn set_menu(s: &mut Session) -> ApiResult {
    let [hwnd, h] = s.args::<2>()?;
    if h != 0 && !s.user.menus.contains_key(&h) {
        s.set_last_error(1401);
        return Ok(ApiRet::Ret(0));
    }
    let Some(w) = s.user.windows.get_mut(&hwnd) else {
        s.set_last_error(1400);
        return Ok(ApiRet::Ret(0));
    };
    if w.style & WS_CHILD != 0 {
        s.set_last_error(1436); // ERROR_CHILD_WINDOW_MENU
        return Ok(ApiRet::Ret(0));
    }
    w.id = h;
    // The client area loses or gains the menu bar.
    let (x, y, cx, cy) = (w.rect.left, w.rect.top, w.rect.w(), w.rect.h());
    let steps = crate::dll::user32::window_pos_steps(s, hwnd, x, y, cx, cy, 0x20 | 0x2 | 0x1 | 0x4 | 0x10)?;
    draw_menu_bar_now(s, hwnd);
    crate::dll::user32::run_seq(s, steps, 1)
}

fn draw_menu_bar_now(s: &mut Session, hwnd: u32) {
    if s.window(hwnd).is_some_and(|w| w.visible) && !s.user.nc_dirty.contains(&hwnd) {
        s.user.nc_dirty.push(hwnd);
        if let Some(w) = s.user.windows.get_mut(&hwnd) {
            // A paint cycle redraws the frame.
            w.invalid.get_or_insert(Rect::new(0, 0, 0, 0));
        }
    }
}

fn draw_menu_bar(s: &mut Session) -> ApiResult {
    let [hwnd] = s.args::<1>()?;
    draw_menu_bar_now(s, hwnd);
    Ok(ApiRet::Ret(1))
}

fn get_system_menu(s: &mut Session) -> ApiResult {
    let [hwnd, revert] = s.args::<2>()?;
    if s.window(hwnd).is_none() {
        return Ok(ApiRet::Ret(0));
    }
    let existing = s.user.system_menus.get(&hwnd).copied();
    if revert != 0 {
        if let Some(h) = existing {
            s.destroy_menu_tree(h);
            s.user.system_menus.remove(&hwnd);
        }
        return Ok(ApiRet::Ret(0));
    }
    if let Some(h) = existing {
        return Ok(ApiRet::Ret(h));
    }
    let item = |id: u32, text: &str| MenuItem {
        flags: 0,
        id,
        text: text.into(),
        sub: 0,
    };
    let items = vec![
        item(0xf120, "&Restore"),
        item(0xf010, "&Move"),
        item(0xf000, "&Size"),
        item(0xf020, "Mi&nimize"),
        item(0xf030, "Ma&ximize"),
        MenuItem {
            flags: MF_SEPARATOR,
            ..Default::default()
        },
        item(0xf060, "&Close\tAlt+F4"),
    ];
    let h = s.new_menu(items);
    s.user.system_menus.insert(hwnd, h);
    Ok(ApiRet::Ret(h))
}

fn get_sub_menu(s: &mut Session) -> ApiResult {
    let [h, pos] = s.args::<2>()?;
    Ok(ApiRet::Ret(
        s.user
            .menus
            .get(&h)
            .and_then(|m| m.items.get(pos as usize))
            .filter(|i| i.flags & MF_POPUP != 0)
            .map(|i| i.sub)
            .unwrap_or(0),
    ))
}

fn get_menu_item_count(s: &mut Session) -> ApiResult {
    let [h] = s.args::<1>()?;
    match s.user.menus.get(&h) {
        Some(m) => Ok(ApiRet::Ret(m.items.len() as u32)),
        None => {
            s.set_last_error(1401);
            Ok(ApiRet::Ret(0xffff_ffff))
        }
    }
}

fn get_menu_item_id(s: &mut Session) -> ApiResult {
    let [h, pos] = s.args::<2>()?;
    Ok(ApiRet::Ret(
        s.user
            .menus
            .get(&h)
            .and_then(|m| m.items.get(pos as usize))
            .map(|i| if i.flags & MF_POPUP != 0 { 0xffff_ffff } else { i.id })
            .unwrap_or(0xffff_ffff),
    ))
}

fn get_menu_state(s: &mut Session) -> ApiResult {
    let [h, item, flags] = s.args::<3>()?;
    let Some((m, i)) = s.find_item(h, item, flags) else {
        return Ok(ApiRet::Ret(0xffff_ffff));
    };
    let it = s.user.menus[&m].items[i].clone();
    if it.flags & MF_POPUP != 0 {
        let n = s.user.menus.get(&it.sub).map(|x| x.items.len() as u32).unwrap_or(0);
        return Ok(ApiRet::Ret((n << 8) | (it.flags & 0xff)));
    }
    Ok(ApiRet::Ret(it.flags & 0xffff))
}

fn get_menu_string_a(s: &mut Session) -> ApiResult {
    let [h, item, buf, n, flags] = s.args::<5>()?;
    let Some((m, i)) = s.find_item(h, item, flags) else {
        return Ok(ApiRet::Ret(0));
    };
    let text = s.user.menus[&m].items[i].text.clone();
    if buf == 0 || n == 0 {
        return Ok(ApiRet::Ret(crate::strings::encode_ansi(&text).len() as u32));
    }
    Ok(ApiRet::Ret(s.write_str_trunc(buf, n, &text)?))
}

fn enable_menu_item(s: &mut Session) -> ApiResult {
    let [h, item, flags] = s.args::<3>()?;
    let Some((m, i)) = s.find_item(h, item, flags) else {
        return Ok(ApiRet::Ret(0xffff_ffff));
    };
    let it = &mut s.user.menus.get_mut(&m).unwrap().items[i];
    let old = it.flags & (MF_GRAYED | MF_DISABLED);
    it.flags = (it.flags & !(MF_GRAYED | MF_DISABLED)) | (flags & (MF_GRAYED | MF_DISABLED));
    Ok(ApiRet::Ret(old))
}

fn check_menu_item(s: &mut Session) -> ApiResult {
    let [h, item, flags] = s.args::<3>()?;
    let Some((m, i)) = s.find_item(h, item, flags) else {
        return Ok(ApiRet::Ret(0xffff_ffff));
    };
    let it = &mut s.user.menus.get_mut(&m).unwrap().items[i];
    let old = it.flags & MF_CHECKED;
    it.flags = (it.flags & !MF_CHECKED) | (flags & MF_CHECKED);
    Ok(ApiRet::Ret(old))
}

/// A new item from AppendMenu/InsertMenu/ModifyMenu arguments.
fn new_item(s: &Session, flags: u32, id: u32, data: u32) -> Result<MenuItem, ApiError> {
    let mut it = MenuItem {
        flags: flags & ITEM_FLAGS,
        ..Default::default()
    };
    if flags & MF_POPUP != 0 {
        it.sub = id;
    } else {
        it.id = id & 0xffff;
    }
    if flags & (MF_BITMAP | MF_OWNERDRAW | MF_SEPARATOR) == 0 && data != 0 {
        it.text = s.read_str(data)?;
    }
    Ok(it)
}

fn append_menu_a(s: &mut Session) -> ApiResult {
    let [h, flags, id, data] = s.args::<4>()?;
    let it = new_item(s, flags, id, data)?;
    match s.user.menus.get_mut(&h) {
        Some(m) => {
            m.items.push(it);
            Ok(ApiRet::Ret(1))
        }
        None => {
            s.set_last_error(1401);
            Ok(ApiRet::Ret(0))
        }
    }
}

fn insert_menu_a(s: &mut Session) -> ApiResult {
    let [h, pos, flags, id, data] = s.args::<5>()?;
    let it = new_item(s, flags, id, data)?;
    if !s.user.menus.contains_key(&h) {
        s.set_last_error(1401);
        return Ok(ApiRet::Ret(0));
    }
    // Position -1 (or an unknown command) appends.
    let (m, i) = if pos == 0xffff_ffff {
        (h, s.user.menus[&h].items.len())
    } else {
        s.find_item(h, pos, flags).unwrap_or((h, s.user.menus[&h].items.len()))
    };
    s.user.menus.get_mut(&m).unwrap().items.insert(i, it);
    Ok(ApiRet::Ret(1))
}

fn modify_menu_a(s: &mut Session) -> ApiResult {
    let [h, pos, flags, id, data] = s.args::<5>()?;
    let it = new_item(s, flags, id, data)?;
    let Some((m, i)) = s.find_item(h, pos, flags) else {
        return Ok(ApiRet::Ret(0));
    };
    s.user.menus.get_mut(&m).unwrap().items[i] = it;
    Ok(ApiRet::Ret(1))
}

fn remove_item(s: &mut Session, destroy: bool) -> ApiResult {
    let [h, pos, flags] = s.args::<3>()?;
    let Some((m, i)) = s.find_item(h, pos, flags) else {
        return Ok(ApiRet::Ret(0));
    };
    let it = s.user.menus.get_mut(&m).unwrap().items.remove(i);
    if destroy && it.flags & MF_POPUP != 0 {
        s.destroy_menu_tree(it.sub);
    }
    Ok(ApiRet::Ret(1))
}

fn delete_menu(s: &mut Session) -> ApiResult {
    remove_item(s, true)
}

fn remove_menu(s: &mut Session) -> ApiResult {
    remove_item(s, false)
}

fn set_menu_item_bitmaps(s: &mut Session) -> ApiResult {
    let [h, pos, flags, _unchecked, _checked] = s.args::<5>()?;
    // Check-mark bitmaps are accepted; menus are drawn with the standard marks.
    Ok(ApiRet::Ret(s.find_item(h, pos, flags).is_some() as u32))
}

fn get_menu_check_mark_dimensions(_s: &mut Session) -> ApiResult {
    Ok(ApiRet::Ret(13 | (13 << 16)))
}

fn track_popup_menu(s: &mut Session) -> ApiResult {
    let [h, flags, x, y, ..] = s.args::<7>()?;
    s.missing(
        "TrackPopupMenu",
        format!("interactive popup menus are not implemented (menu {h:#x} at {x},{y}, flags {flags:#x})"),
    );
    Ok(ApiRet::Ret(0))
}

pub static MENU_EXPORTS: &[ExportDef] = &[
    export!("AppendMenuA", 4, append_menu_a),
    export!("CheckMenuItem", 3, check_menu_item),
    export!("CreateMenu", 0, create_menu),
    export!("CreatePopupMenu", 0, create_menu),
    export!("DeleteMenu", 3, delete_menu),
    export!("DestroyMenu", 1, destroy_menu),
    export!("DrawMenuBar", 1, draw_menu_bar),
    export!("EnableMenuItem", 3, enable_menu_item),
    export!("GetMenu", 1, get_menu),
    export!("GetMenuCheckMarkDimensions", 0, get_menu_check_mark_dimensions),
    export!("GetMenuItemCount", 1, get_menu_item_count),
    export!("GetMenuItemID", 2, get_menu_item_id),
    export!("GetMenuState", 3, get_menu_state),
    export!("GetMenuStringA", 5, get_menu_string_a),
    export!("GetSubMenu", 2, get_sub_menu),
    export!("GetSystemMenu", 2, get_system_menu),
    export!("InsertMenuA", 5, insert_menu_a),
    export!("IsMenu", 1, is_menu),
    export!("LoadMenuA", 2, load_menu_a),
    export!("LoadMenuIndirectA", 1, load_menu_indirect_a),
    export!("ModifyMenuA", 5, modify_menu_a),
    export!("RemoveMenu", 3, remove_menu),
    export!("SetMenu", 2, set_menu),
    export!("SetMenuItemBitmaps", 5, set_menu_item_bitmaps),
    export!("TrackPopupMenu", 7, track_popup_menu),
];

#[cfg(test)]
mod tests {
    use super::*;

    fn w(s: &str) -> Vec<u8> {
        s.encode_utf16().chain([0]).flat_map(|c| c.to_le_bytes()).collect()
    }

    #[test]
    fn parse_standard_template() {
        // &File { &Open (100), separator, E&xit (101) }, &Help (200)
        let mut b = vec![0, 0, 0, 0];
        b.extend(0x10u16.to_le_bytes());
        b.extend(w("&File"));
        b.extend(0u16.to_le_bytes());
        b.extend(100u16.to_le_bytes());
        b.extend(w("&Open"));
        b.extend(0u16.to_le_bytes());
        b.extend(0u16.to_le_bytes());
        b.extend(w(""));
        b.extend(0x80u16.to_le_bytes());
        b.extend(101u16.to_le_bytes());
        b.extend(w("E&xit"));
        b.extend(0x80u16.to_le_bytes());
        b.extend(200u16.to_le_bytes());
        b.extend(w("&Help"));
        let mut s = crate::testutil::session_with(&drydock_fixtures::exit_with(0));
        let h = menu_from_template(&mut s, &b).unwrap();
        let m = s.user.menus[&h].clone();
        assert_eq!(m.items.len(), 2);
        assert_eq!(m.items[1].id, 200);
        let sub = &s.user.menus[&m.items[0].sub];
        assert_eq!(sub.items.len(), 3);
        assert!(sub.items[1].flags & MF_SEPARATOR != 0);
        assert_eq!(s.find_item(h, 101, 0), Some((m.items[0].sub, 2)));
    }
}
