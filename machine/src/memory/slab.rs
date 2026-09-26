//! Mapped size-class slab for `GcData` headers.
//!
//! Chunks stay mapped after sweep; freed slots are poisoned (`kind = 0`) and
//! returned to a free list. Lookup is chunk range + slot origin, not a live
//! HashSet. See `docs/internals/heap-identity.md`.

use std::alloc::Layout;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicPtr, AtomicU64, AtomicUsize, Ordering};

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

const SEGS: usize = 32;
const SEG0: usize = 64;

/// Append-only chunk list that never moves published entries: segment `k`
/// holds `SEG0 << k` chunks. Shared-heap workers look up addresses while the
/// lock-holding allocator appends, so a reallocating `Vec` would race.
struct ChunkTable {
    segs: [AtomicPtr<Chunk>; SEGS],
    len: AtomicUsize,
    /// `[lo, hi)` spans every chunk: immediates probed as addresses miss fast.
    lo: AtomicU64,
    hi: AtomicU64,
}

impl ChunkTable {
    fn new() -> Self {
        Self {
            segs: std::array::from_fn(|_| AtomicPtr::new(std::ptr::null_mut())),
            len: AtomicUsize::new(0),
            lo: AtomicU64::new(u64::MAX),
            hi: AtomicU64::new(0),
        }
    }

    fn may_contain(&self, addr: u64) -> bool {
        addr >= self.lo.load(Ordering::Acquire) && addr < self.hi.load(Ordering::Acquire)
    }

    fn seg_of(i: usize) -> (usize, usize) {
        let k = (usize::BITS - (i / SEG0 + 1).leading_zeros() - 1) as usize;
        (k, i - SEG0 * ((1 << k) - 1))
    }

    fn seg_layout(k: usize) -> Layout {
        Layout::array::<Chunk>(SEG0 << k).expect("chunk segment layout")
    }

    fn len(&self) -> usize {
        self.len.load(Ordering::Acquire)
    }

    fn get(&self, i: usize) -> Option<&Chunk> {
        if i >= self.len() {
            return None;
        }
        let (k, off) = Self::seg_of(i);
        let seg = self.segs[k].load(Ordering::Acquire);
        // Entries below `len` were written before the release store.
        Some(unsafe { &*seg.add(off) })
    }

    /// Single writer (caller holds the heap alloc lock when shared).
    fn push(&mut self, c: Chunk) {
        let i = self.len.load(Ordering::Relaxed);
        let (k, off) = Self::seg_of(i);
        assert!(k < SEGS, "gc slab chunk table full");
        let mut seg = self.segs[k].load(Ordering::Relaxed);
        if seg.is_null() {
            seg = unsafe { std::alloc::alloc(Self::seg_layout(k)) }.cast::<Chunk>();
            if seg.is_null() {
                std::alloc::handle_alloc_error(Self::seg_layout(k));
            }
            self.segs[k].store(seg, Ordering::Release);
        }
        let start = c.ptr as u64;
        unsafe { seg.add(off).write(c) };
        self.lo.fetch_min(start, Ordering::Release);
        self.hi.fetch_max(start + CHUNK as u64, Ordering::Release);
        self.len.store(i + 1, Ordering::Release);
    }

    /// Published entries as per-segment slices (one `len` load).
    fn segments(&self) -> impl Iterator<Item = &[Chunk]> {
        let mut left = self.len();
        (0..SEGS).map_while(move |k| {
            if left == 0 {
                return None;
            }
            let n = left.min(SEG0 << k);
            left -= n;
            let seg = self.segs[k].load(Ordering::Acquire);
            Some(unsafe { std::slice::from_raw_parts(seg, n) })
        })
    }

    /// Index of the chunk containing `addr` (tight per-segment scan: this is
    /// the miss path of every heap-pointer probe).
    fn position(&self, addr: u64) -> Option<usize> {
        let mut base = 0;
        for seg in self.segments() {
            for (j, c) in seg.iter().enumerate() {
                let start = c.ptr as u64;
                if addr >= start && addr < start + CHUNK as u64 {
                    return Some(base + j);
                }
            }
            base += seg.len();
        }
        None
    }
}

impl Drop for ChunkTable {
    fn drop(&mut self) {
        for (k, seg) in self.segs.iter().enumerate() {
            let seg = seg.load(Ordering::Relaxed);
            if !seg.is_null() {
                unsafe { std::alloc::dealloc(seg.cast(), Self::seg_layout(k)) };
            }
        }
    }
}

type FreeLists = Vec<((u32, u32), Vec<NonNull<u8>>)>;

pub struct Slab {
    chunks: ChunkTable,
    /// Free lists keyed by `(slot_size, align)`. A handful of classes; a
    /// linear scan is cheaper than hashing the pair on every alloc.
    free: FreeLists,
    /// Entry of the last chunk that contained a lookup. Entries never move,
    /// so one word stays valid and a racing update cannot tear it.
    last: AtomicPtr<Chunk>,
}

impl Slab {
    pub fn new() -> Self {
        Self {
            chunks: ChunkTable::new(),
            free: Vec::new(),
            last: AtomicPtr::new(std::ptr::null_mut()),
        }
    }

    pub fn alloc(&mut self, layout: Layout) -> NonNull<u8> {
        let (slot_size, align) = slot_dims(layout);
        let key = (slot_size as u32, align as u32);
        if let Some((_, slots)) = self.free.iter_mut().find(|(k, _)| *k == key)
            && let Some(p) = slots.pop() {
                return p;
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
            rel.is_multiple_of(meta.slot_size) && (off as usize) + (meta.slot_size as usize) <= CHUNK
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

    #[inline]
    fn chunk_for(&self, addr: u64) -> Option<(u64, &PageMeta)> {
        let last = self.last.load(Ordering::Relaxed);
        if !last.is_null() {
            let c = unsafe { &*last };
            let start = c.ptr as u64;
            if addr >= start && addr < start + CHUNK as u64 {
                return Some((start, &c.meta));
            }
        }
        if !self.chunks.may_contain(addr) {
            return None;
        }
        self.chunk_for_slow(addr)
    }

    #[inline(never)]
    fn chunk_for_slow(&self, addr: u64) -> Option<(u64, &PageMeta)> {
        let c = self.chunks.get(self.chunks.position(addr)?)?;
        self.last.store(std::ptr::from_ref(c).cast_mut(), Ordering::Relaxed);
        Some((c.ptr as u64, &c.meta))
    }
}

impl Drop for Slab {
    fn drop(&mut self) {
        for c in self.chunks.segments().flatten() {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chunk_table_keeps_entries_across_segment_growth() {
        let mut t = ChunkTable::new();
        let n = SEG0 * 7 + 5;
        let first = |i: usize| (i + 1) as *mut u8;
        for i in 0..n {
            t.push(Chunk {
                ptr: first(i),
                meta: PageMeta { slot_size: 8, slot_align: 8, first_off: 0 },
            });
            if i == 0 {
                let p0: *const Chunk = t.get(0).unwrap();
                assert_eq!(unsafe { (*p0).ptr }, first(0));
            }
        }
        assert_eq!(t.len(), n);
        for i in 0..n {
            assert_eq!(t.get(i).unwrap().ptr, first(i), "entry {i}");
        }
        assert!(t.get(n).is_none());
    }
}
