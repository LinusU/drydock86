//! Thunk registry: guest address → Rust export definition.

use crate::api::{Conv, Handler};
use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};

/// Bytes per thunk slot. Only the first two (`int 0x2e`) are executed.
pub const THUNK_SIZE: u32 = 4;
pub const THUNK_BYTES: [u8; 4] = [0xcd, 0x2e, 0xc3, 0xcc];

#[derive(Clone)]
pub struct ThunkInfo {
    pub module: String,
    pub name: String,
    pub ordinal: u16,
    pub conv: Conv,
    pub handler: Option<Handler>,
}

impl ThunkInfo {
    pub fn full_name(&self) -> String {
        let m = self.module.trim_end_matches(".dll").trim_end_matches(".DLL");
        format!("{m}!{}", self.name)
    }
}

/// Serializable description of a thunk (handlers are re-bound by name on restore).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThunkRecord {
    pub addr: u32,
    pub module: String,
    pub name: String,
    pub ordinal: u16,
    pub conv: Conv,
    /// Whether a Rust handler was bound (unbound thunks report "unimplemented").
    pub bound: bool,
}

#[derive(Default, Clone)]
pub struct Thunks {
    pub map: FxHashMap<u32, ThunkInfo>,
    pub callback_ret: u32,
    pub thread_exit: u32,
    pub thread_start: u32,
}

impl Thunks {
    pub fn get(&self, addr: u32) -> Option<&ThunkInfo> {
        self.map.get(&addr)
    }
    pub fn insert(&mut self, addr: u32, info: ThunkInfo) {
        self.map.insert(addr, info);
    }
    pub fn name_of(&self, addr: u32) -> String {
        self.get(addr)
            .map(|t| t.full_name())
            .unwrap_or_else(|| format!("{addr:#x}"))
    }
    pub fn find(&self, module: &str, name: &str) -> Option<u32> {
        self.map
            .iter()
            .find(|(_, t)| t.module.eq_ignore_ascii_case(module) && t.name == name)
            .map(|(a, _)| *a)
    }
    pub fn records(&self) -> Vec<ThunkRecord> {
        let mut v: Vec<_> = self
            .map
            .iter()
            .map(|(a, t)| ThunkRecord {
                addr: *a,
                module: t.module.clone(),
                name: t.name.clone(),
                ordinal: t.ordinal,
                conv: t.conv,
                bound: t.handler.is_some(),
            })
            .collect();
        v.sort_by_key(|r| r.addr);
        v
    }
}
