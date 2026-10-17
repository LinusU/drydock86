//! The ABI boundary between guest code and Rust API implementations.
//!
//! Every Rust-provided export is a 2-byte `int 0x2e` thunk inside a runtime-owned module
//! image. When the guest reaches a thunk, the session looks up its [`ExportDef`] and calls
//! the handler with the guest stack exactly as the caller left it (`[esp]` = return
//! address, `[esp+4]` = first argument).
//!
//! A handler finishes with an [`ApiRet`]:
//! * `Ret`/`Ret64` complete the call using the export's calling convention;
//! * `Call` invokes guest code (window procedures, enumeration callbacks, DllMain…) and
//!   parks an explicit, serializable continuation ([`Cont`]) on the thread's frame stack;
//! * `Block` parks the thread on a wait queue with a continuation for when it wakes.
//!
//! Continuations are plain data so that a thread blocked inside an API, or a callback in
//! progress, can be inspected and checkpointed. There are no host stack frames or
//! closures holding guest state between guest instructions.

use crate::session::Session;
use drydock_core::mem::MemError;
use serde::{Deserialize, Serialize};

pub type Handler = fn(&mut Session) -> ApiResult;
pub type ApiResult = Result<ApiRet, ApiError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Conv {
    /// Callee pops `n` 32-bit argument slots.
    Stdcall(u8),
    /// Caller cleans the stack.
    Cdecl,
}

impl Conv {
    pub fn pop_bytes(self) -> u32 {
        match self {
            Conv::Stdcall(n) => n as u32 * 4,
            Conv::Cdecl => 0,
        }
    }
}

#[derive(Clone, Copy)]
pub struct ExportDef {
    pub name: &'static str,
    pub ordinal: Option<u16>,
    pub conv: Conv,
    pub handler: Option<Handler>,
}

/// `export!("Name", nargs, handler)` for stdcall exports.
#[macro_export]
macro_rules! export {
    ($name:literal, $n:literal, $h:expr) => {
        $crate::api::ExportDef {
            name: $name,
            ordinal: None,
            conv: $crate::api::Conv::Stdcall($n),
            handler: Some($h),
        }
    };
    ($name:literal @ $ord:literal, $n:literal, $h:expr) => {
        $crate::api::ExportDef {
            name: $name,
            ordinal: Some($ord),
            conv: $crate::api::Conv::Stdcall($n),
            handler: Some($h),
        }
    };
}

/// `cdecl!("Name", handler)` for caller-cleanup exports.
#[macro_export]
macro_rules! cdecl {
    ($name:literal, $h:expr) => {
        $crate::api::ExportDef {
            name: $name,
            ordinal: None,
            conv: $crate::api::Conv::Cdecl,
            handler: Some($h),
        }
    };
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CallArg {
    Val(u32),
    /// Copied onto the guest stack below the API frame; the callee receives a pointer.
    Buf(Vec<u8>),
}

impl From<u32> for CallArg {
    fn from(v: u32) -> Self {
        CallArg::Val(v)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GuestCall {
    pub target: u32,
    pub args: Vec<CallArg>,
    pub cont: Cont,
}

#[derive(Debug)]
pub enum ApiRet {
    Ret(u32),
    /// EDX:EAX
    Ret64(u64),
    /// Return with ST(0) pushed (floating-point result) and EAX unchanged.
    RetF64(f64),
    Call(GuestCall),
    Block(Block),
    ExitThread(u32),
    ExitProcess(u32),
    /// The handler has fully set the CPU state (e.g. continuing after an exception).
    Raw,
}

impl ApiRet {
    pub fn call(target: u32, args: Vec<CallArg>, cont: Cont) -> ApiRet {
        ApiRet::Call(GuestCall { target, args, cont })
    }
}

/// What a blocked thread is waiting for.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WaitKind {
    /// Only the deadline (Sleep).
    Sleep,
    /// Kernel objects (object ids, not handles).
    Objects { objs: Vec<u32>, all: bool },
    /// Posted/sent messages or input for this thread's queue, optionally also objects.
    Message { objs: Vec<u32>, wake_mask: u32 },
    /// A contended CRITICAL_SECTION at this guest address.
    CritSec { addr: u32 },
    /// A modal message box waiting for an answer.
    Dialog { id: u32 },
    /// The multimedia timer thread waiting for the next due timer.
    MmTimer,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Block {
    pub wait: WaitKind,
    /// Absolute virtual time (ns) at which the wait times out.
    pub deadline: Option<u64>,
    pub cont: Cont,
}

/// Value delivered to a continuation.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum Resume {
    /// Guest callback returned EAX (and EDX).
    Returned { eax: u32, edx: u32 },
    /// Wait satisfied by object index `i` (WAIT_OBJECT_0 + i), or a message (index == n).
    Signaled(u32),
    TimedOut,
}

impl Resume {
    pub fn eax(&self) -> u32 {
        match self {
            Resume::Returned { eax, .. } => *eax,
            Resume::Signaled(i) => *i,
            Resume::TimedOut => 0x102,
        }
    }
}

/// Explicit continuation state. Each module that calls back into the guest or blocks
/// defines its own variant payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Cont {
    /// Return the callback's EAX as the API result.
    ReturnValue,
    /// Return a fixed value, ignoring the callback result.
    ReturnConst(u32),
    /// Wait completion: return WAIT_OBJECT_0+i / WAIT_TIMEOUT.
    WaitResult,
    /// Thread or process startup sequence.
    Startup(crate::loader::StartupCont),
    Kernel32(crate::dll::kernel32::K32Cont),
    User32(crate::dll::user32::U32Cont),
    Seh(crate::seh::SehCont),
    Com(crate::com::ComCont),
    Winmm(crate::dll::winmm::MmCont),
    Dialog(crate::dll::dialog::DlgCont),
    /// Finish the first continuation, then feed its result to the second.
    Chain(Box<Cont>, Box<Cont>),
}

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    /// The guest passed an invalid pointer; raised in the guest as an access violation.
    #[error("guest memory error: {0}")]
    Mem(#[from] MemError),
    /// The emulator lacks functionality the guest needs. Never converted to success.
    #[error("unimplemented: {0}")]
    Unimplemented(String),
    #[error("fatal: {0}")]
    Fatal(String),
}

pub fn unimpl<T>(what: impl Into<String>) -> Result<T, ApiError> {
    Err(ApiError::Unimplemented(what.into()))
}

/// Frame pushed while a guest callback runs on behalf of an API.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Frame {
    /// Thunk address of the API that issued the callback.
    pub api: u32,
    /// ESP at API entry (pointing at the API's return address).
    pub api_esp: u32,
    /// EBX, ESI, EDI, EBP at API entry.
    pub saved: [u32; 4],
    /// Guest addresses of `CallArg::Buf` copies, in argument order.
    pub bufs: Vec<u32>,
    pub cont: Cont,
}
