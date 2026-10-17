//! Kernel objects and the process handle table.
//!
//! Objects have stable internal ids (`ObjId`), never host pointers. Guest-visible handles
//! are small multiples of 4 mapped to object ids; several handles may refer to one object
//! (DuplicateHandle) and each handle holds one reference.

use crate::vfs::OpenFile;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub type ObjId = u32;

pub const CURRENT_PROCESS: u32 = 0xffff_ffff;
pub const CURRENT_THREAD: u32 = 0xffff_fffe;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FindState {
    /// Remaining entries (already filtered and sorted) to return.
    pub entries: Vec<crate::vfs::DirEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum KObj {
    Process {
        exit_code: Option<u32>,
    },
    Thread {
        tid: u32,
    },
    Event {
        manual: bool,
        signaled: bool,
    },
    Mutex {
        owner: Option<u32>,
        count: u32,
        abandoned: bool,
    },
    Semaphore {
        count: i32,
        max: i32,
    },
    File(OpenFile),
    Find(FindState),
    /// Pseudo-file for console standard handles (output is captured in diagnostics).
    Console {
        which: u32,
    },
    FileMapping {
        file: Option<ObjId>,
        size: u32,
    },
}

impl KObj {
    pub fn type_name(&self) -> &'static str {
        match self {
            KObj::Process { .. } => "Process",
            KObj::Thread { .. } => "Thread",
            KObj::Event { .. } => "Event",
            KObj::Mutex { .. } => "Mutex",
            KObj::Semaphore { .. } => "Semaphore",
            KObj::File(_) => "File",
            KObj::Find(_) => "FindFile",
            KObj::Console { .. } => "Console",
            KObj::FileMapping { .. } => "FileMapping",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObjEntry {
    pub obj: KObj,
    pub refs: u32,
    pub name: Option<String>,
}

/// The handle does not name an open object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidHandle;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Objects {
    pub objs: BTreeMap<ObjId, ObjEntry>,
    pub handles: BTreeMap<u32, ObjId>,
    next_obj: ObjId,
    next_handle: u32,
}

impl Objects {
    pub fn new() -> Self {
        Objects {
            objs: BTreeMap::new(),
            handles: BTreeMap::new(),
            next_obj: 1,
            next_handle: 0x40,
        }
    }

    pub fn create(&mut self, obj: KObj, name: Option<String>) -> ObjId {
        let id = self.next_obj;
        self.next_obj += 1;
        self.objs.insert(id, ObjEntry { obj, refs: 0, name });
        id
    }

    /// Create a handle referring to `id`.
    pub fn open(&mut self, id: ObjId) -> u32 {
        let h = self.next_handle;
        self.next_handle += 4;
        self.handles.insert(h, id);
        if let Some(e) = self.objs.get_mut(&id) {
            e.refs += 1;
        }
        h
    }

    pub fn create_handle(&mut self, obj: KObj, name: Option<String>) -> (u32, ObjId) {
        let id = self.create(obj, name);
        (self.open(id), id)
    }

    pub fn lookup(&self, h: u32) -> Option<ObjId> {
        self.handles.get(&h).copied()
    }

    pub fn get(&self, id: ObjId) -> Option<&KObj> {
        self.objs.get(&id).map(|e| &e.obj)
    }
    pub fn get_mut(&mut self, id: ObjId) -> Option<&mut KObj> {
        self.objs.get_mut(&id).map(|e| &mut e.obj)
    }

    pub fn by_handle(&self, h: u32) -> Option<&KObj> {
        self.lookup(h).and_then(|id| self.get(id))
    }
    pub fn by_handle_mut(&mut self, h: u32) -> Option<&mut KObj> {
        let id = self.lookup(h)?;
        self.get_mut(id)
    }

    pub fn find_named(&self, name: &str) -> Option<ObjId> {
        self.objs
            .iter()
            .find(|(_, e)| e.name.as_deref() == Some(name))
            .map(|(id, _)| *id)
    }

    /// Add a reference not tied to a handle (e.g. the thread's own reference).
    pub fn add_ref(&mut self, id: ObjId) {
        if let Some(e) = self.objs.get_mut(&id) {
            e.refs += 1;
        }
    }

    pub fn release(&mut self, id: ObjId) -> Option<KObj> {
        let e = self.objs.get_mut(&id)?;
        e.refs = e.refs.saturating_sub(1);
        if e.refs == 0 {
            return self.objs.remove(&id).map(|e| e.obj);
        }
        None
    }

    /// Close a handle; returns the object if this was the last reference.
    pub fn close(&mut self, h: u32) -> Result<Option<KObj>, InvalidHandle> {
        let id = self.handles.remove(&h).ok_or(InvalidHandle)?;
        Ok(self.release(id))
    }
}
