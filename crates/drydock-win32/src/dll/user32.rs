//! user32: window classes, windows, message queues, input delivery, display settings.
//!
//! Messages that Windows sends synchronously (WM_CREATE, WM_ACTIVATE, WM_SIZE…) are
//! delivered to guest window procedures through an explicit message-sequence
//! continuation ([`U32Cont::Seq`]), so each API that triggers them is a resumable state
//! machine rather than nested host calls.

use crate::api::*;
use crate::diag::Severity;
use crate::input::{self, InputEvent};
use crate::session::Session;
use crate::strings::StrOrId;
use crate::{cdecl, export};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, VecDeque};

// Window messages.
pub const WM_NULL: u32 = 0x0000;
pub const WM_CREATE: u32 = 0x0001;
pub const WM_DESTROY: u32 = 0x0002;
pub const WM_MOVE: u32 = 0x0003;
pub const WM_SIZE: u32 = 0x0005;
pub const WM_ACTIVATE: u32 = 0x0006;
pub const WM_SETFOCUS: u32 = 0x0007;
pub const WM_KILLFOCUS: u32 = 0x0008;
pub const WM_ENABLE: u32 = 0x000a;
pub const WM_SETTEXT: u32 = 0x000c;
pub const WM_GETTEXT: u32 = 0x000d;
pub const WM_GETTEXTLENGTH: u32 = 0x000e;
pub const WM_PAINT: u32 = 0x000f;
pub const WM_CLOSE: u32 = 0x0010;
pub const WM_QUIT: u32 = 0x0012;
pub const WM_ERASEBKGND: u32 = 0x0014;
pub const WM_SHOWWINDOW: u32 = 0x0018;
pub const WM_ACTIVATEAPP: u32 = 0x001c;
pub const WM_SETCURSOR: u32 = 0x0020;
pub const WM_MOUSEACTIVATE: u32 = 0x0021;
pub const WM_GETMINMAXINFO: u32 = 0x0024;
pub const WM_WINDOWPOSCHANGING: u32 = 0x0046;
pub const WM_WINDOWPOSCHANGED: u32 = 0x0047;
pub const WM_DISPLAYCHANGE: u32 = 0x007e;
pub const WM_STYLECHANGING: u32 = 0x007c;
pub const WM_STYLECHANGED: u32 = 0x007d;
pub const WM_GETICON: u32 = 0x007f;
pub const WM_NCCREATE: u32 = 0x0081;
pub const WM_NCDESTROY: u32 = 0x0082;
pub const WM_NCCALCSIZE: u32 = 0x0083;
pub const WM_NCHITTEST: u32 = 0x0084;
pub const WM_NCPAINT: u32 = 0x0085;
pub const WM_NCACTIVATE: u32 = 0x0086;
pub const WM_KEYDOWN: u32 = 0x0100;
pub const WM_KEYUP: u32 = 0x0101;
pub const WM_CHAR: u32 = 0x0102;
pub const WM_SYSKEYDOWN: u32 = 0x0104;
pub const WM_SYSKEYUP: u32 = 0x0105;
pub const WM_SYSCHAR: u32 = 0x0106;
pub const WM_SYSCOMMAND: u32 = 0x0112;
pub const WM_TIMER: u32 = 0x0113;
pub const WM_MOUSEMOVE: u32 = 0x0200;
pub const WM_LBUTTONDOWN: u32 = 0x0201;
pub const WM_LBUTTONUP: u32 = 0x0202;
pub const WM_RBUTTONDOWN: u32 = 0x0204;
pub const WM_RBUTTONUP: u32 = 0x0205;
pub const WM_MBUTTONDOWN: u32 = 0x0207;
pub const WM_MBUTTONUP: u32 = 0x0208;
pub const WM_MOUSEWHEEL: u32 = 0x020a;
pub const WM_USER: u32 = 0x0400;

pub const WS_POPUP: u32 = 0x8000_0000;
pub const WS_CHILD: u32 = 0x4000_0000;
pub const WS_MINIMIZE: u32 = 0x2000_0000;
pub const WS_VISIBLE: u32 = 0x1000_0000;
pub const WS_DISABLED: u32 = 0x0800_0000;
pub const WS_MAXIMIZE: u32 = 0x0100_0000;
pub const WS_CAPTION: u32 = 0x00c0_0000;
pub const WS_BORDER: u32 = 0x0080_0000;
pub const WS_DLGFRAME: u32 = 0x0040_0000;
pub const WS_THICKFRAME: u32 = 0x0004_0000;
pub const WS_EX_DLGMODALFRAME: u32 = 0x1;
pub const WS_EX_TOPMOST: u32 = 0x8;
pub const WS_EX_CLIENTEDGE: u32 = 0x200;
pub const WS_EX_WINDOWEDGE: u32 = 0x100;

pub const HWND_DESKTOP: u32 = 0x0001_0010;
const HWND_BASE: u32 = 0x0001_0020;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Rect {
    pub fn new(left: i32, top: i32, right: i32, bottom: i32) -> Rect {
        Rect {
            left,
            top,
            right,
            bottom,
        }
    }
    pub fn w(&self) -> i32 {
        self.right - self.left
    }
    pub fn h(&self) -> i32 {
        self.bottom - self.top
    }
    pub fn bytes(&self) -> [u8; 16] {
        let mut b = [0u8; 16];
        b[0..4].copy_from_slice(&self.left.to_le_bytes());
        b[4..8].copy_from_slice(&self.top.to_le_bytes());
        b[8..12].copy_from_slice(&self.right.to_le_bytes());
        b[12..16].copy_from_slice(&self.bottom.to_le_bytes());
        b
    }
    pub fn from_bytes(b: &[u8]) -> Rect {
        let g = |o: usize| i32::from_le_bytes(b[o..o + 4].try_into().unwrap());
        Rect::new(g(0), g(4), g(8), g(12))
    }
    pub fn is_empty(&self) -> bool {
        self.right <= self.left || self.bottom <= self.top
    }
    pub fn intersect(&self, o: &Rect) -> Rect {
        Rect::new(
            self.left.max(o.left),
            self.top.max(o.top),
            self.right.min(o.right),
            self.bottom.min(o.bottom),
        )
    }
    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.left && x < self.right && y >= self.top && y < self.bottom
    }
}

impl Session {
    pub fn read_rect(&self, p: u32) -> Result<Rect, ApiError> {
        Ok(Rect::from_bytes(&self.mem.read_vec(p, 16)?))
    }
    pub fn write_rect(&mut self, p: u32, r: &Rect) -> Result<(), ApiError> {
        Ok(self.mem.write(p, &r.bytes())?)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WndClass {
    pub name: String,
    pub style: u32,
    pub wndproc: u32,
    pub cls_extra: u32,
    pub wnd_extra: u32,
    pub hinst: u32,
    pub icon: u32,
    pub cursor: u32,
    pub brush: u32,
    pub menu: u32,
    pub atom: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Window {
    pub hwnd: u32,
    pub class: String,
    pub title: String,
    pub style: u32,
    pub exstyle: u32,
    pub parent: u32,
    pub owner: u32,
    pub id: u32,
    /// Window rectangle in screen coordinates.
    pub rect: Rect,
    /// Client rectangle in screen coordinates.
    pub client: Rect,
    pub wndproc: u32,
    pub hinst: u32,
    pub userdata: u32,
    pub extra: Vec<u8>,
    pub tid: u32,
    pub visible: bool,
    pub invalid: Option<Rect>,
    pub erase_pending: bool,
    pub props: BTreeMap<String, u32>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Msg {
    pub hwnd: u32,
    pub message: u32,
    pub wparam: u32,
    pub lparam: u32,
    pub time: u32,
    pub pt: (i32, i32),
}

impl Msg {
    fn bytes(&self) -> [u8; 28] {
        let mut b = [0u8; 28];
        for (i, v) in [
            self.hwnd,
            self.message,
            self.wparam,
            self.lparam,
            self.time,
            self.pt.0 as u32,
            self.pt.1 as u32,
        ]
        .iter()
        .enumerate()
        {
            b[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
        }
        b
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Queue {
    pub posted: VecDeque<Msg>,
    pub input: VecDeque<Msg>,
    pub quit: Option<u32>,
    pub last_time: u32,
    pub last_pt: (i32, i32),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Timer {
    pub hwnd: u32,
    pub id: u32,
    pub period_ns: u64,
    pub due: u64,
    pub proc_: u32,
    pub tid: u32,
    /// A WM_TIMER is pending (only one is generated until retrieved).
    pub fired: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisplayMode {
    pub width: u32,
    pub height: u32,
    pub bpp: u32,
    pub refresh: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Dialog {
    pub id: u32,
    pub tid: u32,
    pub text: String,
    pub caption: String,
    pub style: u32,
    pub answer: Option<u32>,
}

/// A DeferWindowPos entry: (hwnd, x, y, cx, cy, flags).
pub type DeferredPos = (u32, i32, i32, i32, i32, u32);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserState {
    pub classes: BTreeMap<String, WndClass>,
    pub windows: BTreeMap<u32, Window>,
    pub next_hwnd: u32,
    pub next_atom: u32,
    pub queues: BTreeMap<u32, Queue>,
    pub focus: u32,
    pub active: u32,
    pub capture: u32,
    pub app_active: bool,
    pub cursor_count: i32,
    pub cursor: u32,
    pub clip: Option<Rect>,
    pub timers: Vec<Timer>,
    pub input: input::InputState,
    pub mode: DisplayMode,
    pub desktop_mode: DisplayMode,
    pub dialogs: Vec<Dialog>,
    pub next_dialog: u32,
    /// Answer message boxes with their default button (a scripted "press Enter").
    pub auto_dismiss_dialogs: bool,
    pub mouse_hover: u32,
    pub dialogs_mgr: Vec<crate::dll::dialog::DlgState>,
    pub controls: BTreeMap<u32, crate::dll::dialog::CtlState>,
    /// Top-level windows whose frame must be repainted.
    pub nc_dirty: Vec<u32>,
    /// String atoms (window messages, clipboard formats, global atoms), by upper-case name.
    #[serde(default)]
    pub atoms: BTreeMap<String, crate::dll::user32_misc::Atom>,
    /// Installed hooks, most recently installed first.
    #[serde(default)]
    pub hooks: Vec<crate::dll::user32_misc::Hook>,
    #[serde(default)]
    pub next_hook: u32,
    /// Pending DeferWindowPos lists by HDWP.
    #[serde(default)]
    pub defer: BTreeMap<u32, Vec<DeferredPos>>,
    #[serde(default)]
    pub menus: BTreeMap<u32, crate::dll::menu::Menu>,
    /// System menus by window.
    #[serde(default)]
    pub system_menus: BTreeMap<u32, u32>,
}

impl Default for UserState {
    fn default() -> Self {
        let m = DisplayMode {
            width: 1024,
            height: 768,
            bpp: 32,
            refresh: 60,
        };
        UserState {
            classes: BTreeMap::new(),
            windows: BTreeMap::new(),
            next_hwnd: HWND_BASE,
            next_atom: 0xc000,
            queues: BTreeMap::new(),
            focus: 0,
            active: 0,
            capture: 0,
            app_active: false,
            cursor_count: 0,
            cursor: 0,
            clip: None,
            timers: Vec::new(),
            input: input::InputState::default(),
            mode: m,
            desktop_mode: m,
            dialogs: Vec::new(),
            next_dialog: 1,
            auto_dismiss_dialogs: false,
            mouse_hover: 0,
            dialogs_mgr: Vec::new(),
            controls: BTreeMap::new(),
            nc_dirty: Vec::new(),
            atoms: BTreeMap::new(),
            hooks: Vec::new(),
            next_hook: 0,
            defer: BTreeMap::new(),
            menus: BTreeMap::new(),
            system_menus: BTreeMap::new(),
        }
    }
}

/// Display modes offered by the emulated adapter/monitor.
pub fn supported_modes(refresh: u32) -> Vec<DisplayMode> {
    let mut v = Vec::new();
    for bpp in [8, 16, 32] {
        for (w, h) in [
            (640, 480),
            (800, 600),
            (1024, 768),
            (1152, 864),
            (1280, 960),
            (1280, 1024),
            (1600, 1200),
        ] {
            v.push(DisplayMode {
                width: w,
                height: h,
                bpp,
                refresh,
            });
        }
    }
    v
}

// ---------------------------------------------------------------- metrics

const CXFRAME: i32 = 4;
const CYCAPTION: i32 = 19;
const CXDLGFRAME: i32 = 3;
const CXBORDER: i32 = 1;
pub const CYMENU: i32 = 19;
const CXEDGE: i32 = 2;

/// Non-client insets (left, top, right, bottom) for a style.
pub fn nc_insets(style: u32, exstyle: u32, menu: bool) -> Rect {
    let mut l = 0;
    let mut t = 0;
    if style & WS_THICKFRAME != 0 {
        l += CXFRAME;
        t += CXFRAME;
    } else if style & WS_DLGFRAME != 0 || exstyle & WS_EX_DLGMODALFRAME != 0 {
        l += CXDLGFRAME;
        t += CXDLGFRAME;
    } else if style & WS_BORDER != 0 {
        l += CXBORDER;
        t += CXBORDER;
    }
    if exstyle & WS_EX_CLIENTEDGE != 0 {
        l += CXEDGE;
        t += CXEDGE;
    }
    let mut top = t;
    if style & WS_CAPTION == WS_CAPTION {
        top += CYCAPTION;
    }
    if menu {
        top += CYMENU;
    }
    Rect::new(l, top, l, t)
}

pub fn client_from_window(w: &Window) -> Rect {
    let menu = w.style & WS_CHILD == 0 && w.id != 0;
    let i = nc_insets(w.style, w.exstyle, menu);
    Rect::new(
        w.rect.left + i.left,
        w.rect.top + i.top,
        (w.rect.right - i.right).max(w.rect.left + i.left),
        (w.rect.bottom - i.bottom).max(w.rect.top + i.top),
    )
}

fn get_system_metrics(s: &mut Session) -> ApiResult {
    let [i] = s.args::<1>()?;
    let m = s.user.mode;
    let v: i32 = match i {
        0 => m.width as i32,  // SM_CXSCREEN
        1 => m.height as i32, // SM_CYSCREEN
        2 => 16,              // SM_CXVSCROLL
        3 => 16,              // SM_CYHSCROLL
        4 => CYCAPTION,       // SM_CYCAPTION
        5 | 6 => CXBORDER,    // SM_CXBORDER / SM_CYBORDER
        7 | 8 => CXDLGFRAME,  // SM_CXDLGFRAME
        11 | 12 => 32,        // SM_CXICON
        13 | 14 => 32,        // SM_CXCURSOR
        15 => CYMENU,         // SM_CYMENU
        16 => m.width as i32, // SM_CXFULLSCREEN
        17 => m.height as i32 - CYCAPTION,
        19 => 1,            // SM_MOUSEPRESENT
        23 => 0,            // SM_SWAPBUTTON
        28 | 29 => 112,     // SM_CXMIN / SM_CYMIN
        30 | 31 => 18,      // SM_CXSIZE
        32 | 33 => CXFRAME, // SM_CXFRAME
        36 | 37 => 4,       // SM_CXDOUBLECLK
        42 => 0,            // SM_DBCSENABLED
        43 => 3,            // SM_CMOUSEBUTTONS
        45 | 46 => CXEDGE,  // SM_CXEDGE
        49 | 50 => 16,      // SM_CXSMICON
        59 | 60 => m.width.max(m.height) as i32 + 8,
        61 => m.width as i32, // SM_CXMAXIMIZED
        62 => m.height as i32,
        63 => 1,      // SM_NETWORK
        67 => 0,      // SM_CLEANBOOT
        75 => 1,      // SM_MOUSEWHEELPRESENT
        76 | 77 => 0, // SM_XVIRTUALSCREEN
        78 => m.width as i32,
        79 => m.height as i32,
        80 => 1, // SM_CMONITORS
        _ => {
            s.missing("GetSystemMetrics", format!("index {i}"));
            0
        }
    };
    Ok(ApiRet::Ret(v as u32))
}

fn get_sys_color(s: &mut Session) -> ApiResult {
    let [i] = s.args::<1>()?;
    Ok(ApiRet::Ret(sys_color(i)))
}

/// Windows 2000 default ("Windows Standard") colour scheme.
pub fn sys_color(i: u32) -> u32 {
    match i {
        0 => 0xc8d0d4,
        1 => 0xa56e3a,
        2 => 0x6a240a,
        3 => 0x808080,
        4 => 0xc8d0d4,
        5 => 0xffffff,
        6 => 0x000000,
        7..=9 => 0x000000,
        10 | 11 => 0xc8d0d4,
        12 => 0x808080,
        13 => 0x6a240a,
        14 => 0xffffff,
        15 => 0xc8d0d4,
        16 => 0x808080,
        17 => 0x808080,
        18 => 0x000000,
        19 => 0xc8d0d4,
        20 => 0xffffff,
        21 => 0x404040,
        22 => 0xc8d0d4,
        23 => 0x000000,
        24 => 0xe1ffff,
        _ => 0,
    }
}

// ---------------------------------------------------------------- classes

pub(crate) fn class_key(s: &Session, name: &StrOrId) -> String {
    match name {
        StrOrId::Str(n) => n.to_uppercase(),
        StrOrId::Id(a) => s
            .user
            .classes
            .values()
            .find(|c| c.atom == *a)
            .map(|c| c.name.to_uppercase())
            .unwrap_or_else(|| format!("#{a}")),
    }
}

fn register_class_common(s: &mut Session, p: u32, ex: bool) -> ApiResult {
    let o = if ex { 4 } else { 0 };
    let style = s.mem.read_u32(p + o)?;
    let wndproc = s.mem.read_u32(p + o + 4)?;
    let cls_extra = s.mem.read_u32(p + o + 8)?;
    let wnd_extra = s.mem.read_u32(p + o + 12)?;
    let hinst = s.mem.read_u32(p + o + 16)?;
    let icon = s.mem.read_u32(p + o + 20)?;
    let cursor = s.mem.read_u32(p + o + 24)?;
    let brush = s.mem.read_u32(p + o + 28)?;
    let menu = s.mem.read_u32(p + o + 32)?;
    let name_p = s.mem.read_u32(p + o + 36)?;
    let name = match s.read_str_or_id(name_p)? {
        StrOrId::Str(n) => n,
        StrOrId::Id(a) => format!("#{a}"),
    };
    let key = name.to_uppercase();
    if s.user.classes.contains_key(&key) {
        s.set_last_error(1410); // ERROR_CLASS_ALREADY_EXISTS
        return Ok(ApiRet::Ret(0));
    }
    let atom = s.user.next_atom;
    s.user.next_atom += 1;
    s.user.classes.insert(
        key,
        WndClass {
            name,
            style,
            wndproc,
            cls_extra,
            wnd_extra,
            hinst,
            icon,
            cursor,
            brush,
            menu,
            atom,
        },
    );
    Ok(ApiRet::Ret(atom))
}

fn register_class_a(s: &mut Session) -> ApiResult {
    let [p] = s.args::<1>()?;
    register_class_common(s, p, false)
}
fn register_class_ex_a(s: &mut Session) -> ApiResult {
    let [p] = s.args::<1>()?;
    register_class_common(s, p, true)
}

fn unregister_class_a(s: &mut Session) -> ApiResult {
    let [name, _hinst] = s.args::<2>()?;
    let n = s.read_str_or_id(name)?;
    let key = class_key(s, &n);
    if s.user.windows.values().any(|w| w.class == key) {
        s.set_last_error(1412); // ERROR_CLASS_HAS_WINDOWS
        return Ok(ApiRet::Ret(0));
    }
    Ok(ApiRet::Ret(s.user.classes.remove(&key).is_some() as u32))
}

fn get_class_info_a(s: &mut Session) -> ApiResult {
    let [_hinst, name, p] = s.args::<3>()?;
    let n = s.read_str_or_id(name)?;
    let key = class_key(s, &n);
    let Some(c) = s.user.classes.get(&key).cloned() else {
        s.set_last_error(1411); // ERROR_CLASS_DOES_NOT_EXIST
        return Ok(ApiRet::Ret(0));
    };
    for (i, v) in [
        c.style,
        c.wndproc,
        c.cls_extra,
        c.wnd_extra,
        c.hinst,
        c.icon,
        c.cursor,
        c.brush,
        c.menu,
    ]
    .iter()
    .enumerate()
    {
        s.mem.write_u32(p + 4 * i as u32, *v)?;
    }
    s.mem.write_u32(p + 36, name)?;
    Ok(ApiRet::Ret(c.atom))
}

// ---------------------------------------------------------------- message sequences

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum LParam {
    Val(u32),
    Buf(Vec<u8>),
    /// A self-referencing buffer (see [`CallArg::BufRel`]).
    BufRel(Vec<u8>, Vec<u32>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Check {
    None,
    /// Use the result as the API return value.
    Store,
    /// WM_NCCREATE returning FALSE aborts window creation.
    NcCreate(u32),
    /// WM_CREATE returning -1 aborts window creation.
    Create(u32),
    /// Copy the client rectangle computed by WM_NCCALCSIZE.
    NcCalc(u32),
    /// Apply WM_WINDOWPOSCHANGING edits to a pending move.
    PosChanging(u32),
    /// A WH_CBT hook returning non-zero for HCBT_CREATEWND aborts window creation.
    CbtCreate(u32),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Act {
    /// Remove a window record (end of DestroyWindow).
    Remove(u32),
    /// WM_SIZE + WM_MOVE after creation.
    PostCreate(u32),
    /// Show a window that was created with WS_VISIBLE.
    ShowNew(u32),
    Activate(u32),
    Focus(u32),
    Destroy(u32),
    SetRet(u32),
    /// Finish SetWindowPos: commit the pending rectangle.
    CommitPos {
        hwnd: u32,
        rect: Rect,
        flags: u32,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Step {
    /// Call a guest procedure that is not a window procedure (a hook).
    Call {
        target: u32,
        args: Vec<LParam>,
        check: Check,
    },
    Send {
        hwnd: u32,
        msg: u32,
        wp: u32,
        lp: LParam,
        check: Check,
    },
    Act(Act),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum U32Cont {
    Seq {
        steps: VecDeque<Step>,
        ret: u32,
        last: Option<Check>,
    },
    Dialog {
        id: u32,
    },
    RetryGetMessage,
    MsgWait {
        n: u32,
    },
}

impl LParam {
    fn into_arg(self) -> CallArg {
        match self {
            LParam::Val(v) => CallArg::Val(v),
            LParam::Buf(b) => CallArg::Buf(b),
            LParam::BufRel(b, r) => CallArg::BufRel(b, r),
        }
    }
}

pub(crate) fn send(hwnd: u32, msg: u32, wp: u32, lp: u32) -> Step {
    Step::Send {
        hwnd,
        msg,
        wp,
        lp: LParam::Val(lp),
        check: Check::None,
    }
}

pub fn run_seq(s: &mut Session, mut steps: VecDeque<Step>, mut ret: u32) -> ApiResult {
    while let Some(st) = steps.pop_front() {
        match st {
            Step::Send {
                hwnd,
                msg,
                wp,
                lp,
                check,
            } => {
                let Some(w) = s.user.windows.get(&hwnd) else {
                    continue;
                };
                let proc_ = w.wndproc;
                return Ok(ApiRet::call(
                    proc_,
                    vec![hwnd.into(), msg.into(), wp.into(), lp.into_arg()],
                    Cont::User32(U32Cont::Seq {
                        steps,
                        ret,
                        last: Some(check),
                    }),
                ));
            }
            Step::Call { target, args, check } => {
                return Ok(ApiRet::call(
                    target,
                    args.into_iter().map(LParam::into_arg).collect(),
                    Cont::User32(U32Cont::Seq {
                        steps,
                        ret,
                        last: Some(check),
                    }),
                ));
            }
            Step::Act(a) => run_act(s, a, &mut steps, &mut ret)?,
        }
    }
    Ok(ApiRet::Ret(ret))
}

fn run_act(s: &mut Session, a: Act, steps: &mut VecDeque<Step>, ret: &mut u32) -> Result<(), ApiError> {
    match a {
        Act::Remove(h) => {
            s.user.windows.remove(&h);
            s.user.timers.retain(|t| t.hwnd != h);
            if s.user.focus == h {
                s.user.focus = 0;
            }
            if s.user.active == h {
                s.user.active = 0;
            }
            if s.user.capture == h {
                s.user.capture = 0;
            }
        }
        Act::PostCreate(h) => {
            if let Some(w) = s.user.windows.get(&h) {
                let c = w.client;
                let p = if w.parent != 0 {
                    s.user
                        .windows
                        .get(&w.parent)
                        .map(|p| (p.client.left, p.client.top))
                        .unwrap_or((0, 0))
                } else {
                    (0, 0)
                };
                let mut v = vec![
                    send(h, WM_SIZE, 0, (c.w() as u32 & 0xffff) | ((c.h() as u32) << 16)),
                    send(
                        h,
                        WM_MOVE,
                        0,
                        ((c.left - p.0) as u32 & 0xffff) | (((c.top - p.1) as u32) << 16),
                    ),
                ];
                v.reverse();
                for st in v {
                    steps.push_front(st);
                }
            }
        }
        Act::ShowNew(h) => {
            let v = show_steps(s, h, 5)?;
            for st in v.into_iter().rev() {
                steps.push_front(st);
            }
        }
        Act::Activate(h) => {
            let v = activate_steps(s, h);
            for st in v.into_iter().rev() {
                steps.push_front(st);
            }
        }
        Act::Focus(h) => {
            let v = focus_steps(s, h);
            for st in v.into_iter().rev() {
                steps.push_front(st);
            }
        }
        Act::Destroy(h) => {
            let v = destroy_steps(s, h);
            for st in v.into_iter().rev() {
                steps.push_front(st);
            }
        }
        Act::SetRet(v) => *ret = v,
        Act::CommitPos { hwnd, rect, flags } => {
            if let Some(w) = s.user.windows.get_mut(&hwnd) {
                let old = w.rect;
                let old_client = w.client;
                w.rect = rect;
                w.client = client_from_window(w);
                let c = w.client;
                let (visible, top, style) = (w.visible, w.style & WS_CHILD == 0, w.style);
                if w.visible {
                    w.invalid = Some(Rect::new(0, 0, c.w(), c.h()));
                    w.erase_pending = true;
                }
                // Descendants keep their position relative to the client area.
                let (dx, dy) = (c.left - old_client.left, c.top - old_client.top);
                if dx != 0 || dy != 0 {
                    offset_descendants(s, hwnd, dx, dy);
                }
                if visible && old != rect {
                    if top && style & (WS_CAPTION | WS_BORDER | WS_THICKFRAME | WS_DLGFRAME) != 0 {
                        s.user.nc_dirty.push(hwnd);
                    }
                    expose(s, old, hwnd);
                    invalidate_tree(s, hwnd);
                }
                steps.push_front(Step::Send {
                    hwnd,
                    msg: WM_WINDOWPOSCHANGED,
                    wp: 0,
                    lp: LParam::Buf(windowpos(hwnd, &rect, flags).to_vec()),
                    check: Check::None,
                });
            }
        }
    }
    Ok(())
}

fn descendants(s: &Session, h: u32) -> Vec<u32> {
    let mut out = Vec::new();
    let mut stack = vec![h];
    while let Some(p) = stack.pop() {
        for w in s.user.windows.values() {
            if w.parent == p && w.style & WS_CHILD != 0 {
                out.push(w.hwnd);
                stack.push(w.hwnd);
            }
        }
    }
    out
}

fn offset_descendants(s: &mut Session, h: u32, dx: i32, dy: i32) {
    for c in descendants(s, h) {
        if let Some(w) = s.user.windows.get_mut(&c) {
            for r in [&mut w.rect, &mut w.client] {
                *r = Rect::new(r.left + dx, r.top + dy, r.right + dx, r.bottom + dy);
            }
        }
    }
}

/// Mark a visible window and its visible descendants for a full repaint.
pub(crate) fn invalidate_tree(s: &mut Session, h: u32) {
    let mut all = vec![h];
    all.extend(descendants(s, h));
    for c in all {
        if let Some(w) = s.user.windows.get_mut(&c)
            && w.visible
        {
            w.invalid = Some(Rect::new(0, 0, w.client.w(), w.client.h()));
            w.erase_pending = true;
        }
    }
}

/// A screen area no longer covered by `gone`: paint the desktop there and have the
/// other top-level windows that overlap it repaint (frames included). Nothing is
/// painted while an application owns the display through exclusive DirectDraw.
pub(crate) fn expose(s: &mut Session, area: Rect, gone: u32) {
    if s.com.ddraw.exclusive.is_some() || area.is_empty() {
        return;
    }
    let hdc = crate::dll::gdi32::window_dc(s, HWND_DESKTOP, true);
    let _ = crate::dll::gdi32::fill_solid(s, hdc, &area, 0);
    crate::dll::gdi32::release_dc_handle(s, hdc);
    let below: Vec<(u32, u32)> = s
        .user
        .windows
        .values()
        .filter(|w| w.hwnd != gone && w.visible && w.style & WS_CHILD == 0 && !w.rect.intersect(&area).is_empty())
        .map(|w| (w.hwnd, w.style))
        .collect();
    for (h, style) in below {
        if style & (WS_CAPTION | WS_BORDER | WS_THICKFRAME | WS_DLGFRAME) != 0 && !s.user.nc_dirty.contains(&h) {
            s.user.nc_dirty.push(h);
        }
        invalidate_tree(s, h);
    }
}

fn windowpos(hwnd: u32, r: &Rect, flags: u32) -> [u8; 28] {
    let mut b = [0u8; 28];
    let vals = [hwnd, 0, r.left as u32, r.top as u32, r.w() as u32, r.h() as u32, flags];
    for (i, v) in vals.iter().enumerate() {
        b[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
    }
    b
}

pub fn resume(s: &mut Session, c: U32Cont, v: Resume) -> ApiResult {
    match c {
        U32Cont::Seq {
            mut steps,
            mut ret,
            last,
        } => {
            let r = v.eax();
            match last {
                Some(Check::Store) => ret = r,
                Some(Check::CbtCreate(h)) => {
                    if r != 0 {
                        s.user.windows.remove(&h);
                        return Ok(ApiRet::Ret(0));
                    }
                }
                Some(Check::NcCreate(h)) => {
                    if r == 0 {
                        s.user.windows.remove(&h);
                        return Ok(ApiRet::Ret(0));
                    }
                }
                Some(Check::Create(h)) => {
                    if r as i32 == -1 {
                        let mut st: VecDeque<Step> = destroy_steps(s, h).into();
                        st.push_back(Step::Act(Act::SetRet(0)));
                        return run_seq(s, st, 0);
                    }
                }
                Some(Check::NcCalc(h)) => {
                    if let Some(&buf) = s.cb_bufs.first() {
                        let rc = s.read_rect(buf)?;
                        if let Some(w) = s.user.windows.get_mut(&h) {
                            w.client = rc;
                        }
                    }
                }
                Some(Check::PosChanging(h)) => {
                    if let Some(&buf) = s.cb_bufs.first() {
                        let b = s.mem.read_vec(buf, 28)?;
                        let g = |o: usize| i32::from_le_bytes(b[o..o + 4].try_into().unwrap());
                        let (x, y, cx, cy) = (g(8), g(12), g(16), g(20));
                        for st in steps.iter_mut() {
                            if let Step::Act(Act::CommitPos { hwnd, rect, .. }) = st
                                && *hwnd == h
                            {
                                *rect = Rect::new(x, y, x + cx, y + cy);
                                break;
                            }
                        }
                    }
                }
                Some(Check::None) | None => {}
            }
            run_seq(s, steps, ret)
        }
        U32Cont::RetryGetMessage => get_message_a(s),
        U32Cont::MsgWait { n } => Ok(ApiRet::Ret(match v {
            Resume::Signaled(i) => i.min(n),
            Resume::TimedOut => 0x102,
            Resume::Returned { eax, .. } => eax,
        })),
        U32Cont::Dialog { id } => {
            let ans = match v {
                Resume::Signaled(_) => s
                    .user
                    .dialogs
                    .iter()
                    .find(|d| d.id == id)
                    .and_then(|d| d.answer)
                    .unwrap_or(1),
                _ => 1,
            };
            s.user.dialogs.retain(|d| d.id != id);
            Ok(ApiRet::Ret(ans))
        }
    }
}

// ---------------------------------------------------------------- windows

impl Session {
    pub fn window(&self, h: u32) -> Option<&Window> {
        self.user.windows.get(&h)
    }
    fn queue(&mut self, tid: u32) -> &mut Queue {
        self.user.queues.entry(tid).or_default()
    }
    pub fn msg_time(&self) -> u32 {
        (self.uptime_ns() / 1_000_000) as u32
    }
    pub fn top_level_of(&self, mut h: u32) -> u32 {
        while let Some(w) = self.user.windows.get(&h) {
            if w.style & WS_CHILD == 0 || w.parent == 0 {
                return h;
            }
            h = w.parent;
        }
        h
    }
}

/// A CREATESTRUCTA from its fields in declaration order (lpCreateParams … dwExStyle).
pub(crate) fn create_struct(fields: [u32; 12]) -> Vec<u8> {
    fields.iter().flat_map(|v| v.to_le_bytes()).collect()
}

/// The WH_CBT hook call (HCBT_CREATEWND) for a window being created, if a CBT hook is
/// installed. A non-zero hook result aborts the creation.
pub(crate) fn cbt_create_step(s: &Session, hwnd: u32, cs: &[u8]) -> Option<Step> {
    let hook = crate::dll::user32_misc::first_hook(s, crate::dll::user32_misc::WH_CBT)?;
    // CBT_CREATEWND { lpcs, hwndInsertAfter } followed by the CREATESTRUCT it points to.
    let mut b = vec![0u8; 8];
    b[0..4].copy_from_slice(&8u32.to_le_bytes());
    b.extend_from_slice(cs);
    Some(Step::Call {
        target: hook,
        args: vec![LParam::Val(3), LParam::Val(hwnd), LParam::BufRel(b, vec![0])],
        check: Check::CbtCreate(hwnd),
    })
}

fn create_window_ex_a(s: &mut Session) -> ApiResult {
    let [ex, cls_p, name_p, style, x, y, w, h, parent, menu, hinst, param] = s.args::<12>()?;
    let cls = s.read_str_or_id(cls_p)?;
    let key = class_key(s, &cls);
    let title = if name_p != 0 && name_p >= 0x10000 {
        s.read_str(name_p)?
    } else {
        String::new()
    };
    let Some(class) = s.user.classes.get(&key).cloned() else {
        s.event(Severity::Warning, "CreateWindow", format!("unknown class {cls:?}"));
        s.set_last_error(1407); // ERROR_CANNOT_FIND_WND_CLASS
        return Ok(ApiRet::Ret(0));
    };
    let mode = s.user.mode;
    let (mut x, mut y, mut w, mut h) = (x as i32, y as i32, w as i32, h as i32);
    let cw = i32::MIN; // CW_USEDEFAULT
    if style & WS_CHILD == 0 {
        if x == cw {
            x = 66;
            y = 66;
        }
        if w == cw {
            w = mode.width as i32 * 3 / 4;
            h = mode.height as i32 * 3 / 4;
        }
    } else {
        if x == cw {
            x = 0;
            y = 0;
        }
        if w == cw {
            w = 0;
            h = 0;
        }
    }
    // Child coordinates are relative to the parent's client area.
    let (ox, oy) = if style & WS_CHILD != 0 {
        s.window(parent)
            .map(|p| (p.client.left, p.client.top))
            .unwrap_or((0, 0))
    } else {
        (0, 0)
    };
    let hwnd = s.user.next_hwnd;
    s.user.next_hwnd += 0x10;
    let rect = Rect::new(x + ox, y + oy, x + ox + w, y + oy + h);
    let tid = s.tid();
    let mut win = Window {
        hwnd,
        class: key,
        title: String::new(),
        style: style & !WS_VISIBLE,
        exstyle: ex,
        parent: if style & WS_CHILD != 0 { parent } else { 0 },
        owner: if style & WS_CHILD == 0 { parent } else { 0 },
        id: menu,
        rect,
        client: rect,
        wndproc: class.wndproc,
        hinst,
        userdata: 0,
        extra: vec![0; class.wnd_extra as usize],
        tid,
        visible: false,
        invalid: None,
        erase_pending: false,
        props: BTreeMap::new(),
    };
    win.client = client_from_window(&win);
    s.user.windows.insert(hwnd, win);

    let cs = create_struct([
        param, hinst, menu, parent, h as u32, w as u32, y as u32, x as u32, style, name_p, cls_p, ex,
    ]);
    let mut steps = VecDeque::new();
    steps.extend(cbt_create_step(s, hwnd, &cs));
    if style & WS_CHILD == 0
        && (style & WS_THICKFRAME != 0 || style & WS_CAPTION == WS_CAPTION || style & WS_POPUP == 0)
    {
        let mut mmi = vec![0u8; 40];
        let vals: [i32; 10] = [
            0,
            0,
            mode.width as i32,
            mode.height as i32,
            -CXFRAME,
            -CXFRAME,
            112,
            27,
            mode.width as i32 + 2 * CXFRAME,
            mode.height as i32 + 2 * CXFRAME,
        ];
        for (i, v) in vals.iter().enumerate() {
            mmi[i * 4..i * 4 + 4].copy_from_slice(&v.to_le_bytes());
        }
        steps.push_back(Step::Send {
            hwnd,
            msg: WM_GETMINMAXINFO,
            wp: 0,
            lp: LParam::Buf(mmi),
            check: Check::None,
        });
    }
    steps.push_back(Step::Send {
        hwnd,
        msg: WM_NCCREATE,
        wp: 0,
        lp: LParam::Buf(cs.clone()),
        check: Check::NcCreate(hwnd),
    });
    // Title is stored by DefWindowProc(WM_NCCREATE); also set here for windows whose
    // procedure handles WM_NCCREATE itself.
    if let Some(wi) = s.user.windows.get_mut(&hwnd) {
        wi.title = title;
    }
    let mut nccalc = vec![0u8; 16];
    nccalc.copy_from_slice(&rect.bytes());
    steps.push_back(Step::Send {
        hwnd,
        msg: WM_NCCALCSIZE,
        wp: 0,
        lp: LParam::Buf(nccalc),
        check: Check::NcCalc(hwnd),
    });
    steps.push_back(Step::Send {
        hwnd,
        msg: WM_CREATE,
        wp: 0,
        lp: LParam::Buf(cs),
        check: Check::Create(hwnd),
    });
    steps.push_back(Step::Act(Act::PostCreate(hwnd)));
    if style & WS_VISIBLE != 0 {
        steps.push_back(Step::Act(Act::ShowNew(hwnd)));
    }
    run_seq(s, steps, hwnd)
}

fn show_steps(s: &mut Session, hwnd: u32, cmd: u32) -> Result<Vec<Step>, ApiError> {
    let Some(w) = s.user.windows.get(&hwnd).cloned() else {
        return Ok(vec![]);
    };
    let show = cmd != 0;
    let mut v = Vec::new();
    if show != w.visible {
        v.push(send(hwnd, WM_SHOWWINDOW, show as u32, 0));
        let flags = if show { 0x40 } else { 0x80 } | 0x3; // SWP_SHOWWINDOW/HIDEWINDOW|NOSIZE|NOMOVE
        v.push(Step::Send {
            hwnd,
            msg: WM_WINDOWPOSCHANGING,
            wp: 0,
            lp: LParam::Buf(windowpos(hwnd, &w.rect, flags).to_vec()),
            check: Check::None,
        });
        if let Some(wm) = s.user.windows.get_mut(&hwnd) {
            wm.visible = show;
            if show {
                if wm.style & WS_CHILD == 0 && (wm.style & (WS_CAPTION | WS_BORDER | WS_THICKFRAME) != 0) {
                    s.user.nc_dirty.push(hwnd);
                }
                let wm = s.user.windows.get_mut(&hwnd).unwrap();
                wm.style |= WS_VISIBLE;
                invalidate_tree(s, hwnd);
            } else {
                wm.style &= !WS_VISIBLE;
                if wm.style & WS_CHILD == 0 {
                    let r = wm.rect;
                    expose(s, r, hwnd);
                }
            }
        }
        v.push(Step::Send {
            hwnd,
            msg: WM_WINDOWPOSCHANGED,
            wp: 0,
            lp: LParam::Buf(windowpos(hwnd, &w.rect, flags).to_vec()),
            check: Check::None,
        });
    }
    let activate = show && !matches!(cmd, 4 | 7 | 8) && w.style & WS_CHILD == 0;
    if activate {
        v.push(Step::Act(Act::Activate(hwnd)));
    }
    if !show && s.user.active == hwnd {
        // Hiding the active window activates another top-level window (or none).
        let next = s
            .user
            .windows
            .values()
            .find(|x| x.hwnd != hwnd && x.visible && x.style & WS_CHILD == 0)
            .map(|x| x.hwnd)
            .unwrap_or(0);
        v.push(Step::Act(Act::Activate(next)));
    }
    Ok(v)
}

fn activate_steps(s: &mut Session, new: u32) -> Vec<Step> {
    let old = s.user.active;
    if old == new {
        return vec![];
    }
    let mut v = Vec::new();
    if old != 0 && s.user.windows.contains_key(&old) {
        v.push(send(old, WM_NCACTIVATE, 0, 0));
        v.push(send(old, WM_ACTIVATE, 0, new));
    }
    s.user.active = new;
    if new == 0 {
        if s.user.app_active {
            s.user.app_active = false;
            let tops: Vec<u32> = s
                .user
                .windows
                .values()
                .filter(|w| w.style & WS_CHILD == 0)
                .map(|w| w.hwnd)
                .collect();
            for t in tops {
                v.push(send(t, WM_ACTIVATEAPP, 0, 0));
            }
        }
        v.push(Step::Act(Act::Focus(0)));
        return v;
    }
    if !s.user.app_active {
        s.user.app_active = true;
        let tid = s.tid();
        let tops: Vec<u32> = s
            .user
            .windows
            .values()
            .filter(|w| w.style & WS_CHILD == 0)
            .map(|w| w.hwnd)
            .collect();
        for t in tops {
            v.push(send(t, WM_ACTIVATEAPP, 1, tid));
        }
    }
    v.push(send(new, WM_NCACTIVATE, 1, 0));
    // DefWindowProc(WM_ACTIVATE) moves the focus into the window.
    v.push(send(new, WM_ACTIVATE, 1, old));
    v
}

fn focus_steps(s: &mut Session, new: u32) -> Vec<Step> {
    let old = s.user.focus;
    if old == new {
        return vec![];
    }
    let mut v = Vec::new();
    s.user.focus = new;
    if old != 0 && s.user.windows.contains_key(&old) {
        v.push(send(old, WM_KILLFOCUS, new, 0));
    }
    if new != 0 {
        v.push(send(new, WM_SETFOCUS, old, 0));
    }
    v
}

pub fn destroy_steps_pub(s: &mut Session, h: u32) -> Vec<Step> {
    destroy_steps(s, h)
}

pub fn next_message_any(s: &mut Session) -> Option<Msg> {
    get_next_message(s, 0, 0, 0, true)
}

pub fn next_timer_due_pub(s: &Session) -> Option<u64> {
    next_timer_due(s)
}

fn destroy_steps(s: &mut Session, h: u32) -> Vec<Step> {
    let mut v = Vec::new();
    let Some(w) = s.user.windows.get(&h).cloned() else {
        return v;
    };
    if w.visible {
        v.push(send(h, WM_SHOWWINDOW, 0, 0));
        if let Some(wm) = s.user.windows.get_mut(&h) {
            wm.visible = false;
        }
        if w.style & WS_CHILD == 0 {
            expose(s, w.rect, h);
        }
    }
    if s.user.active == h {
        let next = s
            .user
            .windows
            .values()
            .find(|x| x.hwnd != h && x.visible && x.style & WS_CHILD == 0)
            .map(|x| x.hwnd)
            .unwrap_or(0);
        v.extend(activate_steps(s, next));
    }
    v.push(send(h, WM_DESTROY, 0, 0));
    let children: Vec<u32> = s
        .user
        .windows
        .values()
        .filter(|c| c.parent == h || c.owner == h)
        .map(|c| c.hwnd)
        .collect();
    for c in children {
        v.push(Step::Act(Act::Destroy(c)));
    }
    v.push(send(h, WM_NCDESTROY, 0, 0));
    v.push(Step::Act(Act::Remove(h)));
    v
}

fn show_window(s: &mut Session) -> ApiResult {
    let [hwnd, cmd] = s.args::<2>()?;
    let Some(w) = s.window(hwnd) else {
        s.set_last_error(1400);
        return Ok(ApiRet::Ret(0));
    };
    let was = w.visible as u32;
    let steps = show_steps(s, hwnd, cmd)?;
    run_seq(s, steps.into(), was)
}

fn destroy_window(s: &mut Session) -> ApiResult {
    let [hwnd] = s.args::<1>()?;
    if s.window(hwnd).is_none() {
        s.set_last_error(1400); // ERROR_INVALID_WINDOW_HANDLE
        return Ok(ApiRet::Ret(0));
    }
    let steps = destroy_steps(s, hwnd);
    run_seq(s, steps.into(), 1)
}

fn set_focus(s: &mut Session) -> ApiResult {
    let [hwnd] = s.args::<1>()?;
    let old = s.user.focus;
    if hwnd != 0 && s.window(hwnd).is_none() {
        s.set_last_error(1400);
        return Ok(ApiRet::Ret(0));
    }
    let mut steps = VecDeque::new();
    let top = s.top_level_of(hwnd);
    if hwnd != 0 && s.user.active != top {
        steps.extend(activate_steps(s, top));
    }
    steps.push_back(Step::Act(Act::Focus(hwnd)));
    run_seq(s, steps, old)
}

fn set_active_window(s: &mut Session) -> ApiResult {
    let [hwnd] = s.args::<1>()?;
    let old = s.user.active;
    let steps = activate_steps(s, hwnd);
    run_seq(s, steps.into(), old)
}

fn set_foreground_window(s: &mut Session) -> ApiResult {
    let [hwnd] = s.args::<1>()?;
    if s.window(hwnd).is_none() {
        return Ok(ApiRet::Ret(0));
    }
    let top = s.top_level_of(hwnd);
    let steps = activate_steps(s, top);
    run_seq(s, steps.into(), 1)
}

fn get_focus(s: &mut Session) -> ApiResult {
    Ok(ApiRet::Ret(s.user.focus))
}
fn get_active_window(s: &mut Session) -> ApiResult {
    Ok(ApiRet::Ret(s.user.active))
}
fn get_foreground_window(s: &mut Session) -> ApiResult {
    Ok(ApiRet::Ret(if s.user.app_active { s.user.active } else { 0 }))
}
fn get_desktop_window(_s: &mut Session) -> ApiResult {
    Ok(ApiRet::Ret(HWND_DESKTOP))
}
fn is_window(s: &mut Session) -> ApiResult {
    let [h] = s.args::<1>()?;
    Ok(ApiRet::Ret((s.window(h).is_some() || h == HWND_DESKTOP) as u32))
}
fn is_window_visible(s: &mut Session) -> ApiResult {
    let [h] = s.args::<1>()?;
    Ok(ApiRet::Ret(s.window(h).map(|w| w.visible).unwrap_or(false) as u32))
}
fn is_iconic(s: &mut Session) -> ApiResult {
    let [h] = s.args::<1>()?;
    Ok(ApiRet::Ret(
        s.window(h).map(|w| w.style & WS_MINIMIZE != 0).unwrap_or(false) as u32,
    ))
}
fn is_zoomed(s: &mut Session) -> ApiResult {
    let [h] = s.args::<1>()?;
    Ok(ApiRet::Ret(
        s.window(h).map(|w| w.style & WS_MAXIMIZE != 0).unwrap_or(false) as u32,
    ))
}
fn is_window_enabled(s: &mut Session) -> ApiResult {
    let [h] = s.args::<1>()?;
    Ok(ApiRet::Ret(
        s.window(h).map(|w| w.style & WS_DISABLED == 0).unwrap_or(false) as u32,
    ))
}
fn enable_window(s: &mut Session) -> ApiResult {
    let [h, en] = s.args::<2>()?;
    match s.user.windows.get_mut(&h) {
        Some(w) => {
            let was_disabled = w.style & WS_DISABLED != 0;
            if en != 0 {
                w.style &= !WS_DISABLED;
            } else {
                w.style |= WS_DISABLED;
            }
            Ok(ApiRet::Ret(was_disabled as u32))
        }
        None => Ok(ApiRet::Ret(0)),
    }
}
fn get_parent(s: &mut Session) -> ApiResult {
    let [h] = s.args::<1>()?;
    Ok(ApiRet::Ret(
        s.window(h)
            .map(|w| if w.style & WS_CHILD != 0 { w.parent } else { w.owner })
            .unwrap_or(0),
    ))
}

fn get_window_rect(s: &mut Session) -> ApiResult {
    let [h, p] = s.args::<2>()?;
    let r = if h == HWND_DESKTOP || h == 0 {
        Rect::new(0, 0, s.user.mode.width as i32, s.user.mode.height as i32)
    } else {
        match s.window(h) {
            Some(w) => w.rect,
            None => {
                s.set_last_error(1400);
                return Ok(ApiRet::Ret(0));
            }
        }
    };
    s.write_rect(p, &r)?;
    Ok(ApiRet::Ret(1))
}

fn get_client_rect(s: &mut Session) -> ApiResult {
    let [h, p] = s.args::<2>()?;
    let r = if h == HWND_DESKTOP {
        Rect::new(0, 0, s.user.mode.width as i32, s.user.mode.height as i32)
    } else {
        match s.window(h) {
            Some(w) => Rect::new(0, 0, w.client.w(), w.client.h()),
            None => {
                s.set_last_error(1400);
                return Ok(ApiRet::Ret(0));
            }
        }
    };
    s.write_rect(p, &r)?;
    Ok(ApiRet::Ret(1))
}

pub(crate) fn client_origin(s: &Session, h: u32) -> (i32, i32) {
    if h == 0 || h == HWND_DESKTOP {
        return (0, 0);
    }
    s.window(h).map(|w| (w.client.left, w.client.top)).unwrap_or((0, 0))
}

fn map_window_points(s: &mut Session) -> ApiResult {
    let [from, to, pts, n] = s.args::<4>()?;
    let (fx, fy) = client_origin(s, from);
    let (tx, ty) = client_origin(s, to);
    let (dx, dy) = (fx - tx, fy - ty);
    for i in 0..n {
        let p = pts + i * 8;
        let x = s.mem.read_i32(p)?;
        let y = s.mem.read_i32(p + 4)?;
        s.mem.write_u32(p, (x + dx) as u32)?;
        s.mem.write_u32(p + 4, (y + dy) as u32)?;
    }
    Ok(ApiRet::Ret(((dx as u32) & 0xffff) | ((dy as u32) << 16)))
}

fn client_to_screen(s: &mut Session) -> ApiResult {
    let [h, p] = s.args::<2>()?;
    let (ox, oy) = client_origin(s, h);
    let x = s.mem.read_i32(p)?;
    let y = s.mem.read_i32(p + 4)?;
    s.mem.write_u32(p, (x + ox) as u32)?;
    s.mem.write_u32(p + 4, (y + oy) as u32)?;
    Ok(ApiRet::Ret(1))
}
fn screen_to_client(s: &mut Session) -> ApiResult {
    let [h, p] = s.args::<2>()?;
    let (ox, oy) = client_origin(s, h);
    let x = s.mem.read_i32(p)?;
    let y = s.mem.read_i32(p + 4)?;
    s.mem.write_u32(p, (x - ox) as u32)?;
    s.mem.write_u32(p + 4, (y - oy) as u32)?;
    Ok(ApiRet::Ret(1))
}

fn set_window_pos_common(s: &mut Session, hwnd: u32, x: i32, y: i32, cx: i32, cy: i32, flags: u32) -> ApiResult {
    if s.window(hwnd).is_none() {
        s.set_last_error(1400);
        return Ok(ApiRet::Ret(0));
    }
    let steps = window_pos_steps(s, hwnd, x, y, cx, cy, flags)?;
    run_seq(s, steps, 1)
}

/// The message sequence of SetWindowPos for an existing window.
pub(crate) fn window_pos_steps(
    s: &mut Session,
    hwnd: u32,
    x: i32,
    y: i32,
    cx: i32,
    cy: i32,
    flags: u32,
) -> Result<VecDeque<Step>, ApiError> {
    let Some(w) = s.window(hwnd).cloned() else {
        return Ok(VecDeque::new());
    };
    let (ox, oy) = if w.style & WS_CHILD != 0 {
        client_origin(s, w.parent)
    } else {
        (0, 0)
    };
    let mut r = w.rect;
    if flags & 0x2 == 0 {
        // !SWP_NOMOVE
        let (rw, rh) = (r.w(), r.h());
        r = Rect::new(x + ox, y + oy, x + ox + rw, y + oy + rh);
    }
    if flags & 0x1 == 0 {
        // !SWP_NOSIZE
        r.right = r.left + cx;
        r.bottom = r.top + cy;
    }
    let mut steps = VecDeque::new();
    steps.push_back(Step::Send {
        hwnd,
        msg: WM_WINDOWPOSCHANGING,
        wp: 0,
        lp: LParam::Buf(windowpos(hwnd, &r, flags).to_vec()),
        check: Check::PosChanging(hwnd),
    });
    steps.push_back(Step::Act(Act::CommitPos { hwnd, rect: r, flags }));
    if flags & 0x40 != 0 && !w.visible {
        steps.extend(show_steps(s, hwnd, 5)?);
    } else if flags & 0x80 != 0 && w.visible {
        steps.extend(show_steps(s, hwnd, 0)?);
    }
    if flags & 0x10 == 0 && w.style & WS_CHILD == 0 && w.visible {
        // !SWP_NOACTIVATE
        steps.push_back(Step::Act(Act::Activate(hwnd)));
    }
    Ok(steps)
}

fn set_window_pos(s: &mut Session) -> ApiResult {
    let [hwnd, _after, x, y, cx, cy, flags] = s.args::<7>()?;
    set_window_pos_common(s, hwnd, x as i32, y as i32, cx as i32, cy as i32, flags)
}

fn move_window(s: &mut Session) -> ApiResult {
    let [hwnd, x, y, w, h, _repaint] = s.args::<6>()?;
    // MoveWindow = SetWindowPos(SWP_NOZORDER | SWP_NOACTIVATE)
    set_window_pos_common(s, hwnd, x as i32, y as i32, w as i32, h as i32, 0x4 | 0x10)
}

fn get_window_long_a(s: &mut Session) -> ApiResult {
    let [h, idx] = s.args::<2>()?;
    let Some(w) = s.window(h) else {
        s.set_last_error(1400);
        return Ok(ApiRet::Ret(0));
    };
    let v = match idx as i32 {
        -4 => w.wndproc,
        -6 => w.hinst,
        -8 => w.owner,
        -12 => w.id,
        -16 => w.style,
        -20 => w.exstyle,
        -21 => w.userdata,
        i if i >= 0 && (i as usize + 4) <= w.extra.len() => {
            u32::from_le_bytes(w.extra[i as usize..i as usize + 4].try_into().unwrap())
        }
        _ => {
            s.set_last_error(1413); // ERROR_INVALID_INDEX
            return Ok(ApiRet::Ret(0));
        }
    };
    Ok(ApiRet::Ret(v))
}

fn set_window_long_a(s: &mut Session) -> ApiResult {
    let [h, idx, val] = s.args::<3>()?;
    let Some(w) = s.user.windows.get_mut(&h) else {
        s.set_last_error(1400);
        return Ok(ApiRet::Ret(0));
    };
    let idx = idx as i32;
    let old = match idx {
        -4 => std::mem::replace(&mut w.wndproc, val),
        -6 => std::mem::replace(&mut w.hinst, val),
        -8 => std::mem::replace(&mut w.owner, val),
        -12 => std::mem::replace(&mut w.id, val),
        -21 => std::mem::replace(&mut w.userdata, val),
        -16 | -20 => {
            let old = if idx == -16 { w.style } else { w.exstyle };
            let mut ss = [0u8; 8];
            ss[0..4].copy_from_slice(&old.to_le_bytes());
            ss[4..8].copy_from_slice(&val.to_le_bytes());
            if idx == -16 {
                w.style = val;
                w.visible = val & WS_VISIBLE != 0;
            } else {
                w.exstyle = val;
            }
            w.client = client_from_window(w);
            let which = if idx == -16 { 0xffff_fff0u32 } else { 0xffff_ffec };
            let steps = VecDeque::from(vec![
                Step::Send {
                    hwnd: h,
                    msg: WM_STYLECHANGING,
                    wp: which,
                    lp: LParam::Buf(ss.to_vec()),
                    check: Check::None,
                },
                Step::Send {
                    hwnd: h,
                    msg: WM_STYLECHANGED,
                    wp: which,
                    lp: LParam::Buf(ss.to_vec()),
                    check: Check::None,
                },
            ]);
            return run_seq(s, steps, old);
        }
        i if i >= 0 && (i as usize + 4) <= w.extra.len() => {
            let o = i as usize;
            let old = u32::from_le_bytes(w.extra[o..o + 4].try_into().unwrap());
            w.extra[o..o + 4].copy_from_slice(&val.to_le_bytes());
            old
        }
        _ => {
            s.set_last_error(1413);
            return Ok(ApiRet::Ret(0));
        }
    };
    Ok(ApiRet::Ret(old))
}

fn set_window_text_a(s: &mut Session) -> ApiResult {
    let [h, p] = s.args::<2>()?;
    let t = s.read_str_opt(p)?.unwrap_or_default();
    if s.window(h).is_none() {
        s.set_last_error(1400);
        return Ok(ApiRet::Ret(0));
    }
    // SetWindowText sends WM_SETTEXT; DefWindowProc stores the text.
    let mut b = crate::strings::encode_ansi(&t);
    b.push(0);
    let steps = VecDeque::from(vec![Step::Send {
        hwnd: h,
        msg: WM_SETTEXT,
        wp: 0,
        lp: LParam::Buf(b),
        check: Check::Store,
    }]);
    run_seq(s, steps, 1)
}

fn get_window_text_a(s: &mut Session) -> ApiResult {
    let [h, buf, n] = s.args::<3>()?;
    let t = s.window(h).map(|w| w.title.clone()).unwrap_or_default();
    Ok(ApiRet::Ret(s.write_str_trunc(buf, n, &t)?))
}

fn find_window_a(s: &mut Session) -> ApiResult {
    let [cls, title] = s.args::<2>()?;
    let cls = if cls != 0 {
        let c = s.read_str_or_id(cls)?;
        Some(class_key(s, &c))
    } else {
        None
    };
    let title = s.read_str_opt(title)?;
    let found = s
        .user
        .windows
        .values()
        .find(|w| {
            w.style & WS_CHILD == 0
                && cls.as_ref().is_none_or(|c| &w.class == c)
                && title.as_ref().is_none_or(|t| &w.title == t)
        })
        .map(|w| w.hwnd)
        .unwrap_or(0);
    Ok(ApiRet::Ret(found))
}

fn set_prop_a(s: &mut Session) -> ApiResult {
    let [h, name, data] = s.args::<3>()?;
    let n = match s.read_str_or_id(name)? {
        StrOrId::Str(x) => x.to_uppercase(),
        StrOrId::Id(a) => format!("#{a}"),
    };
    match s.user.windows.get_mut(&h) {
        Some(w) => {
            w.props.insert(n, data);
            Ok(ApiRet::Ret(1))
        }
        None => Ok(ApiRet::Ret(0)),
    }
}
fn get_prop_a(s: &mut Session) -> ApiResult {
    let [h, name] = s.args::<2>()?;
    let n = match s.read_str_or_id(name)? {
        StrOrId::Str(x) => x.to_uppercase(),
        StrOrId::Id(a) => format!("#{a}"),
    };
    Ok(ApiRet::Ret(
        s.window(h).and_then(|w| w.props.get(&n).copied()).unwrap_or(0),
    ))
}
fn remove_prop_a(s: &mut Session) -> ApiResult {
    let [h, name] = s.args::<2>()?;
    let n = match s.read_str_or_id(name)? {
        StrOrId::Str(x) => x.to_uppercase(),
        StrOrId::Id(a) => format!("#{a}"),
    };
    Ok(ApiRet::Ret(
        s.user.windows.get_mut(&h).and_then(|w| w.props.remove(&n)).unwrap_or(0),
    ))
}

// ---------------------------------------------------------------- DefWindowProc

fn def_window_proc_a(s: &mut Session) -> ApiResult {
    let [hwnd, msg, wp, lp] = s.args::<4>()?;
    def_window_proc(s, hwnd, msg, wp, lp)
}

pub fn def_window_proc(s: &mut Session, hwnd: u32, msg: u32, wp: u32, lp: u32) -> ApiResult {
    match msg {
        WM_NCCREATE => {
            let name_p = s.mem.read_u32(lp + 36)?;
            if name_p >= 0x10000 {
                let t = s.read_str(name_p)?;
                if let Some(w) = s.user.windows.get_mut(&hwnd) {
                    w.title = t;
                }
            }
            Ok(ApiRet::Ret(1))
        }
        WM_NCCALCSIZE => {
            if let Some(w) = s.window(hwnd).cloned() {
                let r = s.read_rect(lp)?;
                let menu = w.style & WS_CHILD == 0 && w.id != 0;
                let i = nc_insets(w.style, w.exstyle, menu);
                let c = Rect::new(
                    r.left + i.left,
                    r.top + i.top,
                    (r.right - i.right).max(r.left + i.left),
                    (r.bottom - i.bottom).max(r.top + i.top),
                );
                s.write_rect(lp, &c)?;
            }
            Ok(ApiRet::Ret(0))
        }
        WM_NCHITTEST => {
            let (x, y) = (lp as i16 as i32, (lp >> 16) as i16 as i32);
            let Some(w) = s.window(hwnd) else {
                return Ok(ApiRet::Ret(0));
            };
            Ok(ApiRet::Ret(if w.client.contains(x, y) {
                1 // HTCLIENT
            } else if w.rect.contains(x, y) {
                if y < w.client.top && w.style & WS_CAPTION == WS_CAPTION {
                    2 // HTCAPTION
                } else {
                    18 // HTBORDER
                }
            } else {
                0
            }))
        }
        WM_SETCURSOR => {
            if lp & 0xffff == 1 {
                let cur = s
                    .window(hwnd)
                    .and_then(|w| s.user.classes.get(&w.class))
                    .map(|c| c.cursor)
                    .unwrap_or(0);
                if cur != 0 {
                    s.user.cursor = cur;
                }
                return Ok(ApiRet::Ret(1));
            }
            Ok(ApiRet::Ret(0))
        }
        WM_MOUSEACTIVATE => Ok(ApiRet::Ret(1)),
        WM_NCACTIVATE => Ok(ApiRet::Ret(1)),
        WM_ACTIVATE => {
            if wp & 0xffff != 0 && wp >> 16 == 0 {
                let steps = VecDeque::from(vec![Step::Act(Act::Focus(hwnd))]);
                return run_seq(s, steps, 0);
            }
            Ok(ApiRet::Ret(0))
        }
        WM_ERASEBKGND => {
            let brush = s
                .window(hwnd)
                .and_then(|w| s.user.classes.get(&w.class))
                .map(|c| c.brush)
                .unwrap_or(0);
            if brush == 0 {
                return Ok(ApiRet::Ret(0));
            }
            let c = s.window(hwnd).map(|w| w.client).unwrap_or_default();
            crate::dll::gdi32::fill_rect_hdc(s, wp, &Rect::new(0, 0, c.w(), c.h()), brush)?;
            Ok(ApiRet::Ret(1))
        }
        WM_PAINT => {
            // BeginPaint/EndPaint: erase the background if requested, then validate.
            let erase = s.window(hwnd).map(|w| w.erase_pending).unwrap_or(false);
            if let Some(w) = s.user.windows.get_mut(&hwnd) {
                w.invalid = None;
                w.erase_pending = false;
            }
            if erase {
                let hdc = crate::dll::gdi32::window_dc(s, hwnd, false);
                let steps = VecDeque::from(vec![Step::Send {
                    hwnd,
                    msg: WM_ERASEBKGND,
                    wp: hdc,
                    lp: LParam::Val(0),
                    check: Check::None,
                }]);
                return run_seq(s, steps, 0);
            }
            Ok(ApiRet::Ret(0))
        }
        WM_WINDOWPOSCHANGED => {
            let Some(w) = s.window(hwnd).cloned() else {
                return Ok(ApiRet::Ret(0));
            };
            let flags = s.mem.read_u32(lp + 24)?;
            let mut steps = VecDeque::new();
            let (px, py) = if w.style & WS_CHILD != 0 {
                client_origin(s, w.parent)
            } else {
                (0, 0)
            };
            if flags & 0x2 == 0 {
                steps.push_back(send(
                    hwnd,
                    WM_MOVE,
                    0,
                    ((w.client.left - px) as u32 & 0xffff) | (((w.client.top - py) as u32) << 16),
                ));
            }
            if flags & 0x1 == 0 {
                let kind = if w.style & WS_MAXIMIZE != 0 {
                    2
                } else if w.style & WS_MINIMIZE != 0 {
                    1
                } else {
                    0
                };
                steps.push_back(send(
                    hwnd,
                    WM_SIZE,
                    kind,
                    (w.client.w() as u32 & 0xffff) | ((w.client.h() as u32) << 16),
                ));
            }
            run_seq(s, steps, 0)
        }
        WM_CLOSE => {
            let steps = destroy_steps(s, hwnd);
            run_seq(s, steps.into(), 0)
        }
        WM_SYSCOMMAND => {
            match wp & 0xfff0 {
                0xf060 => {
                    // SC_CLOSE
                    let steps = VecDeque::from(vec![send(hwnd, WM_CLOSE, 0, 0)]);
                    return run_seq(s, steps, 0);
                }
                0xf100 | 0xf140 | 0xf170 => return Ok(ApiRet::Ret(0)), // KEYMENU, SCREENSAVE, MONITORPOWER
                c => {
                    s.missing("DefWindowProc", format!("WM_SYSCOMMAND {c:#x}"));
                }
            }
            Ok(ApiRet::Ret(0))
        }
        WM_SYSKEYDOWN => {
            if wp == 0x73 && lp & (1 << 29) != 0 {
                // Alt+F4
                let tid = s.tid();
                s.queue(tid).posted.push_back(Msg {
                    hwnd,
                    message: WM_SYSCOMMAND,
                    wparam: 0xf060,
                    lparam: 0,
                    time: 0,
                    pt: (0, 0),
                });
            }
            Ok(ApiRet::Ret(0))
        }
        WM_SETTEXT => {
            let t = if lp != 0 { s.read_str(lp)? } else { String::new() };
            if let Some(w) = s.user.windows.get_mut(&hwnd) {
                w.title = t;
            }
            Ok(ApiRet::Ret(1))
        }
        WM_GETTEXT => {
            let t = s.window(hwnd).map(|w| w.title.clone()).unwrap_or_default();
            Ok(ApiRet::Ret(s.write_str_trunc(lp, wp, &t)?))
        }
        WM_GETTEXTLENGTH => Ok(ApiRet::Ret(
            s.window(hwnd)
                .map(|w| crate::strings::encode_ansi(&w.title).len())
                .unwrap_or(0) as u32,
        )),
        0x0011 | 0x0013 => Ok(ApiRet::Ret(1)), // WM_QUERYENDSESSION, WM_QUERYOPEN
        _ => Ok(ApiRet::Ret(0)),
    }
}

fn call_window_proc_a(s: &mut Session) -> ApiResult {
    let [proc_, hwnd, msg, wp, lp] = s.args::<5>()?;
    if proc_ == 0 {
        return Ok(ApiRet::Ret(0));
    }
    Ok(ApiRet::call(
        proc_,
        vec![hwnd.into(), msg.into(), wp.into(), lp.into()],
        Cont::ReturnValue,
    ))
}

// ---------------------------------------------------------------- messages

fn send_message_a(s: &mut Session) -> ApiResult {
    let [hwnd, msg, wp, lp] = s.args::<4>()?;
    if hwnd == 0xffff {
        let tops: Vec<u32> = s
            .user
            .windows
            .values()
            .filter(|w| w.style & WS_CHILD == 0)
            .map(|w| w.hwnd)
            .collect();
        let steps: VecDeque<Step> = tops.into_iter().map(|h| send(h, msg, wp, lp)).collect();
        return run_seq(s, steps, 1);
    }
    let Some(w) = s.window(hwnd).cloned() else {
        s.set_last_error(1400);
        return Ok(ApiRet::Ret(0));
    };
    if w.tid != s.tid() {
        s.event(
            Severity::Warning,
            "SendMessage",
            format!("inter-thread SendMessage to {hwnd:#x} delivered on the calling thread"),
        );
    }
    let proc_ = w.wndproc;
    Ok(ApiRet::call(
        proc_,
        vec![hwnd.into(), msg.into(), wp.into(), lp.into()],
        Cont::ReturnValue,
    ))
}

/// Post WM_NULL to `hwnd` so a thread waiting for messages runs its loop again.
pub(crate) fn wake_window(s: &mut Session, hwnd: u32) {
    let Some(tid) = s.window(hwnd).map(|w| w.tid) else {
        return;
    };
    let time = s.msg_time();
    s.queue(tid).posted.push_back(Msg {
        hwnd,
        message: 0,
        wparam: 0,
        lparam: 0,
        time,
        pt: (0, 0),
    });
}

fn post_message_a(s: &mut Session) -> ApiResult {
    let [hwnd, msg, wp, lp] = s.args::<4>()?;
    let tid = if hwnd == 0 {
        s.tid()
    } else if hwnd == 0xffff {
        let tops: Vec<(u32, u32)> = s
            .user
            .windows
            .values()
            .filter(|w| w.style & WS_CHILD == 0)
            .map(|w| (w.hwnd, w.tid))
            .collect();
        let t = s.msg_time();
        for (h, tid) in tops {
            s.queue(tid).posted.push_back(Msg {
                hwnd: h,
                message: msg,
                wparam: wp,
                lparam: lp,
                time: t,
                pt: (0, 0),
            });
        }
        return Ok(ApiRet::Ret(1));
    } else {
        match s.window(hwnd) {
            Some(w) => w.tid,
            None => {
                s.set_last_error(1400);
                return Ok(ApiRet::Ret(0));
            }
        }
    };
    let t = s.msg_time();
    let pt = (s.user.input.mouse_x, s.user.input.mouse_y);
    s.queue(tid).posted.push_back(Msg {
        hwnd,
        message: msg,
        wparam: wp,
        lparam: lp,
        time: t,
        pt,
    });
    Ok(ApiRet::Ret(1))
}

fn post_thread_message_a(s: &mut Session) -> ApiResult {
    let [tid, msg, wp, lp] = s.args::<4>()?;
    if !s.sched.threads.contains_key(&tid) {
        s.set_last_error(1444); // ERROR_INVALID_THREAD_ID
        return Ok(ApiRet::Ret(0));
    }
    let t = s.msg_time();
    s.queue(tid).posted.push_back(Msg {
        hwnd: 0,
        message: msg,
        wparam: wp,
        lparam: lp,
        time: t,
        pt: (0, 0),
    });
    Ok(ApiRet::Ret(1))
}

fn post_quit_message(s: &mut Session) -> ApiResult {
    let [code] = s.args::<1>()?;
    let tid = s.tid();
    s.queue(tid).quit = Some(code);
    Ok(ApiRet::Ret(0))
}

fn filter_match(m: &Msg, hwnd: u32, min: u32, max: u32, s: &Session) -> bool {
    let wnd_ok = match hwnd {
        0 => true,
        0xffff_ffff => m.hwnd == 0,
        h => m.hwnd == h || is_descendant(s, m.hwnd, h),
    };
    let range_ok = (min == 0 && max == 0) || (m.message >= min && m.message <= max);
    wnd_ok && range_ok
}

pub(crate) fn is_descendant(s: &Session, mut h: u32, anc: u32) -> bool {
    while let Some(w) = s.user.windows.get(&h) {
        if w.parent == anc {
            return true;
        }
        if w.parent == 0 {
            return false;
        }
        h = w.parent;
    }
    false
}

/// Try to retrieve a message for the current thread, honouring Win32 priority order.
fn get_next_message(s: &mut Session, hwnd: u32, min: u32, max: u32, remove: bool) -> Option<Msg> {
    let tid = s.tid();
    let q = s.user.queues.entry(tid).or_default().clone();
    if let Some(i) = q.posted.iter().position(|m| filter_match(m, hwnd, min, max, s)) {
        let m = q.posted[i];
        if remove {
            s.queue(tid).posted.remove(i);
        }
        return Some(m);
    }
    if let Some(i) = q.input.iter().position(|m| filter_match(m, hwnd, min, max, s)) {
        let m = q.input[i];
        if remove {
            s.queue(tid).input.remove(i);
        }
        return Some(m);
    }
    if let Some(code) = q.quit
        && (min == 0 && max == 0 || (WM_QUIT >= min && WM_QUIT <= max))
    {
        if remove {
            s.queue(tid).quit = None;
        }
        return Some(Msg {
            hwnd: 0,
            message: WM_QUIT,
            wparam: code,
            lparam: 0,
            time: s.msg_time(),
            pt: (0, 0),
        });
    }
    // WM_PAINT for invalid windows of this thread.
    if min == 0 && max == 0 || (WM_PAINT >= min && WM_PAINT <= max) {
        let paint = s
            .user
            .windows
            .values()
            .find(|w| w.tid == tid && w.invalid.is_some() && (hwnd == 0 || w.hwnd == hwnd) && s.really_visible(w.hwnd))
            .map(|w| w.hwnd);
        if let Some(h) = paint {
            paint_frame_if_dirty(s, h);
            return Some(Msg {
                hwnd: h,
                message: WM_PAINT,
                wparam: 0,
                lparam: 0,
                time: s.msg_time(),
                pt: (0, 0),
            });
        }
    }
    // WM_TIMER
    if min == 0 && max == 0 || (WM_TIMER >= min && WM_TIMER <= max) {
        let now = s.clock.now_ns();
        if let Some(i) = s
            .user
            .timers
            .iter()
            .position(|t| t.tid == tid && t.due <= now && (hwnd == 0 || t.hwnd == hwnd))
        {
            let t = s.user.timers[i].clone();
            if remove {
                let tm = &mut s.user.timers[i];
                tm.due = now + tm.period_ns;
            }
            return Some(Msg {
                hwnd: t.hwnd,
                message: WM_TIMER,
                wparam: t.id,
                lparam: t.proc_,
                time: s.msg_time(),
                pt: (0, 0),
            });
        }
    }
    None
}

fn write_msg(s: &mut Session, p: u32, m: &Msg) -> Result<(), ApiError> {
    s.mem.write(p, &m.bytes())?;
    let tid = s.tid();
    let q = s.queue(tid);
    q.last_time = m.time;
    q.last_pt = m.pt;
    Ok(())
}

fn peek_message_a(s: &mut Session) -> ApiResult {
    let [p, hwnd, min, max, flags] = s.args::<5>()?;
    match get_next_message(s, hwnd, min, max, flags & 1 != 0) {
        Some(m) => {
            write_msg(s, p, &m)?;
            Ok(ApiRet::Ret(1))
        }
        None => Ok(ApiRet::Ret(0)),
    }
}

fn get_message_a(s: &mut Session) -> ApiResult {
    let [p, hwnd, min, max] = s.args::<4>()?;
    match get_next_message(s, hwnd, min, max, true) {
        Some(m) => {
            write_msg(s, p, &m)?;
            Ok(ApiRet::Ret((m.message != WM_QUIT) as u32))
        }
        // Block until input arrives, then re-run GetMessage with the same arguments
        // (the API frame is still on the guest stack).
        None => Ok(ApiRet::Block(Block {
            wait: WaitKind::Message {
                objs: vec![],
                wake_mask: 0x4ff,
            },
            deadline: next_timer_due(s),
            cont: Cont::User32(U32Cont::RetryGetMessage),
        })),
    }
}

fn next_timer_due(s: &Session) -> Option<u64> {
    let tid = s.tid();
    s.user.timers.iter().filter(|t| t.tid == tid).map(|t| t.due).min()
}

fn wait_message(s: &mut Session) -> ApiResult {
    let tid = s.tid();
    if has_input(s, tid, 0x4ff) {
        return Ok(ApiRet::Ret(1));
    }
    Ok(ApiRet::Block(Block {
        wait: WaitKind::Message {
            objs: vec![],
            wake_mask: 0x4ff,
        },
        deadline: next_timer_due(s),
        cont: Cont::ReturnConst(1),
    }))
}

fn msg_wait_for_multiple_objects(s: &mut Session) -> ApiResult {
    let [n, p, all, ms, mask] = s.args::<5>()?;
    if all != 0 {
        s.missing("MsgWaitForMultipleObjects", "fWaitAll=TRUE".into());
    }
    let mut objs = Vec::new();
    for i in 0..n {
        let h = s.mem.read_u32(p + 4 * i)?;
        match s.handle_obj(h) {
            Some(o) => objs.push(o),
            None => {
                s.set_last_error(6);
                return Ok(ApiRet::Ret(0xffff_ffff));
            }
        }
    }
    let tid = s.tid();
    let wait = WaitKind::Message { objs, wake_mask: mask };
    if let Some(r) = crate::sync::try_satisfy(s, tid, &wait) {
        return Ok(ApiRet::Ret(r.eax()));
    }
    if ms == 0 {
        return Ok(ApiRet::Ret(0x102));
    }
    let mut deadline = if ms == 0xffff_ffff {
        None
    } else {
        Some(s.clock.now_ns() + ms as u64 * 1_000_000)
    };
    if mask & 0x10 != 0
        && let Some(t) = next_timer_due(s)
    {
        deadline = Some(deadline.map_or(t, |d| d.min(t)));
    }
    Ok(ApiRet::Block(Block {
        wait,
        deadline,
        cont: Cont::User32(U32Cont::MsgWait { n }),
    }))
}

fn translate_message(s: &mut Session) -> ApiResult {
    let [p] = s.args::<1>()?;
    let msg = s.mem.read_u32(p + 4)?;
    let hwnd = s.mem.read_u32(p)?;
    let vk = s.mem.read_u32(p + 8)?;
    let lp = s.mem.read_u32(p + 12)?;
    Ok(ApiRet::Ret(translate(s, hwnd, msg, vk, lp) as u32))
}

/// TranslateMessage: post WM_CHAR/WM_SYSCHAR for character-producing key downs.
pub fn translate(s: &mut Session, hwnd: u32, msg: u32, vk: u32, lp: u32) -> bool {
    if msg != WM_KEYDOWN && msg != WM_SYSKEYDOWN {
        return false;
    }
    let vk = vk as u8;
    let inp = &s.user.input;
    let shift = inp.keys[0x10];
    let caps = inp.toggles[0x14];
    let ctrl = inp.keys[0x11];
    let Some(mut c) = input::vk_to_char(vk, shift, caps) else {
        return false;
    };
    if ctrl && c.is_ascii_alphabetic() {
        c = c.to_ascii_uppercase() - b'A' + 1;
    }
    let tid = s.tid();
    let t = s.msg_time();
    let m = Msg {
        hwnd,
        message: if msg == WM_SYSKEYDOWN { WM_SYSCHAR } else { WM_CHAR },
        wparam: c as u32,
        lparam: lp,
        time: t,
        pt: (0, 0),
    };
    s.queue(tid).posted.push_front(m);
    true
}

fn dispatch_message_a(s: &mut Session) -> ApiResult {
    let [p] = s.args::<1>()?;
    let hwnd = s.mem.read_u32(p)?;
    let msg = s.mem.read_u32(p + 4)?;
    let wp = s.mem.read_u32(p + 8)?;
    let lp = s.mem.read_u32(p + 12)?;
    let time = s.mem.read_u32(p + 16)?;
    if msg == WM_TIMER && lp != 0 {
        return Ok(ApiRet::call(
            lp,
            vec![hwnd.into(), msg.into(), wp.into(), time.into()],
            Cont::ReturnValue,
        ));
    }
    let Some(w) = s.window(hwnd) else {
        return Ok(ApiRet::Ret(0));
    };
    let proc_ = w.wndproc;
    Ok(ApiRet::call(
        proc_,
        vec![hwnd.into(), msg.into(), wp.into(), lp.into()],
        Cont::ReturnValue,
    ))
}

fn get_message_time(s: &mut Session) -> ApiResult {
    let tid = s.tid();
    Ok(ApiRet::Ret(s.queue(tid).last_time))
}
fn get_message_pos(s: &mut Session) -> ApiResult {
    let tid = s.tid();
    let (x, y) = s.queue(tid).last_pt;
    Ok(ApiRet::Ret((x as u32 & 0xffff) | ((y as u32) << 16)))
}

fn get_queue_status(s: &mut Session) -> ApiResult {
    let tid = s.tid();
    let q = s.queue(tid).clone();
    let mut st = 0u32;
    if !q.posted.is_empty() {
        st |= 0x8;
    }
    if q.input.iter().any(|m| (WM_KEYDOWN..=0x108).contains(&m.message)) {
        st |= 0x1;
    }
    if q.input.iter().any(|m| (WM_MOUSEMOVE..=0x20d).contains(&m.message)) {
        st |= 0x2 | 0x4;
    }
    Ok(ApiRet::Ret(st | (st << 16)))
}

// ---------------------------------------------------------------- painting

fn invalidate_rect(s: &mut Session) -> ApiResult {
    let [h, p, erase] = s.args::<3>()?;
    let targets: Vec<u32> = if h == 0 {
        s.user.windows.keys().copied().collect()
    } else {
        vec![h]
    };
    let r = if p != 0 { Some(s.read_rect(p)?) } else { None };
    for t in targets {
        if let Some(w) = s.user.windows.get_mut(&t) {
            let full = Rect::new(0, 0, w.client.w(), w.client.h());
            let nr = r.unwrap_or(full);
            w.invalid = Some(match w.invalid {
                Some(o) => Rect::new(
                    o.left.min(nr.left),
                    o.top.min(nr.top),
                    o.right.max(nr.right),
                    o.bottom.max(nr.bottom),
                ),
                None => nr,
            });
            w.erase_pending |= erase != 0;
        }
    }
    Ok(ApiRet::Ret(1))
}

fn validate_rect(s: &mut Session) -> ApiResult {
    let [h, _p] = s.args::<2>()?;
    if let Some(w) = s.user.windows.get_mut(&h) {
        w.invalid = None;
        w.erase_pending = false;
    }
    Ok(ApiRet::Ret(1))
}

fn get_update_rect(s: &mut Session) -> ApiResult {
    let [h, p, _erase] = s.args::<3>()?;
    let r = s.window(h).and_then(|w| w.invalid);
    if p != 0 {
        s.write_rect(p, &r.unwrap_or_default())?;
    }
    Ok(ApiRet::Ret(r.is_some() as u32))
}

/// Draw a window's frame when it needs repainting (the WM_NCPAINT of a WM_PAINT cycle).
fn paint_frame_if_dirty(s: &mut Session, h: u32) {
    if s.user.nc_dirty.contains(&h) {
        s.user.nc_dirty.retain(|&x| x != h);
        let _ = crate::dll::dialog::paint_dialog_frame(s, h);
    }
}

impl Session {
    /// A window is visible on screen only when it and all its ancestors are visible.
    pub fn really_visible(&self, mut h: u32) -> bool {
        while let Some(w) = self.user.windows.get(&h) {
            if !w.visible {
                return false;
            }
            if w.style & WS_CHILD == 0 || w.parent == 0 {
                return true;
            }
            h = w.parent;
        }
        false
    }
}

fn update_window(s: &mut Session) -> ApiResult {
    let [h] = s.args::<1>()?;
    let inv = s.window(h).map(|w| w.invalid.is_some()).unwrap_or(false) && s.really_visible(h);
    if inv {
        paint_frame_if_dirty(s, h);
        let steps = VecDeque::from(vec![send(h, WM_PAINT, 0, 0), Step::Act(Act::SetRet(1))]);
        return run_seq(s, steps, 1);
    }
    Ok(ApiRet::Ret(1))
}

fn begin_paint(s: &mut Session) -> ApiResult {
    let [h, ps] = s.args::<2>()?;
    let Some(w) = s.window(h).cloned() else {
        return Ok(ApiRet::Ret(0));
    };
    let hdc = crate::dll::gdi32::window_dc(s, h, false);
    let rc = w.invalid.unwrap_or_default();
    let mut b = vec![0u8; 64];
    b[0..4].copy_from_slice(&hdc.to_le_bytes());
    b[4..8].copy_from_slice(&(w.erase_pending as u32).to_le_bytes());
    b[8..24].copy_from_slice(&rc.bytes());
    s.mem.write(ps, &b)?;
    if let Some(wm) = s.user.windows.get_mut(&h) {
        wm.invalid = None;
    }
    if w.erase_pending {
        if let Some(wm) = s.user.windows.get_mut(&h) {
            wm.erase_pending = false;
        }
        let steps = VecDeque::from(vec![Step::Send {
            hwnd: h,
            msg: WM_ERASEBKGND,
            wp: hdc,
            lp: LParam::Val(0),
            check: Check::None,
        }]);
        return run_seq(s, steps, hdc);
    }
    Ok(ApiRet::Ret(hdc))
}

fn end_paint(s: &mut Session) -> ApiResult {
    let [_h, ps] = s.args::<2>()?;
    let hdc = s.mem.read_u32(ps)?;
    crate::dll::gdi32::release_dc_handle(s, hdc);
    Ok(ApiRet::Ret(1))
}

// ---------------------------------------------------------------- timers

fn set_timer(s: &mut Session) -> ApiResult {
    let [hwnd, id, ms, proc_] = s.args::<4>()?;
    let period = (ms.max(10) as u64) * 1_000_000;
    let now = s.clock.now_ns();
    let tid = s.tid();
    let id = if hwnd == 0 && id == 0 {
        0x7ff0 + s.user.timers.len() as u32
    } else {
        id
    };
    s.user.timers.retain(|t| !(t.hwnd == hwnd && t.id == id));
    s.user.timers.push(Timer {
        hwnd,
        id,
        period_ns: period,
        due: now + period,
        proc_,
        tid,
        fired: false,
    });
    Ok(ApiRet::Ret(if hwnd == 0 { id } else { id.max(1) }))
}

fn kill_timer(s: &mut Session) -> ApiResult {
    let [hwnd, id] = s.args::<2>()?;
    let n = s.user.timers.len();
    s.user.timers.retain(|t| !(t.hwnd == hwnd && t.id == id));
    Ok(ApiRet::Ret((n != s.user.timers.len()) as u32))
}

// ---------------------------------------------------------------- input state

fn get_async_key_state(s: &mut Session) -> ApiResult {
    let [vk] = s.args::<1>()?;
    let v = (vk & 0xff) as usize;
    let inp = &mut s.user.input;
    let down = inp.keys[v];
    let pressed = std::mem::replace(&mut inp.async_pressed[v], false);
    // Applications only see key state while they are the foreground application.
    if !s.user.app_active {
        return Ok(ApiRet::Ret(0));
    }
    Ok(ApiRet::Ret(((down as u32) << 15) | pressed as u32))
}

fn get_key_state(s: &mut Session) -> ApiResult {
    let [vk] = s.args::<1>()?;
    let v = (vk & 0xff) as usize;
    let inp = &s.user.input;
    let r = ((inp.keys[v] as u16) << 15) | inp.toggles[v] as u16;
    Ok(ApiRet::Ret(r as i16 as i32 as u32))
}

fn get_keyboard_state(s: &mut Session) -> ApiResult {
    let [p] = s.args::<1>()?;
    let b: Vec<u8> = (0..256)
        .map(|i| ((s.user.input.keys[i] as u8) << 7) | s.user.input.toggles[i] as u8)
        .collect();
    s.mem.write(p, &b)?;
    Ok(ApiRet::Ret(1))
}

fn map_virtual_key_a(s: &mut Session) -> ApiResult {
    let [code, ty] = s.args::<2>()?;
    let r = match ty {
        0 => input::vk_to_scan(code as u8) as u32,
        1 | 3 => (0..=255u8)
            .find(|&v| input::vk_to_scan(v) as u32 == code && v != 0x10 && v != 0x11 && v != 0x12)
            .map(|v| v as u32)
            .unwrap_or(0),
        2 => input::vk_to_char(code as u8, false, false)
            .map(|c| c.to_ascii_uppercase() as u32)
            .unwrap_or(0),
        _ => 0,
    };
    Ok(ApiRet::Ret(r))
}

fn get_key_name_text_a(s: &mut Session) -> ApiResult {
    let [lp, buf, n] = s.args::<3>()?;
    let scan = ((lp >> 16) & 0xff) as u16;
    let ext = lp & (1 << 24) != 0;
    match input::scan_name(scan, ext) {
        Some(name) => Ok(ApiRet::Ret(s.write_str_trunc(buf, n, name)?)),
        None => {
            s.write_str_trunc(buf, n, "")?;
            Ok(ApiRet::Ret(0))
        }
    }
}

fn get_cursor_pos(s: &mut Session) -> ApiResult {
    let [p] = s.args::<1>()?;
    s.mem.write_u32(p, s.user.input.mouse_x as u32)?;
    s.mem.write_u32(p + 4, s.user.input.mouse_y as u32)?;
    Ok(ApiRet::Ret(1))
}

fn set_cursor_pos(s: &mut Session) -> ApiResult {
    let [x, y] = s.args::<2>()?;
    let (mut x, mut y) = (x as i32, y as i32);
    let m = s.user.mode;
    let clip = s.user.clip.unwrap_or(Rect::new(0, 0, m.width as i32, m.height as i32));
    x = x.clamp(clip.left, clip.right - 1);
    y = y.clamp(clip.top, clip.bottom - 1);
    s.user.input.mouse_x = x;
    s.user.input.mouse_y = y;
    Ok(ApiRet::Ret(1))
}

fn show_cursor(s: &mut Session) -> ApiResult {
    let [show] = s.args::<1>()?;
    s.user.cursor_count += if show != 0 { 1 } else { -1 };
    Ok(ApiRet::Ret(s.user.cursor_count as u32))
}

fn set_cursor(s: &mut Session) -> ApiResult {
    let [c] = s.args::<1>()?;
    let old = std::mem::replace(&mut s.user.cursor, c);
    Ok(ApiRet::Ret(old))
}

fn clip_cursor(s: &mut Session) -> ApiResult {
    let [p] = s.args::<1>()?;
    s.user.clip = if p == 0 { None } else { Some(s.read_rect(p)?) };
    Ok(ApiRet::Ret(1))
}

fn set_capture(s: &mut Session) -> ApiResult {
    let [h] = s.args::<1>()?;
    let old = std::mem::replace(&mut s.user.capture, h);
    Ok(ApiRet::Ret(old))
}
fn release_capture(s: &mut Session) -> ApiResult {
    s.user.capture = 0;
    Ok(ApiRet::Ret(1))
}
fn get_capture(s: &mut Session) -> ApiResult {
    Ok(ApiRet::Ret(s.user.capture))
}

fn window_from_point(s: &mut Session) -> ApiResult {
    let [x, y] = s.args::<2>()?;
    let (x, y) = (x as i32, y as i32);
    Ok(ApiRet::Ret(window_at(s, x, y)))
}

fn window_at(s: &Session, x: i32, y: i32) -> u32 {
    // Topmost-ish: last created visible window containing the point; prefer children.
    let mut best = 0;
    for w in s.user.windows.values() {
        if w.rect.contains(x, y) && s.really_visible(w.hwnd) {
            best = w.hwnd;
        }
    }
    best
}

// ---------------------------------------------------------------- resources

fn load_icon_cursor(s: &mut Session) -> ApiResult {
    let [hinst, name] = s.args::<2>()?;
    // Handles are stable tokens derived from (module, resource id); system cursors and
    // icons (hinst == NULL) are pre-defined.
    let id = match s.read_str_or_id(name)? {
        StrOrId::Id(i) => i,
        StrOrId::Str(n) => {
            let mut h: u32 = 0x811c_9dc5;
            for b in n.to_uppercase().bytes() {
                h = (h ^ b as u32).wrapping_mul(0x0100_0193);
            }
            h & 0xffff
        }
    };
    Ok(ApiRet::Ret(0x0002_0000 | (hinst & 0xff0000) >> 8 | id))
}

fn load_string_a(s: &mut Session) -> ApiResult {
    let [hinst, id, buf, n] = s.args::<4>()?;
    let block = (id >> 4) + 1;
    let Some((ptr, size)) = crate::dll::kernel32::find_resource(
        s,
        hinst,
        drydock_core::pe::ResId::Id(drydock_core::pe::RT_STRING),
        drydock_core::pe::ResId::Id(block),
    ) else {
        if n > 0 {
            s.mem.write_u8(buf, 0)?;
        }
        return Ok(ApiRet::Ret(0));
    };
    let data = s.mem.read_vec(ptr, size)?;
    let mut off = 0usize;
    for i in 0..16 {
        if off + 2 > data.len() {
            break;
        }
        let len = u16::from_le_bytes([data[off], data[off + 1]]) as usize;
        off += 2;
        if i == (id & 15) as usize {
            let w: Vec<u16> = data[off..off + len * 2]
                .as_chunks::<2>()
                .0
                .iter()
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect();
            let st = String::from_utf16_lossy(&w);
            if n == 0 {
                return Ok(ApiRet::Ret(0));
            }
            return Ok(ApiRet::Ret(s.write_str_trunc(buf, n, &st)?));
        }
        off += len * 2;
    }
    if n > 0 {
        s.mem.write_u8(buf, 0)?;
    }
    Ok(ApiRet::Ret(0))
}

// ---------------------------------------------------------------- strings & rects

fn char_prev_a(s: &mut Session) -> ApiResult {
    let [start, cur] = s.args::<2>()?;
    Ok(ApiRet::Ret(if cur > start { cur - 1 } else { start }))
}
fn char_next_a(s: &mut Session) -> ApiResult {
    let [p] = s.args::<1>()?;
    let c = s.mem.read_u8(p)?;
    Ok(ApiRet::Ret(if c == 0 { p } else { p + 1 }))
}
fn char_upper_a(s: &mut Session) -> ApiResult {
    let [p] = s.args::<1>()?;
    if p < 0x10000 {
        return Ok(ApiRet::Ret((p as u8).to_ascii_uppercase() as u32));
    }
    let b = s.mem.read_cstr(p, 0x100000)?;
    let st = crate::strings::decode_ansi(&b).to_uppercase();
    let out = crate::strings::encode_ansi(&st);
    s.mem.write(p, &out[..out.len().min(b.len())])?;
    Ok(ApiRet::Ret(p))
}
fn char_lower_a(s: &mut Session) -> ApiResult {
    let [p] = s.args::<1>()?;
    if p < 0x10000 {
        return Ok(ApiRet::Ret((p as u8).to_ascii_lowercase() as u32));
    }
    let b = s.mem.read_cstr(p, 0x100000)?;
    let st = crate::strings::decode_ansi(&b).to_lowercase();
    let out = crate::strings::encode_ansi(&st);
    s.mem.write(p, &out[..out.len().min(b.len())])?;
    Ok(ApiRet::Ret(p))
}

fn set_rect(s: &mut Session) -> ApiResult {
    let [p, l, t, r, b] = s.args::<5>()?;
    s.write_rect(p, &Rect::new(l as i32, t as i32, r as i32, b as i32))?;
    Ok(ApiRet::Ret(1))
}
fn set_rect_empty(s: &mut Session) -> ApiResult {
    let [p] = s.args::<1>()?;
    s.write_rect(p, &Rect::default())?;
    Ok(ApiRet::Ret(1))
}
fn copy_rect(s: &mut Session) -> ApiResult {
    let [d, src] = s.args::<2>()?;
    let r = s.read_rect(src)?;
    s.write_rect(d, &r)?;
    Ok(ApiRet::Ret(1))
}
fn offset_rect(s: &mut Session) -> ApiResult {
    let [p, dx, dy] = s.args::<3>()?;
    let mut r = s.read_rect(p)?;
    r.left += dx as i32;
    r.right += dx as i32;
    r.top += dy as i32;
    r.bottom += dy as i32;
    s.write_rect(p, &r)?;
    Ok(ApiRet::Ret(1))
}
fn inflate_rect(s: &mut Session) -> ApiResult {
    let [p, dx, dy] = s.args::<3>()?;
    let mut r = s.read_rect(p)?;
    r.left -= dx as i32;
    r.right += dx as i32;
    r.top -= dy as i32;
    r.bottom += dy as i32;
    s.write_rect(p, &r)?;
    Ok(ApiRet::Ret(1))
}
fn pt_in_rect(s: &mut Session) -> ApiResult {
    let [p, x, y] = s.args::<3>()?;
    let r = s.read_rect(p)?;
    Ok(ApiRet::Ret(r.contains(x as i32, y as i32) as u32))
}
fn intersect_rect(s: &mut Session) -> ApiResult {
    let [d, a, b] = s.args::<3>()?;
    let r = s.read_rect(a)?.intersect(&s.read_rect(b)?);
    let r = if r.is_empty() { Rect::default() } else { r };
    s.write_rect(d, &r)?;
    Ok(ApiRet::Ret(!r.is_empty() as u32))
}
fn union_rect(s: &mut Session) -> ApiResult {
    let [d, a, b] = s.args::<3>()?;
    let (ra, rb) = (s.read_rect(a)?, s.read_rect(b)?);
    let r = if ra.is_empty() {
        rb
    } else if rb.is_empty() {
        ra
    } else {
        Rect::new(
            ra.left.min(rb.left),
            ra.top.min(rb.top),
            ra.right.max(rb.right),
            ra.bottom.max(rb.bottom),
        )
    };
    s.write_rect(d, &r)?;
    Ok(ApiRet::Ret(!r.is_empty() as u32))
}
fn is_rect_empty(s: &mut Session) -> ApiResult {
    let [p] = s.args::<1>()?;
    Ok(ApiRet::Ret(s.read_rect(p)?.is_empty() as u32))
}
fn equal_rect(s: &mut Session) -> ApiResult {
    let [a, b] = s.args::<2>()?;
    Ok(ApiRet::Ret((s.read_rect(a)? == s.read_rect(b)?) as u32))
}

fn adjust_window_rect_ex(s: &mut Session) -> ApiResult {
    let [p, style, menu, ex] = s.args::<4>()?;
    let mut r = s.read_rect(p)?;
    let i = nc_insets(style, ex, menu != 0);
    r.left -= i.left;
    r.top -= i.top;
    r.right += i.right;
    r.bottom += i.bottom;
    s.write_rect(p, &r)?;
    Ok(ApiRet::Ret(1))
}
fn adjust_window_rect(s: &mut Session) -> ApiResult {
    let [p, style, menu] = s.args::<3>()?;
    let mut r = s.read_rect(p)?;
    let i = nc_insets(style, 0, menu != 0);
    r.left -= i.left;
    r.top -= i.top;
    r.right += i.right;
    r.bottom += i.bottom;
    s.write_rect(p, &r)?;
    Ok(ApiRet::Ret(1))
}

/// wsprintfA / wvsprintfA formatting (subset of printf used by Windows' implementation).
pub fn format_wsprintf(
    s: &Session,
    fmt: &[u8],
    mut next: impl FnMut() -> Result<u32, ApiError>,
) -> Result<Vec<u8>, ApiError> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < fmt.len() {
        let c = fmt[i];
        i += 1;
        if c != b'%' {
            out.push(c);
            continue;
        }
        if i < fmt.len() && fmt[i] == b'%' {
            out.push(b'%');
            i += 1;
            continue;
        }
        let mut left = false;
        let mut zero = false;
        let mut hash = false;
        while i < fmt.len() && matches!(fmt[i], b'-' | b'0' | b'#') {
            match fmt[i] {
                b'-' => left = true,
                b'0' => zero = true,
                _ => hash = true,
            }
            i += 1;
        }
        let mut width = 0usize;
        while i < fmt.len() && fmt[i].is_ascii_digit() {
            width = width * 10 + (fmt[i] - b'0') as usize;
            i += 1;
        }
        let mut prec: Option<usize> = None;
        if i < fmt.len() && fmt[i] == b'.' {
            i += 1;
            let mut p = 0;
            while i < fmt.len() && fmt[i].is_ascii_digit() {
                p = p * 10 + (fmt[i] - b'0') as usize;
                i += 1;
            }
            prec = Some(p);
        }
        while i < fmt.len() && matches!(fmt[i], b'l' | b'h') {
            i += 1;
        }
        if i >= fmt.len() {
            break;
        }
        let conv = fmt[i];
        i += 1;
        let body: Vec<u8> = match conv {
            b'd' | b'i' => {
                let v = next()? as i32;
                let mut d = v.unsigned_abs().to_string();
                if let Some(p) = prec {
                    while d.len() < p {
                        d.insert(0, '0');
                    }
                }
                if v < 0 {
                    d.insert(0, '-');
                }
                d.into_bytes()
            }
            b'u' => next()?.to_string().into_bytes(),
            b'x' => {
                let v = format!("{:x}", next()?);
                if hash { format!("0x{v}") } else { v }.into_bytes()
            }
            b'X' => {
                let v = format!("{:X}", next()?);
                if hash { format!("0X{v}") } else { v }.into_bytes()
            }
            b'c' => vec![next()? as u8],
            b's' => {
                let p = next()?;
                let mut b = if p == 0 {
                    b"(null)".to_vec()
                } else {
                    s.mem.read_cstr(p, 0x100000)?
                };
                if let Some(pr) = prec {
                    b.truncate(pr);
                }
                b
            }
            b'p' => format!("{:08X}", next()?).into_bytes(),
            other => vec![b'%', other],
        };
        let pad = width.saturating_sub(body.len());
        if left {
            out.extend(&body);
            out.extend(std::iter::repeat_n(b' ', pad));
        } else {
            let fill = if zero && conv != b's' { b'0' } else { b' ' };
            if fill == b'0' && body.first() == Some(&b'-') {
                out.push(b'-');
                out.extend(std::iter::repeat_n(b'0', pad));
                out.extend(&body[1..]);
            } else {
                out.extend(std::iter::repeat_n(fill, pad));
                out.extend(&body);
            }
        }
    }
    Ok(out)
}

fn wsprintf_a(s: &mut Session) -> ApiResult {
    let [buf, fmt] = s.args::<2>()?;
    let f = s.mem.read_cstr(fmt, 0x10000)?;
    let esp = s.cpu.esp();
    let mut idx = 3u32;
    let mem = &s.mem;
    let out = format_wsprintf(s, &f, || {
        let v = mem.read_u32(esp + 4 * idx)?;
        idx += 1;
        Ok(v)
    })?;
    let n = out.len().min(1023);
    s.mem.write(buf, &out[..n])?;
    s.mem.write_u8(buf + n as u32, 0)?;
    Ok(ApiRet::Ret(n as u32))
}

fn wvsprintf_a(s: &mut Session) -> ApiResult {
    let [buf, fmt, va] = s.args::<3>()?;
    let f = s.mem.read_cstr(fmt, 0x10000)?;
    let mut p = va;
    let mem = &s.mem;
    let out = format_wsprintf(s, &f, || {
        let v = mem.read_u32(p)?;
        p += 4;
        Ok(v)
    })?;
    let n = out.len().min(1023);
    s.mem.write(buf, &out[..n])?;
    s.mem.write_u8(buf + n as u32, 0)?;
    Ok(ApiRet::Ret(n as u32))
}

// ---------------------------------------------------------------- dialogs

pub(crate) fn message_box_a(s: &mut Session) -> ApiResult {
    let [hwnd, text, caption, style] = s.args::<4>()?;
    let text = s.read_str_opt(text)?.unwrap_or_default();
    let caption = s.read_str_opt(caption)?.unwrap_or_else(|| "Error".into());
    let id = s.user.next_dialog;
    s.user.next_dialog += 1;
    // Default button: first button of the type, or the one selected by MB_DEFBUTTONn.
    let buttons: &[u32] = match style & 0xf {
        0 => &[1],
        1 => &[1, 2],
        2 => &[3, 4, 5],
        3 => &[6, 7, 2],
        4 => &[6, 7],
        5 => &[4, 2],
        _ => &[1],
    };
    let def = buttons[((style >> 8) & 0xf) as usize % buttons.len()];
    s.event(
        Severity::Warning,
        "dialog",
        format!("MessageBox #{id} {caption:?}: {text:?} (style {style:#x}, default {def})"),
    );
    let tid = s.tid();
    let auto = s.user.auto_dismiss_dialogs;
    s.user.dialogs.push(Dialog {
        id,
        tid,
        text: text.clone(),
        caption: caption.clone(),
        style,
        answer: if auto { Some(def) } else { None },
    });
    if !auto {
        // Shown as a modal dialog; scenarios can still answer it.
        let owner = if s.window(hwnd).is_some() { hwnd } else { 0 };
        return crate::dll::dialog::message_box(s, owner, id, &text, &caption, style);
    }
    Ok(ApiRet::Block(Block {
        wait: WaitKind::Dialog { id },
        deadline: None,
        cont: Cont::User32(U32Cont::Dialog { id }),
    }))
}

// ---------------------------------------------------------------- display

/// (field mask, width, height, bits per pixel, refresh) of a DEVMODE.
type DevMode = (u32, Option<u32>, Option<u32>, Option<u32>, Option<u32>);

fn read_devmode(s: &Session, p: u32) -> Result<DevMode, ApiError> {
    let fields = s.mem.read_u32(p + 40)?;
    let bpp = s.mem.read_u32(p + 104)?;
    let w = s.mem.read_u32(p + 108)?;
    let h = s.mem.read_u32(p + 112)?;
    let f = s.mem.read_u32(p + 120)?;
    Ok((
        fields,
        (fields & 0x40000 != 0).then_some(bpp),
        (fields & 0x80000 != 0).then_some(w),
        (fields & 0x100000 != 0).then_some(h),
        (fields & 0x400000 != 0).then_some(f),
    ))
}

fn write_devmode(s: &mut Session, p: u32, m: &DisplayMode) -> Result<(), ApiError> {
    let size = s.mem.read_u16(p + 36)?.max(148) as u32;
    let mut b = vec![0u8; 148.min(size as usize)];
    let name = b"DRYDOCK";
    b[..name.len()].copy_from_slice(name);
    b[32..34].copy_from_slice(&0x0401u16.to_le_bytes()); // dmSpecVersion
    b[36..38].copy_from_slice(&148u16.to_le_bytes());
    b[40..44].copy_from_slice(&(0x40000u32 | 0x80000 | 0x100000 | 0x400000).to_le_bytes());
    b[104..108].copy_from_slice(&m.bpp.to_le_bytes());
    b[108..112].copy_from_slice(&m.width.to_le_bytes());
    b[112..116].copy_from_slice(&m.height.to_le_bytes());
    b[120..124].copy_from_slice(&m.refresh.to_le_bytes());
    s.mem.write(p, &b)?;
    Ok(())
}

fn enum_display_settings_a(s: &mut Session) -> ApiResult {
    let [_dev, idx, p] = s.args::<3>()?;
    let mode = if idx == 0xffff_ffff {
        Some(s.user.mode)
    } else if idx == 0xffff_fffe {
        Some(s.user.desktop_mode)
    } else {
        supported_modes(s.config.windows.refresh_hz).get(idx as usize).copied()
    };
    match mode {
        Some(m) => {
            write_devmode(s, p, &m)?;
            Ok(ApiRet::Ret(1))
        }
        None => Ok(ApiRet::Ret(0)),
    }
}

fn change_display_settings_a(s: &mut Session) -> ApiResult {
    let [p, flags] = s.args::<2>()?;
    if p == 0 {
        let d = s.user.desktop_mode;
        s.user.mode = d;
        return Ok(ApiRet::Ret(0));
    }
    let (_fields, bpp, w, h, f) = read_devmode(s, p)?;
    let cur = s.user.mode;
    let want = DisplayMode {
        width: w.unwrap_or(cur.width),
        height: h.unwrap_or(cur.height),
        bpp: bpp.unwrap_or(cur.bpp),
        refresh: f.filter(|&f| f != 0).unwrap_or(cur.refresh),
    };
    let ok = supported_modes(s.config.windows.refresh_hz)
        .iter()
        .any(|m| m.width == want.width && m.height == want.height && m.bpp == want.bpp);
    if !ok {
        return Ok(ApiRet::Ret(0xffff_fffe)); // DISP_CHANGE_BADMODE
    }
    if flags & 2 == 0 {
        // !CDS_TEST
        s.user.mode = want;
    }
    Ok(ApiRet::Ret(0))
}

fn system_parameters_info_a(s: &mut Session) -> ApiResult {
    let [action, _ui, pv, _wini] = s.args::<4>()?;
    match action {
        0x30 => {
            // SPI_GETWORKAREA
            let m = s.user.mode;
            s.write_rect(pv, &Rect::new(0, 0, m.width as i32, m.height as i32 - 28))?;
            Ok(ApiRet::Ret(1))
        }
        0x10 | 0x0e | 0x16 | 0x1c | 0x5f => {
            // screensaver active/timeout, key repeat queries
            if pv != 0 {
                s.mem.write_u32(pv, 0)?;
            }
            Ok(ApiRet::Ret(1))
        }
        0x11 | 0x0f | 0x61 => Ok(ApiRet::Ret(1)), // SET screensaver / SPI_SETSCREENSAVERRUNNING
        a => {
            s.missing("SystemParametersInfo", format!("action {a:#x}"));
            Ok(ApiRet::Ret(0))
        }
    }
}

// ---------------------------------------------------------------- scheduling hooks

pub fn has_input(s: &Session, tid: u32, _mask: u32) -> bool {
    let Some(q) = s.user.queues.get(&tid) else {
        return s
            .user
            .windows
            .values()
            .any(|w| w.tid == tid && w.visible && w.invalid.is_some());
    };
    if !q.posted.is_empty() || !q.input.is_empty() || q.quit.is_some() {
        return true;
    }
    let now = s.clock.now_ns();
    s.user.timers.iter().any(|t| t.tid == tid && t.due <= now)
        || s.user
            .windows
            .values()
            .any(|w| w.tid == tid && w.visible && w.invalid.is_some())
}

pub fn next_event(s: &Session) -> Option<u64> {
    s.user.input.next_due()
}

/// Deliver scheduled input that is due.
pub fn service(s: &mut Session) {
    let now = s.clock.now_ns();
    while let Some(e) = s.user.input.pending.front() {
        if e.at_ns > now {
            break;
        }
        let e = s.user.input.pending.pop_front().unwrap();
        deliver_input(s, e.at_ns, e.ev);
    }
}

fn deliver_input(s: &mut Session, at: u64, ev: InputEvent) {
    let prev_keys = s.user.input.keys.clone();
    s.user.input.apply_state(&ev);
    s.user.input.log.push((at, ev));
    s.user.input.recorded.push(input::TimedInput { at_ns: at, ev });
    crate::com::dinput::on_input(s, at, &ev);
    let time = ((at + crate::dll::kernel32::UPTIME_AT_START_NS) / 1_000_000) as u32;
    let pt = (s.user.input.mouse_x, s.user.input.mouse_y);
    match ev {
        InputEvent::Key {
            vk,
            scan,
            extended,
            down,
        } => {
            let target = if s.user.focus != 0 { s.user.focus } else { s.user.active };
            let Some(w) = s.window(target) else {
                return;
            };
            let tid = w.tid;
            let alt = s.user.input.keys[0x12];
            let was_down = prev_keys[vk as usize];
            let mut lp = 1u32 | ((scan as u32 & 0xff) << 16);
            if extended {
                lp |= 1 << 24;
            }
            if alt {
                lp |= 1 << 29;
            }
            if was_down {
                lp |= 1 << 30;
            }
            if !down {
                lp |= 1 << 31 | 1 << 30;
            }
            let sys = alt || vk == 0x79 || vk == 0x12 || vk == 0xa4 || vk == 0xa5;
            let message = match (down, sys) {
                (true, false) => WM_KEYDOWN,
                (false, false) => WM_KEYUP,
                (true, true) => WM_SYSKEYDOWN,
                (false, true) => WM_SYSKEYUP,
            };
            // Messages carry the generic VK for left/right modifiers.
            let vk_msg = match vk {
                0xa0 | 0xa1 => 0x10,
                0xa2 | 0xa3 => 0x11,
                0xa4 | 0xa5 => 0x12,
                v => v,
            };
            s.queue(tid).input.push_back(Msg {
                hwnd: target,
                message,
                wparam: vk_msg as u32,
                lparam: lp,
                time,
                pt,
            });
        }
        InputEvent::MouseMove { .. } | InputEvent::MouseRel { .. } => {
            let m = s.user.mode;
            let clip = s.user.clip.unwrap_or(Rect::new(0, 0, m.width as i32, m.height as i32));
            let inp = &mut s.user.input;
            inp.mouse_x = inp.mouse_x.clamp(clip.left, clip.right - 1);
            inp.mouse_y = inp.mouse_y.clamp(clip.top, clip.bottom - 1);
            let (x, y) = (inp.mouse_x, inp.mouse_y);
            mouse_msg(s, WM_MOUSEMOVE, x, y, time);
        }
        InputEvent::MouseButton { button, down } => {
            let msg = match (button, down) {
                (0, true) => WM_LBUTTONDOWN,
                (0, false) => WM_LBUTTONUP,
                (1, true) => WM_RBUTTONDOWN,
                (1, false) => WM_RBUTTONUP,
                (2, true) => WM_MBUTTONDOWN,
                (2, false) => WM_MBUTTONUP,
                _ => return,
            };
            let (x, y) = pt;
            mouse_msg(s, msg, x, y, time);
        }
        InputEvent::Wheel { delta } => {
            let target = if s.user.focus != 0 { s.user.focus } else { s.user.active };
            if let Some(w) = s.window(target) {
                let tid = w.tid;
                s.queue(tid).input.push_back(Msg {
                    hwnd: target,
                    message: WM_MOUSEWHEEL,
                    wparam: (delta as u32) << 16,
                    lparam: (pt.0 as u32 & 0xffff) | ((pt.1 as u32) << 16),
                    time,
                    pt,
                });
            }
        }
        InputEvent::Focus { .. }
        | InputEvent::JoyAxes { .. }
        | InputEvent::JoyButton { .. }
        | InputEvent::JoyPov { .. } => {}
    }
}

fn mouse_msg(s: &mut Session, msg: u32, x: i32, y: i32, time: u32) {
    let target = if s.user.capture != 0 {
        s.user.capture
    } else {
        window_at(s, x, y)
    };
    let Some(w) = s.window(target) else {
        return;
    };
    let tid = w.tid;
    let (cx, cy) = (x - w.client.left, y - w.client.top);
    let b = s.user.input.buttons;
    let mut keys = 0u32;
    if b & 1 != 0 {
        keys |= 1;
    }
    if b & 2 != 0 {
        keys |= 2;
    }
    if b & 4 != 0 {
        keys |= 0x10;
    }
    if s.user.input.keys[0x10] {
        keys |= 4;
    }
    if s.user.input.keys[0x11] {
        keys |= 8;
    }
    let q = s.queue(tid);
    // Coalesce consecutive mouse moves, as Windows does.
    if msg == WM_MOUSEMOVE
        && let Some(last) = q.input.back_mut()
        && last.message == WM_MOUSEMOVE
        && last.hwnd == target
    {
        last.lparam = (cx as u32 & 0xffff) | ((cy as u32) << 16);
        last.wparam = keys;
        last.time = time;
        last.pt = (x, y);
        return;
    }
    q.input.push_back(Msg {
        hwnd: target,
        message: msg,
        wparam: keys,
        lparam: (cx as u32 & 0xffff) | ((cy as u32) << 16),
        time,
        pt: (x, y),
    });
}

pub static USER32_EXPORTS: &[ExportDef] = &[
    export!("AdjustWindowRect", 3, adjust_window_rect),
    export!("AdjustWindowRectEx", 4, adjust_window_rect_ex),
    export!("BeginPaint", 2, begin_paint),
    export!("CallWindowProcA", 5, call_window_proc_a),
    export!("ChangeDisplaySettingsA", 2, change_display_settings_a),
    export!("CharLowerA", 1, char_lower_a),
    export!("CharNextA", 1, char_next_a),
    export!("CharPrevA", 2, char_prev_a),
    export!("CharUpperA", 1, char_upper_a),
    export!("ClientToScreen", 2, client_to_screen),
    export!("ClipCursor", 1, clip_cursor),
    export!("CopyRect", 2, copy_rect),
    export!("CreateWindowExA", 12, create_window_ex_a),
    export!("DefWindowProcA", 4, def_window_proc_a),
    export!("DestroyWindow", 1, destroy_window),
    export!("DispatchMessageA", 1, dispatch_message_a),
    export!("DrawTextA", 5, crate::dll::gdi32::draw_text_a),
    export!("EnableWindow", 2, enable_window),
    export!("EndPaint", 2, end_paint),
    export!("EnumDisplaySettingsA", 3, enum_display_settings_a),
    export!("EqualRect", 2, equal_rect),
    export!("FillRect", 3, crate::dll::gdi32::fill_rect),
    export!("FindWindowA", 2, find_window_a),
    export!("GetActiveWindow", 0, get_active_window),
    export!("GetAsyncKeyState", 1, get_async_key_state),
    export!("GetCapture", 0, get_capture),
    export!("GetClassInfoA", 3, get_class_info_a),
    export!("GetClientRect", 2, get_client_rect),
    export!("GetCursorPos", 1, get_cursor_pos),
    export!("GetDC", 1, crate::dll::gdi32::get_dc),
    export!("GetDesktopWindow", 0, get_desktop_window),
    export!("GetFocus", 0, get_focus),
    export!("GetForegroundWindow", 0, get_foreground_window),
    export!("GetKeyNameTextA", 3, get_key_name_text_a),
    export!("GetKeyState", 1, get_key_state),
    export!("GetKeyboardState", 1, get_keyboard_state),
    export!("GetMessageA", 4, get_message_a),
    export!("GetMessagePos", 0, get_message_pos),
    export!("GetMessageTime", 0, get_message_time),
    export!("GetParent", 1, get_parent),
    export!("GetPropA", 2, get_prop_a),
    export!("GetQueueStatus", 1, get_queue_status),
    export!("GetSysColor", 1, get_sys_color),
    export!("GetSystemMetrics", 1, get_system_metrics),
    export!("GetUpdateRect", 3, get_update_rect),
    export!("GetWindowDC", 1, crate::dll::gdi32::get_window_dc),
    export!("GetWindowLongA", 2, get_window_long_a),
    export!("GetWindowRect", 2, get_window_rect),
    export!("GetWindowTextA", 3, get_window_text_a),
    export!("InflateRect", 3, inflate_rect),
    export!("IntersectRect", 3, intersect_rect),
    export!("InvalidateRect", 3, invalidate_rect),
    export!("IsIconic", 1, is_iconic),
    export!("IsRectEmpty", 1, is_rect_empty),
    export!("IsWindow", 1, is_window),
    export!("IsWindowEnabled", 1, is_window_enabled),
    export!("IsWindowVisible", 1, is_window_visible),
    export!("IsZoomed", 1, is_zoomed),
    export!("KillTimer", 2, kill_timer),
    export!("LoadCursorA", 2, load_icon_cursor),
    export!("LoadIconA", 2, load_icon_cursor),
    export!("LoadStringA", 4, load_string_a),
    export!("MapVirtualKeyA", 2, map_virtual_key_a),
    export!("MapWindowPoints", 4, map_window_points),
    export!("MessageBoxA", 4, message_box_a),
    export!("MoveWindow", 6, move_window),
    export!("MsgWaitForMultipleObjects", 5, msg_wait_for_multiple_objects),
    export!("OffsetRect", 3, offset_rect),
    export!("PeekMessageA", 5, peek_message_a),
    export!("PostMessageA", 4, post_message_a),
    export!("PostQuitMessage", 1, post_quit_message),
    export!("PostThreadMessageA", 4, post_thread_message_a),
    export!("PtInRect", 3, pt_in_rect),
    export!("RegisterClassA", 1, register_class_a),
    export!("RegisterClassExA", 1, register_class_ex_a),
    export!("ReleaseCapture", 0, release_capture),
    export!("ReleaseDC", 2, crate::dll::gdi32::release_dc),
    export!("RemovePropA", 2, remove_prop_a),
    export!("ScreenToClient", 2, screen_to_client),
    export!("SendMessageA", 4, send_message_a),
    export!("SetActiveWindow", 1, set_active_window),
    export!("SetCapture", 1, set_capture),
    export!("SetCursor", 1, set_cursor),
    export!("SetCursorPos", 2, set_cursor_pos),
    export!("SetFocus", 1, set_focus),
    export!("SetForegroundWindow", 1, set_foreground_window),
    export!("SetPropA", 3, set_prop_a),
    export!("SetRect", 5, set_rect),
    export!("SetRectEmpty", 1, set_rect_empty),
    export!("SetTimer", 4, set_timer),
    export!("SetWindowLongA", 3, set_window_long_a),
    export!("SetWindowPos", 7, set_window_pos),
    export!("SetWindowTextA", 2, set_window_text_a),
    export!("ShowCursor", 1, show_cursor),
    export!("ShowWindow", 2, show_window),
    export!("SystemParametersInfoA", 4, system_parameters_info_a),
    export!("TranslateMessage", 1, translate_message),
    export!("UnionRect", 3, union_rect),
    export!("UnregisterClassA", 2, unregister_class_a),
    export!("UpdateWindow", 1, update_window),
    export!("ValidateRect", 2, validate_rect),
    export!("WaitMessage", 0, wait_message),
    export!("WindowFromPoint", 2, window_from_point),
    cdecl!("wsprintfA", wsprintf_a),
    export!("wvsprintfA", 3, wvsprintf_a),
];
