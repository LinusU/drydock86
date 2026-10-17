//! Pacing: drives a session's virtual clock against the host clock (realtime) or as fast
//! as possible (unthrottled). Both modes run the same guest-time semantics; only how
//! long the host waits differs.

use drydock_win32::session::{RunLimit, RunStop, Session};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum, serde::Serialize, serde::Deserialize)]
pub enum Pacing {
    Realtime,
    Unthrottled,
}

pub struct Driver {
    pub s: Session,
    pub pacing: Pacing,
    anchor_wall: Instant,
    anchor_virt: u64,
    /// Host wall time spent inside `advance` (for performance reports).
    pub busy: Duration,
    pub wall_start: Instant,
}

/// Maximum amount realtime pacing lets virtual time lag behind before re-anchoring
/// (avoids fast-forward bursts after slow phases such as loading).
const MAX_LAG_NS: u64 = 100_000_000;

impl Driver {
    pub fn new(s: Session, pacing: Pacing) -> Driver {
        let v = s.clock.now_ns();
        Driver {
            s,
            pacing,
            anchor_wall: Instant::now(),
            anchor_virt: v,
            busy: Duration::ZERO,
            wall_start: Instant::now(),
        }
    }

    pub fn reanchor(&mut self) {
        self.anchor_wall = Instant::now();
        self.anchor_virt = self.s.clock.now_ns();
    }

    fn wall_virt(&self) -> u64 {
        self.anchor_virt + self.anchor_wall.elapsed().as_nanos() as u64
    }

    /// Advance virtual time by `dt_ns`, or until the session reports a stop that needs
    /// attention (breakpoint, exit, halt, presentation when requested).
    pub fn advance(&mut self, dt_ns: u64, stop_on_present: bool) -> RunStop {
        let t0 = Instant::now();
        let target = self.s.clock.now_ns().saturating_add(dt_ns);
        let r = self.advance_to(target, stop_on_present);
        self.busy += t0.elapsed();
        r
    }

    fn advance_to(&mut self, target: u64, stop_on_present: bool) -> RunStop {
        loop {
            let now = self.s.clock.now_ns();
            if now >= target {
                return RunStop::TimeReached;
            }
            let until = match self.pacing {
                Pacing::Unthrottled => target,
                Pacing::Realtime => {
                    let w = self.wall_virt();
                    if w > now + MAX_LAG_NS {
                        self.reanchor();
                    }
                    let w = self.wall_virt();
                    if w <= now {
                        std::thread::sleep(Duration::from_nanos((now - w).min(2_000_000)));
                        continue;
                    }
                    w.min(target)
                }
            };
            let stop = self.s.run(RunLimit {
                until_ns: until,
                max_insns: None,
                stop_on_present,
                jump_idle: self.pacing == Pacing::Unthrottled,
            });
            match stop {
                RunStop::TimeReached => continue,
                RunStop::Idle { next_event } => {
                    // Every guest thread is blocked. Time still passes: jump to the next
                    // event (or the target if nothing is scheduled).
                    let next = next_event.unwrap_or(u64::MAX).min(target);
                    match self.pacing {
                        Pacing::Unthrottled => self.s.clock.advance_to(next),
                        Pacing::Realtime => {
                            let w = self.wall_virt();
                            if next > w {
                                std::thread::sleep(Duration::from_nanos((next - w).min(5_000_000)));
                            }
                            let w = self.wall_virt();
                            self.s.clock.advance_to(next.min(w));
                        }
                    }
                }
                other => return other,
            }
        }
    }
}
