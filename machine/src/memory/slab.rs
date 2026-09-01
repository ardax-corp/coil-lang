//! Mapped size-class slab for `GcData` headers.
//!
//! Chunks stay mapped after sweep; freed slots are poisoned (`kind = 0`) and
//! returned to a free list. Lookup is chunk range + slot origin, not a live
//! HashSet. See `docs/internals/heap-identity.md`.

use std::alloc::Layout;
use std::collections::HashMap;
use std::ptr::NonNull;

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
    free: HashMap<(u32, u32), Vec<NonNull<u8>>>,
}

impl Slab {
    pub fn new() -> Self {
        Self {
            chunks: Vec::new(),
            free: HashMap::new(),
        }
    }

    pub fn alloc(&mut self, layout: Layout) -> NonNull<u8> {
        let (slot_size, align) = slot_dims(layout);
        let key = (slot_size as u32, align as u32);
        if let Some(p) = self.free.get_mut(&key).and_then(|v| v.pop()) {
            return p;
        }
        self.carve_page(slot_size, align)
    }

    pub fn free(&mut self, ptr: NonNull<u8>) {
        let Some((_, meta)) = self.chunk_for(ptr.as_ptr() as u64) else {
            debug_assert!(false, "slab free of unmapped pointer");
            return;
        };
        self.free
            .entry((meta.slot_size, meta.slot_align))
            .or_default()
            .push(ptr);
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
        while p + slot_size <= end {
            let nn = unsafe { NonNull::new_unchecked(p as *mut u8) };
            if first_slot.is_none() {
                first_slot = Some(nn);
            } else {
                self.free.entry(key).or_default().push(nn);
            }
            p += slot_size;
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
        for c in &self.chunks {
            let start = c.ptr as u64;
            if addr >= start && addr < start + CHUNK as u64 {
                return Some((start, &c.meta));
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
