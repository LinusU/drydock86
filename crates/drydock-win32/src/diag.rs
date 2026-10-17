//! Structured diagnostics: missing-functionality reports, API traces, guest output.
//!
//! Protocol output and diagnostics are separate: diagnostics accumulate here (bounded)
//! and are returned on request or written as artifacts; nothing is printed to stdout.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, VecDeque};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiTraceEntry {
    pub t_ns: u64,
    pub tid: u32,
    pub api: std::sync::Arc<str>,
    pub args: Vec<u32>,
    pub caller: u32,
    pub ret: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Severity {
    Info,
    Warning,
    /// The guest needed behaviour the emulator does not implement.
    Missing,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiagEvent {
    pub t_ns: u64,
    pub tid: u32,
    pub severity: Severity,
    pub kind: String,
    pub message: String,
    pub eip: u32,
}

/// Why the session stopped executing guest code permanently.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Halt {
    pub reason: String,
    pub detail: String,
    pub tid: u32,
    pub eip: u32,
    /// Caller (return address) when halted inside an API.
    pub caller: u32,
    pub api: Option<String>,
    pub args: Vec<u32>,
    pub recent_apis: Vec<ApiTraceEntry>,
    pub recent_blocks: Vec<u32>,
    pub regs: drydock_core::cpu::Regs,
    pub stack: Vec<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TraceFilter {
    /// Case-insensitive substrings of "MODULE!Name"; empty = all.
    pub include: Vec<String>,
    pub exclude: Vec<String>,
}

impl TraceFilter {
    pub fn matches(&self, name: &str) -> bool {
        let up = name.to_uppercase();
        if self.exclude.iter().any(|e| up.contains(&e.to_uppercase())) {
            return false;
        }
        self.include.is_empty() || self.include.iter().any(|i| up.contains(&i.to_uppercase()))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Diagnostics {
    pub events: VecDeque<DiagEvent>,
    pub event_cap: usize,
    /// Always-on short ring of recent API calls (for halt reports).
    pub recent: VecDeque<ApiTraceEntry>,
    pub recent_cap: usize,
    /// Opt-in filtered API trace.
    pub trace: Option<VecDeque<ApiTraceEntry>>,
    pub trace_cap: usize,
    pub trace_filter: TraceFilter,
    pub api_counts: rustc_hash::FxHashMap<std::sync::Arc<str>, u64>,
    pub missing: BTreeMap<String, u64>,
    pub debug_output: VecDeque<String>,
    /// Explicit debugger mutations (memory/register writes) performed in this session.
    pub mutations: Vec<String>,
    pub halt: Option<Halt>,
}

impl Default for Diagnostics {
    fn default() -> Self {
        Diagnostics {
            events: VecDeque::new(),
            event_cap: 2000,
            recent: VecDeque::new(),
            recent_cap: 64,
            trace: None,
            trace_cap: 0,
            trace_filter: TraceFilter::default(),
            api_counts: Default::default(),
            missing: BTreeMap::new(),
            debug_output: VecDeque::new(),
            mutations: Vec::new(),
            halt: None,
        }
    }
}

impl Diagnostics {
    pub fn event(&mut self, e: DiagEvent) {
        if let Severity::Missing = e.severity {
            *self.missing.entry(e.kind.clone()).or_default() += 1;
        }
        log::debug!("[{}] {}: {}", e.t_ns / 1_000_000, e.kind, e.message);
        if self.events.len() >= self.event_cap {
            self.events.pop_front();
        }
        self.events.push_back(e);
    }

    pub fn record_call(&mut self, e: ApiTraceEntry) {
        *self.api_counts.entry(e.api.clone()).or_default() += 1;
        if let Some(t) = self.trace.as_mut()
            && self.trace_filter.matches(&e.api)
        {
            if t.len() >= self.trace_cap {
                t.pop_front();
            }
            t.push_back(e.clone());
        }
        if self.recent.len() >= self.recent_cap {
            self.recent.pop_front();
        }
        self.recent.push_back(e);
    }

    pub fn debug_string(&mut self, s: String) {
        if self.debug_output.len() >= 1000 {
            self.debug_output.pop_front();
        }
        self.debug_output.push_back(s);
    }
}
