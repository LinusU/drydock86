//! Structured exception handling (x86 FS:[0] frame chains).
//!
//! CPU faults and RaiseException build an EXCEPTION_RECORD and CONTEXT on the guest
//! stack and walk the registration chain, calling each handler through the ordinary
//! callback mechanism. Dispositions are honoured (continue execution restores the
//! possibly modified CONTEXT; continue search moves to the next frame). RtlUnwind calls
//! handlers with EXCEPTION_UNWINDING and pops frames. Nothing is swallowed: an exception
//! nobody handles halts the session with a full report.

use crate::api::*;
use crate::session::Session;
use drydock_core::Reg;
use drydock_core::cpu::{Fault, FaultKind, Regs};
use drydock_core::mem::MemError;
use serde::{Deserialize, Serialize};

pub const STATUS_ACCESS_VIOLATION: u32 = 0xC000_0005;
pub const STATUS_ILLEGAL_INSTRUCTION: u32 = 0xC000_001D;
pub const STATUS_NONCONTINUABLE_EXCEPTION: u32 = 0xC000_0025;
pub const STATUS_INTEGER_DIVIDE_BY_ZERO: u32 = 0xC000_0094;
pub const STATUS_INTEGER_OVERFLOW: u32 = 0xC000_0095;
pub const STATUS_PRIVILEGED_INSTRUCTION: u32 = 0xC000_0096;
pub const STATUS_BREAKPOINT: u32 = 0x8000_0003;
pub const STATUS_SINGLE_STEP: u32 = 0x8000_0004;
pub const STATUS_ARRAY_BOUNDS_EXCEEDED: u32 = 0xC000_008C;
pub const STATUS_FLOAT_INVALID_OPERATION: u32 = 0xC000_0090;
pub const STATUS_UNWIND: u32 = 0xC000_0027;

pub const EXCEPTION_NONCONTINUABLE: u32 = 1;
pub const EXCEPTION_UNWINDING: u32 = 2;
pub const EXCEPTION_EXIT_UNWIND: u32 = 4;

pub const CONTEXT_SIZE: u32 = 0x2cc;
pub const CONTEXT_FULL_FLAGS: u32 = 0x1003f;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SehCont {
    Dispatch {
        rec: u32,
        ctx: u32,
        frame: u32,
    },
    TopFilter {
        rec: u32,
        ctx: u32,
    },
    Unwind {
        target: u32,
        rec: u32,
        ctx: u32,
        frame: u32,
        retval: u32,
    },
    /// kernel32!UnhandledExceptionFilter forwarded to the top-level filter.
    UefForward,
}

pub fn write_context(s: &mut Session, at: u32, r: &Regs) -> Result<(), MemError> {
    let mut c = vec![0u8; CONTEXT_SIZE as usize];
    let p = |c: &mut Vec<u8>, o: usize, v: u32| c[o..o + 4].copy_from_slice(&v.to_le_bytes());
    p(&mut c, 0x00, CONTEXT_FULL_FLAGS);
    // FLOATING_SAVE_AREA
    p(&mut c, 0x1c, r.fpcw as u32 | 0xffff_0000);
    p(&mut c, 0x20, r.fpsw as u32 | 0xffff_0000);
    p(&mut c, 0x24, r.fptag as u32 | 0xffff_0000);
    p(&mut c, 0x28, r.fip);
    p(&mut c, 0x30, r.fdp);
    let top = ((r.fpsw >> 11) & 7) as usize;
    for i in 0..8 {
        let phys = (top + i) & 7;
        let o = 0x38 + i * 10;
        c[o..o + 10].copy_from_slice(&r.st[phys]);
    }
    p(&mut c, 0x8c, 0); // GS
    p(&mut c, 0x90, drydock_core::cpu::FS_SELECTOR as u32);
    p(&mut c, 0x94, 0x23);
    p(&mut c, 0x98, 0x23);
    p(&mut c, 0x9c, r.edi);
    p(&mut c, 0xa0, r.esi);
    p(&mut c, 0xa4, r.ebx);
    p(&mut c, 0xa8, r.edx);
    p(&mut c, 0xac, r.ecx);
    p(&mut c, 0xb0, r.eax);
    p(&mut c, 0xb4, r.ebp);
    p(&mut c, 0xb8, r.eip);
    p(&mut c, 0xbc, 0x1b);
    p(&mut c, 0xc0, r.eflags);
    p(&mut c, 0xc4, r.esp);
    p(&mut c, 0xc8, 0x23);
    // ExtendedRegisters (FXSAVE image): FCW/FSW, MXCSR, XMM0-7.
    let x = 0xcc;
    c[x..x + 2].copy_from_slice(&r.fpcw.to_le_bytes());
    c[x + 2..x + 4].copy_from_slice(&r.fpsw.to_le_bytes());
    p(&mut c, x + 24, r.mxcsr);
    for (i, xm) in r.xmm.iter().enumerate() {
        let o = x + 160 + i * 16;
        c[o..o + 16].copy_from_slice(xm);
    }
    s.mem.write(at, &c)
}

pub fn read_context(s: &Session, at: u32, base: &Regs) -> Result<Regs, MemError> {
    let c = s.mem.read_vec(at, CONTEXT_SIZE)?;
    let g = |o: usize| u32::from_le_bytes(c[o..o + 4].try_into().unwrap());
    let flags = g(0);
    let mut r = base.clone();
    if flags & 2 != 0 {
        r.edi = g(0x9c);
        r.esi = g(0xa0);
        r.ebx = g(0xa4);
        r.edx = g(0xa8);
        r.ecx = g(0xac);
        r.eax = g(0xb0);
    }
    if flags & 1 != 0 {
        r.ebp = g(0xb4);
        r.eip = g(0xb8);
        r.eflags = g(0xc0);
        r.esp = g(0xc4);
    }
    if flags & 8 != 0 {
        r.fpcw = g(0x1c) as u16;
        r.fpsw = g(0x20) as u16;
        r.fptag = g(0x24) as u16;
        let top = ((r.fpsw >> 11) & 7) as usize;
        for i in 0..8 {
            let phys = (top + i) & 7;
            let o = 0x38 + i * 10;
            r.st[phys].copy_from_slice(&c[o..o + 10]);
        }
    }
    if flags & 0x20 != 0 {
        let x = 0xcc;
        r.mxcsr = g(x + 24);
        for (i, xm) in r.xmm.iter_mut().enumerate() {
            let o = x + 160 + i * 16;
            xm.copy_from_slice(&c[o..o + 16]);
        }
    }
    Ok(r)
}

fn fault_code(f: &Fault) -> (u32, Vec<u32>, u32) {
    // (code, parameters, exception address)
    match &f.kind {
        FaultKind::ReadUnmapped | FaultKind::ReadProt => (STATUS_ACCESS_VIOLATION, vec![0, f.addr], f.eip),
        FaultKind::WriteUnmapped | FaultKind::WriteProt => (STATUS_ACCESS_VIOLATION, vec![1, f.addr], f.eip),
        FaultKind::FetchUnmapped | FaultKind::FetchProt => (STATUS_ACCESS_VIOLATION, vec![0, f.addr], f.addr),
        FaultKind::InvalidInsn => (STATUS_ILLEGAL_INSTRUCTION, vec![], f.eip),
        FaultKind::Exception(0) => (STATUS_INTEGER_DIVIDE_BY_ZERO, vec![], f.eip),
        FaultKind::Exception(1) => (STATUS_SINGLE_STEP, vec![], f.eip),
        FaultKind::Exception(3) => (STATUS_BREAKPOINT, vec![0], f.eip.wrapping_sub(1)),
        FaultKind::Exception(4) => (STATUS_INTEGER_OVERFLOW, vec![], f.eip),
        FaultKind::Exception(5) => (STATUS_ARRAY_BOUNDS_EXCEEDED, vec![], f.eip),
        FaultKind::Exception(6) => (STATUS_ILLEGAL_INSTRUCTION, vec![], f.eip),
        FaultKind::Exception(13) => (STATUS_PRIVILEGED_INSTRUCTION, vec![], f.eip),
        FaultKind::Exception(16) => (STATUS_FLOAT_INVALID_OPERATION, vec![], f.eip),
        FaultKind::Exception(_) => (STATUS_ACCESS_VIOLATION, vec![0, 0], f.eip),
    }
}

/// Deliver a CPU fault to the guest's SEH chain.
pub fn raise_fault(s: &mut Session, f: Fault) {
    let (code, params, addr) = fault_code(&f);
    let mut regs = s.cpu.read_regs();
    if let FaultKind::Exception(3) = f.kind {
        // The breakpoint context points at the int3 instruction.
        regs.eip = addr;
    } else if matches!(f.kind, FaultKind::Exception(_)) {
        // Faults (as opposed to traps) report the faulting instruction.
        regs.eip = f.eip;
    }
    s.event(
        crate::diag::Severity::Info,
        "exception",
        format!("code {code:#010x} at {addr:#010x} params {params:x?}"),
    );
    let api = s.thunks.find("DRYDOCK.DLL", "ExceptionDispatcher").unwrap();
    let r = dispatch_exception(s, code, 0, &params, addr, &regs);
    s.finish_api(api, r);
}

/// Raise an access violation for an invalid pointer passed to API `api`.
pub fn raise_access_violation_in_api(s: &mut Session, api: u32, e: &MemError) {
    let addr = e.addr().unwrap_or(0);
    let mut regs = s.cpu.read_regs();
    regs.eip = api;
    let disp = s.thunks.find("DRYDOCK.DLL", "ExceptionDispatcher").unwrap();
    let rw = matches!(e, MemError::Protection { what: "writable", .. }) as u32;
    let r = dispatch_exception(s, STATUS_ACCESS_VIOLATION, 0, &[rw, addr], api, &regs);
    s.finish_api(disp, r);
}

/// Build EXCEPTION_RECORD + CONTEXT below `ctx_regs.esp` and call the first handler.
pub fn dispatch_exception(
    s: &mut Session,
    code: u32,
    flags: u32,
    params: &[u32],
    addr: u32,
    ctx_regs: &Regs,
) -> ApiResult {
    let mut sp = (ctx_regs.esp.wrapping_sub(0x100)) & !0xf;
    sp -= CONTEXT_SIZE + 4;
    let ctx = sp & !0xf;
    let rec = ctx - 0x60;
    write_context(s, ctx, ctx_regs)?;
    let mut r = vec![0u8; 0x50];
    let p = |r: &mut Vec<u8>, o: usize, v: u32| r[o..o + 4].copy_from_slice(&v.to_le_bytes());
    p(&mut r, 0, code);
    p(&mut r, 4, flags);
    p(&mut r, 0xc, addr);
    p(&mut r, 0x10, params.len().min(15) as u32);
    for (i, v) in params.iter().take(15).enumerate() {
        p(&mut r, 0x14 + i * 4, *v);
    }
    s.mem.write(rec, &r)?;
    s.cpu.set_reg(Reg::ESP, rec - 0x20);
    let teb = s.sched.cur().teb;
    let head = s.mem.read_u32(teb)?;
    next_handler(s, rec, ctx, head)
}

fn frame_valid(s: &Session, frame: u32) -> bool {
    let t = s.sched.cur();
    frame & 3 == 0 && frame >= t.stack_region && frame.wrapping_add(8) <= t.stack_top
}

fn next_handler(s: &mut Session, rec: u32, ctx: u32, frame: u32) -> ApiResult {
    if frame == 0xffff_ffff || !frame_valid(s, frame) {
        return unhandled(s, rec, ctx);
    }
    let handler = s.mem.read_u32(frame + 4)?;
    Ok(ApiRet::call(
        handler,
        vec![rec.into(), frame.into(), ctx.into(), 0u32.into()],
        Cont::Seh(SehCont::Dispatch { rec, ctx, frame }),
    ))
}

fn unhandled(s: &mut Session, rec: u32, ctx: u32) -> ApiResult {
    let filter = s.process.unhandled_filter;
    if filter != 0 {
        let mut ep = Vec::new();
        ep.extend(rec.to_le_bytes());
        ep.extend(ctx.to_le_bytes());
        return Ok(ApiRet::call(
            filter,
            vec![CallArg::Buf(ep)],
            Cont::Seh(SehCont::TopFilter { rec, ctx }),
        ));
    }
    crash(s, rec)
}

fn crash(s: &mut Session, rec: u32) -> ApiResult {
    let code = s.mem.read_u32(rec)?;
    let addr = s.mem.read_u32(rec + 0xc)?;
    let p0 = s.mem.read_u32(rec + 0x14)?;
    let p1 = s.mem.read_u32(rec + 0x18)?;
    let module = s
        .modules
        .containing(addr)
        .map(|m| format!("{}+{:#x}", m.name, addr - m.base))
        .unwrap_or_else(|| "?".into());
    Err(ApiError::Fatal(format!(
        "unhandled exception {code:#010x} at {addr:#010x} ({module}), params [{p0:#x}, {p1:#x}]"
    )))
}

fn continue_with_context(s: &mut Session, ctx: u32) -> ApiResult {
    let base = s.cpu.read_regs();
    let r = read_context(s, ctx, &base)?;
    s.cpu.write_regs(&mut s.mem, &r).map_err(ApiError::Fatal)?;
    Ok(ApiRet::Raw)
}

pub fn resume(s: &mut Session, c: SehCont, v: Resume) -> ApiResult {
    match c {
        SehCont::Dispatch { rec, ctx, frame } => match v.eax() {
            0 => {
                let flags = s.mem.read_u32(rec + 4)?;
                if flags & EXCEPTION_NONCONTINUABLE != 0 {
                    let code = s.mem.read_u32(rec)?;
                    return Err(ApiError::Fatal(format!(
                        "handler continued non-continuable exception {code:#x}"
                    )));
                }
                continue_with_context(s, ctx)
            }
            _ => {
                let next = s.mem.read_u32(frame)?;
                next_handler(s, rec, ctx, next)
            }
        },
        SehCont::TopFilter { rec, ctx } => match v.eax() as i32 {
            -1 => continue_with_context(s, ctx),
            _ => crash(s, rec),
        },
        SehCont::Unwind {
            target,
            rec,
            ctx,
            frame,
            retval,
        } => {
            let teb = s.sched.cur().teb;
            let next = s.mem.read_u32(frame)?;
            s.mem.write_u32(teb, next)?;
            unwind_step(s, target, rec, ctx, next, retval)
        }
        SehCont::UefForward => Ok(ApiRet::Ret(v.eax())),
    }
}

/// RtlUnwind(TargetFrame, TargetIp, ExceptionRecord, ReturnValue)
pub fn rtl_unwind(s: &mut Session) -> ApiResult {
    let [target, _target_ip, rec_in, retval] = s.args::<4>()?;
    let rec = if rec_in != 0 {
        rec_in
    } else {
        let mut r = vec![0u8; 0x50];
        r[0..4].copy_from_slice(&STATUS_UNWIND.to_le_bytes());
        let p = s.cpu.esp() - 0x80;
        s.mem.write(p, &r)?;
        p
    };
    let fl = s.mem.read_u32(rec + 4)?;
    let add = if target == 0 {
        EXCEPTION_UNWINDING | EXCEPTION_EXIT_UNWIND
    } else {
        EXCEPTION_UNWINDING
    };
    s.mem.write_u32(rec + 4, fl | add)?;
    // A context describing the RtlUnwind call site (handlers may inspect it).
    let ctx = s.cpu.esp() - 0x80 - CONTEXT_SIZE - 0x10;
    let regs = s.cpu.read_regs();
    write_context(s, ctx & !0xf, &regs)?;
    let teb = s.sched.cur().teb;
    let head = s.mem.read_u32(teb)?;
    unwind_step(s, target, rec, ctx & !0xf, head, retval)
}

fn unwind_step(s: &mut Session, target: u32, rec: u32, ctx: u32, frame: u32, retval: u32) -> ApiResult {
    if frame == target || frame == 0xffff_ffff || !frame_valid(s, frame) {
        return Ok(ApiRet::Ret(retval));
    }
    let handler = s.mem.read_u32(frame + 4)?;
    Ok(ApiRet::call(
        handler,
        vec![rec.into(), frame.into(), ctx.into(), 0u32.into()],
        Cont::Seh(SehCont::Unwind {
            target,
            rec,
            ctx,
            frame,
            retval,
        }),
    ))
}

/// RaiseException(code, flags, nparams, params)
pub fn raise_exception(s: &mut Session) -> ApiResult {
    let [code, flags, n, pp] = s.args::<4>()?;
    let n = n.min(15);
    let mut params = Vec::new();
    for i in 0..n {
        params.push(s.mem.read_u32(pp + 4 * i)?);
    }
    let caller = s.caller();
    let mut regs = s.cpu.read_regs();
    // Continuing execution returns from RaiseException to its caller.
    regs.eip = caller;
    regs.esp = s.cpu.esp() + 4 + 16;
    let esp_now = s.cpu.esp();
    let r = dispatch_exception(s, code, flags & EXCEPTION_NONCONTINUABLE, &params, caller, &regs);
    let _ = esp_now;
    r
}

/// kernel32!UnhandledExceptionFilter(EXCEPTION_POINTERS*)
pub fn unhandled_exception_filter(s: &mut Session) -> ApiResult {
    let [ep] = s.args::<1>()?;
    let filter = s.process.unhandled_filter;
    if filter != 0 {
        return Ok(ApiRet::call(filter, vec![ep.into()], Cont::Seh(SehCont::UefForward)));
    }
    let rec = s.mem.read_u32(ep)?;
    // No debugger and no top-level filter: Windows would report the crash and terminate.
    crash(s, rec)
}
