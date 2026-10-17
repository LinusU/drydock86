//! Wait satisfaction for kernel objects, critical sections and message queues.

use crate::api::{Resume, WaitKind};
use crate::objects::{KObj, ObjId};
use crate::sched::ThreadState;
use crate::session::Session;

pub const WAIT_OBJECT_0: u32 = 0;
pub const WAIT_ABANDONED_0: u32 = 0x80;
pub const WAIT_TIMEOUT: u32 = 0x102;
pub const WAIT_FAILED: u32 = 0xffff_ffff;

/// Is `obj` signaled from the point of view of thread `tid`?
pub fn is_signaled(s: &Session, obj: ObjId, tid: u32) -> bool {
    match s.objects.get(obj) {
        Some(KObj::Event { signaled, .. }) => *signaled,
        Some(KObj::Mutex { owner, .. }) => owner.is_none() || *owner == Some(tid),
        Some(KObj::Semaphore { count, .. }) => *count > 0,
        Some(KObj::Thread { tid: t }) => s
            .sched
            .threads
            .get(t)
            .map(|th| matches!(th.state, ThreadState::Terminated))
            .unwrap_or(true),
        Some(KObj::Process { exit_code }) => exit_code.is_some(),
        // File and console handles are signaled when no I/O is pending; all I/O here
        // completes synchronously.
        Some(KObj::File(_) | KObj::Console { .. }) => true,
        _ => false,
    }
}

/// Apply the side effects of a successful wait. Returns true if a mutex was abandoned.
pub fn acquire(s: &mut Session, obj: ObjId, tid: u32) -> bool {
    match s.objects.get_mut(obj) {
        Some(KObj::Event { manual, signaled }) => {
            if !*manual {
                *signaled = false;
            }
            false
        }
        Some(KObj::Mutex {
            owner,
            count,
            abandoned,
        }) => {
            *owner = Some(tid);
            *count += 1;
            std::mem::replace(abandoned, false)
        }
        Some(KObj::Semaphore { count, .. }) => {
            *count -= 1;
            false
        }
        _ => false,
    }
}

pub fn try_satisfy(s: &mut Session, tid: u32, w: &WaitKind) -> Option<Resume> {
    match w {
        WaitKind::Sleep => None,
        WaitKind::Objects { objs, all } => try_objects(s, tid, objs, *all),
        WaitKind::Message { objs, wake_mask } => {
            if let Some(r) = try_objects(s, tid, objs, false) {
                return Some(r);
            }
            if crate::dll::user32::has_input(s, tid, *wake_mask) {
                return Some(Resume::Signaled(objs.len() as u32));
            }
            None
        }
        WaitKind::Dialog { id } => s
            .user
            .dialogs
            .iter()
            .find(|d| d.id == *id)
            .and_then(|d| d.answer)
            .map(|_| Resume::Signaled(0)),
        WaitKind::MmTimer => crate::dll::winmm::timer_due(s).then_some(Resume::Signaled(0)),
        WaitKind::CritSec { addr } => {
            let owner = s.mem.read_u32(addr + 12).ok()?;
            if owner == 0 {
                let _ = s.mem.write_u32(addr + 12, tid);
                let _ = s.mem.write_u32(addr + 8, 1);
                Some(Resume::Signaled(0))
            } else {
                None
            }
        }
    }
}

fn try_objects(s: &mut Session, tid: u32, objs: &[ObjId], all: bool) -> Option<Resume> {
    if objs.is_empty() {
        return None;
    }
    if all {
        if objs.iter().all(|&o| is_signaled(s, o, tid)) {
            let mut abandoned = false;
            for &o in objs {
                abandoned |= acquire(s, o, tid);
            }
            return Some(Resume::Signaled(if abandoned {
                WAIT_ABANDONED_0
            } else {
                WAIT_OBJECT_0
            }));
        }
        return None;
    }
    for (i, &o) in objs.iter().enumerate() {
        if is_signaled(s, o, tid) {
            let ab = acquire(s, o, tid);
            return Some(Resume::Signaled(if ab {
                WAIT_ABANDONED_0 + i as u32
            } else {
                WAIT_OBJECT_0 + i as u32
            }));
        }
    }
    None
}

/// Mutexes owned by a terminating thread become abandoned.
pub fn abandon_mutexes(s: &mut Session, tid: u32) {
    for e in s.objects.objs.values_mut() {
        if let KObj::Mutex {
            owner,
            count,
            abandoned,
        } = &mut e.obj
            && *owner == Some(tid)
        {
            *owner = None;
            *count = 0;
            *abandoned = true;
        }
    }
}
