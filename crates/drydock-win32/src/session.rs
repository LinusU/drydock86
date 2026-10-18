//! A guest session: one emulated Win32 process with its devices.
//!
//! The session owns all guest-visible state explicitly. `run` advances execution until
//! a virtual-time target, an instruction budget or an observable event, so the same
//! runtime serves interactive play (paced by a driver), unthrottled headless runs and
//! manual stepping.

use crate::api::*;
use crate::diag::{ApiTraceEntry, DiagEvent, Diagnostics, Halt, Severity};
use crate::heap::Heaps;
use crate::loader::Modules;
use crate::objects::{KObj, Objects};
use crate::reg::Registry;
use crate::sched::{QUANTUM_INSNS, Sched, ThreadState};
use crate::thunks::Thunks;
use crate::vfs::{GuestPath, Vfs};
use drydock_core::Reg;
use drydock_core::clock::{CpuProfile, VirtualClock};
use drydock_core::cpu::{Cpu, FaultKind, Stop};
use drydock_core::mem::{AddressSpace, MemError};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Virtual cost charged per API call (kernel transition), in ns.
pub const API_COST_NS: u64 = 200;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowsProfile {
    pub major: u32,
    pub minor: u32,
    pub build: u32,
    /// VER_PLATFORM_WIN32_NT = 2, VER_PLATFORM_WIN32_WINDOWS = 1
    pub platform: u32,
    pub csd: String,
    pub sp_major: u16,
    pub ansi_codepage: u32,
    pub oem_codepage: u32,
    pub lcid: u32,
    pub computer_name: String,
    pub user_name: String,
    pub windows_dir: String,
    pub system_dir: String,
    pub temp_dir: String,
    pub screen_width: u32,
    pub screen_height: u32,
    pub screen_bpp: u32,
    pub refresh_hz: u32,
    pub total_phys_mem: u32,
    /// Calendar time at virtual time 0, as a FILETIME (100 ns since 1601, UTC).
    pub epoch_filetime: u64,
    pub timezone_bias_min: i32,
}

impl Default for WindowsProfile {
    fn default() -> Self {
        WindowsProfile {
            major: 5,
            minor: 0,
            build: 2195,
            platform: 2,
            csd: "Service Pack 4".into(),
            sp_major: 4,
            ansi_codepage: 1252,
            oem_codepage: 437,
            lcid: 0x0409,
            computer_name: "DRYDOCK".into(),
            user_name: "Player".into(),
            windows_dir: "C:\\WINNT".into(),
            system_dir: "C:\\WINNT\\system32".into(),
            temp_dir: "C:\\TEMP".into(),
            screen_width: 1024,
            screen_height: 768,
            screen_bpp: 32,
            refresh_hz: 60,
            total_phys_mem: 256 << 20,
            // 2001-06-01 12:00:00 UTC
            epoch_filetime: 126_359_856_000_000_000 + 12 * 36_000_000_000,
            timezone_bias_min: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionConfig {
    /// Guest path of the executable (e.g. `C:\GAME\APP.EXE`).
    pub exe: String,
    /// Extra command-line arguments (after the program name).
    pub args: String,
    /// Guest working directory (defaults to the exe directory).
    pub cwd: Option<String>,
    pub windows: WindowsProfile,
    pub cpu: CpuProfile,
    pub env: Vec<(String, String)>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct RunLimit {
    /// Stop when virtual time reaches this value (ns).
    pub until_ns: u64,
    /// Stop after this many retired instructions (relative).
    pub max_insns: Option<u64>,
    /// Stop after the next presentation (Flip / primary blit).
    pub stop_on_present: bool,
    /// When all threads are blocked, jump virtual time to the next event (bounded by
    /// `until_ns`). Paced drivers set this to false and wait on the host clock instead.
    pub jump_idle: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum RunStop {
    TimeReached,
    InsnLimit,
    /// Every thread is blocked; `next_event` is the earliest scheduled wake-up.
    Idle {
        next_event: Option<u64>,
    },
    Breakpoint {
        tid: u32,
        addr: u32,
    },
    Watchpoint {
        tid: u32,
        addr: u32,
        write: bool,
        eip: u32,
    },
    ApiBreak {
        tid: u32,
        api: String,
    },
    Presented {
        frame: u64,
    },
    Exited {
        code: u32,
    },
    Halted,
    Cancelled,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProcessState {
    pub pid: u32,
    pub peb: u32,
    pub image_base: u32,
    pub exe_module: u32,
    pub cmdline: String,
    pub cmdline_a: u32,
    pub cmdline_w: u32,
    pub env_a: u32,
    pub env_w: u32,
    pub startup_info: u32,
    pub cwd: Option<GuestPath>,
    pub exit_code: Option<u32>,
    pub process_obj: u32,
    /// TLS slot allocation bitmap (64 slots).
    pub tls_used: u64,
    pub sys_heap: u32,
    pub unhandled_filter: u32,
    pub error_mode: u32,
    pub priority_class: u32,
    pub ctrl_handlers: Vec<u32>,
    /// Process-attach initialisation consumed by the main thread's ThreadStart.
    pub startup_pending: Option<crate::loader::StartupCont>,
    pub std_handles: [u32; 3],
    pub comdlg_error: u32,
    pub env: Vec<(String, String)>,
}

/// Host-side device bindings that are not part of guest state.
#[derive(Default)]
pub struct Host {
    pub cancel: Arc<AtomicBool>,
    pub api_breaks: Vec<String>,
    pub gpu_options: drydock_gfx::GpuOptions,
    pub renderer: Option<drydock_gfx::Renderer>,
    pub gpu_error: Option<String>,
    /// Guest-memory version last uploaded per surface.
    pub gpu_versions: std::collections::HashMap<u32, u64>,
    /// Where mixed audio goes (host device, file, or nothing).
    pub audio: Option<Box<dyn drydock_audio::AudioSink>>,
}

pub struct Session {
    pub cpu: Cpu,
    pub mem: AddressSpace,
    pub clock: VirtualClock,
    pub config: SessionConfig,
    pub sched: Sched,
    pub objects: Objects,
    pub modules: Modules,
    pub thunks: Thunks,
    pub vfs: Vfs,
    pub registry: Registry,
    pub heaps: Heaps,
    pub process: ProcessState,
    pub user: crate::dll::user32::UserState,
    pub gdi: crate::dll::gdi32::GdiState,
    pub display: crate::surface::Display,
    pub winmm: crate::dll::winmm::MmState,
    pub vfw: crate::dll::msvfw32::VfwState,
    pub com: crate::com::ComState,
    pub diag: Diagnostics,
    pub host: Host,
    /// Guest addresses of `CallArg::Buf` copies for the callback that just returned.
    pub cb_bufs: Vec<u32>,
    /// Presentation counter (incremented by the graphics layer).
    pub frames_presented: u64,
    pub pending_present_stop: bool,
    pub exited: Option<u32>,
    pub stats: RunStats,
}

/// Execution statistics (host-side observations; not guest state).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RunStats {
    pub slices: u64,
    pub budget_insns: u64,
    pub stop_budget: u64,
    pub stop_api: u64,
    pub stop_fault: u64,
    pub budget_by_horizon: u64,
    pub thread_switches: u64,
    /// Virtual time skipped while every guest thread was blocked.
    #[serde(default)]
    pub idle_ns: u64,
}

impl Session {
    pub fn new(config: SessionConfig, vfs: Vfs, registry: Registry) -> Result<Session, String> {
        let mut s = Session {
            cpu: Cpu::new()?,
            mem: AddressSpace::new(),
            clock: VirtualClock::default(),
            config,
            sched: Sched::new(),
            objects: Objects::new(),
            modules: Modules::default(),
            thunks: Thunks::default(),
            vfs,
            registry,
            heaps: Heaps::default(),
            process: ProcessState::default(),
            user: Default::default(),
            gdi: Default::default(),
            display: Default::default(),
            winmm: Default::default(),
            vfw: Default::default(),
            com: Default::default(),
            diag: Diagnostics::default(),
            host: Host::default(),
            cb_bufs: Vec::new(),
            frames_presented: 0,
            pending_present_stop: false,
            exited: None,
            stats: RunStats::default(),
        };
        {
            let st = s.cpu.state_mut();
            st.ns_per_insn_num = s.config.cpu.ns_per_insn_num;
            st.ns_per_insn_den = s.config.cpu.ns_per_insn_den;
            st.tsc_hz = s.config.cpu.tsc_hz;
        }
        crate::loader::init_process(&mut s)?;
        Ok(s)
    }

    // ---------------------------------------------------------------- arguments

    /// Read argument `i` of the API being dispatched (`[esp + 4 + 4i]`).
    pub fn arg(&self, i: u32) -> Result<u32, ApiError> {
        Ok(self.mem.read_u32(self.cpu.esp() + 4 + 4 * i)?)
    }

    pub fn args<const N: usize>(&self) -> Result<[u32; N], ApiError> {
        let esp = self.cpu.esp();
        let mut out = [0u32; N];
        for (i, o) in out.iter_mut().enumerate() {
            *o = self.mem.read_u32(esp + 4 + 4 * i as u32)?;
        }
        Ok(out)
    }

    /// Return address of the API being dispatched.
    pub fn caller(&self) -> u32 {
        self.mem.read_u32(self.cpu.esp()).unwrap_or(0)
    }

    pub fn tid(&self) -> u32 {
        self.sched.current.unwrap_or(0)
    }

    pub fn now_ns(&self) -> u64 {
        self.clock.now_ns()
    }

    pub fn set_last_error(&mut self, e: u32) {
        let teb = self.sched.cur().teb;
        let _ = self.mem.write_u32(teb + 0x34, e);
    }
    pub fn last_error(&self) -> u32 {
        let teb = self.sched.cur().teb;
        self.mem.read_u32(teb + 0x34).unwrap_or(0)
    }

    /// Allocate runtime-owned guest memory (strings, COM objects, tables).
    pub fn sys_alloc(&mut self, size: u32) -> Result<u32, ApiError> {
        self.heaps
            .alloc(&mut self.mem, self.process.sys_heap, size, true)
            .ok_or_else(|| ApiError::Fatal("system heap exhausted".into()))
    }

    pub fn event(&mut self, severity: Severity, kind: &str, message: String) {
        let e = DiagEvent {
            t_ns: self.clock.now_ns(),
            tid: self.tid(),
            severity,
            kind: kind.to_string(),
            message,
            eip: self.cpu.eip(),
        };
        self.diag.event(e);
    }

    pub fn missing(&mut self, kind: &str, message: String) {
        self.event(Severity::Missing, kind, message);
    }

    // ---------------------------------------------------------------- threads

    fn switch_to(&mut self, tid: u32) -> Result<(), String> {
        if self.sched.loaded == Some(tid) {
            self.sched.current = Some(tid);
            return Ok(());
        }
        if let Some(old) = self.sched.loaded
            && let Some(t) = self.sched.threads.get_mut(&old)
        {
            t.regs = self.cpu.read_regs();
        }
        self.stats.thread_switches += 1;
        let regs = self.sched.threads[&tid].regs.clone();
        self.cpu.write_regs(&mut self.mem, &regs)?;
        self.sched.loaded = Some(tid);
        self.sched.current = Some(tid);
        Ok(())
    }

    /// Save the CPU registers into the loaded thread record (before inspection/checkpoint).
    pub fn sync_regs(&mut self) {
        if let Some(t) = self.sched.loaded
            && let Some(th) = self.sched.threads.get_mut(&t)
        {
            th.regs = self.cpu.read_regs();
        }
    }

    /// Reload CPU registers from the loaded thread's record (after a debugger edit).
    pub fn reload_regs(&mut self) -> Result<(), String> {
        if let Some(t) = self.sched.loaded {
            let regs = self.sched.threads[&t].regs.clone();
            self.cpu.write_regs(&mut self.mem, &regs)?;
        }
        Ok(())
    }

    // ---------------------------------------------------------------- run loop

    pub fn halted(&self) -> bool {
        self.diag.halt.is_some()
    }

    pub fn run(&mut self, limit: RunLimit) -> RunStop {
        let insn_start = self.cpu.state().insns;
        loop {
            if self.halted() {
                return RunStop::Halted;
            }
            if let Some(code) = self.exited {
                return RunStop::Exited { code };
            }
            if self.host.cancel.swap(false, Ordering::SeqCst) {
                return RunStop::Cancelled;
            }
            if self.pending_present_stop {
                self.pending_present_stop = false;
                if limit.stop_on_present {
                    return RunStop::Presented {
                        frame: self.frames_presented,
                    };
                }
            }
            let now = self.clock.now_ns();
            if now >= limit.until_ns {
                return RunStop::TimeReached;
            }
            let used = self.cpu.state().insns - insn_start;
            if let Some(m) = limit.max_insns
                && used >= m
            {
                return RunStop::InsnLimit;
            }
            self.service_events();
            self.wake_threads();
            let insns = self.cpu.state().insns;
            let Some(tid) = self.sched.pick(insns) else {
                let next = self.next_event_time();
                if self.sched.live_count() == 0 {
                    let code = self.process.exit_code.unwrap_or(0);
                    self.finish_process(code);
                    continue;
                }
                match next {
                    Some(t) if limit.jump_idle => {
                        let to = t.min(limit.until_ns);
                        self.stats.idle_ns += to.saturating_sub(self.clock.now_ns());
                        self.clock.advance_to(to);
                        continue;
                    }
                    _ => {
                        if limit.jump_idle && next.is_none() {
                            // Nothing will ever wake a thread: report a deadlock.
                            return RunStop::Idle { next_event: None };
                        }
                        return RunStop::Idle { next_event: next };
                    }
                }
            };
            // A new quantum starts on a switch, or when the running thread's quantum
            // expired and it was picked again.
            if self.sched.current != Some(tid) || insns >= self.sched.threads[&tid].quantum_end {
                let q = insns + QUANTUM_INSNS;
                self.sched.threads.get_mut(&tid).unwrap().quantum_end = q;
            }
            if let Err(e) = self.switch_to(tid) {
                self.halt("cpu", e, None);
                continue;
            }
            // Pending API continuation?
            let state = std::mem::replace(&mut self.sched.cur_mut().state, ThreadState::Ready);
            if let ThreadState::Resume { api, cont, val } = state {
                let r = self.resume_api(api, cont, val);
                self.finish_api(api, r);
                continue;
            }
            self.sched.cur_mut().state = state;
            // Budget for this slice.
            let p = &self.config.cpu;
            let ns_to_insns = |ns: u64| ns.saturating_mul(p.ns_per_insn_den) / p.ns_per_insn_num.max(1);
            let mut budget = self.sched.cur().quantum_end.saturating_sub(insns).max(1);
            let horizon = self.next_event_time().unwrap_or(u64::MAX).min(limit.until_ns);
            let hb = ns_to_insns(horizon.saturating_sub(now)).max(1);
            if hb < budget {
                self.stats.budget_by_horizon += 1;
            }
            budget = budget.min(hb);
            if let Some(m) = limit.max_insns {
                budget = budget.min(m - used);
            }
            let before = self.cpu.state().insns;
            self.stats.slices += 1;
            self.stats.budget_insns += budget;
            let stop = self.cpu.run_slice(&mut self.mem, budget, now);
            match &stop {
                Ok(Stop::Budget) => self.stats.stop_budget += 1,
                Ok(Stop::ApiTrap { .. }) => self.stats.stop_api += 1,
                Ok(Stop::Fault(_)) => self.stats.stop_fault += 1,
                _ => {}
            }
            let ran = self.cpu.state().insns - before;
            self.clock.charge_insns(ran, &self.config.cpu);
            match stop {
                Err(e) => self.halt("cpu", e, None),
                Ok(Stop::Budget) | Ok(Stop::Requested) => {}
                Ok(Stop::ApiTrap { eip }) => {
                    if let Some(r) = self.on_trap(eip - 2) {
                        return r;
                    }
                }
                Ok(Stop::Breakpoint { addr }) => {
                    return RunStop::Breakpoint { tid, addr };
                }
                Ok(Stop::Watchpoint { addr, write, eip, .. }) => {
                    return RunStop::Watchpoint { tid, addr, write, eip };
                }
                Ok(Stop::Fault(f)) => {
                    // First execution from a readable data page: allowed without DEP.
                    if f.kind == drydock_core::cpu::FaultKind::FetchProt && self.mem.grant_exec(f.addr) {
                        continue;
                    }
                    crate::seh::raise_fault(self, f)
                }
            }
        }
    }

    fn on_trap(&mut self, thunk: u32) -> Option<RunStop> {
        if thunk == self.thunks.callback_ret {
            self.callback_returned();
            return None;
        }
        if thunk == self.thunks.thread_exit {
            let code = self.cpu.reg(Reg::EAX);
            self.exit_thread(code);
            return None;
        }
        let Some(info) = self.thunks.get(thunk) else {
            let eip = self.cpu.eip();
            self.halt(
                "stray-trap",
                format!("int 0x2e executed at {eip:#x} outside a runtime thunk"),
                None,
            );
            return None;
        };
        if !self.host.api_breaks.is_empty()
            && let name = info.full_name()
            && self
                .host
                .api_breaks
                .iter()
                .any(|b| name.eq_ignore_ascii_case(b) || info.name.eq_ignore_ascii_case(b))
        {
            // Rewind so that resuming re-dispatches the call.
            self.cpu.set_reg(Reg::EIP, thunk);
            self.host
                .api_breaks
                .retain(|b| !(name.eq_ignore_ascii_case(b) || info.name.eq_ignore_ascii_case(b)));
            return Some(RunStop::ApiBreak {
                tid: self.tid(),
                api: name,
            });
        }
        self.dispatch(thunk);
        None
    }

    pub fn dispatch(&mut self, thunk: u32) {
        let info = self.thunks.get(thunk).unwrap();
        let (conv, handler, full) = (info.conv, info.handler, info.full.clone());
        self.clock.charge_ns(API_COST_NS);
        let nargs = match conv {
            Conv::Stdcall(n) => n as usize,
            Conv::Cdecl => 4,
        };
        let esp = self.cpu.esp();
        let args: Vec<u32> = (0..nargs)
            .map(|i| self.mem.read_u32(esp + 4 + 4 * i as u32).unwrap_or(0))
            .collect();
        self.diag.record_call(ApiTraceEntry {
            t_ns: self.clock.now_ns(),
            tid: self.tid(),
            api: full.clone(),
            args,
            caller: self.caller(),
            ret: None,
        });
        let r = match handler {
            Some(h) => h(self),
            None => Err(ApiError::Unimplemented(full.to_string())),
        };
        self.finish_api(thunk, r);
    }

    /// Apply an API outcome for the activation of `api` on the current thread.
    pub fn finish_api(&mut self, api: u32, r: ApiResult) {
        match r {
            Ok(ApiRet::Ret(v)) => self.complete(api, v, None),
            Ok(ApiRet::Ret64(v)) => self.complete(api, v as u32, Some((v >> 32) as u32)),
            Ok(ApiRet::RetF64(f)) => {
                self.push_st0(f);
                self.complete_keep_eax(api);
            }
            Ok(ApiRet::Call(gc)) => self.begin_callback(api, gc),
            Ok(ApiRet::Block(b)) => {
                // Deadline in the past or zero timeout is handled by the waker.
                self.sched.cur_mut().state = ThreadState::Blocked { api, block: b };
            }
            Ok(ApiRet::ExitThread(code)) => self.exit_thread(code),
            Ok(ApiRet::ExitProcess(code)) => self.finish_process(code),
            Ok(ApiRet::Raw) => {}
            Err(ApiError::Mem(e)) => {
                let name = self.thunks.name_of(api);
                self.event(Severity::Warning, "api-access-violation", format!("{name}: {e}"));
                crate::seh::raise_access_violation_in_api(self, api, &e);
            }
            Err(ApiError::Unimplemented(what)) => {
                let name = self.thunks.name_of(api);
                self.halt("unimplemented", what, Some(name));
            }
            Err(ApiError::Fatal(what)) => {
                let name = self.thunks.name_of(api);
                self.halt("fatal", what, Some(name));
            }
        }
    }

    fn complete(&mut self, api: u32, eax: u32, edx: Option<u32>) {
        let pop = self.thunks.get(api).map(|t| t.conv.pop_bytes()).unwrap_or(0);
        let esp = self.cpu.esp();
        let ret = match self.mem.read_u32(esp) {
            Ok(r) => r,
            Err(e) => {
                self.halt("stack", format!("cannot read return address: {e}"), None);
                return;
            }
        };
        if let Some(t) = self.diag.recent.back_mut()
            && t.ret.is_none()
        {
            t.ret = Some(eax);
        }
        self.cpu.set_reg(Reg::EAX, eax);
        if let Some(d) = edx {
            self.cpu.set_reg(Reg::EDX, d);
        }
        self.cpu.set_reg(Reg::ESP, esp + 4 + pop);
        self.cpu.set_reg(Reg::EIP, ret);
    }

    fn complete_keep_eax(&mut self, api: u32) {
        let eax = self.cpu.reg(Reg::EAX);
        self.complete(api, eax, None);
    }

    fn push_st0(&mut self, f: f64) {
        let mut r = self.cpu.read_regs();
        let top = ((r.fpsw >> 11) & 7).wrapping_sub(1) & 7;
        r.fpsw = (r.fpsw & !0x3800) | (top << 11);
        r.st[top as usize] = f64_to_f80(f);
        r.fptag &= !(3 << (top * 2));
        let _ = self.cpu.write_regs(&mut self.mem, &r);
    }

    fn begin_callback(&mut self, api: u32, gc: GuestCall) {
        let api_esp = self.cpu.esp();
        let saved = [
            self.cpu.reg(Reg::EBX),
            self.cpu.reg(Reg::ESI),
            self.cpu.reg(Reg::EDI),
            self.cpu.reg(Reg::EBP),
        ];
        let mut sp = api_esp - 0x10;
        let mut vals = Vec::with_capacity(gc.args.len());
        let mut bufs = Vec::new();
        for a in &gc.args {
            match a {
                CallArg::Val(v) => vals.push(*v),
                CallArg::Buf(b) => {
                    sp -= (b.len() as u32).div_ceil(4) * 4;
                    if let Err(e) = self.mem.write(sp, b) {
                        self.halt("stack", format!("callback buffer: {e}"), None);
                        return;
                    }
                    vals.push(sp);
                    bufs.push(sp);
                }
                CallArg::BufRel(b, rel) => {
                    sp -= (b.len() as u32).div_ceil(4) * 4;
                    let mut b = b.clone();
                    for &o in rel {
                        let o = o as usize;
                        let v = u32::from_le_bytes(b[o..o + 4].try_into().unwrap()).wrapping_add(sp);
                        b[o..o + 4].copy_from_slice(&v.to_le_bytes());
                    }
                    if let Err(e) = self.mem.write(sp, &b) {
                        self.halt("stack", format!("callback buffer: {e}"), None);
                        return;
                    }
                    vals.push(sp);
                    bufs.push(sp);
                }
            }
        }
        sp &= !3;
        for v in vals.iter().rev() {
            sp -= 4;
            let _ = self.mem.write_u32(sp, *v);
        }
        sp -= 4;
        let _ = self.mem.write_u32(sp, self.thunks.callback_ret);
        self.cpu.set_reg(Reg::ESP, sp);
        self.cpu.set_reg(Reg::EIP, gc.target);
        self.sched.cur_mut().frames.push(Frame {
            api,
            api_esp,
            saved,
            bufs,
            cont: gc.cont,
        });
    }

    fn callback_returned(&mut self) {
        let Some(f) = self.sched.cur_mut().frames.pop() else {
            self.halt("callback", "callback return with no pending frame".into(), None);
            return;
        };
        let eax = self.cpu.reg(Reg::EAX);
        let edx = self.cpu.reg(Reg::EDX);
        self.cpu.set_reg(Reg::ESP, f.api_esp);
        self.cpu.set_reg(Reg::EBX, f.saved[0]);
        self.cpu.set_reg(Reg::ESI, f.saved[1]);
        self.cpu.set_reg(Reg::EDI, f.saved[2]);
        self.cpu.set_reg(Reg::EBP, f.saved[3]);
        self.cb_bufs = f.bufs;
        let r = self.resume_api(f.api, f.cont, Resume::Returned { eax, edx });
        self.finish_api(f.api, r);
    }

    /// Resume an API activation; `Cont::Retry` re-dispatches the API's handler.
    pub fn resume_api(&mut self, api: u32, cont: Cont, v: Resume) -> ApiResult {
        if let Cont::Retry = cont {
            return match self.thunks.get(api).and_then(|t| t.handler) {
                Some(h) => h(self),
                None => Err(ApiError::Unimplemented(self.thunks.name_of(api))),
            };
        }
        self.resume(cont, v)
    }

    pub fn resume(&mut self, cont: Cont, v: Resume) -> ApiResult {
        match cont {
            Cont::Retry => Err(ApiError::Fatal("Retry continuation outside an API".into())),
            Cont::ReturnValue => Ok(ApiRet::Ret(v.eax())),
            Cont::ReturnConst(c) => Ok(ApiRet::Ret(c)),
            Cont::WaitResult => Ok(ApiRet::Ret(v.eax())),
            Cont::Startup(c) => crate::loader::resume_startup(self, c, v),
            Cont::Kernel32(c) => crate::dll::kernel32::resume(self, c, v),
            Cont::User32(c) => crate::dll::user32::resume(self, c, v),
            Cont::Seh(c) => crate::seh::resume(self, c, v),
            Cont::Com(c) => crate::com::resume(self, c, v),
            Cont::Winmm(c) => crate::dll::winmm::resume(self, c, v),
            Cont::Dialog(c) => crate::dll::dialog::resume(self, c, v),
            Cont::Chain(a, b) => crate::dll::dialog::resume_chain(self, *a, *b, v),
        }
    }

    pub fn exit_thread(&mut self, code: u32) {
        let tid = self.tid();
        let t = self.sched.cur_mut();
        t.state = ThreadState::Terminated;
        t.exit_code = Some(code);
        t.frames.clear();
        let region = t.stack_region;
        if self.sched.live_count() == 0 {
            self.process.exit_code.get_or_insert(code);
        }
        // Thread stacks are released on exit (as with NT); the main thread's is kept so
        // post-mortem inspection still works.
        if tid != self.sched.threads.keys().next().copied().unwrap_or(0) {
            let _ = self.mem.release(region);
        }
        self.sched.loaded = None;
        self.sched.current = None;
    }

    pub fn finish_process(&mut self, code: u32) {
        if self.exited.is_some() {
            return;
        }
        self.process.exit_code = Some(code);
        self.exited = Some(code);
        if let Some(KObj::Process { exit_code }) = self.objects.get_mut(self.process.process_obj) {
            *exit_code = Some(code);
        }
        if let Err(e) = self.vfs.persist() {
            self.event(Severity::Error, "persist", format!("overlay persist failed: {e}"));
        }
        if let Err(e) = crate::loader::persist_registry(self) {
            self.event(Severity::Error, "persist", format!("registry persist failed: {e}"));
        }
    }

    pub fn halt(&mut self, reason: &str, detail: String, api: Option<String>) {
        if self.diag.halt.is_some() {
            return;
        }
        let regs = self.cpu.read_regs();
        let esp = regs.esp;
        let stack: Vec<u32> = (0..32).map(|i| self.mem.read_u32(esp + 4 * i).unwrap_or(0)).collect();
        let (caller, args) = if api.is_some() {
            (stack.first().copied().unwrap_or(0), stack[1..9].to_vec())
        } else {
            (0, Vec::new())
        };
        let recent_blocks = self
            .cpu
            .state()
            .trace
            .as_ref()
            .map(|t| t.iter().map(|e| e.eip).collect())
            .unwrap_or_default();
        log::warn!("session halted: {reason}: {detail}");
        self.diag.halt = Some(Halt {
            reason: reason.to_string(),
            detail,
            tid: self.tid(),
            eip: regs.eip,
            caller,
            api,
            args,
            recent_apis: self.diag.recent.iter().cloned().collect(),
            recent_blocks,
            regs,
            stack,
        });
    }

    // ---------------------------------------------------------------- time & waits

    /// Earliest future virtual time at which something may happen.
    pub fn next_event_time(&self) -> Option<u64> {
        let mut t: Option<u64> = None;
        let mut take = |x: Option<u64>| {
            if let Some(x) = x {
                t = Some(t.map_or(x, |y| y.min(x)));
            }
        };
        for th in self.sched.threads.values() {
            if let ThreadState::Blocked { block, .. } = &th.state {
                take(block.deadline);
            }
        }
        take(crate::dll::winmm::next_event(self));
        take(crate::dll::msvfw32::next_event(self));
        take(crate::dll::user32::next_event(self));
        take(crate::com::next_event(self));
        t
    }

    /// Fire everything due at the current virtual time.
    fn service_events(&mut self) {
        crate::dll::winmm::service(self);
        crate::dll::msvfw32::service(self);
        crate::dll::user32::service(self);
        crate::com::service(self);
    }

    fn wake_threads(&mut self) {
        let now = self.clock.now_ns();
        let tids: Vec<u32> = self
            .sched
            .threads
            .iter()
            .filter(|(_, t)| matches!(t.state, ThreadState::Blocked { .. }))
            .map(|(k, _)| *k)
            .collect();
        for tid in tids {
            let ThreadState::Blocked { api, block } = &self.sched.threads[&tid].state else {
                continue;
            };
            let (api, block) = (*api, block.clone());
            if let Some(val) = crate::sync::try_satisfy(self, tid, &block.wait) {
                let t = self.sched.threads.get_mut(&tid).unwrap();
                t.state = ThreadState::Resume {
                    api,
                    cont: block.cont,
                    val,
                };
            } else if block.deadline.is_some_and(|d| d <= now) {
                let t = self.sched.threads.get_mut(&tid).unwrap();
                t.state = ThreadState::Resume {
                    api,
                    cont: block.cont,
                    val: Resume::TimedOut,
                };
            }
        }
    }

    /// Calendar time as FILETIME.
    pub fn filetime_now(&self) -> u64 {
        self.config.windows.epoch_filetime + self.clock.now_ns() / 100
    }

    pub fn mem_err(e: MemError) -> ApiError {
        ApiError::Mem(e)
    }

    pub fn is_fault_kind_av(k: &FaultKind) -> bool {
        matches!(
            k,
            FaultKind::ReadUnmapped
                | FaultKind::WriteUnmapped
                | FaultKind::FetchUnmapped
                | FaultKind::ReadProt
                | FaultKind::WriteProt
                | FaultKind::FetchProt
        )
    }
}

/// Convert an f64 to the x87 80-bit extended format (little-endian bytes).
pub fn f64_to_f80(f: f64) -> [u8; 10] {
    let bits = f.to_bits();
    let sign = (bits >> 63) as u16;
    let exp = ((bits >> 52) & 0x7ff) as i32;
    let frac = bits & ((1u64 << 52) - 1);
    let (e80, m80): (u16, u64) = if exp == 0 && frac == 0 {
        (0, 0)
    } else if exp == 0x7ff {
        (0x7fff, (1u64 << 63) | (frac << 11))
    } else if exp == 0 {
        // Subnormal double: normalise.
        let lz = frac.leading_zeros() - 11;
        let m = frac << (lz + 11);
        ((15361 - lz as i32) as u16, m)
    } else {
        ((exp - 1023 + 16383) as u16, (1u64 << 63) | (frac << 11))
    };
    let mut out = [0u8; 10];
    out[..8].copy_from_slice(&m80.to_le_bytes());
    out[8..].copy_from_slice(&((sign << 15) | e80).to_le_bytes());
    out
}

pub fn f80_to_f64(b: &[u8; 10]) -> f64 {
    let m = u64::from_le_bytes(b[..8].try_into().unwrap());
    let se = u16::from_le_bytes([b[8], b[9]]);
    let sign = if se & 0x8000 != 0 { -1.0 } else { 1.0 };
    let e = (se & 0x7fff) as i32;
    if e == 0 && m == 0 {
        return 0.0 * sign;
    }
    if e == 0x7fff {
        return if m << 1 == 0 { sign * f64::INFINITY } else { f64::NAN };
    }
    sign * (m as f64) * 2f64.powi(e - 16383 - 63)
}

#[cfg(test)]
mod exec_tests {
    use crate::session::{RunLimit, RunStop};
    use drydock_fixtures::PeBuilder;
    use iced_x86::code_asm::*;

    /// Without DEP, code in a read/write data page runs, and rewriting it between
    /// calls takes effect (the JIT translation is invalidated).
    #[test]
    fn data_pages_are_executable_and_rewritable() {
        // mov eax, 42 ; ret
        let pe = PeBuilder::new()
            .import("KERNEL32.DLL", &["ExitProcess"])
            .data(&[0xb8, 42, 0, 0, 0, 0xc3])
            .build(|a, l| {
                a.mov(ebx, l.data)?;
                a.call(ebx)?;
                a.mov(esi, eax)?;
                a.mov(dword_ptr(l.data + 1), 7)?;
                a.call(ebx)?;
                a.add(eax, esi)?;
                a.push(eax)?;
                a.call(dword_ptr(l.import("KERNEL32.DLL", "ExitProcess")))?;
                a.int3()
            });
        let mut s = crate::testutil::session_with(&pe);
        let stop = s.run(RunLimit {
            until_ns: 1_000_000_000,
            max_insns: None,
            stop_on_present: false,
            jump_idle: true,
        });
        assert_eq!(stop, RunStop::Exited { code: 49 }, "halt: {:?}", s.diag.halt);
    }
}
