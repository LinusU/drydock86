//! Win32 heaps (HeapCreate/HeapAlloc) over guest address-space regions.
//!
//! Bookkeeping lives in Rust (not in guest memory), so a guest overrun cannot corrupt
//! allocator metadata. Blocks are 8-byte aligned with an 8-byte gap before each block,
//! similar to the NT heap header layout. Allocation is deterministic best-fit with
//! coalescing of adjacent free ranges.

use drydock_core::mem::{AddressSpace, MemResult, PAGE_READWRITE, RegionKind};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

const HDR: u32 = 8;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Heap {
    pub handle: u32,
    pub flags: u32,
    /// Segment regions (base, size).
    pub segments: Vec<(u32, u32)>,
    /// Allocated blocks: user address → (requested size, reserved span incl. header).
    pub blocks: BTreeMap<u32, (u32, u32)>,
    /// Free ranges: start → length.
    free: BTreeMap<u32, u32>,
    /// (length, start) index of free ranges for best-fit.
    by_size: BTreeSet<(u32, u32)>,
    pub growable: bool,
    pub max_size: u32,
}

impl Heap {
    fn insert_free(&mut self, mut start: u32, mut len: u32) {
        // Coalesce with predecessor.
        if let Some((&ps, &pl)) = self.free.range(..start).next_back()
            && ps + pl == start
        {
            self.free.remove(&ps);
            self.by_size.remove(&(pl, ps));
            start = ps;
            len += pl;
        }
        // Coalesce with successor.
        if let Some(&nl) = self.free.get(&(start + len)) {
            self.free.remove(&(start + len));
            self.by_size.remove(&(nl, start + len));
            len += nl;
        }
        self.free.insert(start, len);
        self.by_size.insert((len, start));
    }

    fn take_free(&mut self, need: u32) -> Option<u32> {
        let &(len, start) = self.by_size.range((need, 0)..).next()?;
        self.by_size.remove(&(len, start));
        self.free.remove(&start);
        if len > need {
            self.insert_free(start + need, len - need);
        }
        Some(start)
    }

    pub fn size_of(&self, addr: u32) -> Option<u32> {
        self.blocks.get(&addr).map(|b| b.0)
    }

    pub fn total_allocated(&self) -> u64 {
        self.blocks.values().map(|b| b.0 as u64).sum()
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Heaps {
    pub heaps: BTreeMap<u32, Heap>,
    pub process_heap: u32,
}

fn span_for(size: u32) -> u32 {
    HDR + size.max(1).div_ceil(8) * 8
}

impl Heaps {
    pub fn create(
        &mut self,
        mem: &mut AddressSpace,
        flags: u32,
        initial: u32,
        max: u32,
    ) -> MemResult<u32> {
        let seg = initial.max(0x10_0000).div_ceil(0x10000) * 0x10000;
        let base = mem.alloc(None, seg, RegionKind::Private, PAGE_READWRITE, "heap")?;
        let mut h = Heap {
            handle: base,
            flags,
            segments: vec![(base, seg)],
            growable: max == 0,
            max_size: max,
            ..Default::default()
        };
        // The first 0x100 bytes stand in for the heap header so the handle points at
        // committed memory, as on Windows.
        h.insert_free(base + 0x100, seg - 0x100);
        self.heaps.insert(base, h);
        Ok(base)
    }

    pub fn alloc(&mut self, mem: &mut AddressSpace, heap: u32, size: u32, zero: bool) -> Option<u32> {
        let need = span_for(size);
        let h = self.heaps.get_mut(&heap)?;
        let start = match h.take_free(need) {
            Some(s) => s,
            None => {
                if !h.growable {
                    return None;
                }
                let seg = (need + 0x1000).max(0x10_0000).div_ceil(0x10000) * 0x10000;
                let base = mem
                    .alloc(None, seg, RegionKind::Private, PAGE_READWRITE, "heap")
                    .ok()?;
                h.segments.push((base, seg));
                h.insert_free(base, seg);
                h.take_free(need)?
            }
        };
        let user = start + HDR;
        h.blocks.insert(user, (size, need));
        if zero {
            let _ = mem.fill(user, size, 0);
        }
        Some(user)
    }

    pub fn free(&mut self, heap: u32, addr: u32) -> bool {
        let Some(h) = self.heaps.get_mut(&heap) else {
            return false;
        };
        match h.blocks.remove(&addr) {
            Some((_, span)) => {
                h.insert_free(addr - HDR, span);
                true
            }
            None => false,
        }
    }

    pub fn realloc(
        &mut self,
        mem: &mut AddressSpace,
        heap: u32,
        addr: u32,
        size: u32,
        zero: bool,
        in_place_only: bool,
    ) -> Option<u32> {
        let h = self.heaps.get_mut(&heap)?;
        let (old_size, old_span) = *h.blocks.get(&addr)?;
        let need = span_for(size);
        if need <= old_span {
            // Shrink (or same span): keep in place, return the tail to the free list.
            if old_span - need >= 16 {
                h.insert_free(addr - HDR + need, old_span - need);
                h.blocks.insert(addr, (size, need));
            } else {
                h.blocks.insert(addr, (size, old_span));
            }
            if zero && size > old_size {
                let _ = mem.fill(addr + old_size, size - old_size, 0);
            }
            return Some(addr);
        }
        // Try to grow in place into an adjacent free range.
        let end = addr - HDR + old_span;
        if let Some(&fl) = h.free.get(&end)
            && old_span + fl >= need
        {
            h.free.remove(&end);
            h.by_size.remove(&(fl, end));
            let extra = old_span + fl - need;
            if extra > 0 {
                h.insert_free(addr - HDR + need, extra);
            }
            h.blocks.insert(addr, (size, need));
            if zero {
                let _ = mem.fill(addr + old_size, size - old_size, 0);
            }
            return Some(addr);
        }
        if in_place_only {
            return None;
        }
        let new = self.alloc(mem, heap, size, zero)?;
        let data = mem.read_vec(addr, old_size).ok()?;
        mem.write(new, &data).ok()?;
        self.free(heap, addr);
        Some(new)
    }

    /// Find the heap owning a block (for GlobalFree/LocalFree style APIs).
    pub fn owner_of(&self, addr: u32) -> Option<u32> {
        self.heaps
            .iter()
            .find(|(_, h)| h.blocks.contains_key(&addr))
            .map(|(k, _)| *k)
    }

    pub fn destroy(&mut self, mem: &mut AddressSpace, heap: u32) -> bool {
        match self.heaps.remove(&heap) {
            Some(h) => {
                for (b, _) in h.segments {
                    let _ = mem.release(b);
                }
                true
            }
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alloc_free_reuse_and_realloc() {
        let mut mem = AddressSpace::new();
        let mut hs = Heaps::default();
        let h = hs.create(&mut mem, 0, 0, 0).unwrap();
        let a = hs.alloc(&mut mem, h, 10, true).unwrap();
        let b = hs.alloc(&mut mem, h, 100, false).unwrap();
        assert_eq!(a % 8, 0);
        assert!(b >= a + 16);
        mem.write(a, b"0123456789").unwrap();
        assert!(hs.free(h, a));
        assert!(!hs.free(h, a));
        let c = hs.alloc(&mut mem, h, 8, false).unwrap();
        assert_eq!(c, a, "freed block is reused");
        mem.write(c, b"abcdefgh").unwrap();
        let d = hs.realloc(&mut mem, h, c, 5000, false, false).unwrap();
        assert_eq!(&mem.read_vec(d, 8).unwrap(), b"abcdefgh");
        assert_eq!(hs.heaps[&h].size_of(d), Some(5000));
        // Large allocations grow the heap with a new segment.
        let big = hs.alloc(&mut mem, h, 3 << 20, false).unwrap();
        assert!(mem.is_writable(big, 3 << 20));
    }
}
