//! Guest threads and the scheduler.
//!
//! There is one virtual CPU. Guest threads are explicit records holding their full
//! register state; the scheduler decides which one runs next. No host thread ever runs
//! guest code on its own. Preemption happens at instruction-count quantum boundaries,
//! so scheduling depends only on guest-visible execution, never on host timing.

use crate::api::{Block, Cont, Frame, Resume};
use crate::objects::ObjId;
use drydock_core::cpu::Regs;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Scheduling quantum in retired instructions.
pub const QUANTUM_INSNS: u64 = 2_000_000;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ThreadState {
    /// Execute guest code from `regs`.
    Ready,
    /// Inside API `api`; continue with `cont` and `val` before executing guest code.
    Resume {
        api: u32,
        cont: Cont,
        val: Resume,
    },
    /// Blocked inside API `api`.
    Blocked {
        api: u32,
        block: Block,
    },
    Terminated,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Thread {
    pub tid: u32,
    pub obj: ObjId,
    pub regs: Regs,
    pub state: ThreadState,
    pub frames: Vec<Frame>,
    pub teb: u32,
    /// Stack region base (lowest address) and top (initial ESP area).
    pub stack_region: u32,
    pub stack_top: u32,
    pub priority: i32,
    pub suspend: u32,
    pub exit_code: Option<u32>,
    pub start_addr: u32,
    /// Instruction count at which this thread's current quantum ends.
    pub quantum_end: u64,
    /// Runtime-internal helper thread (e.g. multimedia timer thread).
    pub internal: bool,
}

impl Thread {
    pub fn runnable(&self) -> bool {
        self.suspend == 0 && matches!(self.state, ThreadState::Ready | ThreadState::Resume { .. })
    }
    pub fn alive(&self) -> bool {
        !matches!(self.state, ThreadState::Terminated)
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Sched {
    pub threads: BTreeMap<u32, Thread>,
    /// Thread whose registers are currently loaded in the CPU.
    pub loaded: Option<u32>,
    /// Thread currently executing (or last executed).
    pub current: Option<u32>,
    pub next_tid: u32,
}

impl Sched {
    pub fn new() -> Self {
        Sched {
            next_tid: 0x100,
            ..Default::default()
        }
    }

    pub fn alloc_tid(&mut self) -> u32 {
        let t = self.next_tid;
        self.next_tid += 4;
        t
    }

    pub fn cur(&self) -> &Thread {
        &self.threads[&self.current.expect("no current thread")]
    }
    pub fn cur_mut(&mut self) -> &mut Thread {
        let t = self.current.expect("no current thread");
        self.threads.get_mut(&t).unwrap()
    }

    /// Choose the next runnable thread: highest priority first, round-robin within a
    /// priority level; the current thread keeps the CPU until its quantum expires.
    pub fn pick(&self, insns: u64) -> Option<u32> {
        let best = self
            .threads
            .values()
            .filter(|t| t.runnable())
            .map(|t| t.priority)
            .max()?;
        if let Some(c) = self.current
            && let Some(t) = self.threads.get(&c)
            && t.runnable()
            && t.priority == best
            && insns < t.quantum_end
        {
            return Some(c);
        }
        let cands: Vec<u32> = self
            .threads
            .values()
            .filter(|t| t.runnable() && t.priority == best)
            .map(|t| t.tid)
            .collect();
        let after = self.current.unwrap_or(0);
        cands
            .iter()
            .copied()
            .find(|&t| t > after)
            .or_else(|| cands.first().copied())
    }

    pub fn live_count(&self) -> usize {
        self.threads.values().filter(|t| t.alive() && !t.internal).count()
    }
}
