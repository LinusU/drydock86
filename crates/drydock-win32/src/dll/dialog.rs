//! Dialog manager and standard controls (BUTTON, STATIC, EDIT, RichEdit).
//!
//! Dialog templates come from guest resources. Modal dialogs run their message loop as
//! an explicit continuation ([`DlgCont::Modal`]); window procedures of system control
//! classes are Rust handlers reached through ordinary thunks, so guest subclassing and
//! CallWindowProc work as on Windows.

use super::user32::*;
use crate::api::*;
use crate::export;
use crate::session::Session;
use crate::strings::StrOrId;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

pub const DWL_MSGRESULT: u32 = 0;
pub const DWL_DLGPROC: u32 = 4;
pub const DWL_USER: u32 = 8;
const DLGWINDOWEXTRA: u32 = 30;

const WM_INITDIALOG: u32 = 0x0110;
const WM_COMMAND: u32 = 0x0111;
const WM_SETFONT: u32 = 0x0030;
const WM_GETDLGCODE: u32 = 0x0087;
const WM_NEXTDLGCTL: u32 = 0x0028;
const DM_GETDEFID: u32 = WM_USER;
const BM_GETCHECK: u32 = 0x00f0;
const BM_SETCHECK: u32 = 0x00f1;
const BM_CLICK: u32 = 0x00f5;
const BN_CLICKED: u32 = 0;
const EM_STREAMIN: u32 = WM_USER + 73;
const EM_SETEVENTMASK: u32 = WM_USER + 69;
const EM_EXLIMITTEXT: u32 = WM_USER + 53;
const EM_SETBKGNDCOLOR: u32 = WM_USER + 67;
const EM_SETREADONLY: u32 = 0x00cf;
const EM_SETSEL: u32 = 0x00b1;
const EM_GETLINECOUNT: u32 = 0x00ba;
const EM_LINESCROLL: u32 = 0x00b6;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DlgState {
    pub hwnd: u32,
    pub ended: Option<u32>,
    pub modal: bool,
    pub owner: u32,
    pub font: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CtlState {
    /// List items of combo/list boxes: (text, item data).
    pub items: Vec<(String, u32)>,
    pub cursel: i32,
    pub checked: u32,
    pub pressed: bool,
    pub text_lines: Vec<String>,
    pub scroll: u32,
    pub font: u32,
    pub readonly: bool,
    /// Drop-down combo box: the list is open.
    #[serde(default)]
    pub dropped: bool,
    /// Drop-down combo box: height including the list, from the creation size.
    #[serde(default)]
    pub drop_h: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DlgCont {
    /// Waiting for the guest dialog procedure's answer to a message.
    AfterProc { hwnd: u32, msg: u32, wp: u32, lp: u32 },
    /// Modal loop of a dialog.
    Modal { hdlg: u32, owner: u32 },
    /// Modal loop: message dispatched, continue the loop.
    ModalDispatched { hdlg: u32, owner: u32 },
    /// Creation: finish after WM_INITDIALOG.
    Created { hdlg: u32, modal: bool, owner: u32 },
    /// EM_STREAMIN callback loop.
    Stream {
        hwnd: u32,
        es: u32,
        buf: u32,
        data: Vec<u8>,
        rtf: bool,
    },
}

impl Session {
    fn dlg(&self, h: u32) -> Option<&DlgState> {
        self.user.dialogs_mgr.iter().find(|d| d.hwnd == h)
    }
    fn dlg_mut(&mut self, h: u32) -> Option<&mut DlgState> {
        self.user.dialogs_mgr.iter_mut().find(|d| d.hwnd == h)
    }
    fn wnd_extra_u32(&self, h: u32, off: u32) -> u32 {
        self.window(h)
            .and_then(|w| w.extra.get(off as usize..off as usize + 4))
            .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
            .unwrap_or(0)
    }
    fn set_wnd_extra_u32(&mut self, h: u32, off: u32, v: u32) {
        if let Some(w) = self.user.windows.get_mut(&h)
            && let Some(b) = w.extra.get_mut(off as usize..off as usize + 4)
        {
            b.copy_from_slice(&v.to_le_bytes());
        }
    }
}

/// Make sure the system control classes exist (wndprocs are builtin thunks).
pub fn ensure_system_classes(s: &mut Session) {
    if s.user.classes.contains_key("#32770") {
        return;
    }
    for (name, proc_name, extra) in [
        ("#32770", "DefDlgProcA", DLGWINDOWEXTRA),
        ("BUTTON", "ButtonWndProcA", 4),
        ("STATIC", "StaticWndProcA", 4),
        ("EDIT", "EditWndProcA", 4),
        ("COMBOBOX", "ComboBoxWndProcA", 4),
        ("LISTBOX", "ListBoxWndProcA", 4),
    ] {
        let wndproc = s.thunks.find("USER32.DLL", proc_name).unwrap_or(0);
        let atom = s.user.next_atom;
        s.user.next_atom += 1;
        s.user.classes.insert(
            name.to_string(),
            WndClass {
                name: name.into(),
                style: 0x8,
                wndproc,
                cls_extra: 0,
                wnd_extra: extra,
                hinst: 0,
                icon: 0,
                cursor: 0,
                brush: if name == "#32770" { 16 } else { 0 },
                menu: 0,
                atom,
            },
        );
    }
}

/// Called when RICHED32.DLL is loaded: registers the "RichEdit" class.
pub fn register_richedit(s: &mut Session) {
    if s.user.classes.contains_key("RICHEDIT") {
        return;
    }
    let wndproc = s.thunks.find("RICHED32.DLL", "RichEditWndProc").unwrap_or(0);
    let atom = s.user.next_atom;
    s.user.next_atom += 1;
    s.user.classes.insert(
        "RICHEDIT".into(),
        WndClass {
            name: "RichEdit".into(),
            style: 0x8,
            wndproc,
            cls_extra: 0,
            wnd_extra: 4,
            hinst: 0,
            icon: 0,
            cursor: 0,
            brush: 0,
            menu: 0,
            atom,
        },
    );
}

// ---------------------------------------------------------------- templates

#[derive(Debug, Clone)]
struct DlgItem {
    style: u32,
    exstyle: u32,
    x: i16,
    y: i16,
    cx: i16,
    cy: i16,
    id: u32,
    class: StrOrId,
    title: StrOrId,
}

#[derive(Debug, Clone)]
struct DlgTemplate {
    style: u32,
    exstyle: u32,
    x: i16,
    y: i16,
    cx: i16,
    cy: i16,
    title: String,
    font: Option<(u16, String)>,
    items: Vec<DlgItem>,
}

fn parse_template(b: &[u8]) -> Option<DlgTemplate> {
    let mut o;
    let u16_at = |o: usize| b.get(o..o + 2).map(|x| u16::from_le_bytes([x[0], x[1]]));
    let u32_at = |o: usize| b.get(o..o + 4).map(|x| u32::from_le_bytes([x[0], x[1], x[2], x[3]]));
    let ex = u16_at(0)? == 1 && u16_at(2)? == 0xffff;
    let (style, exstyle, n);
    if ex {
        exstyle = u32_at(12)?;
        style = u32_at(16)?;
        n = u16_at(20)?;
        o = 22;
    } else {
        style = u32_at(0)?;
        exstyle = u32_at(4)?;
        n = u16_at(8)?;
        o = 10;
    }
    let x = u16_at(o)? as i16;
    let y = u16_at(o + 2)? as i16;
    let cx = u16_at(o + 4)? as i16;
    let cy = u16_at(o + 6)? as i16;
    o += 8;
    let read_sz_or_ord = |o: &mut usize| -> Option<StrOrId> {
        match u16_at(*o)? {
            0 => {
                *o += 2;
                Some(StrOrId::Str(String::new()))
            }
            0xffff => {
                let v = u16_at(*o + 2)?;
                *o += 4;
                Some(StrOrId::Id(v as u32))
            }
            _ => {
                let mut w = Vec::new();
                loop {
                    let c = u16_at(*o)?;
                    *o += 2;
                    if c == 0 {
                        break;
                    }
                    w.push(c);
                }
                Some(StrOrId::Str(String::from_utf16_lossy(&w)))
            }
        }
    };
    let _menu = read_sz_or_ord(&mut o)?;
    let _class = read_sz_or_ord(&mut o)?;
    let title = match read_sz_or_ord(&mut o)? {
        StrOrId::Str(s) => s,
        StrOrId::Id(_) => String::new(),
    };
    let mut font = None;
    if style & 0x40 != 0 {
        // DS_SETFONT
        let pt = u16_at(o)?;
        o += 2;
        if ex {
            o += 4; // weight, italic, charset
        }
        let face = match read_sz_or_ord(&mut o)? {
            StrOrId::Str(s) => s,
            _ => String::new(),
        };
        font = Some((pt, face));
    }
    let mut items = Vec::new();
    for _ in 0..n {
        o = (o + 3) & !3;
        let (istyle, iex, id);
        if ex {
            iex = u32_at(o + 4)?;
            istyle = u32_at(o + 8)?;
            o += 12;
        } else {
            istyle = u32_at(o)?;
            iex = u32_at(o + 4)?;
            o += 8;
        }
        let ix = u16_at(o)? as i16;
        let iy = u16_at(o + 2)? as i16;
        let icx = u16_at(o + 4)? as i16;
        let icy = u16_at(o + 6)? as i16;
        o += 8;
        if ex {
            id = u32_at(o)?;
            o += 4;
        } else {
            id = u16_at(o)? as u32;
            o += 2;
        }
        let class = read_sz_or_ord(&mut o)?;
        let title = read_sz_or_ord(&mut o)?;
        let extra = u16_at(o)? as usize;
        o += 2 + extra;
        items.push(DlgItem {
            style: istyle,
            exstyle: iex,
            x: ix,
            y: iy,
            cx: icx,
            cy: icy,
            id,
            class,
            title,
        });
    }
    Some(DlgTemplate {
        style,
        exstyle,
        x,
        y,
        cx,
        cy,
        title,
        font,
        items,
    })
}

// Dialog base units for MS Sans Serif 8pt.
const BASE_X: i32 = 6;
const BASE_Y: i32 = 13;
fn dlu_x(v: i32) -> i32 {
    v * BASE_X / 4
}
fn dlu_y(v: i32) -> i32 {
    v * BASE_Y / 8
}

fn class_for_item(c: &StrOrId) -> String {
    match c {
        StrOrId::Id(0x80) => "BUTTON".into(),
        StrOrId::Id(0x81) => "EDIT".into(),
        StrOrId::Id(0x82) => "STATIC".into(),
        StrOrId::Id(0x83) => "LISTBOX".into(),
        StrOrId::Id(0x84) => "SCROLLBAR".into(),
        StrOrId::Id(0x85) => "COMBOBOX".into(),
        StrOrId::Id(n) => format!("#{n}"),
        StrOrId::Str(s) => s.to_uppercase(),
    }
}

/// Create a window record directly (used for dialogs and their controls, whose
/// creation messages are then delivered through a message sequence).
fn new_window(
    s: &mut Session,
    class: &str,
    title: &str,
    style: u32,
    ex: u32,
    rect: Rect,
    parent: u32,
    id: u32,
) -> Option<u32> {
    let cls = s.user.classes.get(class).cloned()?;
    let hwnd = s.user.next_hwnd;
    s.user.next_hwnd += 0x10;
    let tid = s.tid();
    let mut w = Window {
        hwnd,
        class: class.to_string(),
        title: title.to_string(),
        style: style & !WS_VISIBLE,
        exstyle: ex,
        parent: if style & WS_CHILD != 0 { parent } else { 0 },
        owner: if style & WS_CHILD == 0 { parent } else { 0 },
        id,
        rect,
        client: rect,
        wndproc: cls.wndproc,
        hinst: 0,
        userdata: 0,
        extra: vec![0; cls.wnd_extra as usize],
        tid,
        visible: false,
        invalid: None,
        erase_pending: false,
        props: Default::default(),
    };
    let menu = false;
    let i = nc_insets(w.style, w.exstyle, menu);
    w.client = Rect::new(
        rect.left + i.left,
        rect.top + i.top,
        rect.right - i.right,
        rect.bottom - i.bottom,
    );
    s.user.windows.insert(hwnd, w);
    Some(hwnd)
}

fn create_dialog_common(
    s: &mut Session,
    hinst: u32,
    tmpl_p: u32,
    owner: u32,
    dlgproc: u32,
    param: u32,
    modal: bool,
    from_resource: bool,
) -> ApiResult {
    ensure_system_classes(s);
    let bytes = if from_resource {
        let id = match s.read_str_or_id(tmpl_p)? {
            StrOrId::Id(i) => drydock_core::pe::ResId::Id(i),
            StrOrId::Str(n) => drydock_core::pe::ResId::Name(n.to_uppercase()),
        };
        let Some((ptr, size)) =
            super::kernel32::find_resource(s, hinst, drydock_core::pe::ResId::Id(drydock_core::pe::RT_DIALOG), id)
        else {
            s.set_last_error(1814);
            return Ok(ApiRet::Ret(if modal { 0xffff_ffff } else { 0 }));
        };
        s.mem.read_vec(ptr, size)?
    } else {
        s.mem
            .read_vec(tmpl_p, 0x4000)
            .or_else(|_| s.mem.read_vec(tmpl_p, 0x400))?
    };
    let Some(t) = parse_template(&bytes) else {
        return Err(ApiError::Fatal("malformed dialog template".into()));
    };
    let mode = s.user.mode;
    let w = dlu_x(t.cx as i32);
    let h = dlu_y(t.cy as i32);
    let mut style = t.style;
    if style & WS_CHILD == 0 {
        style |= 0x0080_0000; // dialogs get a border (WS_BORDER) via DS_MODALFRAME
    }
    let ins = nc_insets(style, t.exstyle | WS_EX_DLGMODALFRAME, false);
    let (ow, oh) = (w + ins.left + ins.right, h + ins.top + ins.bottom);
    let (x, y) = if t.style & 0x800 != 0 || owner == 0 || owner == HWND_DESKTOP {
        ((mode.width as i32 - ow) / 2, (mode.height as i32 - oh) / 2)
    } else {
        let o = s.window(owner).map(|w| w.client).unwrap_or_default();
        (o.left + dlu_x(t.x as i32), o.top + dlu_y(t.y as i32))
    };
    let hdlg = new_window(
        s,
        "#32770",
        &t.title,
        style,
        t.exstyle | WS_EX_DLGMODALFRAME,
        Rect::new(x, y, x + ow, y + oh),
        owner,
        0,
    )
    .ok_or_else(|| ApiError::Fatal("dialog class missing".into()))?;
    s.set_wnd_extra_u32(hdlg, DWL_DLGPROC, dlgproc);
    let font = match &t.font {
        Some((pt, face)) => {
            let height = -((*pt as i32 * 96 + 36) / 72);
            super::gdi32::create_font(s, height, face)
        }
        None => 0,
    };
    s.user.dialogs_mgr.push(DlgState {
        hwnd: hdlg,
        ended: None,
        modal,
        owner,
        font,
    });
    let client = s.window(hdlg).unwrap().client;
    let mut steps = VecDeque::new();
    // The dialog window is created like any window (WC_DIALOG is atom 0x8002), then
    // receives its font before the controls are created.
    let r = s.window(hdlg).unwrap().rect;
    let cs = create_struct([
        param,
        hinst,
        0,
        owner,
        r.h() as u32,
        r.w() as u32,
        r.top as u32,
        r.left as u32,
        style,
        0,
        0x8002,
        t.exstyle | WS_EX_DLGMODALFRAME,
    ]);
    steps.extend(cbt_create_step(s, hdlg, &cs));
    steps.push_back(Step::Send {
        hwnd: hdlg,
        msg: WM_NCCREATE,
        wp: 0,
        lp: LParam::Buf(cs.clone()),
        check: Check::NcCreate(hdlg),
    });
    steps.push_back(Step::Send {
        hwnd: hdlg,
        msg: WM_CREATE,
        wp: 0,
        lp: LParam::Buf(cs),
        check: Check::Create(hdlg),
    });
    if font != 0 {
        steps.push_back(Step::Send {
            hwnd: hdlg,
            msg: WM_SETFONT,
            wp: font,
            lp: LParam::Val(0),
            check: Check::None,
        });
    }
    let mut first_tab = 0;
    for it in &t.items {
        let class = class_for_item(&it.class);
        let title = match &it.title {
            StrOrId::Str(s) => s.clone(),
            StrOrId::Id(_) => String::new(),
        };
        let r = Rect::new(
            client.left + dlu_x(it.x as i32),
            client.top + dlu_y(it.y as i32),
            client.left + dlu_x(it.x as i32 + it.cx as i32),
            client.top + dlu_y(it.y as i32 + it.cy as i32),
        );
        let Some(hc) = new_window(s, &class, &title, it.style | WS_CHILD, it.exstyle, r, hdlg, it.id) else {
            s.missing("dialog", format!("control class {class:?} is not implemented"));
            continue;
        };
        if first_tab == 0 && it.style & 0x0001_0000 != 0 {
            first_tab = hc;
        }
        if it.style & WS_VISIBLE != 0
            && let Some(cw) = s.user.windows.get_mut(&hc)
        {
            cw.visible = true;
            cw.style |= WS_VISIBLE;
            cw.invalid = Some(Rect::new(0, 0, r.w(), r.h()));
        }
        let atom = s.user.classes.get(&class).map(|c| c.atom).unwrap_or(0);
        let ccs = create_struct([
            0,
            hinst,
            it.id,
            hdlg,
            r.h() as u32,
            r.w() as u32,
            (r.top - client.top) as u32,
            (r.left - client.left) as u32,
            it.style | WS_CHILD,
            0,
            atom,
            it.exstyle | 0x4, // WS_EX_NOPARENTNOTIFY
        ]);
        steps.extend(cbt_create_step(s, hc, &ccs));
        steps.push_back(Step::Send {
            hwnd: hc,
            msg: WM_CREATE,
            wp: 0,
            lp: LParam::Buf(ccs),
            check: Check::None,
        });
        if font != 0 {
            steps.push_back(Step::Send {
                hwnd: hc,
                msg: WM_SETFONT,
                wp: font,
                lp: LParam::Val(0),
                check: Check::None,
            });
        }
    }
    if let Some(d) = s.dlg_mut(hdlg) {
        d.font = font;
    }
    let _ = first_tab;
    // WM_INITDIALOG goes through DefDlgProc to the dialog procedure.
    steps.push_back(Step::Send {
        hwnd: hdlg,
        msg: WM_INITDIALOG,
        wp: first_tab,
        lp: LParam::Val(param),
        check: Check::Store,
    });
    let r = run_seq(s, steps, 0)?;
    chain_after(s, r, Cont::Dialog(DlgCont::Created { hdlg, modal, owner }))
}

/// Run `r`; when it completes with a value, continue with `next`.
fn chain_after(s: &mut Session, r: ApiRet, next: Cont) -> ApiResult {
    match r {
        ApiRet::Ret(v) => s.resume(next, Resume::Returned { eax: v, edx: 0 }),
        ApiRet::Call(mut gc) => {
            gc.cont = Cont::Chain(Box::new(gc.cont), Box::new(next));
            Ok(ApiRet::Call(gc))
        }
        ApiRet::Block(mut b) => {
            b.cont = Cont::Chain(Box::new(b.cont), Box::new(next));
            Ok(ApiRet::Block(b))
        }
        other => Ok(other),
    }
}

/// Resume a chained continuation: finish `first`, then feed its result into `then`.
pub fn resume_chain(s: &mut Session, first: Cont, then: Cont, v: Resume) -> ApiResult {
    let r = s.resume(first, v)?;
    chain_after(s, r, then)
}

fn finish_creation(s: &mut Session, hdlg: u32, modal: bool, owner: u32, init_ret: u32) -> ApiResult {
    let mut steps: VecDeque<Step> = VecDeque::new();
    // Focus: WM_INITDIALOG returning TRUE gives focus to the first tab-stop control.
    if init_ret != 0 {
        let first = s
            .user
            .windows
            .values()
            .find(|w| w.parent == hdlg && w.style & 0x0001_0000 != 0 && w.style & WS_DISABLED == 0)
            .map(|w| w.hwnd);
        if let Some(f) = first {
            steps.push_back(Step::Act(Act::Focus(f)));
        }
    }
    let visible = s.window(hdlg).map(|w| w.style & WS_VISIBLE != 0).unwrap_or(false) || modal;
    if visible {
        if let Some(w) = s.user.windows.get_mut(&hdlg) {
            w.visible = true;
            w.style |= WS_VISIBLE;
        }
        invalidate_tree(s, hdlg);
        s.user.nc_dirty.push(hdlg);
        steps.push_back(Step::Act(Act::Activate(hdlg)));
    }
    if !modal {
        return run_seq(s, steps, hdlg);
    }
    if owner != 0
        && let Some(o) = s.user.windows.get_mut(&owner)
    {
        o.style |= WS_DISABLED;
    }
    let r = run_seq(s, steps, 0)?;
    chain_after(s, r, Cont::Dialog(DlgCont::Modal { hdlg, owner }))
}

fn modal_step(s: &mut Session, hdlg: u32, owner: u32) -> ApiResult {
    if let Some(res) = s.dlg(hdlg).and_then(|d| d.ended) {
        if owner != 0
            && let Some(o) = s.user.windows.get_mut(&owner)
        {
            o.style &= !WS_DISABLED;
        }
        s.user.dialogs_mgr.retain(|d| d.hwnd != hdlg);
        let steps = destroy_steps_pub(s, hdlg);
        return run_seq(s, steps.into(), res);
    }
    if s.window(hdlg).is_none() {
        s.user.dialogs_mgr.retain(|d| d.hwnd != hdlg);
        return Ok(ApiRet::Ret(0xffff_ffff));
    }
    let Some(m) = next_message_any(s) else {
        return Ok(ApiRet::Block(Block {
            wait: WaitKind::Message {
                objs: vec![],
                wake_mask: 0x4ff,
            },
            deadline: next_timer_due_pub(s),
            cont: Cont::Dialog(DlgCont::Modal { hdlg, owner }),
        }));
    };
    if m.message == WM_QUIT {
        // Re-post WM_QUIT for the outer loop and end the dialog.
        let tid = s.tid();
        s.user.queues.entry(tid).or_default().quit = Some(m.wparam);
        if let Some(d) = s.dlg_mut(hdlg) {
            d.ended = Some(0);
        }
        return modal_step(s, hdlg, owner);
    }
    // Dialog keyboard navigation (IsDialogMessage).
    if let Some(r) = dialog_keyboard(s, hdlg, &m)? {
        return chain_after(s, r, Cont::Dialog(DlgCont::ModalDispatched { hdlg, owner }));
    }
    translate(s, m.hwnd, m.message, m.wparam, m.lparam);
    let r = dispatch_msg(s, &m)?;
    chain_after(s, r, Cont::Dialog(DlgCont::ModalDispatched { hdlg, owner }))
}

fn dispatch_msg(s: &mut Session, m: &Msg) -> ApiResult {
    if m.message == WM_TIMER && m.lparam != 0 {
        return Ok(ApiRet::call(
            m.lparam,
            vec![m.hwnd.into(), m.message.into(), m.wparam.into(), m.time.into()],
            Cont::ReturnValue,
        ));
    }
    let Some(w) = s.window(m.hwnd) else {
        return Ok(ApiRet::Ret(0));
    };
    let p = w.wndproc;
    Ok(ApiRet::call(
        p,
        vec![m.hwnd.into(), m.message.into(), m.wparam.into(), m.lparam.into()],
        Cont::ReturnValue,
    ))
}

/// Keyboard handling of IsDialogMessage. Returns Some if the message was consumed.
fn dialog_keyboard(s: &mut Session, hdlg: u32, m: &Msg) -> Result<Option<ApiRet>, ApiError> {
    let in_dlg = m.hwnd == hdlg || s.window(m.hwnd).map(|w| w.parent == hdlg).unwrap_or(false);
    if !in_dlg {
        return Ok(None);
    }
    let controls: Vec<Window> = s
        .user
        .windows
        .values()
        .filter(|w| w.parent == hdlg && w.visible && w.style & WS_DISABLED == 0)
        .cloned()
        .collect();
    match m.message {
        WM_KEYDOWN => match m.wparam {
            0x0d => {
                // Enter: the focused push button, else the default button, else IDOK.
                let id = match s.window(s.user.focus) {
                    Some(f) if f.class == "BUTTON" && f.parent == hdlg && f.style & 0xf <= 1 => f.id,
                    _ => controls
                        .iter()
                        .find(|c| c.class == "BUTTON" && c.style & 0xf == 1)
                        .map(|c| c.id)
                        .unwrap_or(1),
                };
                Ok(Some(send_command(s, hdlg, id, BN_CLICKED)?))
            }
            0x1b => Ok(Some(send_command(s, hdlg, 2, BN_CLICKED)?)),
            0x09 => {
                let tabs: Vec<u32> = controls
                    .iter()
                    .filter(|c| c.style & 0x0001_0000 != 0)
                    .map(|c| c.hwnd)
                    .collect();
                if tabs.is_empty() {
                    return Ok(Some(ApiRet::Ret(1)));
                }
                let back = s.user.input.keys[0x10];
                let cur = tabs.iter().position(|&h| h == s.user.focus);
                let n = tabs.len();
                let next = match cur {
                    Some(i) if back => tabs[(i + n - 1) % n],
                    Some(i) => tabs[(i + 1) % n],
                    None => tabs[0],
                };
                Ok(Some(run_seq(s, VecDeque::from(vec![Step::Act(Act::Focus(next))]), 1)?))
            }
            0x20 => {
                if let Some(f) = s.window(s.user.focus)
                    && f.class == "BUTTON"
                    && f.parent == hdlg
                {
                    let id = f.id;
                    return Ok(Some(send_command(s, hdlg, id, BN_CLICKED)?));
                }
                Ok(None)
            }
            _ => Ok(None),
        },
        0x0106 | 0x0102 => {
            // WM_SYSCHAR / WM_CHAR: mnemonics (Alt+letter, or the letter itself).
            let ch = (m.wparam as u8).to_ascii_uppercase();
            if m.message == 0x0102 && s.window(s.user.focus).map(|f| f.class == "EDIT").unwrap_or(false) {
                return Ok(None);
            }
            for c in &controls {
                if c.class != "BUTTON" {
                    continue;
                }
                if let Some(i) = c.title.find('&')
                    && let Some(mc) = c.title[i + 1..].chars().next()
                    && mc.to_ascii_uppercase() as u8 == ch
                {
                    return Ok(Some(send_command(s, hdlg, c.id, BN_CLICKED)?));
                }
            }
            Ok(if m.message == 0x0106 {
                Some(ApiRet::Ret(1))
            } else {
                None
            })
        }
        _ => Ok(None),
    }
}

fn send_command(s: &mut Session, hdlg: u32, id: u32, code: u32) -> ApiResult {
    let ctl = s
        .user
        .windows
        .values()
        .find(|w| w.parent == hdlg && w.id == id)
        .map(|w| w.hwnd)
        .unwrap_or(0);
    run_seq(
        s,
        VecDeque::from(vec![Step::Send {
            hwnd: hdlg,
            msg: WM_COMMAND,
            wp: (code << 16) | (id & 0xffff),
            lp: LParam::Val(ctl),
            check: Check::None,
        }]),
        1,
    )
}

pub fn resume(s: &mut Session, c: DlgCont, v: Resume) -> ApiResult {
    match c {
        DlgCont::AfterProc { hwnd, msg, wp, lp } => {
            let r = v.eax();
            if r != 0 {
                // Handled by the dialog procedure.
                return Ok(ApiRet::Ret(match msg {
                    WM_INITDIALOG | 0x0133..=0x0138 | 0x0019 | WM_GETDLGCODE => r,
                    _ => s.wnd_extra_u32(hwnd, DWL_MSGRESULT),
                }));
            }
            def_dlg_default(s, hwnd, msg, wp, lp)
        }
        DlgCont::Created { hdlg, modal, owner } => finish_creation(s, hdlg, modal, owner, v.eax()),
        DlgCont::Modal { hdlg, owner } | DlgCont::ModalDispatched { hdlg, owner } => modal_step(s, hdlg, owner),
        DlgCont::Stream {
            hwnd,
            es,
            buf,
            mut data,
            rtf,
        } => {
            let cb_ret = v.eax();
            let got = s.mem.read_u32(buf + 0x1000)?;
            if cb_ret == 0 && got > 0 {
                data.extend(s.mem.read_vec(buf, got.min(0x1000))?);
                return stream_call(s, hwnd, es, buf, data, rtf);
            }
            if cb_ret != 0 {
                s.mem.write_u32(es + 4, cb_ret)?;
            }
            let sh = s.process.sys_heap;
            s.heaps.free(sh, buf);
            let text = if rtf {
                rtf_to_text(&data)
            } else {
                crate::strings::decode_ansi(&data)
            };
            let lines: Vec<String> = text.replace("\r\n", "\n").split('\n').map(|l| l.to_string()).collect();
            let n = text.len() as u32;
            let st = ctl_state(s, hwnd);
            st.text_lines = lines;
            st.scroll = 0;
            invalidate(s, hwnd);
            Ok(ApiRet::Ret(n))
        }
    }
}

// ---------------------------------------------------------------- DefDlgProc

fn def_dlg_proc_a(s: &mut Session) -> ApiResult {
    let [hwnd, msg, wp, lp] = s.args::<4>()?;
    let proc_ = s.wnd_extra_u32(hwnd, DWL_DLGPROC);
    if proc_ != 0 {
        s.set_wnd_extra_u32(hwnd, DWL_MSGRESULT, 0);
        return Ok(ApiRet::call(
            proc_,
            vec![hwnd.into(), msg.into(), wp.into(), lp.into()],
            Cont::Dialog(DlgCont::AfterProc { hwnd, msg, wp, lp }),
        ));
    }
    def_dlg_default(s, hwnd, msg, wp, lp)
}

fn def_dlg_default(s: &mut Session, hwnd: u32, msg: u32, wp: u32, lp: u32) -> ApiResult {
    match msg {
        WM_INITDIALOG => Ok(ApiRet::Ret(1)),
        WM_CLOSE => send_command(s, hwnd, 2, BN_CLICKED),
        DM_GETDEFID => {
            let id = s
                .user
                .windows
                .values()
                .find(|w| w.parent == hwnd && w.class == "BUTTON" && w.style & 0xf == 1)
                .map(|w| w.id)
                .unwrap_or(1);
            Ok(ApiRet::Ret(0x534b_0000 | id))
        }
        WM_NEXTDLGCTL => Ok(ApiRet::Ret(1)),
        _ => def_window_proc(s, hwnd, msg, wp, lp),
    }
}

fn dialog_box_param_a(s: &mut Session) -> ApiResult {
    let [hinst, tmpl, owner, proc_, param] = s.args::<5>()?;
    create_dialog_common(s, hinst, tmpl, owner, proc_, param, true, true)
}
fn dialog_box_indirect_param_a(s: &mut Session) -> ApiResult {
    let [hinst, tmpl, owner, proc_, param] = s.args::<5>()?;
    create_dialog_common(s, hinst, tmpl, owner, proc_, param, true, false)
}
fn create_dialog_param_a(s: &mut Session) -> ApiResult {
    let [hinst, tmpl, owner, proc_, param] = s.args::<5>()?;
    create_dialog_common(s, hinst, tmpl, owner, proc_, param, false, true)
}
fn create_dialog_indirect_param_a(s: &mut Session) -> ApiResult {
    let [hinst, tmpl, owner, proc_, param] = s.args::<5>()?;
    create_dialog_common(s, hinst, tmpl, owner, proc_, param, false, false)
}

fn end_dialog(s: &mut Session) -> ApiResult {
    let [h, r] = s.args::<2>()?;
    match s.dlg_mut(h) {
        Some(d) => {
            d.ended = Some(r);
            let modal = d.modal;
            if !modal {
                // Modeless dialogs are hidden; the application destroys them.
                if let Some(w) = s.user.windows.get_mut(&h) {
                    w.visible = false;
                }
            }
            Ok(ApiRet::Ret(1))
        }
        None => Ok(ApiRet::Ret(0)),
    }
}

fn get_dlg_item(s: &mut Session) -> ApiResult {
    let [h, id] = s.args::<2>()?;
    Ok(ApiRet::Ret(
        s.user
            .windows
            .values()
            .find(|w| w.parent == h && w.id == id)
            .map(|w| w.hwnd)
            .unwrap_or(0),
    ))
}

fn get_dlg_ctrl_id(s: &mut Session) -> ApiResult {
    let [h] = s.args::<1>()?;
    Ok(ApiRet::Ret(s.window(h).map(|w| w.id).unwrap_or(0)))
}

fn set_dlg_item_text_a(s: &mut Session) -> ApiResult {
    let [h, id, p] = s.args::<3>()?;
    let t = s.read_str_opt(p)?.unwrap_or_default();
    let Some(c) = s
        .user
        .windows
        .values()
        .find(|w| w.parent == h && w.id == id)
        .map(|w| w.hwnd)
    else {
        return Ok(ApiRet::Ret(0));
    };
    let mut b = crate::strings::encode_ansi(&t);
    b.push(0);
    run_seq(
        s,
        VecDeque::from(vec![Step::Send {
            hwnd: c,
            msg: WM_SETTEXT,
            wp: 0,
            lp: LParam::Buf(b),
            check: Check::Store,
        }]),
        1,
    )
}

fn get_dlg_item_text_a(s: &mut Session) -> ApiResult {
    let [h, id, p, n] = s.args::<4>()?;
    let t = s
        .user
        .windows
        .values()
        .find(|w| w.parent == h && w.id == id)
        .map(|w| w.title.clone())
        .unwrap_or_default();
    Ok(ApiRet::Ret(s.write_str_trunc(p, n, &t)?))
}

fn send_dlg_item_message_a(s: &mut Session) -> ApiResult {
    let [h, id, msg, wp, lp] = s.args::<5>()?;
    let Some(c) = s.user.windows.values().find(|w| w.parent == h && w.id == id).cloned() else {
        return Ok(ApiRet::Ret(0));
    };
    Ok(ApiRet::call(
        c.wndproc,
        vec![c.hwnd.into(), msg.into(), wp.into(), lp.into()],
        Cont::ReturnValue,
    ))
}

fn check_dlg_button(s: &mut Session) -> ApiResult {
    let [h, id, check] = s.args::<3>()?;
    if let Some(c) = s
        .user
        .windows
        .values()
        .find(|w| w.parent == h && w.id == id)
        .map(|w| w.hwnd)
    {
        ctl_state(s, c).checked = check;
        invalidate(s, c);
    }
    Ok(ApiRet::Ret(1))
}

fn is_dlg_button_checked(s: &mut Session) -> ApiResult {
    let [h, id] = s.args::<2>()?;
    let Some(c) = s
        .user
        .windows
        .values()
        .find(|w| w.parent == h && w.id == id)
        .map(|w| w.hwnd)
    else {
        return Ok(ApiRet::Ret(0));
    };
    Ok(ApiRet::Ret(ctl_state(s, c).checked))
}

fn is_dialog_message_a(s: &mut Session) -> ApiResult {
    let [hdlg, p] = s.args::<2>()?;
    let b = s.mem.read_vec(p, 28)?;
    let g = |o: usize| u32::from_le_bytes(b[o..o + 4].try_into().unwrap());
    let m = Msg {
        hwnd: g(0),
        message: g(4),
        wparam: g(8),
        lparam: g(12),
        time: g(16),
        pt: (g(20) as i32, g(24) as i32),
    };
    match dialog_keyboard(s, hdlg, &m)? {
        Some(r) => chain_after(s, r, Cont::ReturnConst(1)),
        None => {
            let in_dlg = m.hwnd == hdlg || s.window(m.hwnd).map(|w| w.parent == hdlg).unwrap_or(false);
            if !in_dlg {
                return Ok(ApiRet::Ret(0));
            }
            let r = dispatch_msg(s, &m)?;
            chain_after(s, r, Cont::ReturnConst(1))
        }
    }
}

fn map_dialog_rect(s: &mut Session) -> ApiResult {
    let [_h, p] = s.args::<2>()?;
    let r = s.read_rect(p)?;
    s.write_rect(
        p,
        &Rect::new(dlu_x(r.left), dlu_y(r.top), dlu_x(r.right), dlu_y(r.bottom)),
    )?;
    Ok(ApiRet::Ret(1))
}

fn get_dialog_base_units(_s: &mut Session) -> ApiResult {
    Ok(ApiRet::Ret(((BASE_Y as u32) << 16) | BASE_X as u32))
}

// ---------------------------------------------------------------- controls

fn ctl_state(s: &mut Session, h: u32) -> &mut CtlState {
    s.user.controls.entry(h).or_insert_with(|| CtlState {
        cursel: -1,
        ..Default::default()
    })
}

fn invalidate(s: &mut Session, h: u32) {
    if let Some(w) = s.user.windows.get_mut(&h) {
        w.invalid = Some(Rect::new(0, 0, w.client.w(), w.client.h()));
    }
}

fn control_common(s: &mut Session, hwnd: u32, msg: u32, wp: u32, lp: u32) -> Option<ApiResult> {
    match msg {
        WM_SETFONT => {
            ctl_state(s, hwnd).font = wp;
            Some(Ok(ApiRet::Ret(0)))
        }
        0x0031 => Some(Ok(ApiRet::Ret(ctl_state(s, hwnd).font))), // WM_GETFONT
        WM_SETTEXT => {
            let t = if lp != 0 {
                s.read_str(lp).unwrap_or_default()
            } else {
                String::new()
            };
            if let Some(w) = s.user.windows.get_mut(&hwnd) {
                w.title = t;
            }
            invalidate(s, hwnd);
            Some(Ok(ApiRet::Ret(1)))
        }
        _ => {
            let _ = wp;
            None
        }
    }
}

fn button_wnd_proc_a(s: &mut Session) -> ApiResult {
    let [hwnd, msg, wp, lp] = s.args::<4>()?;
    if let Some(r) = control_common(s, hwnd, msg, wp, lp) {
        return r;
    }
    let Some(w) = s.window(hwnd).cloned() else {
        return Ok(ApiRet::Ret(0));
    };
    match msg {
        WM_PAINT => {
            paint_button(s, &w)?;
            if let Some(wm) = s.user.windows.get_mut(&hwnd) {
                wm.invalid = None;
            }
            Ok(ApiRet::Ret(0))
        }
        WM_LBUTTONDOWN => {
            ctl_state(s, hwnd).pressed = true;
            s.user.capture = hwnd;
            invalidate(s, hwnd);
            run_seq(s, VecDeque::from(vec![Step::Act(Act::Focus(hwnd))]), 0)
        }
        WM_LBUTTONUP => {
            let was = std::mem::replace(&mut ctl_state(s, hwnd).pressed, false);
            if s.user.capture == hwnd {
                s.user.capture = 0;
            }
            invalidate(s, hwnd);
            let (x, y) = (lp as i16 as i32, (lp >> 16) as i16 as i32);
            if was && x >= 0 && y >= 0 && x < w.client.w() && y < w.client.h() {
                click(s, &w)
            } else {
                Ok(ApiRet::Ret(0))
            }
        }
        BM_CLICK => click(s, &w),
        BM_GETCHECK => Ok(ApiRet::Ret(ctl_state(s, hwnd).checked)),
        BM_SETCHECK => {
            ctl_state(s, hwnd).checked = wp;
            invalidate(s, hwnd);
            Ok(ApiRet::Ret(0))
        }
        WM_GETDLGCODE => Ok(ApiRet::Ret(if w.style & 0xf == 1 { 0x10 } else { 0x20 })),
        WM_SETFOCUS | WM_KILLFOCUS => {
            invalidate(s, hwnd);
            Ok(ApiRet::Ret(0))
        }
        _ => def_window_proc(s, hwnd, msg, wp, lp),
    }
}

fn click(s: &mut Session, w: &Window) -> ApiResult {
    let ty = w.style & 0xf;
    if ty == 2 || ty == 3 {
        // (AUTO)CHECKBOX toggles.
        let st = ctl_state(s, w.hwnd);
        if ty == 3 {
            st.checked ^= 1;
        }
    }
    if ty == 9 {
        // AUTORADIOBUTTON: uncheck siblings.
        let sib: Vec<u32> = s
            .user
            .windows
            .values()
            .filter(|x| x.parent == w.parent && x.class == "BUTTON" && x.style & 0xf == 9)
            .map(|x| x.hwnd)
            .collect();
        for h in sib {
            ctl_state(s, h).checked = (h == w.hwnd) as u32;
            invalidate(s, h);
        }
    }
    run_seq(
        s,
        VecDeque::from(vec![Step::Send {
            hwnd: w.parent,
            msg: WM_COMMAND,
            wp: (BN_CLICKED << 16) | (w.id & 0xffff),
            lp: LParam::Val(w.hwnd),
            check: Check::None,
        }]),
        0,
    )
}

fn static_wnd_proc_a(s: &mut Session) -> ApiResult {
    let [hwnd, msg, wp, lp] = s.args::<4>()?;
    if let Some(r) = control_common(s, hwnd, msg, wp, lp) {
        return r;
    }
    match msg {
        WM_PAINT => {
            if let Some(w) = s.window(hwnd).cloned() {
                paint_static(s, &w)?;
            }
            if let Some(wm) = s.user.windows.get_mut(&hwnd) {
                wm.invalid = None;
            }
            Ok(ApiRet::Ret(0))
        }
        WM_NCHITTEST => Ok(ApiRet::Ret(0xffff_ffff)), // HTTRANSPARENT
        _ => def_window_proc(s, hwnd, msg, wp, lp),
    }
}

/// COMBOBOX / LISTBOX: item storage, selection and a simple closed-state rendering.
fn list_wnd_proc(s: &mut Session, combo: bool) -> ApiResult {
    let [hwnd, msg, wp, lp] = s.args::<4>()?;
    if let Some(r) = control_common(s, hwnd, msg, wp, lp) {
        return r;
    }
    // Message numbers differ between combo boxes (CB_*) and list boxes (LB_*).
    let (add, insert, del, reset, setsel, getsel, count, gettext, gettextlen, setdata, getdata, find) = if combo {
        (
            0x143, 0x14a, 0x144, 0x14b, 0x14e, 0x147, 0x146, 0x148, 0x149, 0x151, 0x150, 0x14c,
        )
    } else {
        (
            0x180, 0x181, 0x182, 0x184, 0x186, 0x188, 0x18b, 0x189, 0x18a, 0x19a, 0x199, 0x18f,
        )
    };
    const ERR: u32 = 0xffff_ffff;
    let r = match msg {
        m if m == add => {
            let t = s.read_str(lp)?;
            let st = ctl_state(s, hwnd);
            st.items.push((t, 0));
            (st.items.len() - 1) as u32
        }
        m if m == insert => {
            let t = s.read_str(lp)?;
            let st = ctl_state(s, hwnd);
            let i = if wp as i32 == -1 {
                st.items.len()
            } else {
                (wp as usize).min(st.items.len())
            };
            st.items.insert(i, (t, 0));
            i as u32
        }
        m if m == del => {
            let st = ctl_state(s, hwnd);
            if (wp as usize) < st.items.len() {
                st.items.remove(wp as usize);
                st.items.len() as u32
            } else {
                ERR
            }
        }
        m if m == reset => {
            let st = ctl_state(s, hwnd);
            st.items.clear();
            st.cursel = -1;
            0
        }
        m if m == setsel => {
            let st = ctl_state(s, hwnd);
            if (wp as i32) < st.items.len() as i32 {
                st.cursel = wp as i32;
                wp
            } else {
                st.cursel = -1;
                ERR
            }
        }
        m if m == getsel => ctl_state(s, hwnd).cursel as u32,
        m if m == count => ctl_state(s, hwnd).items.len() as u32,
        m if m == gettext => {
            let t = ctl_state(s, hwnd).items.get(wp as usize).map(|i| i.0.clone());
            match t {
                Some(t) => {
                    let mut b = crate::strings::encode_ansi(&t);
                    let n = b.len() as u32;
                    b.push(0);
                    s.mem.write(lp, &b)?;
                    n
                }
                None => ERR,
            }
        }
        m if m == gettextlen => ctl_state(s, hwnd)
            .items
            .get(wp as usize)
            .map(|i| crate::strings::encode_ansi(&i.0).len() as u32)
            .unwrap_or(ERR),
        m if m == setdata => {
            let st = ctl_state(s, hwnd);
            match st.items.get_mut(wp as usize) {
                Some(i) => {
                    i.1 = lp;
                    0
                }
                None => ERR,
            }
        }
        m if m == getdata => ctl_state(s, hwnd).items.get(wp as usize).map(|i| i.1).unwrap_or(ERR),
        m if m == find => {
            let t = s.read_str(lp)?.to_uppercase();
            let st = ctl_state(s, hwnd);
            let start = if wp as i32 == -1 { 0 } else { wp as usize + 1 };
            st.items
                .iter()
                .enumerate()
                .skip(start)
                .find(|(_, i)| i.0.to_uppercase().starts_with(&t))
                .map(|(k, _)| k as u32)
                .unwrap_or(ERR)
        }
        WM_PAINT => {
            if let Some(w) = s.window(hwnd).cloned() {
                let st = ctl_state(s, hwnd).clone();
                let label = if st.cursel >= 0 {
                    st.items
                        .get(st.cursel as usize)
                        .map(|i| i.0.clone())
                        .unwrap_or_default()
                } else {
                    String::new()
                };
                let hdc = super::gdi32::window_dc(s, hwnd, false);
                let (cw, ch) = (
                    w.client.w(),
                    w.client.h().min(if combo { COMBO_CLOSED_H } else { i32::MAX }),
                );
                super::gdi32::fill_solid(s, hdc, &Rect::new(0, 0, cw, ch), 0xffffff)?;
                draw_edge(s, hdc, cw, ch, 0x808080, 0xffffff)?;
                let font = ctl_font_lf(s, hwnd);
                if combo && st.dropped {
                    // The open list below the box, the selection highlighted.
                    let top = COMBO_CLOSED_H;
                    let lh = w.client.h() - top;
                    super::gdi32::fill_solid(s, hdc, &Rect::new(0, top, cw, top + lh), 0xffffff)?;
                    super::gdi32::fill_solid(s, hdc, &Rect::new(0, top, cw, top + 1), 0)?;
                    super::gdi32::fill_solid(s, hdc, &Rect::new(0, top + lh - 1, cw, top + lh), 0)?;
                    super::gdi32::fill_solid(s, hdc, &Rect::new(0, top, 1, top + lh), 0)?;
                    super::gdi32::fill_solid(s, hdc, &Rect::new(cw - 1, top, cw, top + lh), 0)?;
                    for (i, it) in st.items.iter().enumerate() {
                        let y = top + 1 + i as i32 * COMBO_ITEM_H;
                        if y + COMBO_ITEM_H > top + lh {
                            break;
                        }
                        let sel = i as i32 == st.cursel;
                        if sel {
                            super::gdi32::fill_solid(s, hdc, &Rect::new(1, y, cw - 1, y + COMBO_ITEM_H), 0x6a240a)?;
                        }
                        super::gdi32::draw_label(
                            s,
                            hdc,
                            font,
                            &it.0,
                            &Rect::new(3, y, cw - 3, y + COMBO_ITEM_H),
                            0x20 | 0x4,
                            if sel { 0xffffff } else { 0 },
                        )?;
                    }
                }
                let text = if combo {
                    label
                } else {
                    st.items.iter().map(|i| i.0.clone()).collect::<Vec<_>>().join("\n")
                };
                super::gdi32::draw_label(
                    s,
                    hdc,
                    font,
                    &text,
                    &Rect::new(3, 2, cw - 3, ch - 2),
                    if combo { 0x20 | 0x4 } else { 0x10 },
                    0,
                )?;
                super::gdi32::release_dc_handle(s, hdc);
            }
            if let Some(wm) = s.user.windows.get_mut(&hwnd) {
                wm.invalid = None;
            }
            0
        }
        WM_KEYDOWN if wp == 0x26 || wp == 0x28 => {
            let st = ctl_state(s, hwnd);
            let n = st.items.len() as i32;
            let old = st.cursel;
            if n > 0 {
                st.cursel = (st.cursel + if wp == 0x28 { 1 } else { -1 }).clamp(0, n - 1);
            }
            let changed = st.cursel != old;
            invalidate(s, hwnd);
            if changed {
                // CBN_SELCHANGE / LBN_SELCHANGE (both 1).
                return run_seq(s, notify_steps(s, hwnd, 1).into(), 0);
            }
            0
        }
        WM_GETDLGCODE => 0x1 | 0x4,
        _ => return def_window_proc(s, hwnd, msg, wp, lp),
    };
    if msg == setsel || msg == add || msg == reset || msg == del || msg == insert {
        invalidate(s, hwnd);
    }
    Ok(ApiRet::Ret(r))
}

/// Height of a closed drop-down combo box and of one list item.
const COMBO_CLOSED_H: i32 = 21;
const COMBO_ITEM_H: i32 = 13;
const CBN_SELCHANGE: u32 = 1;
const CBN_DROPDOWN: u32 = 7;
const CBN_CLOSEUP: u32 = 8;

fn is_drop_down(s: &Session, hwnd: u32) -> bool {
    s.window(hwnd)
        .is_some_and(|w| w.class == "COMBOBOX" && matches!(w.style & 3, 2 | 3))
}

/// Resize a drop-down combo box to its closed or open height.
fn set_combo_height(s: &mut Session, hwnd: u32, h: i32) {
    if let Some(w) = s.user.windows.get_mut(&hwnd) {
        w.rect.bottom = w.rect.top + h;
        w.client.bottom = w.client.top + h;
    }
}

/// WM_COMMAND notification from a control to its parent.
fn notify_steps(s: &Session, hwnd: u32, code: u32) -> Vec<Step> {
    match s.window(hwnd) {
        Some(w) if w.parent != 0 => vec![Step::Send {
            hwnd: w.parent,
            msg: WM_COMMAND,
            wp: (code << 16) | (w.id & 0xffff),
            lp: LParam::Val(hwnd),
            check: Check::None,
        }],
        _ => vec![],
    }
}

fn combo_open(s: &mut Session, hwnd: u32) -> ApiResult {
    let st = ctl_state(s, hwnd);
    let list_h = (st.drop_h - COMBO_CLOSED_H)
        .min(st.items.len().max(1) as i32 * COMBO_ITEM_H + 2)
        .max(COMBO_ITEM_H + 2);
    st.dropped = true;
    set_combo_height(s, hwnd, COMBO_CLOSED_H + list_h);
    // The open list tracks the mouse until it closes.
    s.user.capture = hwnd;
    invalidate(s, hwnd);
    run_seq(s, notify_steps(s, hwnd, CBN_DROPDOWN).into(), 0)
}

fn combo_close(s: &mut Session, hwnd: u32, selected: bool) -> ApiResult {
    ctl_state(s, hwnd).dropped = false;
    set_combo_height(s, hwnd, COMBO_CLOSED_H);
    if s.user.capture == hwnd {
        s.user.capture = 0;
    }
    // The dialog repaints where the list was.
    let parent = s.window(hwnd).map(|w| w.parent).unwrap_or(0);
    invalidate_tree(s, parent);
    let mut steps = notify_steps(s, hwnd, CBN_CLOSEUP);
    if selected {
        steps.extend(notify_steps(s, hwnd, CBN_SELCHANGE));
    }
    run_seq(s, steps.into(), 0)
}

/// Mouse and focus handling of drop-down combo boxes.
fn combo_drop_down(s: &mut Session, hwnd: u32, msg: u32, lp: u32) -> Option<ApiResult> {
    if !is_drop_down(s, hwnd) {
        return None;
    }
    let dropped = ctl_state(s, hwnd).dropped;
    match msg {
        WM_LBUTTONDOWN if !dropped => Some(combo_open(s, hwnd)),
        WM_LBUTTONDOWN => {
            let (x, y) = ((lp & 0xffff) as i16 as i32, (lp >> 16) as i16 as i32);
            let w = s.window(hwnd)?.client;
            let mut selected = false;
            if x >= 0 && x < w.w() && y > COMBO_CLOSED_H && y < w.h() - 1 {
                let i = (y - COMBO_CLOSED_H - 1) / COMBO_ITEM_H;
                let st = ctl_state(s, hwnd);
                if (i as usize) < st.items.len() && st.cursel != i {
                    st.cursel = i;
                    selected = true;
                }
            }
            Some(combo_close(s, hwnd, selected))
        }
        WM_KILLFOCUS if dropped => Some(combo_close(s, hwnd, false)),
        0x0157 => Some(Ok(ApiRet::Ret(dropped as u32))), // CB_GETDROPPEDSTATE
        _ => None,
    }
}

fn combo_box_wnd_proc_a(s: &mut Session) -> ApiResult {
    let [hwnd, msg, wp, lp] = s.args::<4>()?;
    match msg {
        WM_CREATE if is_drop_down(s, hwnd) => {
            // The creation height includes the list; the closed box is one line.
            let h = s.window(hwnd).map(|w| w.rect.h()).unwrap_or(0);
            ctl_state(s, hwnd).drop_h = h.max(COMBO_CLOSED_H);
            set_combo_height(s, hwnd, COMBO_CLOSED_H);
            return Ok(ApiRet::Ret(0));
        }
        0x014f if is_drop_down(s, hwnd) => {
            let dropped = ctl_state(s, hwnd).dropped;
            return match (wp != 0, dropped) {
                (true, false) => combo_open(s, hwnd).map(|_| ApiRet::Ret(1)),
                (false, true) => combo_close(s, hwnd, false).map(|_| ApiRet::Ret(1)),
                _ => Ok(ApiRet::Ret(1)),
            };
        }
        0x0154 => {
            return Ok(ApiRet::Ret(if wp as i32 == -1 {
                COMBO_CLOSED_H as u32 - 6
            } else {
                COMBO_ITEM_H as u32
            }));
        }
        _ => {}
    }
    if let Some(r) = combo_drop_down(s, hwnd, msg, lp) {
        return r;
    }
    list_wnd_proc(s, true)
}
fn list_box_wnd_proc_a(s: &mut Session) -> ApiResult {
    list_wnd_proc(s, false)
}

fn edit_wnd_proc_a(s: &mut Session) -> ApiResult {
    let [hwnd, msg, wp, lp] = s.args::<4>()?;
    if let Some(r) = control_common(s, hwnd, msg, wp, lp) {
        return r;
    }
    text_box_proc(s, hwnd, msg, wp, lp)
}

fn rich_edit_wnd_proc(s: &mut Session) -> ApiResult {
    let [hwnd, msg, wp, lp] = s.args::<4>()?;
    if let Some(r) = control_common(s, hwnd, msg, wp, lp) {
        return r;
    }
    match msg {
        EM_STREAMIN => {
            let rtf = wp & 2 != 0;
            let buf = s.sys_alloc(0x1004)?;
            stream_call(s, hwnd, lp, buf, Vec::new(), rtf)
        }
        EM_SETEVENTMASK | EM_EXLIMITTEXT | EM_SETBKGNDCOLOR => Ok(ApiRet::Ret(0)),
        _ => text_box_proc(s, hwnd, msg, wp, lp),
    }
}

fn stream_call(s: &mut Session, hwnd: u32, es: u32, buf: u32, data: Vec<u8>, rtf: bool) -> ApiResult {
    let cookie = s.mem.read_u32(es)?;
    let cb = s.mem.read_u32(es + 8)?;
    s.mem.write_u32(buf + 0x1000, 0)?;
    Ok(ApiRet::call(
        cb,
        vec![cookie.into(), buf.into(), 0x1000u32.into(), (buf + 0x1000).into()],
        Cont::Dialog(DlgCont::Stream {
            hwnd,
            es,
            buf,
            data,
            rtf,
        }),
    ))
}

fn text_box_proc(s: &mut Session, hwnd: u32, msg: u32, wp: u32, lp: u32) -> ApiResult {
    match msg {
        WM_PAINT => {
            if let Some(w) = s.window(hwnd).cloned() {
                paint_text_box(s, &w)?;
            }
            if let Some(wm) = s.user.windows.get_mut(&hwnd) {
                wm.invalid = None;
            }
            Ok(ApiRet::Ret(0))
        }
        EM_SETREADONLY => {
            ctl_state(s, hwnd).readonly = wp != 0;
            Ok(ApiRet::Ret(1))
        }
        EM_SETSEL => Ok(ApiRet::Ret(0)),
        EM_GETLINECOUNT => Ok(ApiRet::Ret(ctl_state(s, hwnd).text_lines.len().max(1) as u32)),
        EM_LINESCROLL => {
            let st = ctl_state(s, hwnd);
            st.scroll = (st.scroll as i32 + lp as i32).max(0) as u32;
            invalidate(s, hwnd);
            Ok(ApiRet::Ret(1))
        }
        WM_KEYDOWN => {
            let st = ctl_state(s, hwnd);
            match wp {
                0x28 => st.scroll += 1,
                0x26 => st.scroll = st.scroll.saturating_sub(1),
                0x22 => st.scroll += 10,
                0x21 => st.scroll = st.scroll.saturating_sub(10),
                _ => {}
            }
            invalidate(s, hwnd);
            Ok(ApiRet::Ret(0))
        }
        WM_MOUSEWHEEL => {
            let d = (wp >> 16) as i16 as i32;
            let st = ctl_state(s, hwnd);
            st.scroll = (st.scroll as i32 - d / 40).max(0) as u32;
            invalidate(s, hwnd);
            Ok(ApiRet::Ret(0))
        }
        WM_LBUTTONDOWN => run_seq(s, VecDeque::from(vec![Step::Act(Act::Focus(hwnd))]), 0),
        WM_GETDLGCODE => Ok(ApiRet::Ret(0x1 | 0x2 | 0x80)),
        WM_GETTEXT => {
            let t = ctl_text(s, hwnd);
            Ok(ApiRet::Ret(s.write_str_trunc(lp, wp, &t)?))
        }
        WM_GETTEXTLENGTH => Ok(ApiRet::Ret(ctl_text(s, hwnd).len() as u32)),
        _ => def_window_proc(s, hwnd, msg, wp, lp),
    }
}

fn ctl_text(s: &mut Session, hwnd: u32) -> String {
    let lines = ctl_state(s, hwnd).text_lines.clone();
    if lines.is_empty() {
        s.window(hwnd).map(|w| w.title.clone()).unwrap_or_default()
    } else {
        lines.join("\r\n")
    }
}

/// Minimal RTF → plain text: drops groups for fonts/colours/info, control words, and
/// maps \par, \line, \tab and hex escapes.
pub fn rtf_to_text(data: &[u8]) -> String {
    let t = crate::strings::decode_ansi(data);
    let b: Vec<char> = t.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    let mut skip_depth: Vec<bool> = vec![false];
    while i < b.len() {
        let c = b[i];
        match c {
            '{' => {
                let parent = *skip_depth.last().unwrap();
                // Destination groups that carry no body text.
                let rest: String = b[i + 1..(i + 16).min(b.len())].iter().collect();
                let skip = parent
                    || [
                        "\\fonttbl",
                        "\\colortbl",
                        "\\stylesheet",
                        "\\info",
                        "\\*",
                        "\\pict",
                        "\\header",
                        "\\footer",
                    ]
                    .iter()
                    .any(|d| rest.starts_with(d));
                skip_depth.push(skip);
                i += 1;
            }
            '}' => {
                if skip_depth.len() > 1 {
                    skip_depth.pop();
                }
                i += 1;
            }
            '\\' => {
                i += 1;
                if i >= b.len() {
                    break;
                }
                let skipping = *skip_depth.last().unwrap();
                if b[i] == '\'' && i + 2 < b.len() {
                    let h: String = b[i + 1..i + 3].iter().collect();
                    if let Ok(v) = u8::from_str_radix(&h, 16)
                        && !skipping
                    {
                        out.push_str(&crate::strings::decode_ansi(&[v]));
                    }
                    i += 3;
                    continue;
                }
                if !b[i].is_ascii_alphabetic() {
                    if !skipping && matches!(b[i], '\\' | '{' | '}') {
                        out.push(b[i]);
                    }
                    i += 1;
                    continue;
                }
                let start = i;
                while i < b.len() && b[i].is_ascii_alphabetic() {
                    i += 1;
                }
                let word: String = b[start..i].iter().collect();
                while i < b.len() && (b[i].is_ascii_digit() || b[i] == '-') {
                    i += 1;
                }
                if i < b.len() && b[i] == ' ' {
                    i += 1;
                }
                if !skipping {
                    match word.as_str() {
                        "par" | "line" => out.push('\n'),
                        "tab" => out.push('\t'),
                        "bullet" => out.push('•'),
                        "lquote" | "rquote" => out.push('\''),
                        "ldblquote" | "rdblquote" => out.push('"'),
                        "emdash" => out.push('—'),
                        "endash" => out.push('–'),
                        _ => {}
                    }
                }
            }
            '\r' | '\n' => i += 1,
            _ => {
                if !*skip_depth.last().unwrap() {
                    out.push(c);
                }
                i += 1;
            }
        }
    }
    out
}

// ---------------------------------------------------------------- painting

fn ctl_font_lf(s: &mut Session, hwnd: u32) -> u32 {
    let f = ctl_state(s, hwnd).font;
    if f != 0 { f } else { super::gdi32::default_gui_font() }
}

fn paint_button(s: &mut Session, w: &Window) -> Result<(), ApiError> {
    let hdc = super::gdi32::window_dc(s, w.hwnd, false);
    let (cw, ch) = (w.client.w(), w.client.h());
    let pressed = ctl_state(s, w.hwnd).pressed;
    let checked = ctl_state(s, w.hwnd).checked;
    let font = ctl_font_lf(s, w.hwnd);
    let ty = w.style & 0xf;
    let face = 0xc8d0d4;
    if ty == 2 || ty == 3 || ty == 4 || ty == 9 {
        super::gdi32::fill_solid(s, hdc, &Rect::new(0, 0, cw, ch), face)?;
        let bx = Rect::new(0, (ch - 13) / 2, 13, (ch - 13) / 2 + 13);
        super::gdi32::fill_solid(s, hdc, &bx, 0x808080)?;
        super::gdi32::fill_solid(
            s,
            hdc,
            &Rect::new(bx.left + 1, bx.top + 1, bx.right - 1, bx.bottom - 1),
            0xffffff,
        )?;
        if checked != 0 {
            super::gdi32::fill_solid(
                s,
                hdc,
                &Rect::new(bx.left + 3, bx.top + 3, bx.right - 3, bx.bottom - 3),
                0,
            )?;
        }
        super::gdi32::draw_label(s, hdc, font, &w.title, &Rect::new(17, 0, cw, ch), 0x20 | 0x4, 0)?;
    } else {
        super::gdi32::fill_solid(s, hdc, &Rect::new(0, 0, cw, ch), face)?;
        let (tl, br) = if pressed {
            (0x404040, 0xffffff)
        } else {
            (0xffffff, 0x404040)
        };
        draw_edge(s, hdc, cw, ch, tl, br)?;
        let off = pressed as i32;
        super::gdi32::draw_label(
            s,
            hdc,
            font,
            &w.title,
            &Rect::new(off, off, cw + off, ch + off),
            0x20 | 0x4 | 0x1,
            0,
        )?;
        if s.user.focus == w.hwnd {
            for r in [
                Rect::new(4, 4, cw - 4, 5),
                Rect::new(4, ch - 5, cw - 4, ch - 4),
                Rect::new(4, 4, 5, ch - 4),
                Rect::new(cw - 5, 4, cw - 4, ch - 4),
            ] {
                super::gdi32::fill_solid(s, hdc, &r, 0x404040)?;
            }
        }
    }
    super::gdi32::release_dc_handle(s, hdc);
    Ok(())
}

fn draw_edge(s: &mut Session, hdc: u32, cw: i32, ch: i32, tl: u32, br: u32) -> Result<(), ApiError> {
    super::gdi32::fill_solid(s, hdc, &Rect::new(0, 0, cw, 1), tl)?;
    super::gdi32::fill_solid(s, hdc, &Rect::new(0, 0, 1, ch), tl)?;
    super::gdi32::fill_solid(s, hdc, &Rect::new(0, ch - 1, cw, ch), br)?;
    super::gdi32::fill_solid(s, hdc, &Rect::new(cw - 1, 0, cw, ch), br)?;
    Ok(())
}

fn paint_static(s: &mut Session, w: &Window) -> Result<(), ApiError> {
    let hdc = super::gdi32::window_dc(s, w.hwnd, false);
    let (cw, ch) = (w.client.w(), w.client.h());
    let ty = w.style & 0x1f;
    super::gdi32::fill_solid(s, hdc, &Rect::new(0, 0, cw, ch), 0xc8d0d4)?;
    if ty <= 2 {
        let font = ctl_font_lf(s, w.hwnd);
        let align = match ty {
            1 => 0x1,
            2 => 0x2,
            _ => 0,
        };
        super::gdi32::draw_label(s, hdc, font, &w.title, &Rect::new(0, 0, cw, ch), align | 0x10, 0)?;
    } else if ty == 3 {
        // SS_ICON: a neutral information glyph.
        super::gdi32::fill_solid(s, hdc, &Rect::new(0, 0, 32.min(cw), 32.min(ch)), 0xa56e3a)?;
    }
    super::gdi32::release_dc_handle(s, hdc);
    Ok(())
}

fn paint_text_box(s: &mut Session, w: &Window) -> Result<(), ApiError> {
    let hdc = super::gdi32::window_dc(s, w.hwnd, false);
    let (cw, ch) = (w.client.w(), w.client.h());
    super::gdi32::fill_solid(s, hdc, &Rect::new(0, 0, cw, ch), 0xffffff)?;
    draw_edge(s, hdc, cw, ch, 0x808080, 0xffffff)?;
    let font = ctl_font_lf(s, w.hwnd);
    let lines = {
        let st = ctl_state(s, w.hwnd);
        if st.text_lines.is_empty() {
            vec![w.title.clone()]
        } else {
            st.text_lines.clone()
        }
    };
    let scroll = ctl_state(s, w.hwnd).scroll as usize;
    let text: Vec<String> = lines.into_iter().skip(scroll).collect();
    super::gdi32::draw_label(
        s,
        hdc,
        font,
        &text.join("\n"),
        &Rect::new(3, 2, cw - 3, ch - 2),
        0x10,
        0,
    )?;
    super::gdi32::release_dc_handle(s, hdc);
    Ok(())
}

/// Dialog/static background for WM_ERASEBKGND of dialogs, and window frames.
pub fn paint_dialog_frame(s: &mut Session, hwnd: u32) -> Result<(), ApiError> {
    let Some(w) = s.window(hwnd).cloned() else {
        return Ok(());
    };
    let hdc = super::gdi32::window_dc(s, hwnd, true);
    let (ww, wh) = (w.rect.w(), w.rect.h());
    super::gdi32::fill_solid(s, hdc, &Rect::new(0, 0, ww, wh), 0xc8d0d4)?;
    draw_edge(s, hdc, ww, wh, 0xffffff, 0x404040)?;
    let menu = s.menu_bar_items(hwnd);
    let ins = nc_insets(w.style, w.exstyle, false);
    if let Some(items) = menu {
        // The menu bar sits between the caption and the client area.
        let top = w.client.top - w.rect.top - CYMENU;
        let bar = Rect::new(ins.left, top, ww - ins.left, top + CYMENU);
        super::gdi32::fill_solid(s, hdc, &bar, 0xc8d0d4)?;
        let f = super::gdi32::create_font(s, -11, "Tahoma");
        let mut x = bar.left + 6;
        for t in items {
            let r = Rect::new(x, bar.top, bar.right, bar.bottom);
            super::gdi32::draw_label(s, hdc, f, &t, &r, 0x20 | 0x4, 0)?;
            x += 7 * t.chars().count() as i32 + 14;
        }
    }
    if w.style & WS_CAPTION == WS_CAPTION {
        let cap = Rect::new(ins.left, ins.left, ww - ins.left, ins.top - 1);
        super::gdi32::fill_solid(s, hdc, &cap, 0x6a240a)?;
        let f = super::gdi32::create_font(s, -11, "Tahoma Bold");
        super::gdi32::draw_label(
            s,
            hdc,
            f,
            &w.title,
            &Rect::new(cap.left + 4, cap.top, cap.right, cap.bottom),
            0x20 | 0x4,
            0xffffff,
        )?;
    }
    super::gdi32::release_dc_handle(s, hdc);
    Ok(())
}

/// RICHED32.DLL exports.
pub static RICHED32_EXPORTS: &[ExportDef] = &[export!("RichEditWndProc", 4, rich_edit_wnd_proc)];

pub static DIALOG_EXPORTS: &[ExportDef] = &[
    export!("ButtonWndProcA", 4, button_wnd_proc_a),
    export!("CheckDlgButton", 3, check_dlg_button),
    export!("ComboBoxWndProcA", 4, combo_box_wnd_proc_a),
    export!("CreateDialogIndirectParamA", 5, create_dialog_indirect_param_a),
    export!("CreateDialogParamA", 5, create_dialog_param_a),
    export!("DefDlgProcA", 4, def_dlg_proc_a),
    export!("DialogBoxIndirectParamA", 5, dialog_box_indirect_param_a),
    export!("DialogBoxParamA", 5, dialog_box_param_a),
    export!("EditWndProcA", 4, edit_wnd_proc_a),
    export!("EndDialog", 2, end_dialog),
    export!("GetDialogBaseUnits", 0, get_dialog_base_units),
    export!("GetDlgCtrlID", 1, get_dlg_ctrl_id),
    export!("GetDlgItem", 2, get_dlg_item),
    export!("GetDlgItemTextA", 4, get_dlg_item_text_a),
    export!("IsDialogMessageA", 2, is_dialog_message_a),
    export!("IsDlgButtonChecked", 2, is_dlg_button_checked),
    export!("ListBoxWndProcA", 4, list_box_wnd_proc_a),
    export!("MapDialogRect", 2, map_dialog_rect),
    export!("SendDlgItemMessageA", 5, send_dlg_item_message_a),
    export!("SetDlgItemTextA", 3, set_dlg_item_text_a),
    export!("StaticWndProcA", 4, static_wnd_proc_a),
];
