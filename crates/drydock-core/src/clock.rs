//! The single virtual clock.
//!
//! Guest time is an integer count of nanoseconds since session start. It advances only
//! through (a) retired guest instructions, converted with the CPU profile's rational
//! `ns_per_insn`, (b) explicit charges for API/device work, and (c) jumps to the next
//! scheduled event when every guest thread is blocked. Host wall time is never read
//! here; pacing against the host clock happens outside, in the session driver.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CpuProfile {
    pub name: String,
    /// Guest nanoseconds per retired instruction = num / den.
    pub ns_per_insn_num: u64,
    pub ns_per_insn_den: u64,
    /// Time-stamp counter frequency reported via RDTSC.
    pub tsc_hz: u64,
}

impl Default for CpuProfile {
    fn default() -> Self {
        // A Pentium III-class machine at 800 MHz retiring roughly one instruction per cycle.
        CpuProfile {
            name: "pentium3-800".into(),
            ns_per_insn_num: 5,
            ns_per_insn_den: 4,
            tsc_hz: 800_000_000,
        }
    }
}

impl CpuProfile {
    /// A generic x86 profile: `mhz` sets the time-stamp counter rate, `mips` the
    /// sustained instruction rate (one instruction per cycle when equal).
    pub fn with_rates(mhz: u32, mips: u32) -> CpuProfile {
        let mips = mips.max(1) as u64;
        let g = gcd(1000, mips);
        CpuProfile {
            name: format!("x86-{mhz}mhz-{mips}mips"),
            ns_per_insn_num: 1000 / g,
            ns_per_insn_den: mips / g,
            tsc_hz: mhz as u64 * 1_000_000,
        }
    }
}

fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 { a } else { gcd(b, a % b) }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VirtualClock {
    now_ns: u64,
    /// Instructions retired, as charged into the clock.
    pub insns_charged: u64,
    /// Remainder of the instruction→ns conversion (in units of 1/den ns).
    frac: u64,
}

impl VirtualClock {
    pub fn now_ns(&self) -> u64 {
        self.now_ns
    }
    pub fn now_ms(&self) -> u64 {
        self.now_ns / 1_000_000
    }
    /// Charge retired instructions.
    pub fn charge_insns(&mut self, n: u64, p: &CpuProfile) {
        self.insns_charged += n;
        let t = n * p.ns_per_insn_num + self.frac;
        self.now_ns += t / p.ns_per_insn_den;
        self.frac = t % p.ns_per_insn_den;
    }
    /// Charge a fixed amount of virtual time (API/device cost).
    pub fn charge_ns(&mut self, ns: u64) {
        self.now_ns += ns;
    }
    /// Jump forward to `t` (never backwards).
    pub fn advance_to(&mut self, t: u64) {
        if t > self.now_ns {
            self.now_ns = t;
        }
    }
}
