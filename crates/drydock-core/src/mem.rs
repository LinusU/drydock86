//! Guest address space.
//!
//! The guest owns a 32-bit address space made of *regions* (Windows "allocations"). Each
//! region is backed by a page-aligned host buffer which is mapped into Unicorn with
//! `uc_mem_map_ptr`, so Rust code and translated guest code observe the same bytes.
//! Rust never dereferences a guest pointer directly: every access goes through
//! [`AddressSpace`] which validates the region, page state and protection.
//!
//! Writes performed by Rust (API implementations, loaders, debugger writes) bypass
//! Unicorn's self-modifying-code tracking, so they are recorded in a pending
//! invalidation list that the CPU flushes (`uc_ctl_remove_cache`) before resuming.

use serde::{Deserialize, Serialize};
use std::alloc::{Layout, alloc_zeroed, dealloc};
use std::collections::BTreeMap;

pub const PAGE_SIZE: u32 = 0x1000;
pub const ALLOC_GRANULARITY: u32 = 0x10000;

// Win32 PAGE_* protection constants.
pub const PAGE_NOACCESS: u32 = 0x01;
pub const PAGE_READONLY: u32 = 0x02;
pub const PAGE_READWRITE: u32 = 0x04;
pub const PAGE_WRITECOPY: u32 = 0x08;
pub const PAGE_EXECUTE: u32 = 0x10;
pub const PAGE_EXECUTE_READ: u32 = 0x20;
pub const PAGE_EXECUTE_READWRITE: u32 = 0x40;
pub const PAGE_EXECUTE_WRITECOPY: u32 = 0x80;
pub const PAGE_GUARD: u32 = 0x100;
pub const PAGE_NOCACHE: u32 = 0x200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Access {
    pub read: bool,
    pub write: bool,
    pub exec: bool,
}

/// Decode a PAGE_* value into access rights.
///
/// The emulated CPU profile has no NX bit (32-bit non-PAE x86, as on the Windows
/// 9x/2000 machines of the era), so any readable page is also executable.
pub fn decode_protect(protect: u32) -> Access {
    let p = protect & 0xff;
    let (read, write) = match p {
        PAGE_NOACCESS => (false, false),
        PAGE_READONLY | PAGE_EXECUTE | PAGE_EXECUTE_READ => (true, false),
        PAGE_READWRITE | PAGE_WRITECOPY | PAGE_EXECUTE_READWRITE | PAGE_EXECUTE_WRITECOPY => {
            (true, true)
        }
        _ => (false, false),
    };
    Access {
        read,
        write,
        exec: read,
    }
}

/// Unicorn protection for a Win32 page protection.
///
/// Without DEP every readable page is executable for the guest (see
/// [`decode_protect`]). Only pages with an explicit `PAGE_EXECUTE*` protection are
/// mapped executable up front, though: the JIT sends every store to an executable
/// page through code-invalidation checks. Other readable pages gain execute
/// permission on their first instruction fetch ([`AddressSpace::grant_exec`]).
pub fn protect_to_uc(protect: u32) -> unicorn_engine::Prot {
    use unicorn_engine::Prot;
    let a = decode_protect(protect);
    let mut p = Prot::NONE;
    if a.read {
        p |= Prot::READ;
    }
    if a.write {
        p |= Prot::WRITE;
    }
    if a.exec && protect & 0xf0 != 0 {
        p |= Prot::EXEC;
    }
    p
}

/// The `PAGE_EXECUTE*` protection with the same read/write access.
fn with_execute(protect: u32) -> u32 {
    let base = match protect & 0xff {
        PAGE_READONLY => PAGE_EXECUTE_READ,
        PAGE_READWRITE => PAGE_EXECUTE_READWRITE,
        PAGE_WRITECOPY => PAGE_EXECUTE_WRITECOPY,
        other => other,
    };
    (protect & !0xff) | base
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PageState {
    Reserved,
    Committed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Page {
    pub state: PageState,
    pub protect: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RegionKind {
    /// Mapped executable image (guest PE module).
    Image,
    /// VirtualAlloc / heap / stack.
    Private,
    /// Runtime-owned system memory (TEB, PEB, thunk modules, COM objects).
    System,
}

#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MemError {
    #[error("guest address {addr:#010x} is not mapped")]
    Unmapped { addr: u32 },
    #[error("guest address {addr:#010x} is not committed")]
    NotCommitted { addr: u32 },
    #[error("guest address {addr:#010x} is not {what}")]
    Protection { addr: u32, what: &'static str },
    #[error("address range {addr:#010x}+{len:#x} overlaps an existing region")]
    Overlap { addr: u32, len: u32 },
    #[error("no free address range of {len:#x} bytes")]
    NoSpace { len: u32 },
    #[error("invalid argument: {0}")]
    Invalid(&'static str),
    #[error("unicorn error {0}")]
    Unicorn(String),
}

impl MemError {
    pub fn addr(&self) -> Option<u32> {
        match self {
            MemError::Unmapped { addr }
            | MemError::NotCommitted { addr }
            | MemError::Protection { addr, .. }
            | MemError::Overlap { addr, .. } => Some(*addr),
            _ => None,
        }
    }
}

pub type MemResult<T> = Result<T, MemError>;

/// Page-aligned, zero-initialised host allocation with a stable address.
pub struct HostBuf {
    ptr: *mut u8,
    len: usize,
}

unsafe impl Send for HostBuf {}

impl HostBuf {
    pub fn new(len: usize) -> Self {
        assert!(len > 0 && len % PAGE_SIZE as usize == 0);
        let layout = Layout::from_size_align(len, PAGE_SIZE as usize).unwrap();
        let ptr = unsafe { alloc_zeroed(layout) };
        assert!(!ptr.is_null(), "host allocation of {len} bytes failed");
        HostBuf { ptr, len }
    }
    pub fn as_ptr(&self) -> *mut u8 {
        self.ptr
    }
    pub fn as_slice(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
    }
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.len) }
    }
}

impl Drop for HostBuf {
    fn drop(&mut self) {
        let layout = Layout::from_size_align(self.len, PAGE_SIZE as usize).unwrap();
        unsafe { dealloc(self.ptr, layout) };
    }
}

pub struct Region {
    pub base: u32,
    pub size: u32,
    pub kind: RegionKind,
    pub alloc_protect: u32,
    pub tag: String,
    pub pages: Vec<Page>,
    pub host: HostBuf,
}

impl Region {
    fn page(&self, addr: u32) -> &Page {
        &self.pages[((addr - self.base) / PAGE_SIZE) as usize]
    }
    pub fn end(&self) -> u64 {
        self.base as u64 + self.size as u64
    }
    pub fn contains(&self, addr: u32) -> bool {
        addr >= self.base && (addr as u64) < self.end()
    }
}

/// A pending change to Unicorn's view of memory, applied by the CPU before resuming.
#[derive(Debug, Clone)]
pub enum MapOp {
    Map {
        base: u32,
        size: u32,
        ptr: usize,
    },
    Unmap {
        base: u32,
        size: u32,
    },
    Protect {
        base: u32,
        size: u32,
        protect: u32,
        committed: bool,
    },
}

/// Summary of a region, for inspection/diagnostics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegionInfo {
    pub base: u32,
    pub size: u32,
    pub kind: RegionKind,
    pub tag: String,
    pub committed_bytes: u32,
}

pub struct AddressSpace {
    regions: BTreeMap<u32, Region>,
    /// Map/protect operations not yet reflected in Unicorn.
    pub pending_map: Vec<MapOp>,
    /// Ranges written by Rust that may contain translated code.
    pub pending_invalidate: Vec<(u32, u32)>,
    /// Lowest address considered for automatic placement.
    pub alloc_floor: u32,
    /// Exclusive upper bound for automatic placement (user-mode limit).
    pub alloc_ceiling: u32,
}

impl Default for AddressSpace {
    fn default() -> Self {
        Self::new()
    }
}

fn round_up(v: u64, a: u64) -> u64 {
    v.div_ceil(a) * a
}

impl AddressSpace {
    pub fn new() -> Self {
        AddressSpace {
            regions: BTreeMap::new(),
            pending_map: Vec::new(),
            pending_invalidate: Vec::new(),
            alloc_floor: 0x0001_0000,
            alloc_ceiling: 0x7FFE_0000,
        }
    }

    pub fn regions(&self) -> impl Iterator<Item = &Region> {
        self.regions.values()
    }

    pub fn region_infos(&self) -> Vec<RegionInfo> {
        self.regions
            .values()
            .map(|r| RegionInfo {
                base: r.base,
                size: r.size,
                kind: r.kind,
                tag: r.tag.clone(),
                committed_bytes: r
                    .pages
                    .iter()
                    .filter(|p| p.state == PageState::Committed)
                    .count() as u32
                    * PAGE_SIZE,
            })
            .collect()
    }

    pub fn region_at(&self, addr: u32) -> Option<&Region> {
        let (_, r) = self.regions.range(..=addr).next_back()?;
        if r.contains(addr) { Some(r) } else { None }
    }

    fn region_at_mut(&mut self, addr: u32) -> Option<&mut Region> {
        let (_, r) = self.regions.range_mut(..=addr).next_back()?;
        if r.contains(addr) { Some(r) } else { None }
    }

    pub fn is_free(&self, base: u32, size: u32) -> bool {
        let end = base as u64 + size as u64;
        if end > 1u64 << 32 {
            return false;
        }
        if let Some((_, r)) = self.regions.range(..end as u32).next_back()
            && r.end() > base as u64
        {
            return false;
        }
        // range(..end) only examines regions starting below `end`; that suffices.
        true
    }

    /// Find a free, `align`-aligned range of `size` bytes, scanning upward from `floor`.
    pub fn find_free(&self, size: u32, align: u32, floor: u32, ceiling: u32) -> Option<u32> {
        let mut cand = round_up(floor.max(self.alloc_floor) as u64, align as u64);
        for r in self.regions.values() {
            if r.end() <= cand {
                continue;
            }
            if cand + size as u64 <= r.base as u64 {
                break;
            }
            cand = round_up(r.end(), align as u64);
        }
        if cand + size as u64 <= ceiling as u64 {
            Some(cand as u32)
        } else {
            None
        }
    }

    /// Reserve a region. If `base` is `None`, pick a free location.
    pub fn reserve(
        &mut self,
        base: Option<u32>,
        size: u32,
        kind: RegionKind,
        protect: u32,
        tag: &str,
    ) -> MemResult<u32> {
        if size == 0 {
            return Err(MemError::Invalid("zero-sized reservation"));
        }
        let size = round_up(size as u64, PAGE_SIZE as u64) as u32;
        let base = match base {
            Some(b) => {
                if b % PAGE_SIZE != 0 {
                    return Err(MemError::Invalid("unaligned reservation base"));
                }
                if !self.is_free(b, size) {
                    return Err(MemError::Overlap { addr: b, len: size });
                }
                b
            }
            None => self
                .find_free(size, ALLOC_GRANULARITY, self.alloc_floor, self.alloc_ceiling)
                .ok_or(MemError::NoSpace { len: size })?,
        };
        let npages = (size / PAGE_SIZE) as usize;
        let host = HostBuf::new(size as usize);
        self.pending_map.push(MapOp::Map {
            base,
            size,
            ptr: host.as_ptr() as usize,
        });
        self.regions.insert(
            base,
            Region {
                base,
                size,
                kind,
                alloc_protect: protect,
                tag: tag.to_string(),
                pages: vec![
                    Page {
                        state: PageState::Reserved,
                        protect: PAGE_NOACCESS
                    };
                    npages
                ],
                host,
            },
        );
        Ok(base)
    }

    /// Reserve and commit in one step.
    pub fn alloc(
        &mut self,
        base: Option<u32>,
        size: u32,
        kind: RegionKind,
        protect: u32,
        tag: &str,
    ) -> MemResult<u32> {
        let b = self.reserve(base, size, kind, protect, tag)?;
        self.commit(b, size, protect)?;
        Ok(b)
    }

    /// Commit pages `[addr, addr+size)` inside an existing reservation.
    pub fn commit(&mut self, addr: u32, size: u32, protect: u32) -> MemResult<()> {
        let start = addr & !(PAGE_SIZE - 1);
        let end = round_up(addr as u64 + size.max(1) as u64, PAGE_SIZE as u64);
        let r = self
            .region_at_mut(start)
            .ok_or(MemError::Unmapped { addr: start })?;
        if end > r.end() {
            return Err(MemError::Invalid("commit range crosses region end"));
        }
        let first = ((start - r.base) / PAGE_SIZE) as usize;
        let last = ((end - r.base as u64) / PAGE_SIZE as u64) as usize;
        for p in &mut r.pages[first..last] {
            p.state = PageState::Committed;
            p.protect = protect;
        }
        self.pending_map.push(MapOp::Protect {
            base: start,
            size: (end - start as u64) as u32,
            protect,
            committed: true,
        });
        Ok(())
    }

    /// Decommit pages. Contents are discarded (zeroed).
    pub fn decommit(&mut self, addr: u32, size: u32) -> MemResult<()> {
        let start = addr & !(PAGE_SIZE - 1);
        let r = self
            .region_at_mut(start)
            .ok_or(MemError::Unmapped { addr: start })?;
        let end = if size == 0 {
            r.end()
        } else {
            round_up(addr as u64 + size as u64, PAGE_SIZE as u64)
        };
        if end > r.end() {
            return Err(MemError::Invalid("decommit range crosses region end"));
        }
        let first = ((start - r.base) / PAGE_SIZE) as usize;
        let last = ((end - r.base as u64) / PAGE_SIZE as u64) as usize;
        for p in &mut r.pages[first..last] {
            p.state = PageState::Reserved;
            p.protect = PAGE_NOACCESS;
        }
        let off = (start - r.base) as usize;
        let len = (end - start as u64) as usize;
        r.host.as_mut_slice()[off..off + len].fill(0);
        self.pending_map.push(MapOp::Protect {
            base: start,
            size: len as u32,
            protect: PAGE_NOACCESS,
            committed: false,
        });
        self.pending_invalidate.push((start, len as u32));
        Ok(())
    }

    /// Change protection of committed pages; returns the previous protection of the first page.
    pub fn protect(&mut self, addr: u32, size: u32, protect: u32) -> MemResult<u32> {
        let start = addr & !(PAGE_SIZE - 1);
        let end = round_up(addr as u64 + size.max(1) as u64, PAGE_SIZE as u64);
        let r = self
            .region_at_mut(start)
            .ok_or(MemError::Unmapped { addr: start })?;
        if end > r.end() {
            return Err(MemError::Invalid("protect range crosses region end"));
        }
        let first = ((start - r.base) / PAGE_SIZE) as usize;
        let last = ((end - r.base as u64) / PAGE_SIZE as u64) as usize;
        if r.pages[first..last]
            .iter()
            .any(|p| p.state != PageState::Committed)
        {
            return Err(MemError::NotCommitted { addr: start });
        }
        let old = r.pages[first].protect;
        for p in &mut r.pages[first..last] {
            p.protect = protect;
        }
        self.pending_map.push(MapOp::Protect {
            base: start,
            size: (end - start as u64) as u32,
            protect,
            committed: true,
        });
        Ok(old)
    }

    /// Release an entire region.
    pub fn release(&mut self, base: u32) -> MemResult<()> {
        let r = self
            .regions
            .remove(&base)
            .ok_or(MemError::Unmapped { addr: base })?;
        self.pending_map.push(MapOp::Unmap {
            base: r.base,
            size: r.size,
        });
        self.pending_invalidate.push((r.base, r.size));
        Ok(())
    }

    /// Query page info: (region base, page state, protect, run length of identical pages).
    pub fn query(&self, addr: u32) -> Option<(&Region, Page, u32)> {
        let r = self.region_at(addr)?;
        let idx = ((addr - r.base) / PAGE_SIZE) as usize;
        let p = r.pages[idx];
        let run = r.pages[idx..].iter().take_while(|q| **q == p).count() as u32;
        Some((r, p, run * PAGE_SIZE))
    }

    fn check(&self, addr: u32, len: usize, write: bool) -> MemResult<()> {
        let mut a = addr as u64;
        let end = addr as u64 + len as u64;
        if end > 1u64 << 32 {
            return Err(MemError::Unmapped { addr: u32::MAX });
        }
        while a < end {
            let r = self.region_at(a as u32).ok_or(MemError::Unmapped { addr: a as u32 })?;
            let p = r.page(a as u32);
            if p.state != PageState::Committed {
                return Err(MemError::NotCommitted { addr: a as u32 });
            }
            let acc = decode_protect(p.protect);
            if write && !acc.write {
                return Err(MemError::Protection {
                    addr: a as u32,
                    what: "writable",
                });
            }
            if !write && !acc.read {
                return Err(MemError::Protection {
                    addr: a as u32,
                    what: "readable",
                });
            }
            a = (a & !(PAGE_SIZE as u64 - 1)) + PAGE_SIZE as u64;
        }
        Ok(())
    }

    /// Whether `[addr, addr+len)` is committed and readable (IsBadReadPtr semantics).
    pub fn is_readable(&self, addr: u32, len: u32) -> bool {
        len == 0 || self.check(addr, len as usize, false).is_ok()
    }
    pub fn is_writable(&self, addr: u32, len: u32) -> bool {
        len == 0 || self.check(addr, len as usize, true).is_ok()
    }

    pub fn read(&self, addr: u32, out: &mut [u8]) -> MemResult<()> {
        if out.is_empty() {
            return Ok(());
        }
        self.check(addr, out.len(), false)?;
        self.copy_out(addr, out);
        Ok(())
    }

    fn copy_out(&self, addr: u32, out: &mut [u8]) {
        let mut done = 0usize;
        while done < out.len() {
            let a = addr.wrapping_add(done as u32);
            let r = self.region_at(a).unwrap();
            let off = (a - r.base) as usize;
            let n = (out.len() - done).min(r.size as usize - off);
            out[done..done + n].copy_from_slice(&r.host.as_slice()[off..off + n]);
            done += n;
        }
    }

    /// Guest-semantics write (honours page protection).
    pub fn write(&mut self, addr: u32, data: &[u8]) -> MemResult<()> {
        if data.is_empty() {
            return Ok(());
        }
        self.check(addr, data.len(), true)?;
        self.copy_in(addr, data);
        Ok(())
    }

    /// Privileged write (loader, debugger): requires committed pages but ignores protection.
    pub fn write_force(&mut self, addr: u32, data: &[u8]) -> MemResult<()> {
        if data.is_empty() {
            return Ok(());
        }
        let mut a = addr as u64;
        let end = addr as u64 + data.len() as u64;
        while a < end {
            let r = self.region_at(a as u32).ok_or(MemError::Unmapped { addr: a as u32 })?;
            if r.page(a as u32).state != PageState::Committed {
                return Err(MemError::NotCommitted { addr: a as u32 });
            }
            a = (a & !(PAGE_SIZE as u64 - 1)) + PAGE_SIZE as u64;
        }
        self.copy_in(addr, data);
        Ok(())
    }

    fn copy_in(&mut self, addr: u32, data: &[u8]) {
        let mut done = 0usize;
        while done < data.len() {
            let a = addr.wrapping_add(done as u32);
            let r = self.region_at_mut(a).unwrap();
            let off = (a - r.base) as usize;
            let n = (data.len() - done).min(r.size as usize - off);
            r.host.as_mut_slice()[off..off + n].copy_from_slice(&data[done..done + n]);
            done += n;
        }
        self.note_written(addr, data.len() as u32);
    }

    fn note_written(&mut self, addr: u32, len: u32) {
        if let Some(last) = self.pending_invalidate.last_mut()
            && last.0 as u64 + last.1 as u64 == addr as u64
        {
            last.1 += len;
            return;
        }
        self.pending_invalidate.push((addr, len));
    }

    /// Map the page containing `addr` executable for the JIT after an instruction
    /// fetch faulted there. Returns false when the guest protection forbids
    /// execution (the fault is then genuine).
    pub fn grant_exec(&mut self, addr: u32) -> bool {
        let Some(r) = self.region_at(addr) else {
            return false;
        };
        let i = ((addr - r.base) / PAGE_SIZE) as usize;
        let p = r.pages[i];
        if p.state != PageState::Committed || !decode_protect(p.protect).exec || p.protect & 0xf0 != 0 {
            return false;
        }
        self.pending_map.push(MapOp::Protect {
            base: addr & !(PAGE_SIZE - 1),
            size: PAGE_SIZE,
            protect: with_execute(p.protect),
            committed: true,
        });
        true
    }

    /// Borrow a contiguous readable range within one region.
    pub fn slice(&self, addr: u32, len: u32) -> MemResult<&[u8]> {
        self.check(addr, len as usize, false)?;
        let r = self.region_at(addr).unwrap();
        let off = (addr - r.base) as usize;
        if off + len as usize > r.size as usize {
            return Err(MemError::Invalid("range spans multiple regions"));
        }
        Ok(&r.host.as_slice()[off..off + len as usize])
    }

    /// Mutably borrow a contiguous writable range within one region.
    pub fn slice_mut(&mut self, addr: u32, len: u32) -> MemResult<&mut [u8]> {
        self.check(addr, len as usize, true)?;
        self.note_written(addr, len);
        let r = self.region_at_mut(addr).unwrap();
        let off = (addr - r.base) as usize;
        if off + len as usize > r.size as usize {
            return Err(MemError::Invalid("range spans multiple regions"));
        }
        Ok(&mut r.host.as_mut_slice()[off..off + len as usize])
    }

    pub fn read_u8(&self, addr: u32) -> MemResult<u8> {
        let mut b = [0u8; 1];
        self.read(addr, &mut b)?;
        Ok(b[0])
    }
    pub fn read_u16(&self, addr: u32) -> MemResult<u16> {
        let mut b = [0u8; 2];
        self.read(addr, &mut b)?;
        Ok(u16::from_le_bytes(b))
    }
    pub fn read_u32(&self, addr: u32) -> MemResult<u32> {
        let mut b = [0u8; 4];
        self.read(addr, &mut b)?;
        Ok(u32::from_le_bytes(b))
    }
    pub fn read_i32(&self, addr: u32) -> MemResult<i32> {
        Ok(self.read_u32(addr)? as i32)
    }
    pub fn read_u64(&self, addr: u32) -> MemResult<u64> {
        let mut b = [0u8; 8];
        self.read(addr, &mut b)?;
        Ok(u64::from_le_bytes(b))
    }
    pub fn read_f32(&self, addr: u32) -> MemResult<f32> {
        Ok(f32::from_bits(self.read_u32(addr)?))
    }
    pub fn read_vec(&self, addr: u32, len: u32) -> MemResult<Vec<u8>> {
        let mut v = vec![0u8; len as usize];
        self.read(addr, &mut v)?;
        Ok(v)
    }
    pub fn write_u8(&mut self, addr: u32, v: u8) -> MemResult<()> {
        self.write(addr, &[v])
    }
    pub fn write_u16(&mut self, addr: u32, v: u16) -> MemResult<()> {
        self.write(addr, &v.to_le_bytes())
    }
    pub fn write_u32(&mut self, addr: u32, v: u32) -> MemResult<()> {
        self.write(addr, &v.to_le_bytes())
    }
    pub fn write_u64(&mut self, addr: u32, v: u64) -> MemResult<()> {
        self.write(addr, &v.to_le_bytes())
    }
    pub fn write_f32(&mut self, addr: u32, v: f32) -> MemResult<()> {
        self.write_u32(addr, v.to_bits())
    }
    pub fn fill(&mut self, addr: u32, len: u32, byte: u8) -> MemResult<()> {
        self.slice_mut(addr, len)?.fill(byte);
        Ok(())
    }

    /// Read a NUL-terminated byte string, bounded by `max` bytes.
    pub fn read_cstr(&self, addr: u32, max: u32) -> MemResult<Vec<u8>> {
        let mut out = Vec::new();
        let mut a = addr;
        while (out.len() as u32) < max {
            let b = self.read_u8(a)?;
            if b == 0 {
                break;
            }
            out.push(b);
            a = a.wrapping_add(1);
        }
        Ok(out)
    }

    /// Read a NUL-terminated UTF-16 string, bounded by `max` code units.
    pub fn read_wstr(&self, addr: u32, max: u32) -> MemResult<Vec<u16>> {
        let mut out = Vec::new();
        let mut a = addr;
        while (out.len() as u32) < max {
            let c = self.read_u16(a)?;
            if c == 0 {
                break;
            }
            out.push(c);
            a = a.wrapping_add(2);
        }
        Ok(out)
    }

    /// Search committed readable memory for `needle`; returns up to `limit` addresses.
    pub fn search(&self, needle: &[u8], start: u32, end: u32, limit: usize) -> Vec<u32> {
        let mut hits = Vec::new();
        if needle.is_empty() {
            return hits;
        }
        for r in self.regions.values() {
            if r.end() <= start as u64 || r.base >= end {
                continue;
            }
            // Iterate over runs of committed readable pages.
            let mut i = 0;
            while i < r.pages.len() {
                if r.pages[i].state != PageState::Committed || !decode_protect(r.pages[i].protect).read
                {
                    i += 1;
                    continue;
                }
                let j = i + r.pages[i..]
                    .iter()
                    .take_while(|p| p.state == PageState::Committed && decode_protect(p.protect).read)
                    .count();
                let off = i * PAGE_SIZE as usize;
                let data = &r.host.as_slice()[off..j * PAGE_SIZE as usize];
                for (k, w) in data.windows(needle.len()).enumerate() {
                    if w == needle {
                        let a = r.base + (off + k) as u32;
                        if a >= start && a < end {
                            hits.push(a);
                            if hits.len() >= limit {
                                return hits;
                            }
                        }
                    }
                }
                i = j;
            }
        }
        hits
    }

    /// Serializable snapshot of all regions and committed contents.
    pub fn snapshot(&self) -> Vec<RegionSnapshot> {
        self.regions
            .values()
            .map(|r| {
                let mut chunks = Vec::new();
                let data = r.host.as_slice();
                for (i, p) in r.pages.iter().enumerate() {
                    if p.state != PageState::Committed {
                        continue;
                    }
                    let page = &data[i * PAGE_SIZE as usize..(i + 1) * PAGE_SIZE as usize];
                    if page.iter().all(|&b| b == 0) {
                        continue;
                    }
                    chunks.push(PageData { index: i as u32, bytes: page.to_vec() });
                }
                RegionSnapshot {
                    base: r.base,
                    size: r.size,
                    kind: r.kind,
                    alloc_protect: r.alloc_protect,
                    tag: r.tag.clone(),
                    pages: r.pages.clone(),
                    data: chunks,
                }
            })
            .collect()
    }

    /// Replace the entire address space with a snapshot. All Unicorn mappings are rebuilt.
    pub fn restore(&mut self, snap: &[RegionSnapshot]) {
        for r in self.regions.values() {
            self.pending_map.push(MapOp::Unmap {
                base: r.base,
                size: r.size,
            });
        }
        self.regions.clear();
        for s in snap {
            let mut host = HostBuf::new(s.size as usize);
            for PageData { index, bytes } in &s.data {
                let off = *index as usize * PAGE_SIZE as usize;
                host.as_mut_slice()[off..off + bytes.len()].copy_from_slice(bytes);
            }
            self.pending_map.push(MapOp::Map {
                base: s.base,
                size: s.size,
                ptr: host.as_ptr() as usize,
            });
            // Re-apply page protections as runs.
            let mut i = 0;
            while i < s.pages.len() {
                let p = s.pages[i];
                let n = s.pages[i..].iter().take_while(|q| **q == p).count();
                self.pending_map.push(MapOp::Protect {
                    base: s.base + i as u32 * PAGE_SIZE,
                    size: n as u32 * PAGE_SIZE,
                    protect: p.protect,
                    committed: p.state == PageState::Committed,
                });
                i += n;
            }
            self.regions.insert(
                s.base,
                Region {
                    base: s.base,
                    size: s.size,
                    kind: s.kind,
                    alloc_protect: s.alloc_protect,
                    tag: s.tag.clone(),
                    pages: s.pages.clone(),
                    host,
                },
            );
        }
        self.pending_invalidate.push((0, u32::MAX));
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegionSnapshot {
    pub base: u32,
    pub size: u32,
    pub kind: RegionKind,
    pub alloc_protect: u32,
    pub tag: String,
    pub pages: Vec<Page>,
    /// Contents of the committed non-zero pages.
    pub data: Vec<PageData>,
}

/// One 4 KiB page of a region snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PageData {
    pub index: u32,
    #[serde(with = "serde_bytes")]
    pub bytes: Vec<u8>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reserve_commit_access() {
        let mut m = AddressSpace::new();
        let b = m
            .reserve(None, 0x3000, RegionKind::Private, PAGE_READWRITE, "t")
            .unwrap();
        assert_eq!(b % ALLOC_GRANULARITY, 0);
        assert!(matches!(m.read_u32(b), Err(MemError::NotCommitted { .. })));
        m.commit(b + 0x1000, 4, PAGE_READWRITE).unwrap();
        m.write_u32(b + 0x1ffc, 0xdeadbeef).unwrap();
        assert_eq!(m.read_u32(b + 0x1ffc).unwrap(), 0xdeadbeef);
        // Crossing into an uncommitted page fails without partial writes.
        assert!(m.write_u32(b + 0x1ffe, 1).is_err());
        assert_eq!(m.read_u32(b + 0x1ffc).unwrap(), 0xdeadbeef);
        m.protect(b + 0x1000, 0x1000, PAGE_READONLY).unwrap();
        assert!(m.write_u8(b + 0x1000, 1).is_err());
        m.write_force(b + 0x1000, &[7]).unwrap();
        assert_eq!(m.read_u8(b + 0x1000).unwrap(), 7);
    }

    #[test]
    fn placement_skips_existing() {
        let mut m = AddressSpace::new();
        let a = m
            .alloc(Some(0x10000), 0x20000, RegionKind::Private, PAGE_READWRITE, "a")
            .unwrap();
        let b = m
            .alloc(None, 0x1000, RegionKind::Private, PAGE_READWRITE, "b")
            .unwrap();
        assert_eq!(a, 0x10000);
        assert_eq!(b, 0x30000);
        assert!(m.alloc(Some(0x20000), 0x1000, RegionKind::Private, PAGE_READWRITE, "c").is_err());
        m.release(a).unwrap();
        let c = m
            .alloc(None, 0x1000, RegionKind::Private, PAGE_READWRITE, "c")
            .unwrap();
        assert_eq!(c, 0x10000);
    }

    #[test]
    fn snapshot_roundtrip() {
        let mut m = AddressSpace::new();
        let a = m
            .alloc(None, 0x2000, RegionKind::Private, PAGE_READWRITE, "a")
            .unwrap();
        m.write_u32(a + 0x1004, 42).unwrap();
        let snap = m.snapshot();
        let mut n = AddressSpace::new();
        n.restore(&snap);
        assert_eq!(n.read_u32(a + 0x1004).unwrap(), 42);
        assert_eq!(n.read_u32(a).unwrap(), 0);
    }

    #[test]
    fn search_finds_bytes() {
        let mut m = AddressSpace::new();
        let a = m
            .alloc(None, 0x2000, RegionKind::Private, PAGE_READWRITE, "a")
            .unwrap();
        m.write(a + 0xffe, b"needle").unwrap();
        assert_eq!(m.search(b"needle", 0, u32::MAX, 10), vec![a + 0xffe]);
    }
}
