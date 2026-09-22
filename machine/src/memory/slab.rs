//! Mapped size-class slab for `GcData` headers.
//!
//! Chunks stay mapped after sweep; freed slots are poisoned (`kind = 0`) and
//! returned to a free list. Lookup is chunk range + slot origin, not a live
//! HashSet. See `docs/internals/heap-identity.md`.

use std::alloc::Layout;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

const CHUNK: usize = 64 * 1024;

struct Chunk {
    ptr: *mut u8,
    meta: PageMeta,
}

struct PageMeta {
    slot_size: u32,
    slot_align: u32,
    first_off: u32,
}

pub struct Slab {
    chunks: Vec<Chunk>,
    /// Free lists keyed by `(slot_size, align)`. A handful of classes; a
    /// linear scan is cheaper than hashing the pair on every alloc.
    free: Vec<((u32, u32), Vec<NonNull<u8>>)>,
    /// Last chunk that contained a lookup. Relaxed: a stale hint misses and
    /// falls back to the scan. Not a synchronization point.
    last_start: AtomicU64,
    last_end: AtomicU64,
    last_idx: AtomicU32,
}

impl Slab {
    pub fn new() -> Self {
        Self {
            chunks: Vec::new(),
            free: Vec::new(),
            last_start: AtomicU64::new(0),
            last_end: AtomicU64::new(0),
            last_idx: AtomicU32::new(0),
        }
    }

    pub fn alloc(&mut self, layout: Layout) -> NonNull<u8> {
        let (slot_size, align) = slot_dims(layout);
        let key = (slot_size as u32, align as u32);
        if let Some((_, slots)) = self.free.iter_mut().find(|(k, _)| *k == key) {
            if let Some(p) = slots.pop() {
                return p;
            }
        }
        self.carve_page(slot_size, align)
    }

    pub fn free(&mut self, ptr: NonNull<u8>) {
        let Some((_, meta)) = self.chunk_for(ptr.as_ptr() as u64) else {
            debug_assert!(false, "slab free of unmapped pointer");
            return;
        };
        let key = (meta.slot_size, meta.slot_align);
        if let Some((_, slots)) = self.free.iter_mut().find(|(k, _)| *k == key) {
            slots.push(ptr);
            return;
        }
        self.free.push((key, vec![ptr]));
    }

    /// Mapped anonymous bytes (chunks stay mapped after sweep).
    pub fn mapped_bytes(&self) -> usize {
        self.chunks.len() * CHUNK
    }

    pub fn chunk_count(&self) -> usize {
        self.chunks.len()
    }

    /// True when `addr` is a slot origin in a mapped chunk (not necessarily
    /// live — poison is the header kind).
    pub fn contains_slot(&self, addr: u64) -> bool {
        self.chunk_for(addr).is_some_and(|(start, meta)| {
            let off = (addr - start) as u32;
            if off < meta.first_off {
                return false;
            }
            let rel = off - meta.first_off;
            rel % meta.slot_size == 0 && (off as usize) + (meta.slot_size as usize) <= CHUNK
        })
    }

    fn carve_page(&mut self, slot_size: usize, align: usize) -> NonNull<u8> {
        let ptr = map_chunk(CHUNK);
        let first = align_up(ptr as usize, align);
        let first_off = first - ptr as usize;
        let end = ptr as usize + CHUNK;
        let key = (slot_size as u32, align as u32);
        let mut p = first;
        let mut first_slot = None;
        let mut rest = Vec::new();
        while p + slot_size <= end {
            let nn = unsafe { NonNull::new_unchecked(p as *mut u8) };
            if first_slot.is_none() {
                first_slot = Some(nn);
            } else {
                rest.push(nn);
            }
            p += slot_size;
        }
        if let Some((_, slots)) = self.free.iter_mut().find(|(k, _)| *k == key) {
            slots.append(&mut rest);
        } else if !rest.is_empty() {
            self.free.push((key, rest));
        }
        self.chunks.push(Chunk {
            ptr,
            meta: PageMeta {
                slot_size: slot_size as u32,
                slot_align: align as u32,
                first_off: first_off as u32,
            },
        });
        first_slot.expect("gc slab chunk smaller than one slot")
    }

    fn chunk_for(&self, addr: u64) -> Option<(u64, &PageMeta)> {
        let start = self.last_start.load(Ordering::Relaxed);
        let end = self.last_end.load(Ordering::Relaxed);
        let idx = self.last_idx.load(Ordering::Relaxed) as usize;
        if start != 0
            && addr >= start
            && addr < end
            && let Some(c) = self.chunks.get(idx)
            && c.ptr as u64 == start
        {
            return Some((start, &c.meta));
        }
        for (i, c) in self.chunks.iter().enumerate() {
            let cstart = c.ptr as u64;
            let cend = cstart + CHUNK as u64;
            if addr >= cstart && addr < cend {
                self.last_start.store(cstart, Ordering::Relaxed);
                self.last_end.store(cend, Ordering::Relaxed);
                self.last_idx.store(i as u32, Ordering::Relaxed);
                return Some((cstart, &c.meta));
            }
        }
        None
    }
}

impl Drop for Slab {
    fn drop(&mut self) {
        for c in &self.chunks {
            unmap(c.ptr, CHUNK);
        }
    }
}

fn slot_dims(layout: Layout) -> (usize, usize) {
    let align = layout.align();
    let size = layout.size().next_multiple_of(align);
    debug_assert!(size > 0 && size <= CHUNK);
    (size, align)
}

fn align_up(addr: usize, align: usize) -> usize {
    debug_assert!(align.is_power_of_two());
    (addr + align - 1) & !(align - 1)
}

#[cfg(unix)]
fn map_chunk(len: usize) -> *mut u8 {
    let ptr = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            len,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
            -1,
            0,
        )
    };
    if ptr == libc::MAP_FAILED {
        std::alloc::handle_alloc_error(Layout::from_size_align(len, 4096).expect("layout"));
    }
    ptr.cast()
}

#[cfg(unix)]
fn unmap(ptr: *mut u8, len: usize) {
    unsafe {
        libc::munmap(ptr.cast(), len);
    }
}

#[cfg(not(unix))]
fn map_chunk(len: usize) -> *mut u8 {
    let layout = Layout::from_size_align(len, 4096).expect("layout");
    let ptr = unsafe { std::alloc::alloc(layout) };
    if ptr.is_null() {
        std::alloc::handle_alloc_error(layout);
    }
    ptr
}

#[cfg(not(unix))]
fn unmap(ptr: *mut u8, len: usize) {
    let layout = Layout::from_size_align(len, 4096).expect("layout");
    unsafe { std::alloc::dealloc(ptr, layout) };
}
