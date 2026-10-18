//! kernel32: process, threads, synchronisation, memory, heaps, modules, time.

use crate::api::*;
use crate::diag::Severity;
use crate::export;
use crate::loader::{self, StartupCall};
use crate::objects::{CURRENT_PROCESS, CURRENT_THREAD, KObj};
use crate::sched::ThreadState;
use crate::session::Session;
use crate::sync::{WAIT_FAILED, WAIT_TIMEOUT};
use drydock_core::mem::*;
use drydock_core::pe::{ImportName, ResId};
use serde::{Deserialize, Serialize};

pub use super::kernel32_file::*;
pub use super::kernel32_nls::*;

pub const ERROR_SUCCESS: u32 = 0;
pub const ERROR_FILE_NOT_FOUND: u32 = 2;
pub const ERROR_INVALID_HANDLE: u32 = 6;
pub const ERROR_NOT_ENOUGH_MEMORY: u32 = 8;
pub const ERROR_INVALID_PARAMETER: u32 = 87;
pub const ERROR_INSUFFICIENT_BUFFER: u32 = 122;
pub const ERROR_MOD_NOT_FOUND: u32 = 126;
pub const ERROR_PROC_NOT_FOUND: u32 = 127;
pub const ERROR_ALREADY_EXISTS: u32 = 183;
pub const ERROR_NOT_SUPPORTED: u32 = 50;
pub const ERROR_INVALID_ADDRESS: u32 = 487;
pub const ERROR_RESOURCE_TYPE_NOT_FOUND: u32 = 1813;
pub const ERROR_RESOURCE_NAME_NOT_FOUND: u32 = 1814;

/// Uptime reported at session start (tick counters do not start at zero).
pub const UPTIME_AT_START_NS: u64 = 600_000_000_000;
pub const QPC_FREQ: u64 = 3_579_545;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum K32Cont {
    LoadLibrary {
        base: u32,
        calls: Vec<StartupCall>,
        next: usize,
    },
}

pub fn resume(s: &mut Session, c: K32Cont, v: Resume) -> ApiResult {
    match c {
        K32Cont::LoadLibrary { base, calls, next } => {
            if next > 0 && calls[next - 1].check && v.eax() == 0 {
                s.set_last_error(1114); // ERROR_DLL_INIT_FAILED
                return Ok(ApiRet::Ret(0));
            }
            run_init_calls(base, calls, next)
        }
    }
}

fn run_init_calls(base: u32, calls: Vec<StartupCall>, next: usize) -> ApiResult {
    if next >= calls.len() {
        return Ok(ApiRet::Ret(base));
    }
    let c = calls[next].clone();
    Ok(ApiRet::call(
        c.target,
        c.args.iter().map(|&a| CallArg::Val(a)).collect(),
        Cont::Kernel32(K32Cont::LoadLibrary {
            base,
            calls,
            next: next + 1,
        }),
    ))
}

impl Session {
    pub fn uptime_ns(&self) -> u64 {
        self.clock.now_ns() + UPTIME_AT_START_NS
    }
    /// Resolve a handle (including pseudo-handles) to an object id.
    pub fn handle_obj(&self, h: u32) -> Option<u32> {
        match h {
            CURRENT_PROCESS => Some(self.process.process_obj),
            CURRENT_THREAD => Some(self.sched.cur().obj),
            _ => self.objects.lookup(h),
        }
    }
    pub fn thread_of_handle(&self, h: u32) -> Option<u32> {
        if h == CURRENT_THREAD {
            return Some(self.tid());
        }
        match self.objects.by_handle(h)? {
            KObj::Thread { tid } => Some(*tid),
            _ => None,
        }
    }
    fn deadline(&self, ms: u32) -> Option<u64> {
        if ms == 0xffff_ffff {
            None
        } else {
            Some(self.clock.now_ns() + ms as u64 * 1_000_000)
        }
    }
}

// ---------------------------------------------------------------- process

fn exit_process(s: &mut Session) -> ApiResult {
    let [code] = s.args::<1>()?;
    Ok(ApiRet::ExitProcess(code))
}

fn terminate_process(s: &mut Session) -> ApiResult {
    let [h, code] = s.args::<2>()?;
    if s.handle_obj(h) == Some(s.process.process_obj) {
        return Ok(ApiRet::ExitProcess(code));
    }
    s.set_last_error(ERROR_INVALID_HANDLE);
    Ok(ApiRet::Ret(0))
}

fn get_exit_code_process(s: &mut Session) -> ApiResult {
    let [h, p] = s.args::<2>()?;
    match s.handle_obj(h).and_then(|o| s.objects.get(o).cloned()) {
        Some(KObj::Process { exit_code }) => {
            s.mem.write_u32(p, exit_code.unwrap_or(259))?;
            Ok(ApiRet::Ret(1))
        }
        _ => {
            s.set_last_error(ERROR_INVALID_HANDLE);
            Ok(ApiRet::Ret(0))
        }
    }
}

fn get_current_process(_s: &mut Session) -> ApiResult {
    Ok(ApiRet::Ret(CURRENT_PROCESS))
}
fn get_current_thread(_s: &mut Session) -> ApiResult {
    Ok(ApiRet::Ret(CURRENT_THREAD))
}
fn get_current_process_id(s: &mut Session) -> ApiResult {
    Ok(ApiRet::Ret(s.process.pid))
}
fn get_current_thread_id(s: &mut Session) -> ApiResult {
    Ok(ApiRet::Ret(s.tid()))
}

fn get_command_line_a(s: &mut Session) -> ApiResult {
    Ok(ApiRet::Ret(s.process.cmdline_a))
}
fn get_command_line_w(s: &mut Session) -> ApiResult {
    Ok(ApiRet::Ret(s.process.cmdline_w))
}

fn get_startup_info_a(s: &mut Session) -> ApiResult {
    let [p] = s.args::<1>()?;
    let mut b = vec![0u8; 68];
    b[0..4].copy_from_slice(&68u32.to_le_bytes());
    s.mem.write(p, &b)?;
    Ok(ApiRet::Ret(0))
}

fn get_version(s: &mut Session) -> ApiResult {
    let w = &s.config.windows;
    let hi = if w.platform == 2 {
        w.build & 0x7fff
    } else {
        0x8000 | (w.build & 0x7fff)
    };
    Ok(ApiRet::Ret((hi << 16) | (w.minor << 8) | w.major))
}

fn get_version_ex_a(s: &mut Session) -> ApiResult {
    let [p] = s.args::<1>()?;
    let size = s.mem.read_u32(p)?;
    if size != 148 && size != 156 {
        s.set_last_error(ERROR_INSUFFICIENT_BUFFER);
        return Ok(ApiRet::Ret(0));
    }
    let w = s.config.windows.clone();
    s.mem.write_u32(p + 4, w.major)?;
    s.mem.write_u32(p + 8, w.minor)?;
    s.mem.write_u32(p + 12, w.build)?;
    s.mem.write_u32(p + 16, w.platform)?;
    s.mem.fill(p + 20, 128, 0)?;
    s.write_str_trunc(p + 20, 128, &w.csd)?;
    if size == 156 {
        s.mem.write_u16(p + 148, w.sp_major)?;
        s.mem.write_u16(p + 150, 0)?;
        s.mem.write_u16(p + 152, 0)?;
        s.mem.write_u8(p + 154, 1)?; // VER_NT_WORKSTATION
        s.mem.write_u8(p + 155, 0)?;
    }
    Ok(ApiRet::Ret(1))
}

fn get_system_info(s: &mut Session) -> ApiResult {
    let [p] = s.args::<1>()?;
    let mut b = vec![0u8; 36];
    let put = |b: &mut Vec<u8>, o: usize, v: u32| b[o..o + 4].copy_from_slice(&v.to_le_bytes());
    put(&mut b, 4, PAGE_SIZE);
    put(&mut b, 8, 0x10000);
    put(&mut b, 12, 0x7ffe_ffff);
    put(&mut b, 16, 1);
    put(&mut b, 20, 1);
    put(&mut b, 24, 586);
    put(&mut b, 28, ALLOC_GRANULARITY);
    b[32..34].copy_from_slice(&6u16.to_le_bytes());
    b[34..36].copy_from_slice(&0x0703u16.to_le_bytes());
    s.mem.write(p, &b)?;
    Ok(ApiRet::Ret(0))
}

fn global_memory_status(s: &mut Session) -> ApiResult {
    let [p] = s.args::<1>()?;
    let total = s.config.windows.total_phys_mem;
    let used: u64 = s
        .mem
        .region_infos()
        .iter()
        .filter(|r| r.kind != RegionKind::System)
        .map(|r| r.committed_bytes as u64)
        .sum();
    let avail = (total as u64).saturating_sub(used).max(16 << 20) as u32;
    let vals = [
        32,
        ((total - avail) as u64 * 100 / total as u64) as u32,
        total,
        avail,
        total * 2,
        (total * 2).saturating_sub(used as u32),
        0x7ffe_0000,
        0x7ffe_0000u32.saturating_sub(used as u32),
    ];
    for (i, v) in vals.iter().enumerate() {
        s.mem.write_u32(p + 4 * i as u32, *v)?;
    }
    Ok(ApiRet::Ret(0))
}

fn set_error_mode(s: &mut Session) -> ApiResult {
    let [m] = s.args::<1>()?;
    let old = std::mem::replace(&mut s.process.error_mode, m);
    Ok(ApiRet::Ret(old))
}

fn get_last_error(s: &mut Session) -> ApiResult {
    Ok(ApiRet::Ret(s.last_error()))
}
fn set_last_error(s: &mut Session) -> ApiResult {
    let [e] = s.args::<1>()?;
    s.set_last_error(e);
    Ok(ApiRet::Ret(0))
}

fn output_debug_string_a(s: &mut Session) -> ApiResult {
    let [p] = s.args::<1>()?;
    let t = s.read_str(p)?;
    s.diag.debug_string(t);
    Ok(ApiRet::Ret(0))
}

fn debug_break(s: &mut Session) -> ApiResult {
    let caller = s.caller();
    let mut regs = s.cpu.read_regs();
    regs.eip = caller;
    regs.esp = s.cpu.esp() + 4;
    let api = s.cpu.eip() - 2;
    crate::seh::dispatch_exception(s, crate::seh::STATUS_BREAKPOINT, 0, &[0], api, &regs)
}

fn set_unhandled_exception_filter(s: &mut Session) -> ApiResult {
    let [f] = s.args::<1>()?;
    let old = std::mem::replace(&mut s.process.unhandled_filter, f);
    Ok(ApiRet::Ret(old))
}

fn set_console_ctrl_handler(s: &mut Session) -> ApiResult {
    let [h, add] = s.args::<2>()?;
    if add != 0 {
        s.process.ctrl_handlers.push(h);
    } else {
        s.process.ctrl_handlers.retain(|&x| x != h);
    }
    Ok(ApiRet::Ret(1))
}

fn create_process_a(s: &mut Session) -> ApiResult {
    let [app, cmd] = s.args::<2>()?;
    let app = s.read_str_opt(app)?;
    let cmd = s.read_str_opt(cmd)?;
    s.missing(
        "CreateProcess",
        format!("child processes are not supported (app={app:?}, cmd={cmd:?})"),
    );
    s.set_last_error(ERROR_NOT_SUPPORTED);
    Ok(ApiRet::Ret(0))
}

fn get_priority_class(s: &mut Session) -> ApiResult {
    Ok(ApiRet::Ret(s.process.priority_class))
}
fn set_priority_class(s: &mut Session) -> ApiResult {
    let [_h, c] = s.args::<2>()?;
    s.process.priority_class = c;
    Ok(ApiRet::Ret(1))
}

// ---------------------------------------------------------------- threads

fn create_thread(s: &mut Session) -> ApiResult {
    let [_sa, stack, start, param, flags, ptid] = s.args::<6>()?;
    let tid =
        loader::create_thread(s, start, param, stack.max(0x10_0000), flags & 4 != 0, false).map_err(ApiError::Fatal)?;
    let obj = s.sched.threads[&tid].obj;
    let h = s.objects.open(obj);
    if ptid != 0 {
        s.mem.write_u32(ptid, tid)?;
    }
    Ok(ApiRet::Ret(h))
}

fn exit_thread(s: &mut Session) -> ApiResult {
    let [code] = s.args::<1>()?;
    Ok(ApiRet::ExitThread(code))
}

fn terminate_thread(s: &mut Session) -> ApiResult {
    let [h, code] = s.args::<2>()?;
    let Some(tid) = s.thread_of_handle(h) else {
        s.set_last_error(ERROR_INVALID_HANDLE);
        return Ok(ApiRet::Ret(0));
    };
    if tid == s.tid() {
        return Ok(ApiRet::ExitThread(code));
    }
    crate::sync::abandon_mutexes(s, tid);
    if let Some(t) = s.sched.threads.get_mut(&tid) {
        t.state = ThreadState::Terminated;
        t.exit_code = Some(code);
        t.frames.clear();
    }
    Ok(ApiRet::Ret(1))
}

fn get_exit_code_thread(s: &mut Session) -> ApiResult {
    let [h, p] = s.args::<2>()?;
    let Some(tid) = s.thread_of_handle(h) else {
        s.set_last_error(ERROR_INVALID_HANDLE);
        return Ok(ApiRet::Ret(0));
    };
    let code = s.sched.threads.get(&tid).and_then(|t| t.exit_code).unwrap_or(259);
    s.mem.write_u32(p, code)?;
    Ok(ApiRet::Ret(1))
}

fn set_thread_priority(s: &mut Session) -> ApiResult {
    let [h, prio] = s.args::<2>()?;
    match s.thread_of_handle(h) {
        Some(tid) => {
            if let Some(t) = s.sched.threads.get_mut(&tid) {
                t.priority = prio as i32;
            }
            Ok(ApiRet::Ret(1))
        }
        None => {
            s.set_last_error(ERROR_INVALID_HANDLE);
            Ok(ApiRet::Ret(0))
        }
    }
}

fn get_thread_priority(s: &mut Session) -> ApiResult {
    let [h] = s.args::<1>()?;
    match s.thread_of_handle(h) {
        Some(tid) => Ok(ApiRet::Ret(s.sched.threads[&tid].priority as u32)),
        None => {
            s.set_last_error(ERROR_INVALID_HANDLE);
            Ok(ApiRet::Ret(0x7fff_ffff))
        }
    }
}

fn resume_thread(s: &mut Session) -> ApiResult {
    let [h] = s.args::<1>()?;
    match s.thread_of_handle(h).and_then(|t| s.sched.threads.get_mut(&t)) {
        Some(t) => {
            let old = t.suspend;
            t.suspend = t.suspend.saturating_sub(1);
            Ok(ApiRet::Ret(old))
        }
        None => Ok(ApiRet::Ret(0xffff_ffff)),
    }
}

fn suspend_thread(s: &mut Session) -> ApiResult {
    let [h] = s.args::<1>()?;
    let me = s.tid();
    match s.thread_of_handle(h) {
        Some(tid) => {
            let t = s.sched.threads.get_mut(&tid).unwrap();
            let old = t.suspend;
            t.suspend += 1;
            if tid == me {
                // Suspending yourself takes effect immediately.
                t.quantum_end = 0;
            }
            Ok(ApiRet::Ret(old))
        }
        None => Ok(ApiRet::Ret(0xffff_ffff)),
    }
}

fn sleep(s: &mut Session) -> ApiResult {
    let [ms] = s.args::<1>()?;
    if ms == 0 {
        // Yield the remainder of the quantum.
        s.sched.cur_mut().quantum_end = 0;
        return Ok(ApiRet::Ret(0));
    }
    let res = s.winmm.timer_resolution_ns();
    let now = s.clock.now_ns();
    let target = if ms == 0xffff_ffff {
        None
    } else {
        // Wake-ups happen on clock-interrupt boundaries.
        Some((now + ms as u64 * 1_000_000).div_ceil(res) * res)
    };
    Ok(ApiRet::Block(Block {
        wait: WaitKind::Sleep,
        deadline: target,
        cont: Cont::ReturnConst(0),
    }))
}

fn sleep_ex(s: &mut Session) -> ApiResult {
    sleep(s)
}

// ---------------------------------------------------------------- sync objects

fn named(s: &Session, p: u32) -> Result<Option<String>, ApiError> {
    s.read_str_opt(p)
}

fn create_or_open(s: &mut Session, name: Option<String>, obj: KObj) -> u32 {
    if let Some(n) = &name
        && let Some(id) = s.objects.find_named(n)
    {
        let h = s.objects.open(id);
        s.set_last_error(ERROR_ALREADY_EXISTS);
        return h;
    }
    let (h, _) = s.objects.create_handle(obj, name);
    s.set_last_error(0);
    h
}

fn create_event_a(s: &mut Session) -> ApiResult {
    let [_sa, manual, initial, name] = s.args::<4>()?;
    let name = named(s, name)?;
    let h = create_or_open(
        s,
        name,
        KObj::Event {
            manual: manual != 0,
            signaled: initial != 0,
        },
    );
    Ok(ApiRet::Ret(h))
}

fn set_event(s: &mut Session) -> ApiResult {
    let [h] = s.args::<1>()?;
    match s.objects.by_handle_mut(h) {
        Some(KObj::Event { signaled, .. }) => {
            *signaled = true;
            Ok(ApiRet::Ret(1))
        }
        _ => {
            s.set_last_error(ERROR_INVALID_HANDLE);
            Ok(ApiRet::Ret(0))
        }
    }
}

fn reset_event(s: &mut Session) -> ApiResult {
    let [h] = s.args::<1>()?;
    match s.objects.by_handle_mut(h) {
        Some(KObj::Event { signaled, .. }) => {
            *signaled = false;
            Ok(ApiRet::Ret(1))
        }
        _ => {
            s.set_last_error(ERROR_INVALID_HANDLE);
            Ok(ApiRet::Ret(0))
        }
    }
}

fn pulse_event(s: &mut Session) -> ApiResult {
    let [h] = s.args::<1>()?;
    let Some(id) = s.objects.lookup(h) else {
        s.set_last_error(ERROR_INVALID_HANDLE);
        return Ok(ApiRet::Ret(0));
    };
    // Release current waiters, then leave the event non-signaled.
    if let Some(KObj::Event { signaled, .. }) = s.objects.get_mut(id) {
        *signaled = true;
    }
    wake_now(s);
    if let Some(KObj::Event { signaled, .. }) = s.objects.get_mut(id) {
        *signaled = false;
    }
    Ok(ApiRet::Ret(1))
}

/// Re-evaluate blocked threads immediately (used by PulseEvent).
fn wake_now(s: &mut Session) {
    let tids: Vec<u32> = s.sched.threads.keys().copied().collect();
    for tid in tids {
        let ThreadState::Blocked { api, block } = &s.sched.threads[&tid].state else {
            continue;
        };
        let (api, block) = (*api, block.clone());
        if let Some(val) = crate::sync::try_satisfy(s, tid, &block.wait) {
            s.sched.threads.get_mut(&tid).unwrap().state = ThreadState::Resume {
                api,
                cont: block.cont,
                val,
            };
        }
    }
}

fn create_mutex_a(s: &mut Session) -> ApiResult {
    let [_sa, owner, name] = s.args::<3>()?;
    let name = named(s, name)?;
    let tid = s.tid();
    let h = create_or_open(
        s,
        name,
        KObj::Mutex {
            owner: if owner != 0 { Some(tid) } else { None },
            count: (owner != 0) as u32,
            abandoned: false,
        },
    );
    Ok(ApiRet::Ret(h))
}

fn open_mutex_a(s: &mut Session) -> ApiResult {
    let [_access, _inherit, name] = s.args::<3>()?;
    let name = s.read_str(name)?;
    match s.objects.find_named(&name) {
        Some(id) => Ok(ApiRet::Ret(s.objects.open(id))),
        None => {
            s.set_last_error(ERROR_FILE_NOT_FOUND);
            Ok(ApiRet::Ret(0))
        }
    }
}

fn release_mutex(s: &mut Session) -> ApiResult {
    let [h] = s.args::<1>()?;
    let tid = s.tid();
    match s.objects.by_handle_mut(h) {
        Some(KObj::Mutex { owner, count, .. }) if *owner == Some(tid) => {
            *count -= 1;
            if *count == 0 {
                *owner = None;
            }
            Ok(ApiRet::Ret(1))
        }
        Some(KObj::Mutex { .. }) => {
            s.set_last_error(288); // ERROR_NOT_OWNER
            Ok(ApiRet::Ret(0))
        }
        _ => {
            s.set_last_error(ERROR_INVALID_HANDLE);
            Ok(ApiRet::Ret(0))
        }
    }
}

fn create_semaphore_a(s: &mut Session) -> ApiResult {
    let [_sa, initial, max, name] = s.args::<4>()?;
    let name = named(s, name)?;
    let h = create_or_open(
        s,
        name,
        KObj::Semaphore {
            count: initial as i32,
            max: max as i32,
        },
    );
    Ok(ApiRet::Ret(h))
}

fn release_semaphore(s: &mut Session) -> ApiResult {
    let [h, n, prev] = s.args::<3>()?;
    let r = match s.objects.by_handle_mut(h) {
        Some(KObj::Semaphore { count, max }) => {
            if *count + n as i32 > *max {
                Err(298) // ERROR_TOO_MANY_POSTS
            } else {
                let old = *count;
                *count += n as i32;
                Ok(old)
            }
        }
        _ => Err(ERROR_INVALID_HANDLE),
    };
    match r {
        Ok(old) => {
            if prev != 0 {
                s.mem.write_u32(prev, old as u32)?;
            }
            Ok(ApiRet::Ret(1))
        }
        Err(e) => {
            s.set_last_error(e);
            Ok(ApiRet::Ret(0))
        }
    }
}

pub fn wait_objects(s: &mut Session, objs: Vec<u32>, all: bool, ms: u32) -> ApiResult {
    let tid = s.tid();
    if let Some(r) = crate::sync::try_satisfy(
        s,
        tid,
        &WaitKind::Objects {
            objs: objs.clone(),
            all,
        },
    ) {
        return Ok(ApiRet::Ret(r.eax()));
    }
    if ms == 0 {
        return Ok(ApiRet::Ret(WAIT_TIMEOUT));
    }
    let deadline = s.deadline(ms);
    Ok(ApiRet::Block(Block {
        wait: WaitKind::Objects { objs, all },
        deadline,
        cont: Cont::WaitResult,
    }))
}

fn wait_for_single_object(s: &mut Session) -> ApiResult {
    let [h, ms] = s.args::<2>()?;
    let Some(obj) = s.handle_obj(h) else {
        s.set_last_error(ERROR_INVALID_HANDLE);
        return Ok(ApiRet::Ret(WAIT_FAILED));
    };
    wait_objects(s, vec![obj], false, ms)
}

fn wait_for_multiple_objects(s: &mut Session) -> ApiResult {
    let [n, p, all, ms] = s.args::<4>()?;
    if n == 0 || n > 64 {
        s.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(ApiRet::Ret(WAIT_FAILED));
    }
    let mut objs = Vec::new();
    for i in 0..n {
        let h = s.mem.read_u32(p + 4 * i)?;
        match s.handle_obj(h) {
            Some(o) => objs.push(o),
            None => {
                s.set_last_error(ERROR_INVALID_HANDLE);
                return Ok(ApiRet::Ret(WAIT_FAILED));
            }
        }
    }
    wait_objects(s, objs, all != 0, ms)
}

fn close_handle(s: &mut Session) -> ApiResult {
    let [h] = s.args::<1>()?;
    if h == CURRENT_PROCESS || h == CURRENT_THREAD {
        return Ok(ApiRet::Ret(1));
    }
    match s.objects.close(h) {
        Ok(Some(KObj::File(f))) => {
            if f.delete_on_close {
                let _ = s.vfs.delete(&f.path);
            }
            Ok(ApiRet::Ret(1))
        }
        Ok(_) => Ok(ApiRet::Ret(1)),
        Err(crate::objects::InvalidHandle) => {
            if s.registry.close(h) {
                return Ok(ApiRet::Ret(1));
            }
            s.set_last_error(ERROR_INVALID_HANDLE);
            Ok(ApiRet::Ret(0))
        }
    }
}

fn duplicate_handle(s: &mut Session) -> ApiResult {
    let [_sp, src, _tp, dst, _acc, _inh, opts] = s.args::<7>()?;
    let Some(obj) = s.handle_obj(src) else {
        s.set_last_error(ERROR_INVALID_HANDLE);
        return Ok(ApiRet::Ret(0));
    };
    let h = s.objects.open(obj);
    if dst != 0 {
        s.mem.write_u32(dst, h)?;
    }
    if opts & 1 != 0 && src != CURRENT_PROCESS && src != CURRENT_THREAD {
        let _ = s.objects.close(src);
    }
    Ok(ApiRet::Ret(1))
}

// Critical sections (the guest-memory struct is authoritative).
fn initialize_critical_section(s: &mut Session) -> ApiResult {
    let [cs] = s.args::<1>()?;
    let mut b = [0u8; 24];
    b[4..8].copy_from_slice(&0xffff_ffffu32.to_le_bytes());
    s.mem.write(cs, &b)?;
    Ok(ApiRet::Ret(0))
}

fn initialize_critical_section_and_spin_count(s: &mut Session) -> ApiResult {
    initialize_critical_section(s)?;
    Ok(ApiRet::Ret(1))
}

fn delete_critical_section(s: &mut Session) -> ApiResult {
    let [_cs] = s.args::<1>()?;
    Ok(ApiRet::Ret(0))
}

fn enter_critical_section(s: &mut Session) -> ApiResult {
    let [cs] = s.args::<1>()?;
    let tid = s.tid();
    let lock = s.mem.read_i32(cs + 4)?;
    let rec = s.mem.read_u32(cs + 8)?;
    let owner = s.mem.read_u32(cs + 12)?;
    s.mem.write_u32(cs + 4, (lock + 1) as u32)?;
    if owner == 0 {
        s.mem.write_u32(cs + 12, tid)?;
        s.mem.write_u32(cs + 8, 1)?;
        Ok(ApiRet::Ret(0))
    } else if owner == tid {
        s.mem.write_u32(cs + 8, rec + 1)?;
        Ok(ApiRet::Ret(0))
    } else {
        Ok(ApiRet::Block(Block {
            wait: WaitKind::CritSec { addr: cs },
            deadline: None,
            cont: Cont::ReturnConst(0),
        }))
    }
}

fn try_enter_critical_section(s: &mut Session) -> ApiResult {
    let [cs] = s.args::<1>()?;
    let owner = s.mem.read_u32(cs + 12)?;
    let tid = s.tid();
    if owner == 0 || owner == tid {
        enter_critical_section(s)?;
        Ok(ApiRet::Ret(1))
    } else {
        Ok(ApiRet::Ret(0))
    }
}

fn leave_critical_section(s: &mut Session) -> ApiResult {
    let [cs] = s.args::<1>()?;
    let lock = s.mem.read_i32(cs + 4)?;
    let rec = s.mem.read_u32(cs + 8)?;
    let owner = s.mem.read_u32(cs + 12)?;
    if owner != s.tid() || rec == 0 {
        // Leaving a section you do not own is undefined; NT corrupts the lock state.
        s.event(
            Severity::Warning,
            "critsec",
            format!("LeaveCriticalSection({cs:#x}) by non-owner"),
        );
        return Ok(ApiRet::Ret(0));
    }
    s.mem.write_u32(cs + 4, (lock - 1) as u32)?;
    s.mem.write_u32(cs + 8, rec - 1)?;
    if rec == 1 {
        s.mem.write_u32(cs + 12, 0)?;
    }
    Ok(ApiRet::Ret(0))
}

fn interlocked_increment(s: &mut Session) -> ApiResult {
    let [p] = s.args::<1>()?;
    let v = s.mem.read_u32(p)?.wrapping_add(1);
    s.mem.write_u32(p, v)?;
    Ok(ApiRet::Ret(v))
}
fn interlocked_decrement(s: &mut Session) -> ApiResult {
    let [p] = s.args::<1>()?;
    let v = s.mem.read_u32(p)?.wrapping_sub(1);
    s.mem.write_u32(p, v)?;
    Ok(ApiRet::Ret(v))
}
fn interlocked_exchange(s: &mut Session) -> ApiResult {
    let [p, v] = s.args::<2>()?;
    let old = s.mem.read_u32(p)?;
    s.mem.write_u32(p, v)?;
    Ok(ApiRet::Ret(old))
}
fn interlocked_exchange_add(s: &mut Session) -> ApiResult {
    let [p, v] = s.args::<2>()?;
    let old = s.mem.read_u32(p)?;
    s.mem.write_u32(p, old.wrapping_add(v))?;
    Ok(ApiRet::Ret(old))
}
fn interlocked_compare_exchange(s: &mut Session) -> ApiResult {
    let [p, xchg, cmp] = s.args::<3>()?;
    let old = s.mem.read_u32(p)?;
    if old == cmp {
        s.mem.write_u32(p, xchg)?;
    }
    Ok(ApiRet::Ret(old))
}

// TLS
fn tls_alloc(s: &mut Session) -> ApiResult {
    let Some(i) = (0..64u32).find(|i| s.process.tls_used & (1u64 << i) == 0) else {
        s.set_last_error(ERROR_NOT_ENOUGH_MEMORY);
        return Ok(ApiRet::Ret(0xffff_ffff));
    };
    s.process.tls_used |= 1u64 << i;
    let tebs: Vec<u32> = s.sched.threads.values().map(|t| t.teb).collect();
    for teb in tebs {
        let _ = s.mem.write_u32(teb + 0xe10 + 4 * i, 0);
    }
    Ok(ApiRet::Ret(i))
}
fn tls_free(s: &mut Session) -> ApiResult {
    let [i] = s.args::<1>()?;
    if i >= 64 || s.process.tls_used & (1u64 << i) == 0 {
        s.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(ApiRet::Ret(0));
    }
    s.process.tls_used &= !(1u64 << i);
    Ok(ApiRet::Ret(1))
}
fn tls_get_value(s: &mut Session) -> ApiResult {
    let [i] = s.args::<1>()?;
    if i >= 64 {
        s.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(ApiRet::Ret(0));
    }
    let teb = s.sched.cur().teb;
    let v = s.mem.read_u32(teb + 0xe10 + 4 * i)?;
    s.set_last_error(0);
    Ok(ApiRet::Ret(v))
}
fn tls_set_value(s: &mut Session) -> ApiResult {
    let [i, v] = s.args::<2>()?;
    if i >= 64 {
        s.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(ApiRet::Ret(0));
    }
    let teb = s.sched.cur().teb;
    s.mem.write_u32(teb + 0xe10 + 4 * i, v)?;
    Ok(ApiRet::Ret(1))
}

// ---------------------------------------------------------------- memory

const MEM_COMMIT: u32 = 0x1000;
const MEM_RESERVE: u32 = 0x2000;
const MEM_DECOMMIT: u32 = 0x4000;
const MEM_RELEASE: u32 = 0x8000;
const MEM_FREE: u32 = 0x10000;
const MEM_RESET: u32 = 0x80000;

fn virtual_alloc(s: &mut Session) -> ApiResult {
    let [addr, size, ty, prot] = s.args::<4>()?;
    if size == 0 {
        s.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(ApiRet::Ret(0));
    }
    if ty & MEM_RESET != 0 {
        return Ok(ApiRet::Ret(addr & !(PAGE_SIZE - 1)));
    }
    let r = if ty & MEM_RESERVE != 0 || addr == 0 {
        let base = if addr != 0 {
            Some(addr & !(ALLOC_GRANULARITY - 1))
        } else {
            None
        };
        let span = if let Some(b) = base { addr + size - b } else { size };
        match s.mem.reserve(base, span, RegionKind::Private, prot, "VirtualAlloc") {
            Ok(b) => {
                if ty & MEM_COMMIT != 0 {
                    s.mem.commit(b, span, prot).map(|_| b)
                } else {
                    Ok(b)
                }
            }
            Err(e) => Err(e),
        }
    } else {
        s.mem.commit(addr, size, prot).map(|_| addr & !(PAGE_SIZE - 1))
    };
    match r {
        Ok(b) => Ok(ApiRet::Ret(b)),
        Err(e) => {
            s.set_last_error(match e {
                MemError::NoSpace { .. } => ERROR_NOT_ENOUGH_MEMORY,
                _ => ERROR_INVALID_ADDRESS,
            });
            Ok(ApiRet::Ret(0))
        }
    }
}

fn virtual_free(s: &mut Session) -> ApiResult {
    let [addr, size, ty] = s.args::<3>()?;
    let r = if ty & MEM_RELEASE != 0 {
        if size != 0 {
            s.set_last_error(ERROR_INVALID_PARAMETER);
            return Ok(ApiRet::Ret(0));
        }
        s.mem.release(addr)
    } else if ty & MEM_DECOMMIT != 0 {
        s.mem.decommit(addr, size)
    } else {
        s.set_last_error(ERROR_INVALID_PARAMETER);
        return Ok(ApiRet::Ret(0));
    };
    match r {
        Ok(()) => Ok(ApiRet::Ret(1)),
        Err(_) => {
            s.set_last_error(ERROR_INVALID_ADDRESS);
            Ok(ApiRet::Ret(0))
        }
    }
}

fn virtual_protect(s: &mut Session) -> ApiResult {
    let [addr, size, prot, old] = s.args::<4>()?;
    match s.mem.protect(addr, size, prot) {
        Ok(o) => {
            if old != 0 {
                s.mem.write_u32(old, o)?;
            }
            Ok(ApiRet::Ret(1))
        }
        Err(_) => {
            s.set_last_error(ERROR_INVALID_ADDRESS);
            Ok(ApiRet::Ret(0))
        }
    }
}

fn virtual_query(s: &mut Session) -> ApiResult {
    let [addr, p, len] = s.args::<3>()?;
    if len < 28 {
        s.set_last_error(ERROR_INSUFFICIENT_BUFFER);
        return Ok(ApiRet::Ret(0));
    }
    let page = addr & !(PAGE_SIZE - 1);
    let vals: [u32; 7] = match s.mem.query(page) {
        Some((r, pg, run)) => [
            page,
            r.base,
            r.alloc_protect,
            run,
            if pg.state == PageState::Committed {
                MEM_COMMIT
            } else {
                MEM_RESERVE
            },
            if pg.state == PageState::Committed {
                pg.protect
            } else {
                0
            },
            match r.kind {
                RegionKind::Image => 0x100_0000,
                _ => 0x2_0000,
            },
        ],
        None => {
            let next = s
                .mem
                .regions()
                .map(|r| r.base)
                .find(|&b| b > page)
                .unwrap_or(0x8000_0000);
            [page, 0, 0, next - page, MEM_FREE, PAGE_NOACCESS, 0]
        }
    };
    for (i, v) in vals.iter().enumerate() {
        s.mem.write_u32(p + 4 * i as u32, *v)?;
    }
    Ok(ApiRet::Ret(28))
}

fn is_bad_read_ptr(s: &mut Session) -> ApiResult {
    let [p, n] = s.args::<2>()?;
    Ok(ApiRet::Ret((!s.mem.is_readable(p, n)) as u32))
}
fn is_bad_write_ptr(s: &mut Session) -> ApiResult {
    let [p, n] = s.args::<2>()?;
    Ok(ApiRet::Ret((!s.mem.is_writable(p, n)) as u32))
}
fn is_bad_code_ptr(s: &mut Session) -> ApiResult {
    let [p] = s.args::<1>()?;
    Ok(ApiRet::Ret((!s.mem.is_readable(p, 1)) as u32))
}
fn is_bad_string_ptr_a(s: &mut Session) -> ApiResult {
    let [p, max] = s.args::<2>()?;
    let ok = s.mem.read_cstr(p, max).is_ok();
    Ok(ApiRet::Ret((!ok) as u32))
}

// Heaps
const HEAP_GENERATE_EXCEPTIONS: u32 = 0x4;
const HEAP_ZERO_MEMORY: u32 = 0x8;
const HEAP_REALLOC_IN_PLACE_ONLY: u32 = 0x10;

fn heap_create(s: &mut Session) -> ApiResult {
    let [flags, init, max] = s.args::<3>()?;
    match s.heaps.create(&mut s.mem, flags, init, max) {
        Ok(h) => Ok(ApiRet::Ret(h)),
        Err(_) => {
            s.set_last_error(ERROR_NOT_ENOUGH_MEMORY);
            Ok(ApiRet::Ret(0))
        }
    }
}
fn heap_destroy(s: &mut Session) -> ApiResult {
    let [h] = s.args::<1>()?;
    if h == s.heaps.process_heap {
        return Ok(ApiRet::Ret(0));
    }
    Ok(ApiRet::Ret(s.heaps.destroy(&mut s.mem, h) as u32))
}
fn get_process_heap(s: &mut Session) -> ApiResult {
    Ok(ApiRet::Ret(s.heaps.process_heap))
}
fn heap_alloc(s: &mut Session) -> ApiResult {
    let [h, flags, size] = s.args::<3>()?;
    match s.heaps.alloc(&mut s.mem, h, size, flags & HEAP_ZERO_MEMORY != 0) {
        Some(p) => Ok(ApiRet::Ret(p)),
        None => {
            if (flags | s.heaps.heaps.get(&h).map(|x| x.flags).unwrap_or(0)) & HEAP_GENERATE_EXCEPTIONS != 0 {
                let mut regs = s.cpu.read_regs();
                regs.eip = s.caller();
                regs.esp = s.cpu.esp() + 16;
                let api = s.cpu.eip() - 2;
                return crate::seh::dispatch_exception(s, 0xC000_0017, 0, &[], api, &regs);
            }
            s.set_last_error(ERROR_NOT_ENOUGH_MEMORY);
            Ok(ApiRet::Ret(0))
        }
    }
}
fn heap_free(s: &mut Session) -> ApiResult {
    let [h, _flags, p] = s.args::<3>()?;
    if p == 0 {
        return Ok(ApiRet::Ret(1));
    }
    if s.heaps.free(h, p) {
        Ok(ApiRet::Ret(1))
    } else {
        s.event(
            Severity::Warning,
            "heap",
            format!("HeapFree({h:#x}, {p:#x}) of an invalid block"),
        );
        s.set_last_error(ERROR_INVALID_PARAMETER);
        Ok(ApiRet::Ret(0))
    }
}
fn heap_realloc(s: &mut Session) -> ApiResult {
    let [h, flags, p, size] = s.args::<4>()?;
    match s.heaps.realloc(
        &mut s.mem,
        h,
        p,
        size,
        flags & HEAP_ZERO_MEMORY != 0,
        flags & HEAP_REALLOC_IN_PLACE_ONLY != 0,
    ) {
        Some(n) => Ok(ApiRet::Ret(n)),
        None => {
            s.set_last_error(ERROR_NOT_ENOUGH_MEMORY);
            Ok(ApiRet::Ret(0))
        }
    }
}
fn heap_size(s: &mut Session) -> ApiResult {
    let [h, _flags, p] = s.args::<3>()?;
    Ok(ApiRet::Ret(
        s.heaps.heaps.get(&h).and_then(|x| x.size_of(p)).unwrap_or(0xffff_ffff),
    ))
}
fn heap_validate(s: &mut Session) -> ApiResult {
    let [h, _f, p] = s.args::<3>()?;
    let ok = match s.heaps.heaps.get(&h) {
        Some(x) => p == 0 || x.size_of(p).is_some(),
        None => false,
    };
    Ok(ApiRet::Ret(ok as u32))
}
fn heap_compact(_s: &mut Session) -> ApiResult {
    Ok(ApiRet::Ret(0x10_0000))
}

// Global/Local (fixed and moveable blocks; a moveable handle equals its pointer).
const GMEM_ZEROINIT: u32 = 0x40;
fn global_alloc(s: &mut Session) -> ApiResult {
    let [flags, size] = s.args::<2>()?;
    let ph = s.heaps.process_heap;
    match s.heaps.alloc(&mut s.mem, ph, size.max(1), flags & GMEM_ZEROINIT != 0) {
        Some(p) => Ok(ApiRet::Ret(p)),
        None => {
            s.set_last_error(ERROR_NOT_ENOUGH_MEMORY);
            Ok(ApiRet::Ret(0))
        }
    }
}
fn global_free(s: &mut Session) -> ApiResult {
    let [p] = s.args::<1>()?;
    let ph = s.heaps.process_heap;
    if p == 0 || s.heaps.free(ph, p) {
        Ok(ApiRet::Ret(0))
    } else {
        s.set_last_error(ERROR_INVALID_HANDLE);
        Ok(ApiRet::Ret(p))
    }
}
fn global_realloc(s: &mut Session) -> ApiResult {
    let [p, size, flags] = s.args::<3>()?;
    let ph = s.heaps.process_heap;
    match s
        .heaps
        .realloc(&mut s.mem, ph, p, size.max(1), flags & GMEM_ZEROINIT != 0, false)
    {
        Some(n) => Ok(ApiRet::Ret(n)),
        None => {
            s.set_last_error(ERROR_NOT_ENOUGH_MEMORY);
            Ok(ApiRet::Ret(0))
        }
    }
}
fn global_lock(s: &mut Session) -> ApiResult {
    let [p] = s.args::<1>()?;
    Ok(ApiRet::Ret(p))
}
fn global_unlock(s: &mut Session) -> ApiResult {
    let [_p] = s.args::<1>()?;
    s.set_last_error(0);
    Ok(ApiRet::Ret(0))
}
fn global_handle(s: &mut Session) -> ApiResult {
    let [p] = s.args::<1>()?;
    Ok(ApiRet::Ret(p))
}
fn global_size(s: &mut Session) -> ApiResult {
    let [p] = s.args::<1>()?;
    let ph = s.heaps.process_heap;
    Ok(ApiRet::Ret(
        s.heaps.heaps.get(&ph).and_then(|h| h.size_of(p)).unwrap_or(0),
    ))
}
fn global_flags(_s: &mut Session) -> ApiResult {
    Ok(ApiRet::Ret(0))
}

// ---------------------------------------------------------------- modules

fn get_module_handle_a(s: &mut Session) -> ApiResult {
    let [p] = s.args::<1>()?;
    if p == 0 {
        return Ok(ApiRet::Ret(s.process.image_base));
    }
    let name = s.read_str(p)?;
    get_module_handle_common(s, &name)
}
fn get_module_handle_w(s: &mut Session) -> ApiResult {
    let [p] = s.args::<1>()?;
    if p == 0 {
        return Ok(ApiRet::Ret(s.process.image_base));
    }
    let name = s.read_wstr(p)?;
    get_module_handle_common(s, &name)
}
fn get_module_handle_common(s: &mut Session, name: &str) -> ApiResult {
    match s.modules.by_name(name) {
        Some(m) => Ok(ApiRet::Ret(m.base)),
        None => {
            s.set_last_error(ERROR_MOD_NOT_FOUND);
            Ok(ApiRet::Ret(0))
        }
    }
}

fn get_module_file_name_a(s: &mut Session) -> ApiResult {
    let [h, buf, n] = s.args::<3>()?;
    let h = if h == 0 { s.process.image_base } else { h };
    let Some(m) = s.modules.get(h) else {
        s.set_last_error(ERROR_MOD_NOT_FOUND);
        return Ok(ApiRet::Ret(0));
    };
    let path = m.path.clone();
    match s.write_str_buf(buf, n, &path)? {
        Ok(len) => Ok(ApiRet::Ret(len)),
        Err(_) => {
            s.write_str_trunc(buf, n, &path)?;
            s.set_last_error(ERROR_INSUFFICIENT_BUFFER);
            Ok(ApiRet::Ret(n))
        }
    }
}

fn load_library_common(s: &mut Session, name: &str) -> ApiResult {
    let mut inits = Vec::new();
    let already = s.modules.by_name(name).map(|m| m.base);
    match loader::load_dll_by_name(s, name, &mut inits) {
        Ok(base) => {
            if already.is_some()
                && let Some(m) = s.modules.list.get_mut(&base)
            {
                m.refs += 1;
            }
            let mut calls = Vec::new();
            for d in inits {
                let m = s.modules.get(d).unwrap().clone();
                for cb in &m.tls_callbacks {
                    calls.push(StartupCall {
                        target: *cb,
                        args: vec![d, 1, 0],
                        check: false,
                    });
                }
                if m.entry != 0 {
                    calls.push(StartupCall {
                        target: m.entry,
                        args: vec![d, 1, 0],
                        check: true,
                    });
                }
            }
            run_init_calls(base, calls, 0)
        }
        Err(e) => {
            if crate::loader::is_windows_system_dll(name) {
                s.missing(
                    "LoadLibrary",
                    format!("{name} is part of the emulated Windows installation but is not provided"),
                );
            } else {
                s.event(Severity::Info, "LoadLibrary", format!("{name}: {e}"));
            }
            s.set_last_error(ERROR_MOD_NOT_FOUND);
            Ok(ApiRet::Ret(0))
        }
    }
}

fn load_library_a(s: &mut Session) -> ApiResult {
    let [p] = s.args::<1>()?;
    let name = s.read_str(p)?;
    load_library_common(s, &name)
}
fn load_library_w(s: &mut Session) -> ApiResult {
    let [p] = s.args::<1>()?;
    let name = s.read_wstr(p)?;
    load_library_common(s, &name)
}
fn load_library_ex_a(s: &mut Session) -> ApiResult {
    let [p, _f, flags] = s.args::<3>()?;
    let name = s.read_str(p)?;
    if flags & 0x2 != 0 {
        s.missing("LoadLibraryEx", format!("LOAD_LIBRARY_AS_DATAFILE for {name}"));
    }
    load_library_common(s, &name)
}

fn free_library(s: &mut Session) -> ApiResult {
    let [h] = s.args::<1>()?;
    match s.modules.list.get_mut(&h) {
        Some(m) => {
            // Modules stay mapped; the reference count is tracked for GetModuleHandle.
            m.refs = m.refs.saturating_sub(1);
            Ok(ApiRet::Ret(1))
        }
        None => {
            s.set_last_error(ERROR_MOD_NOT_FOUND);
            Ok(ApiRet::Ret(0))
        }
    }
}

fn disable_thread_library_calls(s: &mut Session) -> ApiResult {
    let [h] = s.args::<1>()?;
    if let Some(m) = s.modules.list.get_mut(&h) {
        m.thread_calls = false;
    }
    Ok(ApiRet::Ret(1))
}

fn get_proc_address(s: &mut Session) -> ApiResult {
    let [h, p] = s.args::<2>()?;
    let imp = if p < 0x10000 {
        ImportName::Ordinal(p as u16)
    } else {
        ImportName::Name {
            hint: 0,
            name: s.read_str(p)?,
        }
    };
    let h = if h == 0 { s.process.image_base } else { h };
    if s.modules.get(h).is_none() {
        s.set_last_error(ERROR_MOD_NOT_FOUND);
        return Ok(ApiRet::Ret(0));
    }
    match loader::resolve_export(s, h, &imp, 0) {
        Some(a) => Ok(ApiRet::Ret(a)),
        None => {
            let m = s.modules.get(h).unwrap();
            let (mn, builtin) = (m.name.clone(), m.builtin);
            s.event(
                if builtin { Severity::Missing } else { Severity::Info },
                "GetProcAddress",
                format!("{mn}!{imp} not exported"),
            );
            s.set_last_error(ERROR_PROC_NOT_FOUND);
            Ok(ApiRet::Ret(0))
        }
    }
}

// Resources
pub(crate) fn res_id(s: &Session, p: u32) -> Result<ResId, ApiError> {
    if p < 0x10000 {
        return Ok(ResId::Id(p));
    }
    let name = s.read_str(p)?;
    if let Some(n) = name.strip_prefix('#')
        && let Ok(v) = n.parse()
    {
        return Ok(ResId::Id(v));
    }
    Ok(ResId::Name(name.to_uppercase()))
}

pub fn find_resource(s: &mut Session, hmod: u32, ty: ResId, name: ResId) -> Option<(u32, u32)> {
    let hmod = if hmod == 0 { s.process.image_base } else { hmod };
    let m = s.modules.get(hmod)?;
    let e = m.resources.iter().find(|r| r.ty == ty && r.name == name)?;
    Some((hmod + e.data_rva, e.size))
}

pub(crate) fn find_resource_a(s: &mut Session) -> ApiResult {
    let [h, name, ty] = s.args::<3>()?;
    let name = res_id(s, name)?;
    let ty = res_id(s, ty)?;
    match find_resource(s, h, ty, name) {
        Some((ptr, size)) => {
            let rec = s.sys_alloc(8)?;
            s.mem.write_u32(rec, ptr)?;
            s.mem.write_u32(rec + 4, size)?;
            Ok(ApiRet::Ret(rec))
        }
        None => {
            s.set_last_error(ERROR_RESOURCE_NAME_NOT_FOUND);
            Ok(ApiRet::Ret(0))
        }
    }
}
fn load_resource(s: &mut Session) -> ApiResult {
    let [_h, r] = s.args::<2>()?;
    if r == 0 {
        return Ok(ApiRet::Ret(0));
    }
    Ok(ApiRet::Ret(s.mem.read_u32(r)?))
}
fn lock_resource(s: &mut Session) -> ApiResult {
    let [p] = s.args::<1>()?;
    Ok(ApiRet::Ret(p))
}
fn sizeof_resource(s: &mut Session) -> ApiResult {
    let [_h, r] = s.args::<2>()?;
    if r == 0 {
        return Ok(ApiRet::Ret(0));
    }
    Ok(ApiRet::Ret(s.mem.read_u32(r + 4)?))
}
fn free_resource(_s: &mut Session) -> ApiResult {
    Ok(ApiRet::Ret(0))
}

// ---------------------------------------------------------------- time

fn get_tick_count(s: &mut Session) -> ApiResult {
    let res = s.winmm.timer_resolution_ns();
    let t = s.uptime_ns() / res * res;
    Ok(ApiRet::Ret((t / 1_000_000) as u32))
}

fn query_performance_counter(s: &mut Session) -> ApiResult {
    let [p] = s.args::<1>()?;
    let v = (s.uptime_ns() as u128 * QPC_FREQ as u128 / 1_000_000_000) as u64;
    s.mem.write_u64(p, v)?;
    Ok(ApiRet::Ret(1))
}
fn query_performance_frequency(s: &mut Session) -> ApiResult {
    let [p] = s.args::<1>()?;
    s.mem.write_u64(p, QPC_FREQ)?;
    Ok(ApiRet::Ret(1))
}

pub fn filetime_to_systemtime(ft: u64) -> [u16; 8] {
    let total_ms = ft / 10_000;
    let ms = (total_ms % 1000) as u16;
    let secs = total_ms / 1000;
    let days = (secs / 86400) as i64;
    let rem = secs % 86400;
    // Days since 1601-01-01 → civil date (proleptic Gregorian).
    let z = days - 134_774 + 719_468; // shift to 1970 epoch, then to 0000-03-01
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    let dow = ((days + 1).rem_euclid(7)) as u16; // 1601-01-01 was a Monday
    [
        y as u16,
        m as u16,
        dow,
        d as u16,
        (rem / 3600) as u16,
        ((rem / 60) % 60) as u16,
        (rem % 60) as u16,
        ms,
    ]
}

pub fn systemtime_to_filetime(st: &[u16; 8]) -> Option<u64> {
    let (y, m, d) = (st[0] as i64, st[1] as i64, st[3] as i64);
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) || y < 1601 {
        return None;
    }
    let y2 = if m <= 2 { y - 1 } else { y };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days_1970 = era * 146_097 + doe - 719_468;
    let days = days_1970 + 134_774;
    let secs = days as u64 * 86400 + st[4] as u64 * 3600 + st[5] as u64 * 60 + st[6] as u64;
    Some(secs * 10_000_000 + st[7] as u64 * 10_000)
}

pub(crate) fn write_systemtime(s: &mut Session, p: u32, st: [u16; 8]) -> Result<(), ApiError> {
    for (i, v) in st.iter().enumerate() {
        s.mem.write_u16(p + 2 * i as u32, *v)?;
    }
    Ok(())
}
fn read_systemtime(s: &Session, p: u32) -> Result<[u16; 8], ApiError> {
    let mut st = [0u16; 8];
    for (i, v) in st.iter_mut().enumerate() {
        *v = s.mem.read_u16(p + 2 * i as u32)?;
    }
    Ok(st)
}

impl Session {
    pub fn local_filetime_now(&self) -> u64 {
        let bias = self.config.windows.timezone_bias_min as i64 * 60 * 10_000_000;
        (self.filetime_now() as i64 - bias) as u64
    }
}

fn get_system_time(s: &mut Session) -> ApiResult {
    let [p] = s.args::<1>()?;
    let ft = s.filetime_now();
    write_systemtime(s, p, filetime_to_systemtime(ft))?;
    Ok(ApiRet::Ret(0))
}
fn get_local_time(s: &mut Session) -> ApiResult {
    let [p] = s.args::<1>()?;
    let ft = s.local_filetime_now();
    write_systemtime(s, p, filetime_to_systemtime(ft))?;
    Ok(ApiRet::Ret(0))
}
fn get_system_time_as_file_time(s: &mut Session) -> ApiResult {
    let [p] = s.args::<1>()?;
    let ft = s.filetime_now();
    s.mem.write_u64(p, ft)?;
    Ok(ApiRet::Ret(0))
}
fn file_time_to_system_time(s: &mut Session) -> ApiResult {
    let [f, p] = s.args::<2>()?;
    let ft = s.mem.read_u64(f)?;
    write_systemtime(s, p, filetime_to_systemtime(ft))?;
    Ok(ApiRet::Ret(1))
}
fn system_time_to_file_time(s: &mut Session) -> ApiResult {
    let [p, f] = s.args::<2>()?;
    let st = read_systemtime(s, p)?;
    match systemtime_to_filetime(&st) {
        Some(ft) => {
            s.mem.write_u64(f, ft)?;
            Ok(ApiRet::Ret(1))
        }
        None => {
            s.set_last_error(ERROR_INVALID_PARAMETER);
            Ok(ApiRet::Ret(0))
        }
    }
}
fn file_time_to_local_file_time(s: &mut Session) -> ApiResult {
    let [a, b] = s.args::<2>()?;
    let ft = s.mem.read_u64(a)? as i64;
    let bias = s.config.windows.timezone_bias_min as i64 * 60 * 10_000_000;
    s.mem.write_u64(b, (ft - bias) as u64)?;
    Ok(ApiRet::Ret(1))
}
fn local_file_time_to_file_time(s: &mut Session) -> ApiResult {
    let [a, b] = s.args::<2>()?;
    let ft = s.mem.read_u64(a)? as i64;
    let bias = s.config.windows.timezone_bias_min as i64 * 60 * 10_000_000;
    s.mem.write_u64(b, (ft + bias) as u64)?;
    Ok(ApiRet::Ret(1))
}
fn compare_file_time(s: &mut Session) -> ApiResult {
    let [a, b] = s.args::<2>()?;
    let (x, y) = (s.mem.read_u64(a)?, s.mem.read_u64(b)?);
    Ok(ApiRet::Ret(match x.cmp(&y) {
        std::cmp::Ordering::Less => 0xffff_ffff,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => 1,
    }))
}
fn file_time_to_dos_date_time(s: &mut Session) -> ApiResult {
    let [f, d, t] = s.args::<3>()?;
    let st = filetime_to_systemtime(s.mem.read_u64(f)?);
    let date = ((st[0].saturating_sub(1980)) << 9) | (st[1] << 5) | st[3];
    let time = (st[4] << 11) | (st[5] << 5) | (st[6] / 2);
    s.mem.write_u16(d, date)?;
    s.mem.write_u16(t, time)?;
    Ok(ApiRet::Ret(1))
}

fn get_time_zone_information(s: &mut Session) -> ApiResult {
    let [p] = s.args::<1>()?;
    s.mem.fill(p, 172, 0)?;
    let bias = s.config.windows.timezone_bias_min;
    s.mem.write_u32(p, bias as u32)?;
    let name: Vec<u8> = "Coordinated Universal Time"
        .encode_utf16()
        .flat_map(|c| c.to_le_bytes())
        .collect();
    s.mem.write(p + 4, &name)?;
    s.mem.write(p + 88, &name)?;
    Ok(ApiRet::Ret(0)) // TIME_ZONE_ID_UNKNOWN: no daylight transitions
}

fn mul_div(s: &mut Session) -> ApiResult {
    let [a, b, c] = s.args::<3>()?;
    let (a, b, c) = (a as i32 as i64, b as i32 as i64, c as i32 as i64);
    if c == 0 {
        return Ok(ApiRet::Ret(0xffff_ffff));
    }
    let p = a * b;
    // Round half away from zero.
    let q = if (p < 0) != (c < 0) {
        (p - c / 2) / c
    } else {
        (p + c / 2) / c
    };
    if q > i32::MAX as i64 || q < i32::MIN as i64 {
        return Ok(ApiRet::Ret(0xffff_ffff));
    }
    Ok(ApiRet::Ret(q as i32 as u32))
}

fn is_processor_feature_present(s: &mut Session) -> ApiResult {
    let [f] = s.args::<1>()?;
    // Pentium III profile: CMPXCHG8B, MMX, SSE and RDTSC; no SSE2/3DNow!/PAE.
    Ok(ApiRet::Ret(matches!(f, 2 | 3 | 6 | 8) as u32))
}

fn get_computer_name_a(s: &mut Session) -> ApiResult {
    let [buf, psize] = s.args::<2>()?;
    let n = s.mem.read_u32(psize)?;
    let name = s.config.windows.computer_name.clone();
    match s.write_str_buf(buf, n, &name)? {
        Ok(len) => {
            s.mem.write_u32(psize, len)?;
            Ok(ApiRet::Ret(1))
        }
        Err(need) => {
            s.mem.write_u32(psize, need)?;
            s.set_last_error(111); // ERROR_BUFFER_OVERFLOW
            Ok(ApiRet::Ret(0))
        }
    }
}

// Runtime-internal module.
fn noop_internal(_s: &mut Session) -> ApiResult {
    Ok(ApiRet::Raw)
}

pub static DRYDOCK_EXPORTS: &[ExportDef] = &[
    export!("CallbackReturn", 0, noop_internal),
    export!("ThreadExit", 0, noop_internal),
    export!("ThreadStart", 2, loader::thread_start),
    export!("ExceptionDispatcher", 0, noop_internal),
    export!("MmTimerThread", 1, crate::dll::winmm::mm_timer_thread),
];

pub static KERNEL32_EXPORTS: &[ExportDef] = &[
    export!("CloseHandle", 1, close_handle),
    export!("CompareFileTime", 2, compare_file_time),
    export!("CreateDirectoryA", 2, create_directory_a),
    export!("CreateEventA", 4, create_event_a),
    export!("CreateFileA", 7, create_file_a),
    export!("CreateMutexA", 3, create_mutex_a),
    export!("CreateProcessA", 10, create_process_a),
    export!("CreateSemaphoreA", 4, create_semaphore_a),
    export!("CreateThread", 6, create_thread),
    export!("DebugBreak", 0, debug_break),
    export!("DeleteCriticalSection", 1, delete_critical_section),
    export!("DeleteFileA", 1, delete_file_a),
    export!("DisableThreadLibraryCalls", 1, disable_thread_library_calls),
    export!("DuplicateHandle", 7, duplicate_handle),
    export!("EnterCriticalSection", 1, enter_critical_section),
    export!("ExitProcess", 1, exit_process),
    export!("ExitThread", 1, exit_thread),
    export!("FileTimeToDosDateTime", 3, file_time_to_dos_date_time),
    export!("FileTimeToLocalFileTime", 2, file_time_to_local_file_time),
    export!("FileTimeToSystemTime", 2, file_time_to_system_time),
    export!("FindClose", 1, find_close),
    export!("FindFirstFileA", 2, find_first_file_a),
    export!("FindNextFileA", 2, find_next_file_a),
    export!("FindResourceA", 3, find_resource_a),
    export!("FlushFileBuffers", 1, flush_file_buffers),
    export!("FreeEnvironmentStringsA", 1, free_environment_strings),
    export!("FreeEnvironmentStringsW", 1, free_environment_strings),
    export!("FreeLibrary", 1, free_library),
    export!("FreeResource", 1, free_resource),
    export!("GetACP", 0, get_acp),
    export!("GetCPInfo", 2, get_cp_info),
    export!("GetCommandLineA", 0, get_command_line_a),
    export!("GetCommandLineW", 0, get_command_line_w),
    export!("GetComputerNameA", 2, get_computer_name_a),
    export!("GetCurrentDirectoryA", 2, get_current_directory_a),
    export!("GetCurrentProcess", 0, get_current_process),
    export!("GetCurrentProcessId", 0, get_current_process_id),
    export!("GetCurrentThread", 0, get_current_thread),
    export!("GetCurrentThreadId", 0, get_current_thread_id),
    export!("GetDiskFreeSpaceA", 5, get_disk_free_space_a),
    export!("GetDriveTypeA", 1, get_drive_type_a),
    export!("GetEnvironmentStrings", 0, get_environment_strings_a),
    export!("GetEnvironmentStringsA", 0, get_environment_strings_a),
    export!("GetEnvironmentStringsW", 0, get_environment_strings_w),
    export!("GetEnvironmentVariableA", 3, get_environment_variable_a),
    export!("GetExitCodeProcess", 2, get_exit_code_process),
    export!("GetExitCodeThread", 2, get_exit_code_thread),
    export!("GetFileAttributesA", 1, get_file_attributes_a),
    export!("GetFileSize", 2, get_file_size),
    export!("GetFileTime", 4, get_file_time),
    export!("GetFileType", 1, get_file_type),
    export!("GetFullPathNameA", 4, get_full_path_name_a),
    export!("GetLastError", 0, get_last_error),
    export!("GetLocalTime", 1, get_local_time),
    export!("GetLocaleInfoA", 4, get_locale_info_a),
    export!("GetLogicalDrives", 0, get_logical_drives),
    export!("GetModuleFileNameA", 3, get_module_file_name_a),
    export!("GetModuleHandleA", 1, get_module_handle_a),
    export!("GetModuleHandleW", 1, get_module_handle_w),
    export!("GetOEMCP", 0, get_oemcp),
    export!("GetPriorityClass", 1, get_priority_class),
    export!("GetProcAddress", 2, get_proc_address),
    export!("GetProcessHeap", 0, get_process_heap),
    export!("GetStartupInfoA", 1, get_startup_info_a),
    export!("GetStdHandle", 1, get_std_handle),
    export!("GetStringTypeA", 5, get_string_type_a),
    export!("GetStringTypeExA", 5, get_string_type_ex_a),
    export!("GetStringTypeW", 4, get_string_type_w),
    export!("GetSystemDefaultLCID", 0, get_user_default_lcid),
    export!("GetSystemDefaultLangID", 0, get_system_default_lang_id),
    export!("GetSystemDirectoryA", 2, get_system_directory_a),
    export!("GetSystemInfo", 1, get_system_info),
    export!("GetSystemTime", 1, get_system_time),
    export!("GetSystemTimeAsFileTime", 1, get_system_time_as_file_time),
    export!("GetTempPathA", 2, get_temp_path_a),
    export!("GetThreadLocale", 0, get_user_default_lcid),
    export!("GetThreadPriority", 1, get_thread_priority),
    export!("GetTickCount", 0, get_tick_count),
    export!("GetTimeZoneInformation", 1, get_time_zone_information),
    export!("GetUserDefaultLCID", 0, get_user_default_lcid),
    export!("GetUserDefaultLangID", 0, get_system_default_lang_id),
    export!("GetVersion", 0, get_version),
    export!("GetVersionExA", 1, get_version_ex_a),
    export!("GetVolumeInformationA", 8, get_volume_information_a),
    export!("GetWindowsDirectoryA", 2, get_windows_directory_a),
    export!("GlobalAddAtomA", 1, crate::dll::user32_misc::global_add_atom_a),
    export!("GlobalAlloc", 2, global_alloc),
    export!("GlobalDeleteAtom", 1, crate::dll::user32_misc::global_delete_atom),
    export!("GlobalFindAtomA", 1, crate::dll::user32_misc::global_find_atom_a),
    export!("GlobalGetAtomNameA", 3, crate::dll::user32_misc::global_get_atom_name_a),
    export!("GlobalFlags", 1, global_flags),
    export!("GlobalFree", 1, global_free),
    export!("GlobalHandle", 1, global_handle),
    export!("GlobalLock", 1, global_lock),
    export!("GlobalMemoryStatus", 1, global_memory_status),
    export!("GlobalReAlloc", 3, global_realloc),
    export!("GlobalSize", 1, global_size),
    export!("GlobalUnlock", 1, global_unlock),
    export!("HeapAlloc", 3, heap_alloc),
    export!("HeapCompact", 2, heap_compact),
    export!("HeapCreate", 3, heap_create),
    export!("HeapDestroy", 1, heap_destroy),
    export!("HeapFree", 3, heap_free),
    export!("HeapReAlloc", 4, heap_realloc),
    export!("HeapSize", 3, heap_size),
    export!("HeapValidate", 3, heap_validate),
    export!("InitializeCriticalSection", 1, initialize_critical_section),
    export!(
        "InitializeCriticalSectionAndSpinCount",
        2,
        initialize_critical_section_and_spin_count
    ),
    export!("InterlockedCompareExchange", 3, interlocked_compare_exchange),
    export!("InterlockedDecrement", 1, interlocked_decrement),
    export!("InterlockedExchange", 2, interlocked_exchange),
    export!("InterlockedExchangeAdd", 2, interlocked_exchange_add),
    export!("InterlockedIncrement", 1, interlocked_increment),
    export!("IsBadCodePtr", 1, is_bad_code_ptr),
    export!("IsBadReadPtr", 2, is_bad_read_ptr),
    export!("IsBadStringPtrA", 2, is_bad_string_ptr_a),
    export!("IsBadWritePtr", 2, is_bad_write_ptr),
    export!("IsDBCSLeadByte", 1, is_dbcs_lead_byte),
    export!("IsProcessorFeaturePresent", 1, is_processor_feature_present),
    export!("IsValidCodePage", 1, is_valid_code_page),
    export!("LCMapStringA", 6, lc_map_string_a),
    export!("LCMapStringW", 6, lc_map_string_w),
    export!("LeaveCriticalSection", 1, leave_critical_section),
    export!("LoadLibraryA", 1, load_library_a),
    export!("LoadLibraryExA", 3, load_library_ex_a),
    export!("LoadLibraryW", 1, load_library_w),
    export!("LoadResource", 2, load_resource),
    export!("LocalAlloc", 2, global_alloc),
    export!("LocalFileTimeToFileTime", 2, local_file_time_to_file_time),
    export!("LocalFree", 1, global_free),
    export!("LocalLock", 1, global_lock),
    export!("LocalReAlloc", 3, global_realloc),
    export!("LocalSize", 1, global_size),
    export!("LocalUnlock", 1, global_unlock),
    export!("LockResource", 1, lock_resource),
    export!("MoveFileA", 2, move_file_a),
    export!("MulDiv", 3, mul_div),
    export!("MultiByteToWideChar", 6, multi_byte_to_wide_char),
    export!("CompareStringA", 6, compare_string_a),
    export!("CompareStringW", 6, compare_string_w),
    export!("CopyFileA", 3, copy_file_a),
    export!("OpenMutexA", 3, open_mutex_a),
    export!("OutputDebugStringA", 1, output_debug_string_a),
    export!("PulseEvent", 1, pulse_event),
    export!("QueryPerformanceCounter", 1, query_performance_counter),
    export!("QueryPerformanceFrequency", 1, query_performance_frequency),
    export!("RaiseException", 4, crate::seh::raise_exception),
    export!("ReadFile", 5, read_file),
    export!("ReleaseMutex", 1, release_mutex),
    export!("ReleaseSemaphore", 3, release_semaphore),
    export!("RemoveDirectoryA", 1, remove_directory_a),
    export!("ResetEvent", 1, reset_event),
    export!("ResumeThread", 1, resume_thread),
    export!("RtlUnwind", 4, crate::seh::rtl_unwind),
    export!("SetConsoleCtrlHandler", 2, set_console_ctrl_handler),
    export!("SetCurrentDirectoryA", 1, set_current_directory_a),
    export!("SetEndOfFile", 1, set_end_of_file),
    export!("SetEnvironmentVariableA", 2, set_environment_variable_a),
    export!("SetErrorMode", 1, set_error_mode),
    export!("SetEvent", 1, set_event),
    export!("SetFileAttributesA", 2, set_file_attributes_a),
    export!("SetFilePointer", 4, set_file_pointer),
    export!("SetHandleCount", 1, set_handle_count),
    export!("SetLastError", 1, set_last_error),
    export!("SetPriorityClass", 2, set_priority_class),
    export!("SetStdHandle", 2, set_std_handle),
    export!("SetThreadPriority", 2, set_thread_priority),
    export!("SetUnhandledExceptionFilter", 1, set_unhandled_exception_filter),
    export!("SizeofResource", 2, sizeof_resource),
    export!("Sleep", 1, sleep),
    export!("SleepEx", 2, sleep_ex),
    export!("SuspendThread", 1, suspend_thread),
    export!("SystemTimeToFileTime", 2, system_time_to_file_time),
    export!("TerminateProcess", 2, terminate_process),
    export!("TerminateThread", 2, terminate_thread),
    export!("TlsAlloc", 0, tls_alloc),
    export!("TlsFree", 1, tls_free),
    export!("TlsGetValue", 1, tls_get_value),
    export!("TlsSetValue", 2, tls_set_value),
    export!("TryEnterCriticalSection", 1, try_enter_critical_section),
    export!("UnhandledExceptionFilter", 1, crate::seh::unhandled_exception_filter),
    export!("VirtualAlloc", 4, virtual_alloc),
    export!("VirtualFree", 3, virtual_free),
    export!("VirtualProtect", 4, virtual_protect),
    export!("VirtualQuery", 3, virtual_query),
    export!("WaitForMultipleObjects", 4, wait_for_multiple_objects),
    export!("WaitForSingleObject", 2, wait_for_single_object),
    export!("WideCharToMultiByte", 8, wide_char_to_multi_byte),
    export!("WriteFile", 5, write_file),
    export!("lstrcatA", 2, lstrcat_a),
    export!("lstrcmpA", 2, lstrcmp_a),
    export!("lstrcmpiA", 2, lstrcmpi_a),
    export!("lstrcpyA", 2, lstrcpy_a),
    export!("lstrcpynA", 3, lstrcpyn_a),
    export!("lstrlenA", 1, lstrlen_a),
    export!("lstrlenW", 1, lstrlen_w),
];
