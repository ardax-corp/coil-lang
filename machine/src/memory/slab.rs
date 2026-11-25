//! Mapped size-class slab for `GcData` headers.
//!
//! Chunks stay mapped after sweep; freed slots are poisoned (`kind = 0`) and
//! returned to a free list. Lookup is chunk range + slot origin, not a live
//! HashSet. See `docs/internals/heap-identity.md`.

use std::alloc::Layout;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicPtr, AtomicU64, AtomicUsize, Ordering};

const CHUNK: usize = 64 * 1024;
/// Entries of [`Slab::chunk_for`]'s recent-chunk cache.
const RECENT: usize = 4096;

struct Chunk {
    ptr: *mut u8,
    meta: PageMeta,
}

struct PageMeta {
    slot_size: u32,
    slot_align: u32,
    first_off: u32,
    /// `ceil(2^64 / slot_size)`: an offset `n` is a multiple of the slot
    /// size iff `n * slot_magic <= slot_magic - 1` (wrapping), a multiply
    /// where `%` divides on every heap-pointer probe.
    slot_magic: u64,
}

impl PageMeta {
    fn new(slot_size: u32, slot_align: u32, first_off: u32) -> Self {
        Self { slot_size, slot_align, first_off, slot_magic: (u64::MAX / u64::from(slot_size)).wrapping_add(1) }
    }

    /// Whether `rel` (an offset from the first slot) starts a slot.
    fn is_slot_offset(&self, rel: u32) -> bool {
        u64::from(rel).wrapping_mul(self.slot_magic) <= self.slot_magic.wrapping_sub(1)
    }
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

/// Offsets in a [`Slab`] of its chunk span's `[lo, hi)` words, which native
/// code reads to reject a word that names no object before a runtime call.
pub(crate) const SLAB_LO_OFFSET: usize = std::mem::offset_of!(Slab, chunks) + std::mem::offset_of!(ChunkTable, lo);
pub(crate) const SLAB_HI_OFFSET: usize = std::mem::offset_of!(Slab, chunks) + std::mem::offset_of!(ChunkTable, hi);

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
    /// Entries of the chunks that last contained a lookup, one per 64 KiB
    /// window of the address (`addr >> 16`, mod [`RECENT`]): a loop reading
    /// strings of a few size classes touches a few chunks, and one entry
    /// alone missed on every switch. Entries never move, so each word stays
    /// valid and a racing update cannot tear it.
    recent: [AtomicPtr<Chunk>; RECENT],
    /// Per chunk index: pages given back (the slot walk skips them so it
    /// does not fault zero pages back in).
    released_mask: Vec<bool>,
    /// Nursery state per size class.
    young: Vec<YoungClass>,
    /// Each nursery class's bump cursor, at [`YoungClass::cursor`]. Boxed so
    /// native code bumps it in place ([`super::heap::InlineLayout`]).
    bumps: Box<[Bump; YOUNG_CLASSES]>,
}

/// Nursery size classes a slab can have: one per nursery object type at
/// most.
pub const YOUNG_CLASSES: usize = 32;

/// A nursery class's bump cursor: the next slot of the current chunk and
/// the end of its last slot. `next == end` (both 0 at first and after a
/// promotion): the next allocation refills. `free` heads the class's
/// freed slots, linked
/// through each slot's first payload word ([`FREE_LINK`]); allocation,
/// native code's inline path too, takes those once the bump pointer
/// runs out, before a refill (#2881).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct Bump {
    pub next: u64,
    pub end: u64,
    pub free: u64,
}

/// Byte offset in a freed young slot of the link to the next one: the
/// first word after the header (every nursery slot is at least 16 bytes).
pub const FREE_LINK: u64 = 8;

/// One size class's nursery: the chunk being bump-allocated and the reset
/// chunks waiting for reuse.
struct YoungClass {
    key: (u32, u32),
    /// Chunks of this class allocated into since the last promotion, the
    /// current one last.
    used: Vec<usize>,
    /// Index of this class's cursor in [`Slab::bumps`].
    cursor: usize,
    /// Reset nursery chunks of this class (every slot poisoned).
    spare: Vec<usize>,
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
    /// `(slot size, align)` class of the current chunk.
    class: (u32, u32),
    /// Index + 1 of that class's free list in `Slab::free` once a slot of
    /// this chunk was freed (classes are only ever appended); 0 before.
    free_class: usize,
}


impl Slab {
    pub fn new() -> Self {
        Self {
            chunks: ChunkTable::new(),
            free: Vec::new(),
            released: Vec::new(),
            recent: std::array::from_fn(|_| AtomicPtr::new(std::ptr::null_mut())),
            released_mask: Vec::new(),
            young: Vec::new(),
            bumps: Box::new([Bump::default(); YOUNG_CLASSES]),
        }
    }

    /// Bump-allocate a slot of `layout` in a nursery chunk, or take one of
    /// the class's freed slots once the chunk is full. Nursery slots are
    /// never on the old space's free lists until a promotion
    /// ([`Self::finish_young`]).
    #[inline]
    pub fn alloc_young(&mut self, layout: Layout) -> NonNull<u8> {
        let (slot_size, align) = slot_dims(layout);
        let key = (slot_size as u32, align as u32);
        if let Some(class) = self.young.iter_mut().find(|c| c.key == key) {
            let bump = &mut self.bumps[class.cursor];
            if bump.next + slot_size as u64 <= bump.end {
                let p = bump.next;
                bump.next += slot_size as u64;
                return unsafe { NonNull::new_unchecked(p as *mut u8) };
            }
            if bump.free != 0 {
                let p = bump.free;
                // SAFETY: a freed young slot holds the next one's address
                // at `FREE_LINK` (`free_young`).
                bump.free = unsafe { *((p + FREE_LINK) as *const u64) };
                return unsafe { NonNull::new_unchecked(p as *mut u8) };
            }
        }
        self.young_refill(slot_size, align)
    }

    /// The nursery class of `layout`, made if new: its index in
    /// [`Self::bumps`]. A new slab makes the classes of the objects native
    /// code allocates inline first, so their indexes are fixed.
    pub fn young_class(&mut self, layout: Layout) -> usize {
        let (slot_size, align) = slot_dims(layout);
        self.young_class_at(slot_size, align)
    }

    fn young_class_at(&mut self, slot_size: usize, align: usize) -> usize {
        let key = (slot_size as u32, align as u32);
        if let Some(at) = self.young.iter().position(|c| c.key == key) {
            return at;
        }
        let cursor = self.young.len();
        assert!(cursor < YOUNG_CLASSES, "more than {YOUNG_CLASSES} nursery size classes");
        self.young.push(YoungClass {
            key,
            used: Vec::new(),
            cursor,
            spare: Vec::new(),
        });
        cursor
    }

    /// The index of class `key`'s nursery cursor, if it has one
    /// ([`Self::young_class`]).
    pub fn young_cursor(&self, key: (u32, u32)) -> Option<usize> {
        self.young.iter().position(|c| c.key == key)
    }

    /// The size class of nursery cursor `cursor`.
    pub fn young_key(&self, cursor: usize) -> (u32, u32) {
        self.young[cursor].key
    }

    /// The nursery cursors, indexed by [`Self::young_class`].
    pub fn bumps_ptr(&mut self) -> *mut Bump {
        self.bumps.as_mut_ptr()
    }

    #[inline(never)]
    fn young_refill(&mut self, slot_size: usize, align: usize) -> NonNull<u8> {
        let at = self.young_class_at(slot_size, align);
        let i = match self.young[at].spare.pop() {
            Some(i) => i,
            None => self.map_young_chunk(slot_size, align),
        };
        let c = self.chunks.get(i).expect("young chunk");
        let first = c.ptr as u64 + u64::from(c.meta.first_off);
        let slots = (CHUNK as u64 - u64::from(c.meta.first_off)) / slot_size as u64;
        let class = &mut self.young[at];
        class.used.push(i);
        let free = self.bumps[class.cursor].free;
        self.bumps[class.cursor] = Bump {
            next: first + slot_size as u64,
            end: first + slots * slot_size as u64,
            free,
        };
        unsafe { NonNull::new_unchecked(first as *mut u8) }
    }

    /// A chunk of the class for the nursery: a released one of that class,
    /// else a new mapping. Its slots stay off the free lists.
    fn map_young_chunk(&mut self, slot_size: usize, align: usize) -> usize {
        let key = (slot_size as u32, align as u32);
        if let Some((_, list)) = self.released.iter_mut().find(|(k, _)| *k == key)
            && let Some(i) = list.pop()
        {
            self.set_released(i, false);
            i
        } else {
            let ptr = map_chunk(CHUNK);
            let first_off = align_up(ptr as usize, align) - ptr as usize;
            self.chunks.push(Chunk {
                ptr,
                meta: PageMeta::new(slot_size as u32, align as u32, first_off as u32),
            });
            self.chunks.len() - 1
        }
    }

    /// Give a young slot (already poisoned) back to its class's nursery.
    pub fn free_young(&mut self, ptr: NonNull<u8>) {
        let Some((_, meta)) = self.chunk_for(ptr.as_ptr() as u64) else {
            debug_assert!(false, "slab free of unmapped pointer");
            return;
        };
        let key = (meta.slot_size, meta.slot_align);
        let cursor = self.young.iter().find(|c| c.key == key).map(|c| c.cursor);
        if let Some(cursor) = cursor
            && meta.slot_size as u64 >= FREE_LINK + 8
        {
            let bump = &mut self.bumps[cursor];
            // SAFETY: the slot is this class's, freed and poisoned; its
            // first payload word is free to hold the link.
            unsafe { *((ptr.as_ptr() as u64 + FREE_LINK) as *mut u64) = bump.free };
            bump.free = ptr.as_ptr() as u64;
        }
    }

    /// Whether any nursery chunk holds objects.
    #[inline]
    pub fn has_young_chunks(&self) -> bool {
        self.young.iter().any(|c| !c.used.is_empty())
    }

    /// The nursery chunks in use: `(chunk index, first slot, end, slot
    /// size)`, where `end` is past the last slot written (the bump pointer
    /// in the current chunk). Slots past it were never written or were
    /// poisoned by the last reset.
    pub fn young_spans(&self) -> Vec<(usize, u64, u64, u64)> {
        let mut out = Vec::new();
        for class in &self.young {
            let last = class.used.len();
            for (n, &i) in class.used.iter().enumerate() {
                let Some(c) = self.chunks.get(i) else {
                    continue;
                };
                let size = u64::from(c.meta.slot_size);
                let first = c.ptr as u64 + u64::from(c.meta.first_off);
                let end = if n + 1 == last {
                    self.bumps[class.cursor].next
                } else {
                    first + (CHUNK as u64 - u64::from(c.meta.first_off)) / size * size
                };
                out.push((i, first, end, size));
            }
        }
        out
    }

    /// End of the last slot of chunk `i` (a promoted chunk's unused tail).
    pub fn chunk_slots_end(&self, i: usize) -> u64 {
        let c = self.chunks.get(i).expect("chunk");
        let size = u64::from(c.meta.slot_size);
        c.ptr as u64 + u64::from(c.meta.first_off) + (CHUNK as u64 - u64::from(c.meta.first_off)) / size * size
    }

    /// End a promotion: `promoted` chunks join the old space with `free`
    /// (their dead and never-used slots) on the free lists, and every other
    /// chunk in use is reset for reuse (its slots must already be
    /// poisoned). The current chunks restart empty.
    pub fn finish_young(&mut self, promoted: &[usize], free: Vec<NonNull<u8>>) {
        for class in &mut self.young {
            for i in class.used.drain(..) {
                if !promoted.contains(&i) {
                    class.spare.push(i);
                }
            }
            self.bumps[class.cursor] = Bump::default();
        }
        for p in free {
            self.free(p);
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
            cur.class = (c.meta.slot_size, c.meta.slot_align);
            cur.free_class = 0;
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

    /// Size class of `layout`: the key a batch of its slots is filed under.
    pub fn class_of(layout: Layout) -> (u32, u32) {
        let (slot_size, align) = slot_dims(layout);
        (slot_size as u32, align as u32)
    }

    /// Hand out `n` slots of `layout`'s class at once (a steal-epoch
    /// mutator's allocation batch). The slots stay poisoned until written.
    /// Like the free list, the batch is popped from the back, and its last
    /// slot is the one a plain [`Self::alloc`] would have returned first.
    pub fn alloc_batch(&mut self, layout: Layout, n: usize, out: &mut Vec<NonNull<u8>>) {
        let start = out.len();
        out.reserve(n);
        for _ in 0..n {
            out.push(self.alloc(layout));
        }
        out[start..].reverse();
    }

    /// Return never-written slots of class `key` from a batch. The batch
    /// keeps free-list order, so the next alloc hands out the same slot it
    /// would have if the batch had never been taken.
    pub fn give_back(&mut self, key: (u32, u32), slots: &mut Vec<NonNull<u8>>) {
        if slots.is_empty() {
            return;
        }
        if let Some(class) = self.free.iter_mut().find(|c| c.key == key) {
            class.slots.append(slots);
            return;
        }
        let rest = std::mem::take(slots);
        let low_water = rest.len();
        self.free.push(FreeClass {
            key,
            slots: rest,
            low_water,
        });
    }

    pub fn free(&mut self, ptr: NonNull<u8>) {
        let Some((_, meta)) = self.chunk_for(ptr.as_ptr() as u64) else {
            debug_assert!(false, "slab free of unmapped pointer");
            return;
        };
        let key = (meta.slot_size, meta.slot_align);
        self.free_in_class(key, ptr);
    }

    fn free_in_class(&mut self, key: (u32, u32), ptr: NonNull<u8>) {
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

    /// Mapped bytes less the chunks whose pages went back to the OS.
    pub fn resident_bytes(&self) -> usize {
        let released: usize = self.released.iter().map(|(_, l)| l.len()).sum();
        (self.chunks.len() - released) * CHUNK
    }

    /// Give back the pages of every chunk with no live slot: its free
    /// slots (the free lists, a nursery class's freed slots and the rest of
    /// its bump chunk) are all of its slots, or it is a spare nursery
    /// chunk. Its slots leave the free lists and it waits on
    /// [`Self::released`] to be carved again. Chunks stay mapped: a
    /// released page reads as zeros, so every header in it is poisoned
    /// and a lookup stays defined (`docs/internals/heap-identity.md`).
    /// Returns the bytes released.
    pub fn release_empty_chunks(&mut self) -> usize {
        if !cfg!(unix) {
            return 0;
        }
        let n = self.chunks.len();
        let mut free = vec![0usize; n];
        let chunks = &self.chunks;
        let mut tally = |addr: u64, k: usize| {
            if let Some(i) = chunks.position(addr) {
                free[i] += k;
            }
        };
        for class in &self.free {
            for p in &class.slots {
                tally(p.as_ptr() as u64, 1);
            }
        }
        for class in &self.young {
            let b = self.bumps[class.cursor];
            let mut p = b.free;
            while p != 0 {
                tally(p, 1);
                // SAFETY: a freed nursery slot links the next at `FREE_LINK`.
                p = unsafe { *((p + FREE_LINK) as *const u64) };
            }
            if b.next < b.end {
                tally(b.next, ((b.end - b.next) / u64::from(class.key.0)) as usize);
            }
        }
        let mut empty = vec![false; n];
        for (i, e) in empty.iter_mut().enumerate() {
            let Some(c) = self.chunks.get(i) else { continue };
            let slots = (CHUNK - c.meta.first_off as usize) / c.meta.slot_size as usize;
            *e = free[i] == slots && !self.released_mask.get(i).copied().unwrap_or(false);
        }
        for class in &self.young {
            for &i in &class.spare {
                if !self.released_mask.get(i).copied().unwrap_or(false) {
                    empty[i] = true;
                }
            }
        }
        let mut bytes = 0;
        for i in (0..n).filter(|&i| empty[i]) {
            let Some(c) = self.chunks.get(i) else { continue };
            let (ptr, key) = (c.ptr, (c.meta.slot_size, c.meta.slot_align));
            release_pages(ptr, CHUNK);
            self.set_released(i, true);
            match self.released.iter_mut().find(|(k, _)| *k == key) {
                Some((_, list)) => list.push(i),
                None => self.released.push((key, vec![i])),
            }
            bytes += CHUNK;
        }
        if bytes == 0 {
            return 0;
        }
        let chunks = &self.chunks;
        let gone = |addr: u64| chunks.position(addr).is_some_and(|i| empty[i]);
        for class in &mut self.free {
            class.slots.retain(|p| !gone(p.as_ptr() as u64));
            class.low_water = class.low_water.min(class.slots.len());
        }
        for class in &mut self.young {
            class.used.retain(|&i| !empty[i]);
            class.spare.retain(|&i| !empty[i]);
            let b = &mut self.bumps[class.cursor];
            if b.next < b.end && gone(b.next) {
                // Its bump chunk went: the next allocation refills.
                b.next = 0;
                b.end = 0;
            }
            // Relink the freed slots that are left, in their order.
            let mut keep = Vec::new();
            let mut p = b.free;
            while p != 0 {
                if !gone(p) {
                    keep.push(p);
                }
                // SAFETY: as above; a released slot's link was read before
                // its page went (`release_pages` zeroes it).
                p = unsafe { *((p + FREE_LINK) as *const u64) };
            }
            b.free = 0;
            for &p in keep.iter().rev() {
                // SAFETY: a freed slot of this class, kept.
                unsafe { *((p + FREE_LINK) as *mut u64) = b.free };
                b.free = p;
            }
        }
        bytes
    }

    /// Address lies in the span of mapped chunks (cheap reject for values).
    #[inline]
    pub fn may_contain(&self, addr: u64) -> bool {
        self.chunks.may_contain(addr)
    }

    /// `[lo, hi)` of every chunk address, for a scan that tests many
    /// words while no chunk is mapped ([`Self::may_contain`] per word
    /// reloads them).
    pub fn bounds(&self) -> (u64, u64) {
        (self.chunks.lo.load(Ordering::Acquire), self.chunks.hi.load(Ordering::Acquire))
    }

    pub fn chunk_count(&self) -> usize {
        self.chunks.len()
    }

    /// True when `addr` is a slot origin in a mapped chunk (not necessarily
    /// live — poison is the header kind).
    #[inline]
    pub fn contains_slot(&self, addr: u64) -> bool {
        self.chunk_for(addr).is_some_and(|(start, meta)| {
            let off = (addr - start) as u32;
            if off < meta.first_off {
                return false;
            }
            let rel = off - meta.first_off;
            meta.is_slot_offset(rel) && (off as usize) + (meta.slot_size as usize) <= CHUNK
        })
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

    /// A whole chunk of `layout`'s class, fresh or released, for one
    /// steal-epoch mutator to bump-allocate from: its slots' range
    /// `(first, end)`. Its objects then sit in allocation order, as in the
    /// nursery; what is left goes back through [`Self::give_back_run`].
    pub fn alloc_run(&mut self, layout: Layout) -> (u64, u64) {
        let (slot_size, align) = slot_dims(layout);
        let key = (slot_size as u32, align as u32);
        let reused = if let Some((_, list)) = self.released.iter_mut().find(|(k, _)| *k == key)
            && let Some(i) = list.pop()
            && let Some(c) = self.chunks.get(i)
        {
            let ptr = c.ptr;
            self.set_released(i, false);
            Some(ptr)
        } else {
            None
        };
        let ptr = reused.unwrap_or_else(|| map_chunk(CHUNK));
        let first = align_up(ptr as usize, align);
        if reused.is_none() {
            self.chunks.push(Chunk {
                ptr,
                meta: PageMeta::new(slot_size as u32, align as u32, (first - ptr as usize) as u32),
            });
        }
        let n = (ptr as usize + CHUNK - first) / slot_size;
        (first as u64, (first + n * slot_size) as u64)
    }

    /// The untaken slots `[first, end)` of a run ([`Self::alloc_run`]) go
    /// to `layout`'s free list, to be handed out in address order.
    pub fn give_back_run(&mut self, key: (u32, u32), first: u64, end: u64) {
        let size = u64::from(key.0);
        let mut slots: Vec<NonNull<u8>> =
            (0..(end - first) / size).rev().map(|i| unsafe { NonNull::new_unchecked((first + i * size) as *mut u8) }).collect();
        self.give_back(key, &mut slots);
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
        // The free list pops from its back: hand the chunk out in address
        // order, so objects allocated (or copied) one after another sit
        // that way too (locality step 5).
        rest.reverse();
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
                meta: PageMeta::new(slot_size as u32, align as u32, first_off as u32),
            });
        }
        first_slot.expect("gc slab chunk smaller than one slot")
    }

    #[inline]
    fn chunk_for(&self, addr: u64) -> Option<(u64, &PageMeta)> {
        let last = self.recent[Self::recent_index(addr)].load(Ordering::Relaxed);
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
        self.recent[Self::recent_index(addr)].store(std::ptr::from_ref(c).cast_mut(), Ordering::Relaxed);
        Some((c.ptr as u64, &c.meta))
    }

    fn recent_index(addr: u64) -> usize {
        (addr >> 16) as usize % RECENT
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
        // Say why before the abort: `memory allocation of 65536 bytes failed`
        // alone does not tell an exhausted map count from exhausted memory
        // (coil-lang#804).
        let err = std::io::Error::last_os_error();
        eprintln!("coil: mmap of a {len}-byte heap chunk failed: {err}{}", process_memory_note());
        std::alloc::handle_alloc_error(Layout::from_size_align(len, 4096).expect("layout"));
    }
    ptr.cast()
}

/// The process's mapped and resident size and its mapping count, where
/// `/proc` has them.
#[cfg(unix)]
fn process_memory_note() -> String {
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    let field = |name: &str| {
        status
            .lines()
            .find_map(|l| l.strip_prefix(name))
            .map(|v| v.trim().to_string())
            .unwrap_or_else(|| "?".to_string())
    };
    let maps = std::fs::read_to_string("/proc/self/maps").map_or(0, |m| m.lines().count());
    if status.is_empty() {
        return String::new();
    }
    format!(" (VmSize {}, VmRSS {}, {maps} mappings)", field("VmSize:"), field("VmRSS:"))
}

/// Give `len` bytes at `ptr` back to the OS; they read as zeros after.
#[cfg(unix)]
fn release_pages(ptr: *mut u8, len: usize) {
    unsafe {
        libc::madvise(ptr.cast(), len, libc::MADV_DONTNEED);
    }
}

#[cfg(not(unix))]
fn release_pages(_ptr: *mut u8, _len: usize) {}

#[cfg(unix)]
fn unmap(ptr: *mut u8, len: usize) {
    unsafe {
        libc::munmap(ptr.cast(), len);
    }
}

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

    #[cfg(unix)]
    #[test]
    fn empty_nursery_chunks_go_back_and_are_carved_again() {
        let mut slab = Slab::new();
        let layout = Layout::from_size_align(32, 8).unwrap();
        let per_chunk = CHUNK / 32;
        let slots: Vec<NonNull<u8>> = (0..per_chunk * 10).map(|_| slab.alloc_young(layout)).collect();
        // Keep one slot, marked, in the first chunk; free the rest.
        let kept = slots[0];
        unsafe { *(kept.as_ptr().add(16) as *mut u64) = 0xC011 };
        for &p in &slots[1..] {
            slab.free_young(p);
        }
        let before = slab.resident_bytes();
        let released = slab.release_empty_chunks();
        assert!(released >= 8 * CHUNK, "released {released}");
        assert_eq!(slab.resident_bytes(), before - released);
        // Nothing left to release, and the kept slot is intact.
        assert_eq!(slab.release_empty_chunks(), 0);
        assert_eq!(unsafe { *(kept.as_ptr().add(16) as *const u64) }, 0xC011);
        // Allocation goes on: the freed slots left, then released chunks.
        let again: Vec<NonNull<u8>> = (0..per_chunk * 10).map(|_| slab.alloc_young(layout)).collect();
        assert!(!again.contains(&kept));
        let mut uniq: Vec<usize> = again.iter().map(|p| p.as_ptr() as usize).collect();
        uniq.sort_unstable();
        uniq.dedup();
        assert_eq!(uniq.len(), again.len());
        // The released chunks were carved again, not new ones mapped.
        assert!(slab.chunk_count() <= 11, "{} chunks", slab.chunk_count());
    }

    #[cfg(unix)]
    #[test]
    fn empty_old_chunks_go_back() {
        let mut slab = Slab::new();
        let layout = Layout::from_size_align(48, 8).unwrap();
        let slots: Vec<NonNull<u8>> = (0..(CHUNK / 48) * 6).map(|_| slab.alloc(layout)).collect();
        let mapped = slab.chunk_count();
        for &p in &slots {
            slab.free(p);
        }
        assert_eq!(slab.release_empty_chunks(), mapped * CHUNK);
        assert_eq!(slab.resident_bytes(), 0);
        // A new allocation re-carves a released chunk instead of mapping one.
        let p = slab.alloc(layout);
        assert_eq!(slab.chunk_count(), mapped);
        slab.free(p);
    }

    #[test]
    fn slot_offset_test_matches_remainder() {
        for size in [1u32, 8, 16, 24, 40, 48, 56, 72, 96, 120, 136, 4096, 65536] {
            let meta = PageMeta::new(size, 8, 0);
            for rel in (0..70_000u32).chain([u32::MAX - 7, u32::MAX]) {
                assert_eq!(meta.is_slot_offset(rel), rel % size == 0, "size {size} rel {rel}");
            }
        }
    }

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
                meta: PageMeta::new(8, 8, 0),
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
