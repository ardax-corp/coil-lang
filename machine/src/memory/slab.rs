//! Mapped size-class slab for `GcData` headers.
//!
//! Chunks stay mapped after sweep; freed slots are poisoned (`kind = 0`) and
//! returned to a free list. Lookup is chunk range + slot origin, not a live
//! HashSet. See `docs/internals/heap-identity.md`.

use std::alloc::Layout;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicPtr, AtomicU64, AtomicUsize, Ordering};

const CHUNK: usize = 64 * 1024;

/// Sweep cycles a free slot must stay unused before its empty chunk's pages
/// go back to the OS. Longer than the phase of typical periodic churn, so
/// released chunks are not refaulted a cycle later.
const RELEASE_WINDOW: u32 = 8;

#[cfg(test)]
pub(crate) const RELEASE_WINDOW_FOR_TEST: u32 = RELEASE_WINDOW;

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

/// Bits of a 64 KiB bucket key (`addr >> 16`) resolved by each directory level.
const DIR_BITS: u32 = 16;
const DIR_LEN: usize = 1 << DIR_BITS;
type DirLeaf = [AtomicU64; DIR_LEN];

/// `addr >> 16` → up to two chunk indices (+1; 0 = empty), in two lazily
/// allocated levels. A 64 KiB chunk straddles at most two buckets, so one
/// `u64` per bucket holds every candidate. Zeroed allocations keep untouched
/// pages non-resident. Lookups are two loads instead of a scan of every
/// chunk. Published after the chunk table entry, so concurrent readers
/// (shared-heap workers) only see indices that `ChunkTable::get` resolves.
struct ChunkDir {
    root: AtomicPtr<[AtomicPtr<DirLeaf>; DIR_LEN]>,
    /// Allocated leaves, for `Drop` (writer-only; readers never touch it).
    leaves: std::cell::UnsafeCell<Vec<*mut DirLeaf>>,
}

impl ChunkDir {
    const fn new() -> Self {
        Self {
            root: AtomicPtr::new(std::ptr::null_mut()),
            leaves: std::cell::UnsafeCell::new(Vec::new()),
        }
    }

    fn split(addr: u64) -> Option<(usize, usize)> {
        let key = addr >> 16;
        let hi = (key >> DIR_BITS) as usize;
        (hi < DIR_LEN).then_some((hi, (key as usize) & (DIR_LEN - 1)))
    }

    fn zeroed<T>() -> *mut T {
        let layout = Layout::new::<T>();
        let p = unsafe { std::alloc::alloc_zeroed(layout) };
        if p.is_null() {
            std::alloc::handle_alloc_error(layout);
        }
        p.cast()
    }

    /// Single writer, like [`ChunkTable::push`].
    fn insert(&self, start: u64, index: usize) {
        let mut root = self.root.load(Ordering::Acquire);
        if root.is_null() {
            root = Self::zeroed();
            self.root.store(root, Ordering::Release);
        }
        let tag = index as u64 + 1;
        let mut key_addr = start & !0xffff;
        while key_addr < start + CHUNK as u64 {
            let Some((hi, lo)) = Self::split(key_addr) else {
                return;
            };
            let slot = unsafe { &(*root)[hi] };
            let mut leaf = slot.load(Ordering::Acquire);
            if leaf.is_null() {
                leaf = Self::zeroed();
                // SAFETY: single writer; `leaves` is only read by `Drop`.
                unsafe { (*self.leaves.get()).push(leaf) };
                slot.store(leaf, Ordering::Release);
            }
            let entry = unsafe { &(*leaf)[lo] };
            let cur = entry.load(Ordering::Relaxed);
            let packed = if cur & 0xffff_ffff == 0 {
                cur | tag
            } else {
                debug_assert!(cur >> 32 == 0, "more than two chunks in one 64 KiB bucket");
                cur | (tag << 32)
            };
            entry.store(packed, Ordering::Release);
            key_addr += 1 << 16;
        }
    }

    /// Candidate chunk indices for `addr` (at most two).
    fn candidates(&self, addr: u64) -> [usize; 2] {
        let none = [usize::MAX; 2];
        let root = self.root.load(Ordering::Acquire);
        let Some((hi, lo)) = Self::split(addr) else {
            return none;
        };
        if root.is_null() {
            return none;
        }
        let leaf = unsafe { &(*root)[hi] }.load(Ordering::Acquire);
        if leaf.is_null() {
            return none;
        }
        let e = unsafe { &(*leaf)[lo] }.load(Ordering::Acquire);
        let at = |t: u64| if t == 0 { usize::MAX } else { t as usize - 1 };
        [at(e & 0xffff_ffff), at(e >> 32)]
    }
}

impl Drop for ChunkDir {
    fn drop(&mut self) {
        let root = *self.root.get_mut();
        if root.is_null() {
            return;
        }
        unsafe {
            for &leaf in (*self.leaves.get()).iter() {
                std::alloc::dealloc(leaf.cast(), Layout::new::<DirLeaf>());
            }
            std::alloc::dealloc(root.cast(), Layout::new::<[AtomicPtr<DirLeaf>; DIR_LEN]>());
        }
    }
}

/// Append-only chunk list that never moves published entries: segment `k`
/// holds `SEG0 << k` chunks. Shared-heap workers look up addresses while the
/// lock-holding allocator appends, so a reallocating `Vec` would race.
struct ChunkTable {
    segs: [AtomicPtr<Chunk>; SEGS],
    len: AtomicUsize,
    dir: ChunkDir,
    /// `[lo, hi)` spans every chunk: immediates probed as addresses miss fast.
    lo: AtomicU64,
    hi: AtomicU64,
}

impl ChunkTable {
    fn new() -> Self {
        Self {
            segs: std::array::from_fn(|_| AtomicPtr::new(std::ptr::null_mut())),
            len: AtomicUsize::new(0),
            dir: ChunkDir::new(),
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
        self.dir.insert(start, i);
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

    /// Index of the chunk containing `addr` (the miss path of every
    /// heap-pointer probe): two directory loads, then a range check.
    fn position(&self, addr: u64) -> Option<usize> {
        self.dir.candidates(addr).into_iter().find(|&i| {
            self.get(i).is_some_and(|c| {
                let start = c.ptr as u64;
                addr >= start && addr < start + CHUNK as u64
            })
        })
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

/// Free slots of one `(slot_size, align)` class.
struct FreeClass {
    key: (u32, u32),
    slots: Vec<NonNull<u8>>,
    /// Shortest `slots` got since the last release scan: that many slots
    /// sat unused for the whole cycle.
    low_water: usize,
}

type FreeLists = Vec<FreeClass>;

pub struct Slab {
    chunks: ChunkTable,
    /// Free lists keyed by `(slot_size, align)`. A handful of classes; a
    /// linear scan is cheaper than hashing the pair on every alloc.
    free: FreeLists,
    /// Empty chunks whose pages went back to the OS, by `(slot_size, align)`.
    /// They stay mapped (zero pages read as poisoned headers) and are
    /// re-carved before a new chunk is mapped. Chunk indices.
    released: Vec<((u32, u32), Vec<usize>)>,
    /// Sweep cycles since the last idle scan (see [`RELEASE_WINDOW`]).
    cycles_since_scan: u32,
    /// Entry of the last chunk that contained a lookup. Entries never move,
    /// so one word stays valid and a racing update cannot tear it.
    last: AtomicPtr<Chunk>,
    /// Per chunk index: pages given back (the slot walk skips them so it
    /// does not fault zero pages back in).
    released_mask: Vec<bool>,
}

/// Position of a walk over every slot of the resident chunks (sweep, heap
/// iteration). A slot is live iff its header `kind != 0`: chunks are carved
/// whole and keep their size class, and a freed slot is poisoned.
///
/// The walk runs top-down (last chunk first, high slots first), so a sweep
/// pushes freed slots high-to-low and the LIFO free list hands out the
/// lowest ones first: survivors pack low and high chunks can go idle and be
/// released. Chunks mapped after the walk starts are not visited.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SlotCursor {
    started: bool,
    /// Current chunk index; the next chunk entered is `chunk - 1`.
    chunk: usize,
    /// Slots of the current chunk not returned yet.
    remaining: u64,
    /// Next slot address to return (descending).
    next: u64,
    slot_size: u64,
}

impl Slab {
    pub fn new() -> Self {
        Self {
            chunks: ChunkTable::new(),
            free: Vec::new(),
            released: Vec::new(),
            cycles_since_scan: 0,
            last: AtomicPtr::new(std::ptr::null_mut()),
            released_mask: Vec::new(),
        }
    }

    fn set_released(&mut self, chunk: usize, released: bool) {
        if self.released_mask.len() <= chunk {
            self.released_mask.resize(chunk + 1, false);
        }
        self.released_mask[chunk] = released;
    }

    /// Next slot address of the walk, or `None` past the last chunk. Chunks
    /// mapped after the walk started are visited too.
    #[inline]
    pub fn next_slot(&self, cur: &mut SlotCursor) -> Option<u64> {
        if cur.remaining > 0 {
            let addr = cur.next;
            cur.next = cur.next.wrapping_sub(cur.slot_size);
            cur.remaining -= 1;
            return Some(addr);
        }
        self.enter_next_chunk(cur)
    }

    /// True when this walk will not visit `addr` any more: it already
    /// returned it, or `addr` lies in a chunk mapped after the walk started.
    pub fn walk_passed(&self, cur: &SlotCursor, addr: u64) -> bool {
        if !cur.started {
            return false;
        }
        let Some(i) = self.chunks.position(addr) else {
            return false;
        };
        i > cur.chunk || (i == cur.chunk && (cur.remaining == 0 || addr > cur.next))
    }

    #[inline(never)]
    fn enter_next_chunk(&self, cur: &mut SlotCursor) -> Option<u64> {
        if !cur.started {
            cur.started = true;
            cur.chunk = self.chunks.len();
        }
        loop {
            if cur.chunk == 0 {
                return None;
            }
            cur.chunk -= 1;
            let i = cur.chunk;
            let Some(c) = self.chunks.get(i) else {
                continue;
            };
            if self.released_mask.get(i).copied().unwrap_or(false) {
                continue;
            }
            let base = c.ptr as u64;
            let size = u64::from(c.meta.slot_size);
            let first = base + u64::from(c.meta.first_off);
            let slots = (base + CHUNK as u64 - first) / size;
            if slots == 0 {
                continue;
            }
            let top = first + (slots - 1) * size;
            cur.slot_size = size;
            cur.remaining = slots - 1;
            cur.next = top.wrapping_sub(size);
            return Some(top);
        }
    }

    pub fn alloc(&mut self, layout: Layout) -> NonNull<u8> {
        let (slot_size, align) = slot_dims(layout);
        let key = (slot_size as u32, align as u32);
        if let Some(class) = self.free.iter_mut().find(|c| c.key == key)
            && let Some(p) = class.slots.pop()
        {
            class.low_water = class.low_water.min(class.slots.len());
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
        if let Some(class) = self.free.iter_mut().find(|c| c.key == key) {
            class.slots.push(ptr);
            return;
        }
        self.free.push(FreeClass {
            key,
            slots: vec![ptr],
            low_water: 0,
        });
    }

    /// Mapped anonymous bytes (chunks stay mapped after sweep).
    pub fn mapped_bytes(&self) -> usize {
        self.chunks.len() * CHUNK
    }

    /// Address lies in the span of mapped chunks (cheap reject for values).
    #[inline]
    pub fn may_contain(&self, addr: u64) -> bool {
        self.chunks.may_contain(addr)
    }

    /// `(chunk index, slot size, slots in that chunk)` for a slot address.
    #[cfg(feature = "gc-stats")]
    pub fn slot_meta(&self, addr: u64) -> Option<(usize, usize, usize)> {
        let i = self.chunks.position(addr)?;
        let c = self.chunks.get(i)?;
        let size = c.meta.slot_size as usize;
        let slots = (CHUNK - c.meta.first_off as usize) / size;
        Some((i, size, slots))
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

    /// Give back the pages of chunks the program did not need for the last
    /// [`RELEASE_WINDOW`] sweep cycles. Call once per completed sweep.
    ///
    /// Per size class, `low_water` free slots were never handed out since the
    /// previous scan; only that many slots' worth of *empty* chunks is
    /// released, so steady churn (which drains its free list) keeps
    /// everything and never refaults. Chunks stay mapped: a released page
    /// reads as zeros, so every header in it is poisoned (`kind == 0`) and a
    /// stale or conservative lookup stays defined
    /// (`docs/internals/heap-identity.md`). Returns bytes released.
    pub fn release_idle_chunks(&mut self) -> usize {
        self.cycles_since_scan += 1;
        if self.cycles_since_scan < RELEASE_WINDOW {
            return 0;
        }
        self.cycles_since_scan = 0;
        let idle: Vec<((u32, u32), usize)> = self
            .free
            .iter_mut()
            .map(|c| {
                let idle = c.low_water;
                c.low_water = c.slots.len();
                (c.key, idle)
            })
            .collect();
        if !cfg!(unix) {
            return 0;
        }
        // Skip the scan unless some class idled at least two chunks' worth.
        let chunk_slots = |(size, _): (u32, u32)| CHUNK / size as usize;
        let mut budget: Vec<((u32, u32), usize)> = idle
            .into_iter()
            .map(|(k, n)| (k, n / chunk_slots(k)))
            .filter(|&(_, chunks)| chunks >= 2)
            .collect();
        if budget.is_empty() {
            return 0;
        }
        #[cfg(feature = "gc-stats")]
        eprintln!("gc-stats idle chunks by class: {budget:?}");
        let n = self.chunks.len();
        let mut starts: Vec<(u64, usize)> = Vec::with_capacity(n);
        for i in 0..n {
            if let Some(c) = self.chunks.get(i) {
                starts.push((c.ptr as u64, i));
            }
        }
        starts.sort_unstable();
        let chunk_of = |addr: u64| -> Option<usize> {
            let at = starts.partition_point(|&(s, _)| s <= addr).checked_sub(1)?;
            let (s, i) = starts[at];
            (addr < s + CHUNK as u64).then_some(i)
        };
        let mut free_slots = vec![0usize; n];
        for class in self.free.iter().filter(|c| budget.iter().any(|(k, _)| *k == c.key)) {
            for p in &class.slots {
                if let Some(i) = chunk_of(p.as_ptr() as u64) {
                    free_slots[i] += 1;
                }
            }
        }
        let mut release = vec![false; n];
        let mut released_bytes = 0;
        for (i, &free) in free_slots.iter().enumerate() {
            let Some(c) = self.chunks.get(i) else {
                continue;
            };
            let key = (c.meta.slot_size, c.meta.slot_align);
            let slots = (CHUNK - c.meta.first_off as usize) / c.meta.slot_size as usize;
            if free == 0 || free != slots {
                continue;
            }
            let Some((_, left)) = budget.iter_mut().find(|(k, _)| *k == key) else {
                continue;
            };
            if *left == 0 {
                continue;
            }
            *left -= 1;
            release_pages(c.ptr, CHUNK);
            release[i] = true;
            self.set_released(i, true);
            released_bytes += CHUNK;
            match self.released.iter_mut().find(|(k, _)| *k == key) {
                Some((_, list)) => list.push(i),
                None => self.released.push((key, vec![i])),
            }
        }
        if released_bytes != 0 {
            for class in &mut self.free {
                class
                    .slots
                    .retain(|p| chunk_of(p.as_ptr() as u64).is_none_or(|i| !release[i]));
                class.low_water = class.slots.len();
            }
        }
        released_bytes
    }

    /// Bytes of chunks whose pages were given back and not yet reused.
    #[cfg(any(test, feature = "gc-stats"))]
    pub fn released_bytes(&self) -> usize {
        self.released.iter().map(|(_, l)| l.len()).sum::<usize>() * CHUNK
    }

    fn carve_page(&mut self, slot_size: usize, align: usize) -> NonNull<u8> {
        let key = (slot_size as u32, align as u32);
        if let Some((_, list)) = self.released.iter_mut().find(|(k, _)| *k == key)
            && let Some(i) = list.pop()
            && let Some(c) = self.chunks.get(i)
        {
            // Same size class, so the table's meta still describes it.
            let ptr = c.ptr;
            self.set_released(i, false);
            return self.carve_slots(ptr, slot_size, align, false);
        }
        let ptr = map_chunk(CHUNK);
        self.carve_slots(ptr, slot_size, align, true)
    }

    fn carve_slots(&mut self, ptr: *mut u8, slot_size: usize, align: usize, new_chunk: bool) -> NonNull<u8> {
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
        if let Some(class) = self.free.iter_mut().find(|c| c.key == key) {
            class.slots.append(&mut rest);
        } else if !rest.is_empty() {
            let low_water = rest.len();
            self.free.push(FreeClass {
                key,
                slots: rest,
                low_water,
            });
        }
        if new_chunk {
            self.chunks.push(Chunk {
                ptr,
                meta: PageMeta {
                    slot_size: slot_size as u32,
                    slot_align: align as u32,
                    first_off: first_off as u32,
                },
            });
        }
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

/// Drop a chunk's pages but keep the range mapped: later reads see zeros.
#[cfg(unix)]
fn release_pages(ptr: *mut u8, len: usize) {
    unsafe {
        libc::madvise(ptr.cast(), len, libc::MADV_DONTNEED);
    }
}

#[cfg(not(unix))]
fn release_pages(_ptr: *mut u8, _len: usize) {}

/// Zeroed like `mmap`: an untouched slot must read as a poisoned header
/// (`kind == 0`), or the slot walk would take it for a live object.
#[cfg(not(unix))]
fn map_chunk(len: usize) -> *mut u8 {
    let layout = Layout::from_size_align(len, 4096).expect("layout");
    let ptr = unsafe { std::alloc::alloc_zeroed(layout) };
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
        // Fake, non-overlapping chunk ranges (never dereferenced), offset so
        // they straddle 64 KiB buckets like real `mmap` results do.
        let first = |i: usize| ((i + 1) * CHUNK + 0x1234) as *mut u8;
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
        // The directory resolves every address inside a chunk, and nothing
        // just past the last one.
        for i in [0, 1, SEG0, n - 1] {
            let start = first(i) as u64;
            for addr in [start, start + 1, start + CHUNK as u64 - 1] {
                assert_eq!(t.position(addr), Some(i), "chunk {i} addr {addr:#x}");
            }
        }
        assert_eq!(t.position(first(n - 1) as u64 + CHUNK as u64), None);
        assert_eq!(t.position(0x10), None);
    }
}
