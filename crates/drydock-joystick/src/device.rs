//! Host game controllers through hidapi: input, and PID force feedback when the
//! controller has it.

use crate::ff::{self, Command, Effect, INFINITE};
use crate::hid::{Descriptor, Report, ReportKind};
use crate::input::{JoyState, Layout};
use crate::pid::{self, Pid, Transport};
use hidapi::{DeviceInfo, HidApi, HidDevice};
use std::collections::BTreeMap;
use std::ffi::CString;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, TryRecvError, channel};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// A game controller the host offers.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub name: String,
    pub vid: u16,
    pub pid: u16,
    pub path: CString,
}

impl Candidate {
    /// Matches "045e:001b" or part of the product name (case-insensitive).
    pub fn matches(&self, filter: &str) -> bool {
        let f = filter.to_lowercase();
        f == format!("{:04x}:{:04x}", self.vid, self.pid) || self.name.to_lowercase().contains(&f)
    }
}

impl std::fmt::Display for Candidate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({:04x}:{:04x})", self.name, self.vid, self.pid)
    }
}

pub fn api() -> Result<HidApi, String> {
    let api = HidApi::new().map_err(|e| e.to_string())?;
    // Share the controller with the rest of the system rather than seizing it.
    #[cfg(target_os = "macos")]
    api.set_open_exclusive(false);
    Ok(api)
}

/// Joysticks, gamepads and multi-axis controllers (Generic Desktop 4, 5 and 8).
pub fn candidates(api: &HidApi) -> Vec<Candidate> {
    let mut out: Vec<Candidate> = Vec::new();
    for d in api.device_list() {
        if !is_controller(d) || out.iter().any(|c| c.path.as_c_str() == d.path()) {
            continue;
        }
        out.push(Candidate {
            name: d.product_string().unwrap_or("game controller").trim().to_string(),
            vid: d.vendor_id(),
            pid: d.product_id(),
            path: d.path().to_owned(),
        });
    }
    out
}

fn is_controller(d: &DeviceInfo) -> bool {
    d.usage_page() == 1 && matches!(d.usage(), 4 | 5 | 8)
}

struct HidTransport {
    dev: Rc<HidDevice>,
}

impl Transport for HidTransport {
    fn write(&mut self, r: &[u8]) -> Result<(), String> {
        self.dev.write(r).map(|_| ()).map_err(|e| e.to_string())
    }
    fn set_feature(&mut self, r: &[u8]) -> Result<(), String> {
        self.dev.send_feature_report(r).map_err(|e| e.to_string())
    }
    fn get_feature(&mut self, buf: &mut [u8]) -> Result<usize, String> {
        self.dev.get_feature_report(buf).map_err(|e| e.to_string())
    }
}

/// Input reports read so far, by kind.
#[derive(Debug, Clone, Copy, Default)]
pub struct ReadStats {
    /// Reports of the controls (axes, buttons, hat).
    pub controls: u32,
    /// PID state reports, and those saying an effect is playing.
    pub effect_states: u32,
    pub playing: u32,
}

/// An open controller.
pub struct Controller {
    pub info: Candidate,
    dev: Rc<HidDevice>,
    pub descriptor: Vec<u8>,
    pub desc: Descriptor,
    pub layout: Layout,
    ff: Option<Pid<HidTransport>>,
    /// Why force feedback is unavailable, when the descriptor offers it.
    pub ff_error: Option<String>,
    pub stats: ReadStats,
    buf: Vec<u8>,
}

impl Controller {
    /// Open a controller; with `force_feedback` (a strength in percent) its force
    /// feedback is taken over too.
    pub fn open(api: &HidApi, c: &Candidate, force_feedback: Option<u32>) -> Result<Controller, String> {
        let dev = Rc::new(api.open_path(&c.path).map_err(|e| format!("{c}: {e}"))?);
        let mut raw = vec![0u8; 4096];
        let n = dev
            .get_report_descriptor(&mut raw)
            .map_err(|e| format!("{c}: report descriptor: {e}"))?;
        raw.truncate(n);
        let desc = Descriptor::parse(&raw).map_err(|e| format!("{c}: report descriptor: {e}"))?;
        let layout = Layout::new(&desc);
        let (mut ff, mut ff_error) = (None, None);
        if let Some(strength) = force_feedback
            && pid::supports(&desc)
        {
            match Pid::new(desc.clone(), HidTransport { dev: dev.clone() }, strength) {
                Ok(p) => ff = Some(p),
                Err(e) => ff_error = Some(e),
            }
        }
        let len = desc
            .report_ids(ReportKind::Input)
            .iter()
            .map(|&id| desc.report_len(ReportKind::Input, id))
            .max()
            .unwrap_or(64);
        Ok(Controller {
            info: c.clone(),
            dev,
            descriptor: raw,
            desc,
            layout,
            ff,
            ff_error,
            stats: ReadStats::default(),
            buf: vec![0u8; len.max(64)],
        })
    }

    pub fn has_force_feedback(&self) -> bool {
        self.ff.is_some()
    }

    /// Facts the force-feedback device reported (effect memory, simultaneous effects).
    pub fn ff_info(&self) -> Vec<(String, i64)> {
        self.ff.as_ref().map(|p| p.info.clone()).unwrap_or_default()
    }

    /// Force-feedback problems that left it working.
    pub fn ff_notes(&self) -> Vec<String> {
        self.ff.as_ref().map(|p| p.notes.clone()).unwrap_or_default()
    }

    /// Tally a report for [`Controller::stats`].
    fn count(&mut self, r: &Report) {
        let playing = crate::hid::usage(crate::hid::PAGE_PID, 0x94);
        match self.desc.find(ReportKind::Input, 0, playing) {
            Some((f, i)) if f.report_id == r.id() => {
                self.stats.effect_states += 1;
                if r.get_raw(f, i) != 0 {
                    self.stats.playing += 1;
                }
            }
            _ => self.stats.controls += 1,
        }
    }

    /// The controller's current state, asked for rather than waited on (controllers
    /// report only changes, so a resting throttle would otherwise stay unknown).
    pub fn current(&mut self, st: &mut JoyState) {
        for id in self.desc.report_ids(ReportKind::Input) {
            let mut buf = vec![0u8; self.desc.report_len(ReportKind::Input, id)];
            buf[0] = id;
            // The SideWinder Force Feedback 2 stalls this request.
            if let Ok(n) = self.dev.get_input_report(&mut buf)
                && n > 0
            {
                buf.truncate(n);
                self.layout.decode(&Report::from_bytes(self.desc.numbered, buf), st);
            }
        }
    }

    /// Read every pending input report (waiting up to `timeout_ms` for the first) into
    /// `st`. Returns whether a report arrived.
    pub fn read(&mut self, st: &mut JoyState, timeout_ms: i32) -> Result<bool, String> {
        let mut got = false;
        let mut wait = timeout_ms;
        loop {
            let n = self.dev.read_timeout(&mut self.buf, wait).map_err(|e| e.to_string())?;
            if n == 0 {
                return Ok(got);
            }
            let r = Report::from_bytes(self.desc.numbered, self.buf[..n].to_vec());
            self.layout.decode(&r, st);
            self.count(&r);
            got = true;
            wait = 0;
        }
    }

    pub fn force(&mut self, c: Command) -> Result<(), String> {
        match &mut self.ff {
            Some(p) => p.command(c),
            None => Ok(()),
        }
    }
}

impl Drop for Controller {
    fn drop(&mut self) {
        if let Some(p) = &mut self.ff {
            p.release();
        }
    }
}

/// What the guest has asked of the force-feedback device, for replaying it on a
/// controller that is connected later (or again).
#[derive(Default)]
struct Shadow {
    effects: BTreeMap<u32, Effect>,
    /// Effects started to play forever.
    looping: BTreeMap<u32, bool>,
    gain: Option<u32>,
    autocenter: Option<bool>,
    actuators: Option<bool>,
}

impl Shadow {
    fn apply(&mut self, c: &Command) {
        match c {
            Command::Download { id, effect } => {
                self.effects.insert(*id, effect.clone());
            }
            Command::Start { id, iterations, solo } => {
                if *solo {
                    self.looping.clear();
                }
                let forever =
                    *iterations == INFINITE || self.effects.get(id).is_some_and(|e| e.duration_us == INFINITE);
                self.looping.insert(*id, forever);
            }
            Command::Stop { id } => {
                self.looping.remove(id);
            }
            Command::Unload { id } => {
                self.effects.remove(id);
                self.looping.remove(id);
            }
            Command::Reset => {
                self.effects.clear();
                self.looping.clear();
            }
            Command::StopAll => self.looping.clear(),
            Command::Pause | Command::Continue => {}
            Command::Actuators(on) => self.actuators = Some(*on),
            Command::Gain(g) => self.gain = Some(*g),
            Command::Autocenter(on) => self.autocenter = Some(*on),
        }
    }

    fn replay(&self) -> Vec<Command> {
        let mut v = Vec::new();
        v.extend(self.actuators.map(Command::Actuators));
        v.extend(self.gain.map(Command::Gain));
        v.extend(self.autocenter.map(Command::Autocenter));
        for (id, e) in &self.effects {
            v.push(Command::Download {
                id: *id,
                effect: e.clone(),
            });
        }
        for (id, forever) in &self.looping {
            if *forever {
                v.push(Command::Start {
                    id: *id,
                    iterations: INFINITE,
                    solo: false,
                });
            }
        }
        v
    }
}

pub struct Options {
    /// Which controller to use ("045e:001b" or part of its name); the first otherwise.
    pub filter: Option<String>,
    /// Force-feedback strength in percent; `None` leaves force feedback alone.
    pub force_feedback: Option<u32>,
}

/// Messages from the controller thread.
pub enum Event {
    State(JoyState),
    /// Connection changes and errors, for the user.
    Status(String),
}

/// A controller thread. Dropping it stops the thread and resets the controller, so no
/// effect keeps playing after the program is gone.
pub struct Running {
    /// Force-feedback commands for the controller.
    pub commands: Sender<Command>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for Running {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Set to stop every controller thread (see [`shutdown_all`]).
static SHUTDOWN: AtomicBool = AtomicBool::new(false);
/// Controller threads still running.
static RUNNING: AtomicUsize = AtomicUsize::new(0);

/// Stop every controller thread, waiting up to `timeout` for them to reset their
/// controllers. For signal handlers that exit the process right after.
pub fn shutdown_all(timeout: Duration) {
    SHUTDOWN.store(true, Ordering::SeqCst);
    let end = Instant::now() + timeout;
    while RUNNING.load(Ordering::SeqCst) > 0 && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(5));
    }
}

struct Counted;

impl Counted {
    fn new() -> Counted {
        RUNNING.fetch_add(1, Ordering::SeqCst);
        Counted
    }
}

impl Drop for Counted {
    fn drop(&mut self) {
        RUNNING.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Run a controller on its own thread. Input arrives through `events`; the thread also
/// ends when `events` stops accepting or every command sender is gone.
pub fn spawn(opts: Options, events: Box<dyn Fn(Event) -> bool + Send>) -> Running {
    let (tx, rx) = channel::<Command>();
    let stop = Arc::new(AtomicBool::new(false));
    let flag = stop.clone();
    let thread = std::thread::Builder::new()
        .name("drydock-joystick".into())
        .spawn(move || run(opts, rx, events, flag))
        .ok();
    Running {
        commands: tx,
        stop,
        thread,
    }
}

fn run(opts: Options, rx: Receiver<Command>, events: Box<dyn Fn(Event) -> bool + Send>, stop: Arc<AtomicBool>) {
    // Counts until after the controller (declared later) has been dropped and reset.
    let _counted = Counted::new();
    let mut api = match api() {
        Ok(a) => a,
        Err(e) => {
            events(Event::Status(format!("joystick: HID unavailable: {e}")));
            return;
        }
    };
    let mut ctrl: Option<Controller> = None;
    let mut shadow = Shadow::default();
    let mut st = JoyState::default();
    let mut last_scan: Option<Instant> = None;
    let mut waiting_reported = false;
    let mut read_errors = 0;
    // Report a force-feedback failure once until commands work again.
    let mut ff_failing = false;
    let mut ff_result = |r: Result<(), String>, events: &dyn Fn(Event) -> bool| match r {
        Ok(()) => ff_failing = false,
        Err(e) => {
            if !ff_failing {
                events(Event::Status(format!("joystick: force feedback: {e}")));
            }
            ff_failing = true;
        }
    };
    while !stop.load(Ordering::Relaxed) && !SHUTDOWN.load(Ordering::Relaxed) {
        if ctrl.is_none() && last_scan.is_none_or(|t| t.elapsed() >= Duration::from_secs(1)) {
            last_scan = Some(Instant::now());
            let _ = api.refresh_devices();
            let found = candidates(&api)
                .into_iter()
                .find(|c| opts.filter.as_deref().is_none_or(|f| c.matches(f)));
            match found.map(|c| Controller::open(&api, &c, opts.force_feedback)) {
                Some(Ok(mut c)) => {
                    let ff = match (&c.ff, &c.ff_error) {
                        (Some(_), _) => "force feedback".to_string(),
                        (None, Some(e)) => format!("force feedback unavailable: {e}"),
                        (None, None) => "no force feedback".to_string(),
                    };
                    let msg = format!(
                        "joystick: {} — {} axes, {} buttons{}, {ff}",
                        c.info,
                        c.layout.axis_count(),
                        c.layout.button_count(),
                        if c.layout.has_hat() { ", hat" } else { "" },
                    );
                    for note in c.ff_notes() {
                        events(Event::Status(format!("joystick: {note}")));
                    }
                    for cmd in shadow.replay() {
                        ff_result(c.force(cmd), &*events);
                    }
                    st = JoyState::default();
                    c.current(&mut st);
                    if !events(Event::Status(msg)) || !events(Event::State(st)) {
                        return;
                    }
                    ctrl = Some(c);
                    waiting_reported = false;
                }
                Some(Err(e)) => {
                    if !waiting_reported {
                        events(Event::Status(format!("joystick: {e}")));
                        waiting_reported = true;
                    }
                }
                None => {
                    if !waiting_reported {
                        let what = opts.filter.as_deref().unwrap_or("a joystick");
                        events(Event::Status(format!("joystick: waiting for {what} to be connected")));
                        waiting_reported = true;
                    }
                }
            }
        }
        // Input.
        match &mut ctrl {
            Some(c) => match c.read(&mut st, 4) {
                Ok(got) => {
                    read_errors = 0;
                    if got && !events(Event::State(st)) {
                        return;
                    }
                }
                // A read can fail once (an interrupted system call); an unplugged
                // controller fails every time and leaves the device list.
                Err(_)
                    if read_errors < 25 && {
                        let _ = api.refresh_devices();
                        candidates(&api).iter().any(|x| x.path == c.info.path)
                    } =>
                {
                    read_errors += 1;
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(e) => {
                    read_errors = 0;
                    events(Event::Status(format!("joystick: {} disconnected ({e})", c.info)));
                    ctrl = None;
                    // The guest sees the stick let go.
                    if !events(Event::State(JoyState::default())) {
                        return;
                    }
                }
            },
            None => match rx.recv_timeout(Duration::from_millis(50)) {
                Ok(cmd) => {
                    shadow.apply(&cmd);
                    continue;
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            },
        }
        // Force feedback.
        let mut cmds = Vec::new();
        loop {
            match rx.try_recv() {
                Ok(c) => cmds.push(c),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return,
            }
        }
        for c in ff::coalesce(cmds) {
            shadow.apply(&c);
            if let Some(ctl) = &mut ctrl {
                ff_result(ctl.force(c), &*events);
            }
        }
    }
}
