//! winmm: timeGetTime, clock resolution, multimedia timers, joystick, mixer.
//!
//! Multimedia timer callbacks run on a runtime-created, time-critical guest thread (as
//! on Windows), scheduled from the virtual clock.

use crate::api::*;
use crate::export;
use crate::objects::KObj;
use crate::session::Session;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MmTimer {
    pub id: u32,
    pub period_ns: u64,
    pub due: u64,
    pub callback: u32,
    pub user: u32,
    pub periodic: bool,
    pub flags: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MmState {
    /// Current clock-interrupt period (timeBeginPeriod), in ns.
    pub period_ns: u64,
    pub period_requests: Vec<u32>,
    pub timers: Vec<MmTimer>,
    pub next_timer: u32,
    pub timer_thread: Option<u32>,
    pub mci: std::collections::BTreeMap<u32, MciDevice>,
    pub next_mci: u32,
    /// Open Audio Compression Manager streams.
    #[serde(default)]
    pub acm: std::collections::BTreeMap<u32, crate::dll::msacm32::AcmStream>,
    #[serde(default)]
    pub next_acm: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MciDevice {
    pub kind: String,
    pub element: String,
    pub alias: String,
}

impl Default for MmState {
    fn default() -> Self {
        MmState {
            period_ns: 10_000_000,
            period_requests: Vec::new(),
            timers: Vec::new(),
            next_timer: 1,
            timer_thread: None,
            mci: Default::default(),
            next_mci: 1,
            acm: Default::default(),
            next_acm: 1,
        }
    }
}

impl MmState {
    pub fn timer_resolution_ns(&self) -> u64 {
        self.period_ns
    }
    fn recompute_period(&mut self) {
        let min = self.period_requests.iter().copied().min().unwrap_or(10);
        self.period_ns = min.clamp(1, 10) as u64 * 1_000_000;
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MmCont {
    TimerLoop,
}

pub fn resume(s: &mut Session, c: MmCont, _v: Resume) -> ApiResult {
    match c {
        MmCont::TimerLoop => timer_thread_step(s),
    }
}

fn time_get_time(s: &mut Session) -> ApiResult {
    let res = s.winmm.period_ns;
    let t = s.uptime_ns() / res * res;
    Ok(ApiRet::Ret((t / 1_000_000) as u32))
}

fn time_begin_period(s: &mut Session) -> ApiResult {
    let [p] = s.args::<1>()?;
    if !(1..=1_000_000).contains(&p) {
        return Ok(ApiRet::Ret(97)); // TIMERR_NOCANDO
    }
    s.winmm.period_requests.push(p);
    s.winmm.recompute_period();
    Ok(ApiRet::Ret(0))
}

fn time_end_period(s: &mut Session) -> ApiResult {
    let [p] = s.args::<1>()?;
    match s.winmm.period_requests.iter().position(|&x| x == p) {
        Some(i) => {
            s.winmm.period_requests.remove(i);
            s.winmm.recompute_period();
            Ok(ApiRet::Ret(0))
        }
        None => Ok(ApiRet::Ret(97)),
    }
}

fn time_get_dev_caps(s: &mut Session) -> ApiResult {
    let [p, n] = s.args::<2>()?;
    if n < 8 {
        return Ok(ApiRet::Ret(97));
    }
    s.mem.write_u32(p, 1)?;
    s.mem.write_u32(p + 4, 1_000_000)?;
    Ok(ApiRet::Ret(0))
}

fn time_get_system_time(s: &mut Session) -> ApiResult {
    let [p, n] = s.args::<2>()?;
    if n < 12 {
        return Ok(ApiRet::Ret(11));
    }
    let res = s.winmm.period_ns;
    let t = (s.uptime_ns() / res * res / 1_000_000) as u32;
    s.mem.write_u32(p, 1)?; // TIME_MS
    s.mem.write_u32(p + 4, t)?;
    Ok(ApiRet::Ret(0))
}

fn time_set_event(s: &mut Session) -> ApiResult {
    let [delay, _res, cb, user, flags] = s.args::<5>()?;
    if delay == 0 || cb == 0 {
        return Ok(ApiRet::Ret(0));
    }
    let id = s.winmm.next_timer;
    s.winmm.next_timer += 1;
    let now = s.clock.now_ns();
    let period = delay as u64 * 1_000_000;
    s.winmm.timers.push(MmTimer {
        id,
        period_ns: period,
        due: now + period,
        callback: cb,
        user,
        periodic: flags & 1 != 0,
        flags,
    });
    if flags & 0x30 == 0 && s.winmm.timer_thread.is_none() {
        let start = s.thunks.find("DRYDOCK.DLL", "MmTimerThread").unwrap();
        let tid = crate::loader::create_thread(s, start, 0, 0x10000, false, true).map_err(ApiError::Fatal)?;
        s.sched.threads.get_mut(&tid).unwrap().priority = 15; // THREAD_PRIORITY_TIME_CRITICAL
        s.winmm.timer_thread = Some(tid);
    }
    Ok(ApiRet::Ret(id))
}

fn time_kill_event(s: &mut Session) -> ApiResult {
    let [id] = s.args::<1>()?;
    let n = s.winmm.timers.len();
    s.winmm.timers.retain(|t| t.id != id);
    Ok(ApiRet::Ret(if n != s.winmm.timers.len() { 0 } else { 97 }))
}

/// Body of the multimedia timer thread (entered via its start routine).
pub fn mm_timer_thread(s: &mut Session) -> ApiResult {
    timer_thread_step(s)
}

fn timer_thread_step(s: &mut Session) -> ApiResult {
    let now = s.clock.now_ns();
    let due = s
        .winmm
        .timers
        .iter()
        .enumerate()
        .filter(|(_, t)| t.flags & 0x30 == 0 && t.due <= now)
        .min_by_key(|(_, t)| (t.due, t.id))
        .map(|(i, _)| i);
    if let Some(i) = due {
        let t = s.winmm.timers[i].clone();
        if t.periodic {
            let tm = &mut s.winmm.timers[i];
            tm.due += tm.period_ns;
            if tm.due <= now {
                tm.due = now + tm.period_ns;
            }
        } else {
            s.winmm.timers.remove(i);
        }
        return Ok(ApiRet::call(
            t.callback,
            vec![t.id.into(), 0u32.into(), t.user.into(), 0u32.into(), 0u32.into()],
            Cont::Winmm(MmCont::TimerLoop),
        ));
    }
    let next = s
        .winmm
        .timers
        .iter()
        .filter(|t| t.flags & 0x30 == 0)
        .map(|t| t.due)
        .min();
    Ok(ApiRet::Block(Block {
        wait: WaitKind::MmTimer,
        deadline: next,
        cont: Cont::Winmm(MmCont::TimerLoop),
    }))
}

pub fn timer_due(s: &Session) -> bool {
    let now = s.clock.now_ns();
    s.winmm.timers.iter().any(|t| t.flags & 0x30 == 0 && t.due <= now)
}

pub fn next_event(s: &Session) -> Option<u64> {
    s.winmm.timers.iter().map(|t| t.due).min()
}

/// Event-signalling timers fire without a guest thread.
pub fn service(s: &mut Session) {
    let now = s.clock.now_ns();
    let mut i = 0;
    while i < s.winmm.timers.len() {
        let t = s.winmm.timers[i].clone();
        if t.flags & 0x30 != 0 && t.due <= now {
            if let Some(KObj::Event { signaled, .. }) = s.objects.by_handle_mut(t.callback) {
                *signaled = true;
            }
            if t.periodic {
                let tm = &mut s.winmm.timers[i];
                tm.due += tm.period_ns;
                if tm.due <= now {
                    tm.due = now + tm.period_ns;
                }
            } else {
                s.winmm.timers.remove(i);
                continue;
            }
        }
        i += 1;
    }
}

// ---------------------------------------------------------------- joystick

const JOYERR_PARMS: u32 = 165;
const JOYERR_UNPLUGGED: u32 = 167;

fn joy_get_num_devs(_s: &mut Session) -> ApiResult {
    // The joystick driver supports 16 devices; which are plugged in is reported per id.
    Ok(ApiRet::Ret(16))
}

fn joy_get_pos_ex(s: &mut Session) -> ApiResult {
    let [id, p] = s.args::<2>()?;
    if id >= 16 {
        return Ok(ApiRet::Ret(JOYERR_PARMS));
    }
    if id != 0 || !s.user.input.joy_present {
        return Ok(ApiRet::Ret(JOYERR_UNPLUGGED));
    }
    let a = s.user.input.joy_axes;
    let vals = [
        a[0] as u32,
        a[1] as u32,
        a[2] as u32,
        a[3] as u32,
        32768,
        32768,
        s.user.input.joy_buttons,
    ];
    for (i, v) in vals.iter().enumerate() {
        s.mem.write_u32(p + 8 + 4 * i as u32, *v)?;
    }
    s.mem.write_u32(p + 36, s.user.input.joy_buttons.count_ones())?;
    let pov = s.user.input.joy_pov;
    s.mem
        .write_u32(p + 40, if pov == crate::input::POV_CENTERED { 0xffff } else { pov })?;
    Ok(ApiRet::Ret(0))
}

fn joy_get_pos(s: &mut Session) -> ApiResult {
    let [id, p] = s.args::<2>()?;
    if id >= 16 {
        return Ok(ApiRet::Ret(JOYERR_PARMS));
    }
    if id != 0 || !s.user.input.joy_present {
        return Ok(ApiRet::Ret(JOYERR_UNPLUGGED));
    }
    let a = s.user.input.joy_axes;
    s.mem.write_u32(p, a[0] as u32)?;
    s.mem.write_u32(p + 4, a[1] as u32)?;
    s.mem.write_u32(p + 8, a[2] as u32)?;
    s.mem.write_u32(p + 12, s.user.input.joy_buttons & 0xf)?;
    Ok(ApiRet::Ret(0))
}

fn joy_get_dev_caps_a(s: &mut Session) -> ApiResult {
    let [id, p, n] = s.args::<3>()?;
    if id >= 16 && id != 0xffff_ffff {
        return Ok(ApiRet::Ret(JOYERR_PARMS));
    }
    if !s.user.input.joy_present || id != 0 {
        return Ok(ApiRet::Ret(JOYERR_UNPLUGGED));
    }
    let mut b = vec![0u8; (n as usize).min(404)];
    if b.len() >= 52 {
        // The SideWinder Force Feedback 2: X, Y, throttle (Z), twist (R), 8 buttons, hat.
        b[0..2].copy_from_slice(&0x045eu16.to_le_bytes());
        b[2..4].copy_from_slice(&0x001bu16.to_le_bytes());
        let name = b"SideWinder Force Feedback 2";
        b[4..4 + name.len()].copy_from_slice(name);
        // X/Y/Z ranges, buttons, polling periods, R/U/V ranges, wCaps (HASZ, HASR,
        // HASPOV, POVCTS), most and present axes, most buttons.
        let caps = [
            0u32, 65535, 0, 65535, 0, 65535, 8, 10, 1000, 0, 65535, 0, 0, 0, 0, 0x53, 6, 4, 32,
        ];
        for (i, v) in caps.iter().enumerate() {
            let o = 36 + i * 4;
            if o + 4 <= b.len() {
                b[o..o + 4].copy_from_slice(&v.to_le_bytes());
            }
        }
    }
    s.mem.write(p, &b)?;
    Ok(ApiRet::Ret(0))
}

// ---------------------------------------------------------------- MCI

const MCI_OPEN: u32 = 0x803;
const MCI_CLOSE: u32 = 0x804;
const MCI_PLAY: u32 = 0x806;
const MCI_STOP: u32 = 0x808;
const MCI_PAUSE: u32 = 0x809;
const MCIERR_INVALID_DEVICE_ID: u32 = 257;
const MCIERR_UNRECOGNIZED_COMMAND: u32 = 261;
const MCIERR_INVALID_DEVICE_NAME: u32 = 263;
const MCIERR_DEVICE_NOT_READY: u32 = 276;
const MCIERR_DEVICE_NOT_INSTALLED: u32 = 306;

fn mci_error_text(e: u32) -> &'static str {
    match e {
        0 => "The specified command was carried out.",
        MCIERR_INVALID_DEVICE_ID => "Invalid MCI device ID. Use the ID returned when opening the MCI device.",
        MCIERR_UNRECOGNIZED_COMMAND => "The driver cannot recognize the specified command.",
        MCIERR_INVALID_DEVICE_NAME => "The specified device is not open or is not recognized by MCI.",
        MCIERR_DEVICE_NOT_READY => "The device is not ready.",
        MCIERR_DEVICE_NOT_INSTALLED => "The specified device is not installed on the system.",
        _ => "Unknown MCI error.",
    }
}

fn mci_open(s: &mut Session, flags: u32, parms: u32) -> Result<u32, ApiError> {
    let ty_p = s.mem.read_u32(parms + 8)?;
    let el_p = s.mem.read_u32(parms + 12)?;
    let al_p = s.mem.read_u32(parms + 16)?;
    let kind = if flags & 0x1000 != 0 {
        // MCI_OPEN_TYPE_ID: LOWORD = device type constant.
        match ty_p & 0xffff {
            516 => "cdaudio".to_string(),
            519 => "avivideo".to_string(),
            522 => "waveaudio".to_string(),
            523 => "sequencer".to_string(),
            other => format!("#{other}"),
        }
    } else if flags & 0x2000 != 0 {
        s.read_str(ty_p)?.to_lowercase()
    } else if flags & 0x200 != 0 {
        // Type from the element's extension.
        let e = s.read_str(el_p)?.to_lowercase();
        match e.rsplit('.').next() {
            Some("avi") => "avivideo".into(),
            Some("wav") => "waveaudio".into(),
            Some("mid") | Some("rmi") => "sequencer".into(),
            _ => String::new(),
        }
    } else {
        String::new()
    };
    let element = if flags & 0x200 != 0 && el_p != 0 {
        s.read_str(el_p)?
    } else {
        String::new()
    };
    let alias = if flags & 0x400 != 0 && al_p != 0 {
        s.read_str(al_p)?
    } else {
        String::new()
    };
    match kind.as_str() {
        "cdaudio" => {
            let has_cd = s.vfs.drives.values().any(|d| d.kind == crate::vfs::DriveKind::CdRom);
            if !has_cd {
                s.event(
                    crate::diag::Severity::Warning,
                    "mci",
                    "CD audio requested but no CD-ROM drive is mapped in this session".into(),
                );
                return Ok(MCIERR_DEVICE_NOT_READY);
            }
            s.missing("mci", "CD audio playback from a mapped CD-ROM drive".into());
            Ok(MCIERR_DEVICE_NOT_READY)
        }
        "" => Ok(MCIERR_INVALID_DEVICE_NAME),
        other => {
            s.missing("mci", format!("MCI device type {other:?} (element {element:?})"));
            let _ = alias;
            Ok(MCIERR_DEVICE_NOT_INSTALLED)
        }
    }
}

fn mci_send_command_a(s: &mut Session) -> ApiResult {
    let [dev, msg, flags, parms] = s.args::<4>()?;
    let r = match msg {
        MCI_OPEN => mci_open(s, flags, parms)?,
        MCI_CLOSE | MCI_STOP | MCI_PAUSE | MCI_PLAY if dev == 0xffff_ffff || dev == 0xffff => {
            // Applies to every open device (none are open when this succeeds trivially).
            if msg == MCI_CLOSE {
                s.winmm.mci.clear();
            }
            if !s.winmm.mci.is_empty() && msg != MCI_CLOSE {
                s.missing("mci", format!("command {msg:#x} on all devices with open devices"));
            }
            0
        }
        _ => {
            if !s.winmm.mci.contains_key(&dev) {
                MCIERR_INVALID_DEVICE_ID
            } else {
                s.missing("mci", format!("command {msg:#x} for device {dev}"));
                MCIERR_UNRECOGNIZED_COMMAND
            }
        }
    };
    Ok(ApiRet::Ret(r))
}

fn mci_get_error_string_a(s: &mut Session) -> ApiResult {
    let [e, buf, n] = s.args::<3>()?;
    s.write_str_trunc(buf, n, mci_error_text(e))?;
    Ok(ApiRet::Ret(1))
}

pub static WINMM_EXPORTS: &[ExportDef] = &[
    export!("mciGetErrorStringA", 3, mci_get_error_string_a),
    export!("mciSendCommandA", 4, mci_send_command_a),
    export!("joyGetDevCapsA", 3, joy_get_dev_caps_a),
    export!("joyGetNumDevs", 0, joy_get_num_devs),
    export!("joyGetPos", 2, joy_get_pos),
    export!("joyGetPosEx", 2, joy_get_pos_ex),
    export!("timeBeginPeriod", 1, time_begin_period),
    export!("timeEndPeriod", 1, time_end_period),
    export!("timeGetDevCaps", 2, time_get_dev_caps),
    export!("timeGetSystemTime", 2, time_get_system_time),
    export!("timeGetTime", 0, time_get_time),
    export!("timeKillEvent", 1, time_kill_event),
    export!("timeSetEvent", 5, time_set_event),
];
