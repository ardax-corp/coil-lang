//! Mark-and-sweep heap: intrusive object list, string interning, and GC.
//!
//! Interpreter collections mark to completion at an alloc safepoint, then
//! sweep lazily from a cursor. The mutator never runs with gray objects, so
//! stores need no write barrier (see [`Heap::resurrect_during_mark`]).
//! Explicit `Heap::collect` / `gc::collect` drain a cycle. Objects do not move.

use std::alloc::Layout;
use std::collections::HashMap;
use std::ptr::{self, NonNull};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use common::{promise, unlikely};

use super::slab::Slab;
use super::AddrHashBuilder;

const GC_NEXT_THRESHOLD: usize = 1024 * 1024;
const GC_GROWTH_FACTOR: usize = 2;
/// A steal epoch cannot collect, so crossing the normal threshold aborts it
/// and its forks rerun sequentially. Inside an epoch the heap may instead grow
/// to this multiple of the threshold it entered with...
const EPOCH_GC_HEADROOM_FACTOR: usize = 4;
/// ...and never to less than this many bytes past the entry heap size.
const EPOCH_GC_HEADROOM_MIN: usize = 256 * 1024 * 1024;
/// Growth when most of the heap survived its last collection: the live set is
/// still growing, so re-marking it every doubling is mostly wasted work.
const GC_GROWTH_FACTOR_SURVIVING: usize = 4;
/// Nursery size in 64 KiB slab chunks: filling this many asks for a minor
/// collection (`feature/escape-memory` step 4).
const NURSERY_CHUNKS: usize = 32;
/// Nursery chunks a `region` block may fill (64 MiB) before a minor
/// collection runs inside it.
const REGION_CHUNKS: usize = 1024;
/// Unmarked objects considered at one alloc safepoint during lazy sweep.
pub const GC_SWEEP_QUANTUM: usize = 128;

/// Slots one steal-epoch mutator takes per size class under the epoch lock.
const EPOCH_BATCH: usize = 64;

/// Whether the collector checks its own invariants as it goes (the
/// missed-barrier check at the top of each minor, #2687): `gc-stress` and
/// debug builds, and any run with `COIL_MIR_VERIFY` set. A heap reads it
/// once, when it is made.
pub(crate) fn heap_checks_on() -> bool {
    if cfg!(any(feature = "gc-stress", debug_assertions)) {
        return true;
    }
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("COIL_MIR_VERIFY").is_some_and(|v| !v.is_empty() && v != "0"))
}

/// One size class's untaken slots: `(slot size, align)` and the slots.
type BatchClass = ((u32, u32), Vec<NonNull<u8>>);

/// One thread's allocation batch for the open steal epoch.
///
/// During a Layer A epoch every mutator shares one [`Heap`], so a plain
/// `alloc` takes the epoch lock. Instead, a mutator takes [`EPOCH_BATCH`]
/// slots of a size class under the lock once, then writes objects into them
/// without it, and folds the byte / object counts back in on the next refill.
/// Untaken slots stay poisoned (`kind == 0`), which is what a free slot looks
/// like, and no collection runs inside an epoch, so a batch is invisible to
/// the GC. [`Heap::flush_epoch_batch`] returns the leftovers before the
/// epoch's last job ends (workers before publishing their result, the root
/// before it leaves the epoch).
#[derive(Default)]
struct EpochBatch {
    /// The epoch heap the slots belong to; null while the batch is empty.
    heap: *const Heap,
    classes: Vec<BatchClass>,
    bytes: usize,
    objects: usize,
    /// Fresh `Weak` / finalizable objects for [`Heap::weaks`] /
    /// [`Heap::finalizable`].
    weaks: Vec<u64>,
    finalizable: Vec<u64>,
    /// Objects that can hold references, for [`Heap::epoch_born`].
    born: Vec<u64>,
}

impl EpochBatch {
    fn is_empty(&self) -> bool {
        self.classes.iter().all(|(_, v)| v.is_empty())
            && self.bytes == 0
            && self.objects == 0
            && self.weaks.is_empty()
            && self.finalizable.is_empty()
            && self.born.is_empty()
    }
}

/// The GC side lists ([`Heap::weaks`], [`Heap::finalizable`]).
#[derive(Clone, Copy)]
enum SideList {
    Weak,
    Finalizable,
}

thread_local! {
    static EPOCH_BATCH_TLS: std::cell::RefCell<EpochBatch> =
        std::cell::RefCell::new(EpochBatch::default());
}

/// This thread's epoch batch, set aside while it helps with a job that may
/// allocate in another heap's epoch (a join help-steals unrelated jobs: other
/// test cases, other runtimes' auto-par work). Dropping it puts the batch back.
#[must_use]
pub struct EpochBatchStash(Option<EpochBatch>);

/// Set aside this thread's epoch batch until the returned stash drops.
pub fn stash_epoch_batch() -> EpochBatchStash {
    EpochBatchStash(Some(EPOCH_BATCH_TLS.with(|cell| std::mem::take(&mut *cell.borrow_mut()))))
}

impl Drop for EpochBatchStash {
    fn drop(&mut self) {
        let Some(saved) = self.0.take() else {
            return;
        };
        EPOCH_BATCH_TLS.with(|cell| {
            let mut batch = cell.borrow_mut();
            debug_assert!(batch.is_empty(), "a helped job left its epoch batch unflushed");
            *batch = saved;
        });
    }
}

// Shared-heap epoch write guard (debug / gc-stress only, #1892): see
// `docs/internals/shared-heap-guard.md`.
#[cfg(any(debug_assertions, feature = "gc-stress"))]
thread_local! {
    /// Generation of the steal epoch whose shared-heap job this thread is
    /// running, or 0 when it runs none. While non-zero, every
    /// [`GcData::as_mut`] on this thread must target an object allocated in
    /// that epoch.
    static EPOCH_WRITE_GEN: std::cell::Cell<u8> = const { std::cell::Cell::new(0) };
}

/// While alive, this thread runs one reactor job: a shared-heap job may then
/// write only objects allocated in its epoch, an isolate job anything in its
/// own heap. Dropping it restores the enclosing job's rule (a join that
/// help-steals nests jobs on one thread). Zero-sized and a no-op unless
/// `debug_assertions` or the `gc-stress` feature is on.
#[must_use]
pub struct EpochWriteScope {
    #[cfg(any(debug_assertions, feature = "gc-stress"))]
    prev: u8,
}

impl EpochWriteScope {
    /// Enter a job on `heap` (the heap the job's runtime is bound to): a
    /// shared-heap job of `heap`'s open epoch when `shared`, otherwise a job
    /// that may write anything ([`Self::unchecked`]).
    #[cfg(any(debug_assertions, feature = "gc-stress"))]
    #[inline]
    pub fn enter(heap: &Heap, shared: bool) -> Self {
        Self::set(if shared { heap.epoch_gen } else { 0 })
    }

    /// Lift the guard until dropped: a join that help-steals runs the stolen
    /// job (and the reset of its helper runtime's own heap) on this thread.
    #[cfg(any(debug_assertions, feature = "gc-stress"))]
    #[inline]
    pub fn unchecked() -> Self {
        Self::set(0)
    }

    #[cfg(any(debug_assertions, feature = "gc-stress"))]
    fn set(generation: u8) -> Self {
        Self {
            prev: EPOCH_WRITE_GEN.with(|g| g.replace(generation)),
        }
    }

    /// Release build: no guard.
    #[cfg(not(any(debug_assertions, feature = "gc-stress")))]
    #[inline(always)]
    pub fn enter(_heap: &Heap, _shared: bool) -> Self {
        Self {}
    }

    /// Release build: no guard.
    #[cfg(not(any(debug_assertions, feature = "gc-stress")))]
    #[inline(always)]
    pub fn unchecked() -> Self {
        Self {}
    }
}

#[cfg(any(debug_assertions, feature = "gc-stress"))]
impl Drop for EpochWriteScope {
    fn drop(&mut self) {
        let _ = EPOCH_WRITE_GEN.try_with(|g| g.set(self.prev));
    }
}

/// A shared-heap job wrote an object that was not allocated in its epoch:
/// the compiler's purity / disjointness proof was wrong. A worker's Rust
/// panic is caught and turned into a failed join (the arm reruns
/// sequentially), which would hide this, so outside the crate's own unit
/// tests the process aborts.
#[cfg(any(debug_assertions, feature = "gc-stress"))]
#[cold]
#[inline(never)]
fn epoch_write_violation(addr: u64, object_gen: u8, epoch_gen: u8) -> ! {
    let msg = format!(
        "shared-heap epoch guard: a worker wrote object {addr:#x} that is not \
         from its epoch (object epoch {object_gen}, job epoch {epoch_gen}); \
         see docs/internals/shared-heap-guard.md (#1892)"
    );
    if cfg!(test) {
        panic!("{msg}");
    }
    eprintln!("{msg}");
    std::process::abort()
}

/// Collector phase. `Idle` means the last cycle finished.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GcPhase {
    Idle,
    Marking,
    Sweeping,
}

/// The heap's allocation counters and the gate of native code's inline
/// allocation, boxed so native code updates them in place
/// ([`InlineLayout`]).
#[repr(C)]
#[derive(Debug, Default)]
pub struct HeapCounts {
    /// Non-zero while a nursery object may be bump-allocated inline: the
    /// conditions [`Heap::alloc`] checks before it allocates young that do
    /// not depend on the object ([`Heap::sync_inline_gate`]).
    pub inline_gate: u64,
    /// [`Heap::finalizer_types`]: its words' address and count, so inline
    /// code leaves an instance whose type has a `drop` to the helper.
    pub fin_words: u64,
    pub fin_len: u64,
    pub alloc_bytes: usize,
    /// Bytes allocated in the nursery since the last minor collection
    /// (part of `alloc_bytes`; an estimate, arrays grow in place).
    pub young_bytes: usize,
    /// Live object count (alloc +1, sweep/dealloc −1). Not a membership set.
    pub live_count: usize,
}

/// Managed heap. Objects are linked in an intrusive list for traversal.
/// `Gc<T>` handles are copyable; the runtime controls when objects become unreachable.
pub struct Heap {
    counts: Box<HeapCounts>,
    gc_next_threshold: usize,
    /// Most bytes the heap may hold ([`Self::set_byte_cap`]); `usize::MAX`
    /// when unlimited. The collection threshold never exceeds it, so an
    /// allocation past it reaches a safepoint.
    byte_cap: usize,
    gc_growth_factor: usize,
    /// Heap bytes when the current sweep started (survival for the next budget).
    gc_sweep_start_bytes: usize,
    /// Bytes the current sweep has freed: what survived the mark is
    /// `gc_sweep_start_bytes` less this, whatever was allocated meanwhile.
    gc_sweep_freed: usize,
    strings: Table<()>,
    slab: Slab,
    /// Per class `type_id`: field word kinds from static types
    /// (`common::WORD_*`). Missing rows / fields are unknown.
    class_kinds: std::sync::Arc<Vec<Box<[u8]>>>,
    /// Every `Weak` object, so [`Self::clear_dead_weaks`] visits the weaks
    /// instead of the heap (#1870). A superset: entries a sweep freed or a
    /// slot reuse replaced drop out when it next runs.
    weaks: Vec<u64>,
    /// Class / enum `type_id`s with a `drop` (the runtime's finalizer registry),
    /// one bit per id: every allocation asks while any is registered.
    finalizer_types: Vec<u64>,
    /// The [`Self::finalizer_types`] whose `drop` keeps nothing of `self`
    /// (the compiler's escape facts): an early free may run it at the free.
    early_drop_types: Vec<u64>,
    /// Objects [`Self::cow_unique`] copied.
    cow_copies: u64,
    /// Objects `Cow` shared.
    cow_shares: u64,
    /// `--cow-report`: copies by native site.
    cow_sites: Option<std::collections::HashMap<u32, u64>>,
    /// Every object of a [`Self::finalizer_types`] type, a superset pruned
    /// by [`Self::unmarked_finalizable`] like [`Self::weaks`].
    finalizable: Vec<u64>,
    /// Immortal arity-0 enum singletons keyed by tag (never swept).
    immortal_enums: HashMap<u32, Object, AddrHashBuilder>,
    /// Last tag returned by [`Self::immortal_unit_enum`]. Unit constructors
    /// (binary-tree leaves) hit this instead of the map.
    unit_enum: Option<(u32, Object)>,
    /// Shared function values that capture and fill nothing, by entry,
    /// arity and rest flag ([`Self::immortal_plain_fn`]), never swept.
    immortal_fns: HashMap<(u32, u32, bool), Object>,
    /// Reused gray worklist / root buffers across collections.
    gc_gray: Vec<Object>,
    gc_root_objects: Vec<Object>,
    gc_roots: Vec<u64>,
    gc_dangling_strings: Vec<RefString>,
    /// Check mode of [`Self::free_early`]: objects it would have freed,
    /// which the next mark must not reach ([`Self::check_doomed`]).
    doomed: Vec<u64>,
    /// What the last [`Self::free_early`] gave back.
    early_freed: Vec<u64>,
    /// Scratch of a deep [`Heap::free_early`] walk (found, mark work,
    /// children), kept so a free does not allocate.
    early_scratch: (Vec<Object>, Vec<Object>, Vec<u64>),
    /// [`Self::free_early`] freed something: a dead frame word or a dead
    /// object may now point at a freed or reused slot, so a typed handle
    /// is looked up before it is marked ([`Self::early_freed_slot`]).
    freed_early: bool,
    /// Allocate movable kinds in the nursery ([`Self::set_nursery`]).
    nursery: bool,
    /// Objects born in the open steal epoch that can hold references. The
    /// nursery is empty during an epoch, so they start out naming nothing
    /// young; when it closes they join the write barrier
    /// ([`Self::exit_epoch_stw`]).
    epoch_born: Vec<u64>,
    /// Nursery chunks in use that ask for a minor collection.
    nursery_chunks: usize,
    /// Held nursery chunks the last major cycle found live
    /// ([`Self::set_held_after_major`]).
    held_after_major: usize,
    /// Held nursery chunks that ask for a major cycle
    /// ([`Self::held_over_budget`]).
    held_budget: usize,
    /// [`heap_checks_on`], read once.
    pub(super) heap_checks: bool,
    /// Bytes still to allocate old after a minor collection most of the
    /// nursery survived ([`Self::note_survival`]).
    pretenure_bytes: usize,
    /// Minor collections in a row most of the nursery survived.
    pretenure_streak: u32,
    /// Open `region` blocks ([`Self::region_enter`]); while non-zero the
    /// nursery takes strings too, never pretenures, and holds up to
    /// [`REGION_CHUNKS`].
    region_depth: u32,
    /// [`Self::nursery_chunks`] outside the region.
    region_saved_chunks: usize,
    /// Incremental mark / lazy sweep (COI-309 S4).
    gc_phase: GcPhase,
    /// Next object to consider while sweeping; `None` when the cursor is idle.
    gc_sweep_cursor: Option<super::slab::SlotCursor>,
    /// CString arena for the current FFI invoke (reset after each call).
    ffi_strings: Vec<std::ffi::CString>,
    /// Layer A steal epoch: no collect; `alloc` takes [`Self::alloc_lock`].
    epoch_stw: bool,
    /// Non-null while a C1 steal epoch is live (points at the epoch mutex).
    alloc_lock: *const Mutex<()>,
    /// `alloc_bytes` past which a steal epoch aborts (see
    /// [`EPOCH_GC_HEADROOM_FACTOR`]); only meaningful while `epoch_stw`.
    epoch_gc_ceiling: usize,
    /// Debug / gc-stress: generation of the current (or last) steal epoch,
    /// never 0 once an epoch opened; stamped on objects allocated in it
    /// ([`EpochWriteScope`]).
    #[cfg(any(debug_assertions, feature = "gc-stress"))]
    epoch_gen: u8,
}

/// A runtime's own heap that can borrow the root Heap for a C1 steal job.
#[derive(Default)]
pub struct HeapSlot {
    owned: Heap,
    borrowed: Option<NonNull<Heap>>,
}

impl Default for Heap {
    fn default() -> Self {
        let mut heap = Self {
            counts: Box::default(),
            gc_next_threshold: GC_NEXT_THRESHOLD,
            byte_cap: usize::MAX,
            gc_growth_factor: GC_GROWTH_FACTOR,
            gc_sweep_start_bytes: 0,
            gc_sweep_freed: 0,
            strings: Table::default(),
            slab: Slab::new(),
            class_kinds: std::sync::Arc::default(),
            weaks: Vec::new(),
            finalizer_types: Vec::new(),
            early_drop_types: Vec::new(),
            cow_copies: 0,
            cow_shares: 0,
            cow_sites: None,
            finalizable: Vec::new(),
            immortal_enums: HashMap::default(),
            unit_enum: None,
            immortal_fns: HashMap::new(),
            gc_gray: Vec::new(),
            gc_root_objects: Vec::new(),
            gc_roots: Vec::new(),
            gc_dangling_strings: Vec::new(),
            doomed: Vec::new(),
            early_freed: Vec::new(),
            early_scratch: (Vec::new(), Vec::new(), Vec::new()),
            freed_early: false,
            nursery: false,
            epoch_born: Vec::new(),
            nursery_chunks: NURSERY_CHUNKS,
            held_after_major: 0,
            held_budget: NURSERY_CHUNKS,
            heap_checks: heap_checks_on(),
            pretenure_bytes: 0,
            pretenure_streak: 0,
            region_depth: 0,
            region_saved_chunks: 0,
            gc_phase: GcPhase::Idle,
            gc_sweep_cursor: None,
            ffi_strings: Vec::new(),
            epoch_stw: false,
            epoch_gc_ceiling: 0,
            alloc_lock: ptr::null(),
            #[cfg(any(debug_assertions, feature = "gc-stress"))]
            epoch_gen: 0,
        };
        inline::make_inline_classes(&mut heap.slab);
        heap
    }
}


impl HeapSlot {
    pub fn get(&self) -> &Heap {
        match self.borrowed {
            Some(p) => unsafe { p.as_ref() },
            None => &self.owned,
        }
    }

    pub fn get_mut(&mut self) -> &mut Heap {
        match self.borrowed {
            Some(mut p) => unsafe { p.as_mut() },
            None => &mut self.owned,
        }
    }

    pub fn owned_mut(&mut self) -> &mut Heap {
        &mut self.owned
    }

    pub fn owned_ptr(&mut self) -> *mut Heap {
        &mut self.owned
    }

    pub fn is_borrowed(&self) -> bool {
        self.borrowed.is_some()
    }

    /// Bind this slot to the epoch Heap. The owned isolate slab is unused
    /// until [`Self::unbind`].
    pub fn bind(&mut self, heap: *mut Heap) {
        self.borrowed = NonNull::new(heap);
    }

    pub fn unbind(&mut self) {
        self.borrowed = None;
    }
}

impl std::ops::Deref for HeapSlot {
    type Target = Heap;
    fn deref(&self) -> &Heap {
        self.get()
    }
}

impl std::ops::DerefMut for HeapSlot {
    fn deref_mut(&mut self) -> &mut Heap {
        self.get_mut()
    }
}

impl Heap {
    /// Drop interned C strings from the last FFI invoke.
    pub fn reset_ffi_strings(&mut self) {
        self.ffi_strings.clear();
    }

    /// Number of live interned CString boxes (tests; leak detector).
    pub fn ffi_string_live_count(&self) -> usize {
        self.ffi_strings.len()
    }

    /// Intern bytes as a NUL-terminated C string for this invoke.
    /// Errors on an interior NUL.
    pub fn intern_ffi_bytes(
        &mut self,
        bytes: &[u8],
    ) -> Result<*const std::os::raw::c_char, InteriorNul> {
        let s = std::ffi::CString::new(bytes).map_err(|_| InteriorNul)?;
        self.ffi_strings.push(s);
        Ok(self.ffi_strings.last().unwrap().as_ptr())
    }

    /// Look up a heap string and intern it in the FFI arena (no `Box::leak`).
    /// `Ok(None)` if `addr` is not a string. `Err(InteriorNul)` on interior NUL.
    pub fn cstr_from_addr(
        &mut self,
        addr: u64,
    ) -> Result<Option<*const std::os::raw::c_char>, InteriorNul> {
        let bytes = match self.find_object_by_addr(addr) {
            Some(crate::memory::Object::String(gc)) => gc.as_ref().data.as_bytes().to_vec(),
            _ => return Ok(None),
        };
        Ok(Some(self.intern_ffi_bytes(&bytes)?))
    }

    /// Layer A C1: steal epoch is live; collect is forbidden.
    pub fn epoch_stw(&self) -> bool {
        self.epoch_stw
    }

    pub fn enter_epoch_stw(&mut self, lock: &Mutex<()>) {
        // Epoch mutators allocate old and have no barrier of their own: no
        // young object may exist for them to store.
        self.promote_young();
        self.epoch_stw = true;
        self.sync_inline_gate();
        self.epoch_gc_ceiling = self
            .gc_next_threshold
            .saturating_mul(EPOCH_GC_HEADROOM_FACTOR)
            .max(self.counts.alloc_bytes.saturating_add(EPOCH_GC_HEADROOM_MIN));
        self.alloc_lock = lock as *const Mutex<()>;
        #[cfg(any(debug_assertions, feature = "gc-stress"))]
        {
            // Skip 0, which marks objects allocated outside any epoch.
            self.epoch_gen = self.epoch_gen.wrapping_add(1).max(1);
        }
    }

    pub fn exit_epoch_stw(&mut self) {
        self.epoch_stw = false;
        self.sync_inline_gate();
        self.alloc_lock = ptr::null();
        for addr in std::mem::take(&mut self.epoch_born) {
            if let Some(obj) = self.find_object_by_addr(addr)
                && obj.generation() & GEN_YOUNG == 0
            {
                obj.set_generation(GEN_UNLOGGED);
            }
        }
    }

    /// Allocates an object and returns its handle. The object is pushed to the
    /// front of the list of allocated objects.
    pub fn alloc<T: GcSized, F>(&mut self, data: T, map: F) -> (Object, Gc<T>)
    where
        F: Fn(Gc<T>) -> Object,
    {
        if unlikely(!self.alloc_lock.is_null()) {
            return self.alloc_from_epoch_batch(data, map);
        }
        self.alloc_unlocked(data, map)
    }

    /// Steal-epoch alloc: write into this thread's batch, taking the epoch
    /// lock only to refill it (see [`EpochBatch`]).
    #[inline(never)]
    fn alloc_from_epoch_batch<T: GcSized, F>(&mut self, data: T, map: F) -> (Object, Gc<T>)
    where
        F: Fn(Gc<T>) -> Object,
    {
        debug_assert_eq!(self.gc_phase, GcPhase::Idle, "steal epochs open with GC idle");
        let layout = Layout::new::<GcData<T>>();
        let key = Slab::class_of(layout);
        let me: *const Heap = self;
        let slot = EPOCH_BATCH_TLS.with(|cell| {
            let mut batch = cell.borrow_mut();
            debug_assert!(
                batch.heap.is_null() || batch.heap == me,
                "epoch batch left over from another heap"
            );
            batch.heap = me;
            let at = match batch.classes.iter().position(|(k, _)| *k == key) {
                Some(at) => at,
                None => {
                    batch.classes.push((key, Vec::with_capacity(EPOCH_BATCH)));
                    batch.classes.len() - 1
                }
            };
            if let Some(p) = batch.classes[at].1.pop() {
                return p;
            }
            let _epoch_guard = self.epoch_guard();
            self.fold_epoch_counts(&mut batch);
            let slots = &mut batch.classes[at].1;
            self.slab.alloc_batch(layout, EPOCH_BATCH, slots);
            slots.pop().expect("epoch batch refill")
        });
        let (object, content) = Self::init_slot(slot.cast::<GcData<T>>(), data, map);
        #[cfg(any(debug_assertions, feature = "gc-stress"))]
        content.header.epoch.set(self.epoch_gen);
        EPOCH_BATCH_TLS.with(|cell| {
            let mut batch = cell.borrow_mut();
            batch.bytes += object.size();
            batch.objects += 1;
            match self.side_list_of(object) {
                Some(SideList::Weak) => batch.weaks.push(object.addr()),
                Some(SideList::Finalizable) => batch.finalizable.push(object.addr()),
                None => {}
            }
            if T::HOLDS_REFS && self.nursery {
                batch.born.push(object.addr());
            }
        });
        (object, content)
    }

    /// Add a batch's pending byte / object counts to the heap (lock held).
    fn fold_epoch_counts(&mut self, batch: &mut EpochBatch) {
        self.counts.alloc_bytes += std::mem::take(&mut batch.bytes);
        self.counts.live_count += std::mem::take(&mut batch.objects);
        self.weaks.append(&mut batch.weaks);
        self.finalizable.append(&mut batch.finalizable);
        self.epoch_born.append(&mut batch.born);
    }

    /// Give this thread's unused epoch slots back and fold its counts in.
    ///
    /// Every mutator calls this before the epoch's last job ends: a worker
    /// before it publishes its join result, the root before it leaves the
    /// epoch. A no-op outside an epoch or with an empty batch.
    pub fn flush_epoch_batch(&mut self) {
        EPOCH_BATCH_TLS.with(|cell| {
            let mut batch = cell.borrow_mut();
            if batch.is_empty() {
                batch.heap = ptr::null();
                return;
            }
            debug_assert!(
                std::ptr::eq(batch.heap, self),
                "epoch batch flushed into another heap"
            );
            let _epoch_guard = self.epoch_guard();
            self.fold_epoch_counts(&mut batch);
            for (key, slots) in &mut batch.classes {
                self.slab.give_back(*key, slots);
            }
            batch.heap = ptr::null();
        });
    }

    /// Serializes heap-structure mutation while a shared-heap steal epoch is
    /// open (workers share this `Heap`). `None` outside an epoch.
    fn epoch_guard(&self) -> Option<std::sync::MutexGuard<'static, ()>> {
        let lock = self.alloc_lock;
        // The epoch's Mutex outlives every job bound to it.
        (!lock.is_null()).then(|| unsafe { &*lock }.lock().unwrap_or_else(|e| e.into_inner()))
    }

    fn alloc_unlocked<T: GcSized, F>(&mut self, data: T, map: F) -> (Object, Gc<T>)
    where
        F: Fn(Gc<T>) -> Object,
    {
        self.alloc_unlocked_in(data, map, true)
    }

    /// [`Self::alloc_unlocked`]; `may_young: false` puts it in the old space
    /// whatever its kind (an interned string, which the intern table names).
    fn alloc_unlocked_in<T: GcSized, F>(&mut self, data: T, map: F, may_young: bool) -> (Object, Gc<T>)
    where
        F: Fn(Gc<T>) -> Object,
    {
        let layout = Layout::new::<GcData<T>>();
        // Objects allocated while a mark is open are black, and a young
        // object must not carry a major-cycle mark: those go old.
        let young = (T::NURSERY || (T::REGION && self.region_depth != 0))
            && may_young
            && self.nursery
            && !self.epoch_stw
            && self.pretenure_bytes == 0
            && self.gc_phase != GcPhase::Marking
            && (self.finalizer_types.is_empty()
                || data.finalizer_type_id().is_none_or(|t| !self.has_finalizer(t)));
        let slot = if young { self.slab.alloc_young(layout) } else { self.slab.alloc(layout) };
        let (object, content) = Self::init_slot(slot.cast::<GcData<T>>(), data, map);
        let size = object.size();
        self.counts.alloc_bytes += size;
        self.counts.live_count += 1;
        if young {
            content.header.generation.set(GEN_YOUNG);
            self.counts.young_bytes += size;
            return (object, content);
        }
        if T::NURSERY && self.pretenure_bytes != 0 {
            self.pretenure_bytes = self.pretenure_bytes.saturating_sub(size);
            if self.pretenure_bytes == 0 {
                self.sync_inline_gate();
            }
        }
        if T::HOLDS_REFS && self.nursery {
            if self.epoch_stw {
                self.epoch_born.push(object.addr());
            } else if self.slab.young_chunks_in_use() == 0 {
                // Nothing young to name yet (pretenuring right after a
                // minor): the barrier records its first write.
                content.header.generation.set(GEN_UNLOGGED);
            } else {
                // Born old, maybe naming young objects: remembered at once.
                REMEMBERED.with_borrow_mut(|r| r.push(object.addr()));
            }
        }
        match self.side_list_of(object) {
            Some(SideList::Weak) => self.weaks.push(object.addr()),
            Some(SideList::Finalizable) => self.finalizable.push(object.addr()),
            None => {}
        }
        // Objects allocated while a cycle is open (finalizers) are black.
        if self.gc_phase == GcPhase::Marking {
            let _ = content.mark();
        } else if self.gc_phase == GcPhase::Sweeping
            && let Some(cur) = &self.gc_sweep_cursor
            && !self.slab.walk_passed(cur, object.addr())
        {
            // Ahead of the sweep cursor: the sweep keeps (and clears) a
            // `fresh` object instead of freeing an unmarked newcomer. Slots
            // behind it (most reuse, freed by this sweep) need no tag.
            content.set_fresh();
        }
        debug_assert!(
            self.find_object_by_addr(object.addr()).is_some(),
            "slab alloc must be findable at {:#x}",
            object.addr()
        );

        (object, content)
    }

    /// Open or close native code's inline nursery allocation
    /// ([`HeapCounts::inline_gate`]): called wherever one of the
    /// object-independent conditions of a young allocation changes.
    pub(crate) fn sync_inline_gate(&mut self) {
        let open = !cfg!(feature = "gc-stress")
            && self.nursery
            && !self.epoch_stw
            && self.pretenure_bytes == 0
            && self.gc_phase != GcPhase::Marking;
        self.counts.inline_gate = u64::from(open);
    }

    /// Where native code's inline allocation reads and bumps: the
    /// counters and the nursery cursors ([`InlineLayout`]).
    pub fn inline_alloc_ptrs(&mut self) -> (*mut HeapCounts, *mut super::slab::Bump) {
        (std::ptr::from_mut(self.counts.as_mut()), self.slab.bumps_ptr())
    }

    /// The GC side list a fresh object joins, if any.
    #[inline(always)]
    fn side_list_of(&self, object: Object) -> Option<SideList> {
        match object {
            Object::Weak(_) => Some(SideList::Weak),
            Object::Instance(_) | Object::Enum(_)
                if !self.finalizer_types.is_empty()
                    && Self::finalizer_type_of(object).is_some_and(|t| self.has_finalizer(t)) =>
            {
                Some(SideList::Finalizable)
            }
            _ => None,
        }
    }

    fn finalizer_type_of(object: Object) -> Option<u32> {
        match object {
            Object::Instance(gc) => Some(gc.as_ref().type_id),
            Object::Enum(gc) => Some(gc.as_ref().type_id),
            _ => None,
        }
    }

    /// Note that `type_id` has a `drop`: its objects join
    /// [`Self::finalizable`], the ones already allocated by one heap walk.
    pub fn register_finalizer_type(&mut self, type_id: u32) {
        if type_id == 0 || self.has_finalizer(type_id) {
            return;
        }
        let word = type_id as usize / 64;
        if self.finalizer_types.len() <= word {
            self.finalizer_types.resize(word + 1, 0);
        }
        self.finalizer_types[word] |= 1 << (type_id % 64);
        self.sync_finalizer_words();
        let existing: Vec<u64> = self
            .objects()
            .filter(|&o| Self::finalizer_type_of(o) == Some(type_id))
            .map(|o| o.addr())
            .collect();
        self.finalizable.extend(existing);
    }

    #[inline(always)]
    pub(crate) fn has_finalizer(&self, type_id: u32) -> bool {
        self.finalizer_types
            .get(type_id as usize / 64)
            .is_some_and(|w| w & (1 << (type_id % 64)) != 0)
    }

    /// Note that `type_id`'s `drop` may run where an object of it is freed
    /// early ([`Self::early_drops`]).
    pub fn register_early_drop_type(&mut self, type_id: u32) {
        let word = type_id as usize / 64;
        if self.early_drop_types.len() <= word {
            self.early_drop_types.resize(word + 1, 0);
        }
        self.early_drop_types[word] |= 1 << (type_id % 64);
    }

    fn early_drop_type(&self, type_id: u32) -> bool {
        self.early_drop_types
            .get(type_id as usize / 64)
            .is_some_and(|w| w & (1 << (type_id % 64)) != 0)
    }

    /// Point [`HeapCounts::fin_words`] at the finalizer type words.
    fn sync_finalizer_words(&mut self) {
        self.counts.fin_words = self.finalizer_types.as_ptr() as u64;
        self.counts.fin_len = self.finalizer_types.len() as u64;
    }

    /// Forget every finalizer type (a runtime reset).
    pub fn clear_finalizer_types(&mut self) {
        self.finalizer_types.clear();
        self.sync_finalizer_words();
        self.early_drop_types.clear();
        self.finalizable.clear();
    }

    /// A class with a `drop` is an identity: [`Self::cow_unique`] never copies it.
    fn is_identity_type(&self, type_id: u32) -> bool {
        self.has_finalizer(type_id)
    }

    /// Copy-on-write: a second place now names `v`'s object, so the next
    /// write through either place copies it first. Anything but a class
    /// instance or an array is left alone. A pinned object
    /// ([`Object::is_pinned`]: a name writes it in place, untested) is
    /// never marked: the new place gets a copy of it instead, made as
    /// [`Self::cow_unique`] makes one. So a pinned object is never shared.
    #[inline]
    pub fn cow_share(&mut self, v: Value) -> CowShare {
        match self.find_object_by_addr(v.heap_addr()) {
            Some(obj @ (Object::Instance(_) | Object::Array(_))) if obj.is_pinned() => match self.cow_copy(obj, &mut HashMap::new()) {
                Some(copy) => CowShare::Copied(copy),
                None => CowShare::Untouched,
            },
            Some(obj @ (Object::Instance(_) | Object::Array(_))) => {
                obj.set_shared();
                CowShare::Marked
            }
            _ => CowShare::Untouched,
        }
    }

    /// Copy-on-write: before a write through `v`, a shallow copy of its
    /// object when another place may name it. The original and the copy
    /// then both name the children, so those are marked shared in turn,
    /// and a pinned child is copied for the copy instead.
    /// `None` when `v` can be written in place: not shared, an identity
    /// class, or not a class instance or array.
    pub fn cow_unique(&mut self, v: Value) -> Option<Value> {
        let obj = self.find_object_by_addr(v.heap_addr())?;
        if !obj.is_shared() {
            return None;
        }
        self.cow_copy(obj, &mut HashMap::new())
    }

    /// A shallow copy of `obj` (a class instance or an array), whose
    /// children both now name: each is marked shared, or, pinned, copied
    /// in turn for the copy (deep through pinned objects; `done` maps what
    /// this copy made already, so a child held twice is copied once).
    /// `None` for an identity class or another kind of object. Allocates
    /// and never collects, as the `Cow` helpers do.
    fn cow_copy(&mut self, obj: Object, done: &mut HashMap<u64, u64>) -> Option<Value> {
        let copy = match obj {
            Object::Instance(gc) => {
                let inst = gc.as_ref();
                if inst.type_id == 0 || self.is_identity_type(inst.type_id) {
                    return None;
                }
                let slots = inst.storage.as_slice().to_vec();
                self.alloc(ObjInstance::with_slots(inst.type_id, slots), Object::Instance).0
            }
            Object::Array(gc) => {
                let copy = gc.as_ref().copy();
                self.alloc(copy, Object::Array).0
            }
            _ => return None,
        };
        done.insert(obj.addr(), copy.addr());
        let mut pinned = Vec::new();
        copy.for_each_reference(self, &mut |addr, _| {
            if let Some(child @ (Object::Instance(_) | Object::Array(_))) = self.find_object_by_addr(addr) {
                if child.is_pinned() {
                    pinned.push(child);
                } else {
                    child.set_shared();
                }
            }
        });
        for child in pinned {
            let to = match done.get(&child.addr()) {
                Some(&to) => Some(Value::from(to)),
                None => self.cow_copy(child, done),
            };
            if let Some(to) = to {
                copy.replace_reference(child.addr(), to);
            }
        }
        self.cow_copies += 1;
        Some(Value::from(copy.addr()))
    }

    /// How many objects [`Self::cow_unique`] copied.
    pub fn cow_copies(&self) -> u64 {
        self.cow_copies
    }

    /// `--cow-report`: count the copies each site makes from now on.
    pub fn report_cow(&mut self) {
        self.cow_sites = Some(std::collections::HashMap::new());
    }

    pub fn cow_report_on(&self) -> bool {
        self.cow_sites.is_some()
    }

    /// A `Cow` at `site` ran; `mark` is [`Self::cow_copies`] before it.
    pub fn cow_report_note(&mut self, mark: u64, site: u32) {
        if self.cow_copies > mark
            && let Some(sites) = self.cow_sites.as_mut()
        {
            *sites.entry(site).or_insert(0) += 1;
        }
    }

    /// A `Cow` shared an object.
    pub fn note_cow_share(&mut self) {
        self.cow_shares += 1;
    }

    pub fn cow_shares(&self) -> u64 {
        self.cow_shares
    }

    /// The copies by site since [`Self::report_cow`].
    pub fn take_cow_sites(&mut self) -> Option<std::collections::HashMap<u32, u64>> {
        self.cow_sites.take()
    }

    /// A generator suspended with a `defer` armed: it owes a close, which
    /// the collector (or teardown) runs once nothing reaches it.
    pub fn note_closeable(&mut self, object: Object) {
        self.finalizable.push(object.addr());
    }

    /// An enum typed after its allocation (`TagEnumType`).
    pub fn note_enum_type(&mut self, object: Object, type_id: u32) {
        if self.has_finalizer(type_id) {
            self.finalizable.push(object.addr());
        }
    }

    /// Objects the last mark left unmarked that still owe their `drop`,
    /// in address order. Prunes [`Self::finalizable`] to the objects that
    /// still exist, still have a `drop` type and have not been finalized.
    pub fn unmarked_finalizable(&mut self) -> Vec<Object> {
        if self.finalizable.is_empty() {
            return Vec::new();
        }
        let mut list = std::mem::take(&mut self.finalizable);
        list.sort_unstable();
        list.dedup();
        let mut out = Vec::new();
        list.retain(|&addr| {
            let Some(obj) = self.find_object_by_addr(addr) else {
                return false;
            };
            let (type_id, finalized) = match obj {
                Object::Instance(gc) => (gc.as_ref().type_id, obj.is_finalized()),
                Object::Enum(gc) => (gc.as_ref().type_id, obj.is_finalized()),
                // A closeable generator stays listed until it is done.
                Object::Coroutine(gc) => {
                    let open = gc.as_ref().closeable && gc.as_ref().state == CoroState::Suspended;
                    if open && !obj.is_marked() {
                        out.push(obj);
                    }
                    return open;
                }
                _ => return false,
            };
            if finalized || !self.has_finalizer(type_id) {
                return false;
            }
            if !obj.is_marked() {
                out.push(obj);
            }
            true
        });
        self.finalizable = list;
        out
    }

    /// Write a fresh object into a slab slot and stamp its kind.
    #[inline(always)]
    fn init_slot<T: GcSized, F>(slot: NonNull<GcData<T>>, data: T, map: F) -> (Object, Gc<T>)
    where
        F: Fn(Gc<T>) -> Object,
    {
        // Write the header and the payload straight into the slot. Building a
        // `GcData` on the stack first cost a second payload copy, and its
        // misaligned read-back straddled the separately stored header bytes
        // (a store-forwarding stall on every allocation).
        unsafe {
            let p = slot.as_ptr();
            ptr::addr_of_mut!((*p).header).write(GcHeader::new());
            ptr::addr_of_mut!((*p).data).write(data);
        }
        let content = Gc::from_slot(slot);
        let object = map(content);
        content.set_kind(object.kind());
        (object, content)
    }

    /// Interns a string and returns its handle. The same reference is returned
    /// for two equal strings.
    pub fn intern(&mut self, data: String) -> RefString {
        let _epoch_guard = self.epoch_guard();
        let hash = ObjString::hash(&data);
        if let Some(s) = self.strings.find(&data, hash) {
            return s;
        }
        self.intern_new(data, hash)
    }

    /// Allocate a runtime string without interning it. Equality compares
    /// content, and table keys are interned on use ([`Self::intern_ref`]),
    /// so values built by concat / format / decoding skip the intern hash.
    pub fn alloc_string(&mut self, data: String) -> RefString {
        self.alloc(ObjString::new(data), Object::String).1
    }

    /// Allocate bytes `[from, to)` of `src`, sharing its buffer when that is
    /// cheap ([`super::StrData::slice`]). `None` when an offset is out of
    /// range or splits a UTF-8 sequence.
    pub fn alloc_slice(&mut self, src: RefString, from: usize, to: usize) -> Option<RefString> {
        let data = src.as_ref().data.slice(from, to)?;
        Some(self.alloc(ObjString::from_data(data), Object::String).1)
    }

    /// Intern a borrowed string without allocating when it is already cached.
    pub fn intern_str(&mut self, data: &str) -> RefString {
        let _epoch_guard = self.epoch_guard();
        let hash = ObjString::hash(data);
        if let Some(s) = self.strings.find(data, hash) {
            return s;
        }
        self.intern_new(data.to_owned(), hash)
    }

    /// Register an existing string object in the intern table when needed.
    pub fn intern_ref(&mut self, string: RefString) -> RefString {
        let _epoch_guard = self.epoch_guard();
        let data = string.as_ref();
        if let Some(s) = self.strings.find(&data.data, data.hash_code()) {
            return s;
        }
        // The table is not traced by a minor collection: a young key would
        // be freed under it. Intern an old copy instead.
        if string.header.generation.get() & GEN_YOUNG != 0 {
            let hash = data.hash_code();
            return self.intern_new(data.data.to_string(), hash);
        }
        self.strings.insert(string, ());
        string
    }

    fn intern_new(&mut self, data: String, hash: u32) -> RefString {
        let obj_string = ObjString::with_hash(data, hash);
        let (_, s) = self.alloc_unlocked_in(obj_string, Object::String, false);
        self.strings.insert(s, ());
        s
    }

    /// Allocate a loaded FFI library as `Object::Library`.
    pub fn alloc_library(
        &mut self,
        library: std::sync::Arc<crate::ffi::Library>,
    ) -> (Object, crate::memory::Gc<ObjLibrary>) {
        let obj_lib = ObjLibrary {
            library,
            signatures: Vec::new(),
            by_name: std::collections::HashMap::new(),
            closures: Vec::new(),
            static_symbols: None,
        };
        self.alloc(obj_lib, Object::Library)
    }

    /// Allocate an enum value, reusing the immortal object for unit variants.
    pub fn alloc_enum_value(&mut self, tag: u32, payload: impl Into<EnumPayload>) -> common::Value {
        let payload = payload.into();
        let object = if payload.is_empty() {
            self.immortal_unit_enum(tag)
        } else {
            self.alloc(ObjEnum::new(tag, payload), Object::Enum).0
        };
        common::Value::from(object.addr())
    }

    /// Releases all objects that aren't marked. This method also removes
    /// interned strings when no object is referencing them.
    ///
    /// ## Safety
    ///
    /// The caller must ensure that all reachable pointers have been marked.
    /// Otherwise, we'll deallocate objects that are in use and leave dangling
    /// pointers.
    pub unsafe fn sweep(&mut self) {
        if self.gc_phase == GcPhase::Sweeping {
            self.finish_sweep();
            return;
        }
        self.unlink_unmarked_interns();
        self.gc_phase = GcPhase::Sweeping;
        self.sync_inline_gate();
        self.unmark_young();
        self.gc_sweep_start_bytes = self.counts.alloc_bytes;
        self.gc_sweep_freed = 0;
        self.gc_sweep_cursor = Some(super::slab::SlotCursor::sweep());
        self.finish_sweep();
    }

    #[inline]
    pub fn gc_phase(&self) -> GcPhase {
        self.gc_phase
    }

    #[inline]
    pub fn gc_is_idle(&self) -> bool {
        self.gc_phase == GcPhase::Idle
    }

    #[inline]
    pub fn gc_is_sweeping(&self) -> bool {
        self.gc_phase == GcPhase::Sweeping
    }

    /// Mark `v` and everything it reaches when a cycle is open.
    ///
    /// Marking always drains before the mutator runs again (finalizers run
    /// with an empty gray list), so the only way user code can reach an
    /// unmarked object is a `Weak` upgrade before dead weaks are cleared.
    /// Draining here restores the "no gray objects while the mutator runs"
    /// invariant, which is why stores need no write barrier.
    pub fn resurrect_during_mark(&mut self, v: Value) {
        if unlikely(self.gc_phase == GcPhase::Marking) {
            self.shade_value(v);
            while !self.mark_quantum(usize::MAX) {}
        }
    }

    /// Mark a finalizable object (and later its graph) before its `drop` runs.
    pub fn shade_for_finalizer(&mut self, obj: Object) {
        if self.gc_phase == GcPhase::Marking {
            self.shade_object(obj);
        }
    }

    fn shade_value(&mut self, v: Value) {
        let addr = v.heap_addr();
        if addr == 0 {
            return;
        }
        if let Some(obj) = self.find_object_by_addr(addr) {
            self.shade_object(obj);
        }
    }

    fn shade_object(&mut self, obj: Object) {
        let mut gray = std::mem::take(&mut self.gc_gray);
        obj.mark(&mut gray);
        self.gc_gray = gray;
    }

    /// Seed the gray list from `root_addrs` via O(1) slab lookup (no list walk).
    pub fn begin_mark(&mut self, root_addrs: &[u64]) {
        if self.gc_phase != GcPhase::Idle {
            return;
        }
        self.gc_phase = GcPhase::Marking;
        self.sync_inline_gate();
        let mut gray = std::mem::take(&mut self.gc_gray);
        gray.clear();
        for &addr in root_addrs {
            let addr = addr & !1;
            if addr == 0 {
                continue;
            }
            if let Some(obj) = self.find_object_by_addr(addr) {
                obj.mark(&mut gray);
            }
        }
        self.gc_gray = gray;
    }

    /// Scan up to `n` gray objects. Returns true when the worklist is empty.
    pub fn mark_quantum(&mut self, n: usize) -> bool {
        let mut gray = std::mem::take(&mut self.gc_gray);
        let mut i = 0;
        while i < n {
            let Some(obj) = gray.pop() else {
                break;
            };
            obj.mark_references(self, &mut gray);
            i += 1;
        }
        let done = gray.is_empty();
        self.gc_gray = gray;
        done
    }

    pub fn begin_sweep(&mut self) {
        if self.gc_phase == GcPhase::Sweeping {
            return;
        }
        self.unlink_unmarked_interns();
        self.unmark_young();
        self.gc_phase = GcPhase::Sweeping;
        self.sync_inline_gate();
        self.gc_sweep_start_bytes = self.counts.alloc_bytes;
        self.gc_sweep_freed = 0;
        self.gc_sweep_cursor = Some(super::slab::SlotCursor::sweep());
    }

    /// Reclaim up to `n` unmarked objects. Returns true when sweep finished.
    pub fn sweep_quantum(&mut self, n: usize) -> bool {
        if self.gc_phase != GcPhase::Sweeping {
            return true;
        }
        for _ in 0..n {
            if self.gc_sweep_cursor.is_none() {
                self.finish_sweep_cycle();
                return true;
            }
            self.sweep_one();
        }
        if self.gc_sweep_cursor.is_none() {
            self.finish_sweep_cycle();
            true
        } else {
            false
        }
    }

    /// Drain the remaining sweep cursor and rescale the byte threshold.
    pub fn finish_sweep(&mut self) {
        if self.gc_phase != GcPhase::Sweeping {
            return;
        }
        while self.gc_sweep_cursor.is_some() {
            self.sweep_one();
        }
        self.finish_sweep_cycle();
    }

    /// Visit the next object (free slots are skipped): free it if unmarked,
    /// unmark it if marked, and let one allocated during this sweep
    /// (`fresh`) through.
    fn sweep_one(&mut self) {
        let Some(mut cur) = self.gc_sweep_cursor else {
            return;
        };
        let obj = loop {
            let Some(addr) = self.slab.next_slot(&mut cur) else {
                self.gc_sweep_cursor = None;
                return;
            };
            if let Some(obj) = unsafe { Object::from_header(addr) } {
                break obj;
            }
        };
        // A `fresh` object is kept whatever its mark (it may postdate the
        // mark phase) — but still unmarked: a stale mark would make the next
        // cycle skip tracing its children.
        // SAFETY: a live slot starts with its header.
        let header = unsafe { &*(obj.addr() as *const GcHeader) };
        let fresh = header.fresh.replace(false);
        if !header.marked.replace(false) && !fresh {
            unsafe { self.dealloc_swept(obj, &mut cur) };
        }
        self.gc_sweep_cursor = Some(cur);
    }

    fn finish_sweep_cycle(&mut self) {
        // Floor at the initial budget: a tiny live set would otherwise
        // schedule a collection every few allocations.
        let growth = if self.counts.alloc_bytes.saturating_mul(2) > self.gc_sweep_start_bytes {
            self.gc_growth_factor.max(GC_GROWTH_FACTOR_SURVIVING)
        } else {
            self.gc_growth_factor
        };
        // What the mark kept, not what a lazy sweep let the program allocate
        // meanwhile: counting those (pretenured objects, mostly) as live
        // would push the next collection out by the growth factor.
        let survived = self.gc_sweep_start_bytes.saturating_sub(self.gc_sweep_freed).min(self.counts.alloc_bytes);
        self.gc_next_threshold = (survived.saturating_sub(self.counts.young_bytes))
            .saturating_mul(growth)
            .max(GC_NEXT_THRESHOLD)
            .min(self.byte_cap);
        // Empty chunks the program did not touch all cycle go back to the OS.
        self.slab.release_idle_chunks();
        self.gc_phase = GcPhase::Idle;
        self.sync_inline_gate();
        self.gc_sweep_cursor = None;
    }

    fn unlink_unmarked_interns(&mut self) {
        let mut dangling_strings = std::mem::take(&mut self.gc_dangling_strings);
        dangling_strings.clear();
        for (k, ()) in self.strings.iter() {
            if !k.is_marked() {
                dangling_strings.push(k);
            }
        }
        for s in dangling_strings.drain(..) {
            self.strings.remove(s);
        }
        self.gc_dangling_strings = dangling_strings;
    }

    /// Free the object `v` points at before a collection would, because
    /// the compiler proved nothing else reaches it (`feature/escape-memory`
    /// step 2). `deep` also frees every object it reaches through precise
    /// pointer words. `keep` names objects the runtime still holds itself (frame
    /// pins); those and anything this cannot free safely stay for the GC:
    /// interned strings, the shared unit variants and `()`, objects whose
    /// `drop` has not run (the runtime runs it first when [`Self::early_drops`]
    /// lists it), handles, and everything while a cycle or a steal epoch is
    /// open. `check` frees nothing and records the objects instead, which
    /// the next mark must not reach. Returns how many objects it freed.
    /// [`Self::early_freed`] lists the addresses it freed.
    pub fn free_early(&mut self, v: Value, deep: bool, check: bool, keep: &dyn Fn(u64) -> bool) -> usize {
        self.early_freed.clear();
        if !deep {
            // One object: no walk.
            let Some(root) = self.early_root(v, keep) else {
                return 0;
            };
            if self.pending_drop(root) {
                return 0;
            }
            if check {
                self.doomed.push(root.addr());
                return 0;
            }
            self.freed_early = true;
            self.early_freed.push(root.addr());
            unsafe { self.dealloc(root) };
            return 1;
        }
        let Some((mut found, reaches_drop, _)) = self.early_walk(v, deep, keep) else {
            return 0;
        };
        // A `drop` that has not run yet keeps its object (and what reaches
        // it: the `drop` may read anything the walk would free) for the
        // collector; [`Self::early_drops`] lets the runtime run it first.
        let n = if reaches_drop {
            0
        } else if check {
            self.doomed.extend(found.iter().map(Object::addr));
            0
        } else {
            self.freed_early = true;
            self.early_freed.extend(found.iter().map(Object::addr));
            for &obj in &found {
                unsafe { self.dealloc(obj) };
            }
            found.len()
        };
        found.clear();
        self.early_scratch.0 = found;
        n
    }

    /// The objects an early free of `v` would give back once their `drop`s
    /// ran, in walk order (the root first, a parent before what it
    /// reaches): `None` when the free would free nothing, or when one of
    /// them has a `drop` that may not run there. Empty when no `drop` is
    /// pending. The runtime runs these `drop`s, then calls [`Self::free_early`].
    pub fn early_drops(&mut self, v: Value, deep: bool, keep: &dyn Fn(u64) -> bool) -> Option<Vec<Object>> {
        if self.finalizer_types.is_empty() {
            return Some(Vec::new());
        }
        let (found, reaches_drop, blocked) = self.early_walk(v, deep, keep)?;
        if !reaches_drop {
            return Some(Vec::new());
        }
        if blocked {
            return None;
        }
        let mut drops = Vec::new();
        for obj in found {
            if self.pending_drop(obj) {
                if !Self::finalizer_type_of(obj).is_some_and(|t| self.early_drop_type(t)) {
                    return None;
                }
                drops.push(obj);
            }
        }
        Some(drops)
    }

    /// The objects an early free of `v` gives back, whether a pending
    /// `drop` object is among them or reached from them, and whether one
    /// it reaches stays behind (shared or kept). `None` when it
    /// frees nothing (a collection is under way, the root is kept or not
    /// freeable). A pending `drop` object is walked like any other.
    fn early_walk(&mut self, v: Value, deep: bool, keep: &dyn Fn(u64) -> bool) -> Option<(Vec<Object>, bool, bool)> {
        let root = self.early_root(v, keep)?;
        let mut reaches_drop = self.pending_drop(root);
        if !deep {
            return Some((vec![root], reaches_drop, false));
        }
        let mut blocked = false;
        let (mut found, mut work, mut children) = std::mem::take(&mut self.early_scratch);
        found.clear();
        found.push(root);
        // Every mark bit is clear while idle: a set bit means visited.
        work.clear();
        root.mark(&mut work);
        children.clear();
        let mut i = 0;
        while i < found.len() {
            // Walk in `found` order (breadth first): a parent comes before
            // what it reaches, so its `drop` runs first.
            let obj = found[i];
            i += 1;
            work.clear();
            obj.for_each_reference(self, &mut |addr, precise| {
                if precise {
                    children.push(addr);
                }
            });
            for addr in children.drain(..) {
                // SAFETY: `for_each_reference` visits only the addresses of
                // live objects (it looked each word up).
                let Some(child) = (unsafe { Object::from_header(addr) }) else {
                    continue;
                };
                if child.is_marked() {
                    continue;
                }
                let pending = self.pending_drop(child);
                reaches_drop |= pending;
                if self.early_freeable(child) && !keep(addr) {
                    child.mark(&mut work);
                    found.push(child);
                } else if pending {
                    // A `drop` the free leaves pending reads what it
                    // reaches when it runs.
                    blocked = true;
                }
            }
        }
        for obj in &found {
            obj.unmark();
        }
        work.clear();
        self.early_scratch = (Vec::new(), work, children);
        Some((found, reaches_drop, blocked))
    }

    /// The object an early free of `v` starts at: `None` while a collection
    /// or a steal epoch is open, or when it is kept or not freeable.
    fn early_root(&self, v: Value, keep: &dyn Fn(u64) -> bool) -> Option<Object> {
        if self.gc_phase != GcPhase::Idle || self.epoch_stw || !self.alloc_lock.is_null() {
            return None;
        }
        let root = self.find_object_by_addr(v.heap_addr())?;
        (self.early_freeable(root) && !keep(root.addr())).then_some(root)
    }

    /// The addresses the last [`Self::free_early`] gave back.
    pub fn early_freed(&self) -> &[u64] {
        &self.early_freed
    }

    /// Whether `addr` may be a slot [`Self::free_early`] gave back: a stale
    /// reference there is no heap corruption.
    pub fn early_freed_slot(&self, addr: u64) -> bool {
        self.freed_early && self.slab.contains_slot(addr)
    }

    /// Whether check mode recorded objects since the last check.
    pub fn has_doomed(&self) -> bool {
        !self.doomed.is_empty()
    }

    /// Whether `obj` is an object of a type with a `drop`.
    fn finalizer_object(&self, obj: Object) -> bool {
        !self.finalizer_types.is_empty() && Self::finalizer_type_of(obj).is_some_and(|t| self.has_finalizer(t))
    }

    /// Whether `obj` has a `drop` that has not run yet.
    fn pending_drop(&self, obj: Object) -> bool {
        self.finalizer_object(obj)
            && matches!(obj, Object::Instance(_) | Object::Enum(_))
            && !obj.is_finalized()
    }

    /// Whether [`Self::free_early`] may free `obj` (when nothing reaches it).
    fn early_freeable(&self, obj: Object) -> bool {
        match obj {
            Object::String(s) => {
                // Interning hashes a string first; an unhashed one is not interned.
                let s_ref = s.as_ref();
                !s_ref.hash_set() || self.strings.find(&s_ref.data, s_ref.hash_code()).is_none_or(|i| i.as_ptr() != s.as_ptr())
            }
            Object::Array(_) => true,
            // An object whose `drop` is pending is walked; [`Self::free_early`]
            // leaves it to the collector unless its `drop` already ran.
            Object::Enum(e) => !e.as_ref().payload.is_empty(),
            Object::Instance(_) => true,
            _ => false,
        }
    }

    /// Check mode of [`Self::free_early`], once a mark from the roots that
    /// are certainly live is complete: an object it would have freed that
    /// the mark reached was still in use.
    pub fn check_doomed(&mut self) {
        for addr in std::mem::take(&mut self.doomed) {
            if let Some(obj) = self.find_object_by_addr(addr)
                && obj.is_marked()
            {
                let root = self.gc_roots.contains(&addr);
                let mut holders = Vec::new();
                for o in self.objects() {
                    o.for_each_reference(self, &mut |a, precise| {
                        if a == addr {
                            holders.push(format!("{o}{}", if precise { "" } else { " (imprecise)" }));
                        }
                    });
                }
                eprintln!("early free check: {obj} at {addr:#x} was freed while still reachable (root: {root}, held by: {holders:?})");
                std::process::abort();
            }
        }
    }

    /// Returns the number of bytes that are being allocated.
    pub const fn size(&self) -> usize {
        self.counts.alloc_bytes
    }

    /// Limit the heap to `cap` bytes (`usize::MAX`: unlimited). The heap
    /// itself never refuses an allocation; the runtime checks
    /// [`Self::over_byte_cap`] at its GC safepoints.
    pub fn set_byte_cap(&mut self, cap: usize) {
        self.byte_cap = cap;
        self.gc_next_threshold = self.gc_next_threshold.min(cap);
    }

    /// An empty heap with this one's settings.
    pub(crate) fn emptied(&self) -> Self {
        let mut heap = Self::default();
        heap.set_nursery(self.nursery);
        heap.set_byte_cap(self.byte_cap);
        heap
    }

    /// The cap from [`Self::set_byte_cap`].
    pub fn byte_cap(&self) -> usize {
        self.byte_cap
    }

    /// True when the heap holds more than [`Self::set_byte_cap`] allows.
    #[inline]
    pub fn over_byte_cap(&self) -> bool {
        self.counts.alloc_bytes > self.byte_cap
    }

    /// Slab chunks currently mapped (64KiB each). Sweep does not unmap.
    pub fn slab_chunk_count(&self) -> usize {
        self.slab.chunk_count()
    }

    /// Mapped slab bytes (not payload `Vec`s).
    pub fn mapped_bytes(&self) -> usize {
        self.slab.mapped_bytes()
    }

    /// Number of live heap objects (for GC pressure after `HostInvoke`).
    #[inline]
    pub fn live_object_count(&self) -> usize {
        self.counts.live_count
    }

    /// True when idle and live heap bytes exceed the collection threshold.
    /// Mid-cycle work is paced from the alloc safepoint, not a second start.
    /// In a steal epoch, where a collect means an abort, the bar is the
    /// epoch ceiling instead; the joiner collects normally once it closes.
    #[inline]
    pub fn should_collect(&self) -> bool {
        let limit = if self.epoch_stw {
            self.epoch_gc_ceiling
        } else {
            self.gc_next_threshold
        };
        self.gc_phase == GcPhase::Idle
            && (cfg!(feature = "gc-stress")
                || self.counts.alloc_bytes.saturating_sub(self.counts.young_bytes) > limit
                || self.held_over_budget()
                || self.slab.young_chunks_in_use() >= self.nursery_chunks)
    }

    /// Enough nursery chunks are held for pinned survivors to ask for a
    /// major cycle, the only one that frees them: a nursery's worth more
    /// than the last major kept, and at least twice that. A budget from
    /// what the last major found live is what keeps the work linear (#2350):
    /// a growing set of pinned survivors (a map's values, in a vector a
    /// shared generic body made, whose words are of unknown kind) left
    /// every safepoint over a fixed
    /// budget, and each started a major that marked the whole heap again.
    /// Like the byte threshold's growth factor, it also bounds the held
    /// chunks to about twice the live ones.
    #[inline]
    fn held_over_budget(&self) -> bool {
        self.slab.held_chunks() >= self.held_budget
    }

    /// A major cycle (or a promotion) left `held` nursery chunks held.
    pub(super) fn set_held_after_major(&mut self, held: usize) {
        self.held_after_major = held;
        self.sync_held_budget();
    }

    /// Recompute [`Self::held_budget`]. Native code checks
    /// [`Self::should_collect`] on every allocation, so the budget is kept
    /// rather than computed there.
    fn sync_held_budget(&mut self) {
        self.held_budget = self.held_after_major + self.nursery_chunks.max(self.held_after_major);
    }

    /// The nursery is full: run a minor collection.
    #[inline]
    pub fn should_collect_young(&self) -> bool {
        self.nursery
            && (cfg!(feature = "gc-stress") || self.slab.young_chunks_in_use() >= self.nursery_chunks)
    }

    /// A minor collection that could not move promoted the nursery in
    /// place: allocate old for a nursery's worth, into the promoted
    /// chunks' free tails. Otherwise a nested run, where every minor
    /// promotes, maps a fresh nursery chunk per collection: under
    /// `gc-stress` one per allocation, thousands of mostly empty chunks
    /// that every later collection sweeps.
    pub fn pretenure_after_promote(&mut self) {
        self.pretenure_bytes = self.nursery_chunks * super::slab::CHUNK_BYTES;
        self.sync_inline_gate();
    }

    /// A minor collection found `survivors` of `objects` young objects
    /// live. When most survived (a large structure being built), copying
    /// them buys nothing: allocate old for a while, longer each time in a
    /// row, and try the nursery again after.
    pub(super) fn note_survival(&mut self, survivors: usize, objects: usize) {
        // A minor that found nothing young (only held chunks) says nothing
        // about survival.
        if self.region_depth != 0 || objects == 0 {
            return;
        }
        if survivors * 2 > objects {
            self.pretenure_streak = (self.pretenure_streak + 1).min(6);
            self.pretenure_bytes = (self.nursery_chunks * super::slab::CHUNK_BYTES) << (self.pretenure_streak - 1);
            self.sync_inline_gate();
        } else {
            self.pretenure_streak = 0;
        }
    }

    /// A `region` block opens. Returns true for the outermost one, which
    /// the runtime starts with an empty nursery.
    pub fn region_enter(&mut self) -> bool {
        self.region_depth += 1;
        if self.region_depth != 1 {
            return false;
        }
        self.region_saved_chunks = self.nursery_chunks;
        self.nursery_chunks = self.nursery_chunks.max(REGION_CHUNKS);
        self.sync_held_budget();
        self.pretenure_bytes = 0;
        self.sync_inline_gate();
        self.pretenure_streak = 0;
        true
    }

    /// A `region` block ends. Returns true for the outermost one, after
    /// which the runtime collects the nursery.
    pub fn region_exit(&mut self) -> bool {
        if self.region_depth == 0 {
            return false;
        }
        self.region_depth -= 1;
        if self.region_depth != 0 {
            return false;
        }
        self.nursery_chunks = self.region_saved_chunks;
        self.sync_held_budget();
        true
    }

    /// The old space crossed its budget: run a major cycle.
    #[inline]
    pub fn should_collect_old(&self) -> bool {
        let limit = if self.epoch_stw {
            self.epoch_gc_ceiling
        } else {
            self.gc_next_threshold
        };
        cfg!(feature = "gc-stress")
            || self.counts.alloc_bytes.saturating_sub(self.counts.young_bytes) > limit
            || self.held_over_budget()
    }

    /// Allocate instances, enums, boxes, tuples, arrays and closures in the
    /// nursery (the runtime turns it on; `--nursery off` keeps it off).
    pub fn set_nursery(&mut self, on: bool) {
        self.nursery = on;
        self.sync_inline_gate();
    }

    pub fn nursery(&self) -> bool {
        self.nursery
    }

    /// Objects to sweep at one safepoint (doubles under pressure).
    #[inline]
    pub fn gc_sweep_quantum(&self) -> usize {
        if self.counts.alloc_bytes > self.gc_next_threshold {
            GC_SWEEP_QUANTUM.saturating_mul(2)
        } else {
            GC_SWEEP_QUANTUM
        }
    }

    /// Lower the byte threshold so the next [`Self::should_collect`] check
    /// fires (test helper for GC stress). The nursery then collects at every
    /// safepoint that finds a young object.
    #[cfg(test)]
    pub fn set_gc_threshold_for_test(&mut self, bytes: usize) {
        self.gc_next_threshold = bytes;
        self.nursery_chunks = 1;
        self.sync_held_budget();
    }

    /// Count an array's storage change (see [`ObjArray::take_resize`]).
    #[inline]
    pub fn account_array(&mut self, arr: &mut ObjArray) {
        if let Some((old, new)) = arr.take_resize() {
            self.account_resize(old, new);
        }
    }

    /// Adjust tracked heap bytes after an in-place grow/shrink of a managed
    /// object's internal Rust allocation (for example `ObjArray.elements`).
    pub fn account_resize(&mut self, old_size: usize, new_size: usize) {
        let _epoch_guard = self.epoch_guard();
        if new_size >= old_size {
            self.counts.alloc_bytes += new_size - old_size;
        } else {
            self.counts.alloc_bytes -= old_size - new_size;
        }
    }

    /// Deallocates an object.
    ///
    /// ## Safety
    ///
    /// + The caller must ensure that no other piece of code will ever use this
    ///   reference. Otherwise, we'll risk dereferencing a dangling pointer.
    /// + Before calling this method, the caller must ensure that the object was
    ///   removed from the linked list of heap-allocated objects.
    unsafe fn dealloc(&mut self, object: Object) {
        let size = object.size();
        self.counts.alloc_bytes -= size;
        debug_assert!(self.counts.live_count > 0);
        self.counts.live_count -= 1;
        let ptr = unsafe { NonNull::new_unchecked(object.addr() as *mut u8) };
        let young = object.generation() & GEN_YOUNG != 0;
        unsafe { object.recycle_payload() };
        if young {
            // Reused by the nursery until its chunk is reset.
            self.counts.young_bytes = self.counts.young_bytes.saturating_sub(size);
            self.slab.free_young(ptr);
        } else {
            self.slab.free(ptr);
        }
    }

    /// [`Self::dealloc`] of the slot the sweep walk is at.
    unsafe fn dealloc_swept(&mut self, object: Object, cur: &mut super::slab::SlotCursor) {
        let size = object.size();
        self.counts.alloc_bytes -= size;
        self.gc_sweep_freed += size;
        debug_assert!(self.counts.live_count > 0);
        self.counts.live_count -= 1;
        let ptr = unsafe { NonNull::new_unchecked(object.addr() as *mut u8) };
        unsafe { object.recycle_payload() };
        self.slab.free_at_cursor(cur, ptr);
    }

    pub fn trace(&mut self, values: &[u64]) {
        let mut gray = std::mem::take(&mut self.gc_gray);
        gray.clear();
        for &addr in values {
            let addr = addr & !1;
            if addr == 0 {
                continue;
            }
            if let Some(obj) = self.find_object_by_addr(addr) {
                obj.mark(&mut gray);
            }
        }
        gray.clear();
        self.gc_gray = gray;
    }

    /// Mark a `Value` if it is a live heap pointer.
    ///
    /// Strips the Result `Err` low bit before lookup so tagged payloads stay live.
    pub fn mark_value(&self, v: Value, gray: &mut Vec<Object>) {
        let addr = v.heap_addr();
        if addr == 0 {
            return;
        }
        if let Some(child) = self.find_object_by_addr(addr) {
            child.mark(gray);
        }
    }

    /// Push `root_addrs` onto the gray list without draining it (end-of-mark remark).
    pub fn remark_roots(&mut self, root_addrs: &[u64]) {
        let mut gray = std::mem::take(&mut self.gc_gray);
        for &addr in root_addrs {
            let addr = addr & !1;
            if addr == 0 {
                continue;
            }
            if let Some(obj) = self.find_object_by_addr(addr) {
                obj.mark(&mut gray);
            }
        }
        self.gc_gray = gray;
    }

    /// Mark `root_addrs` and walk children via [`Object::mark_references`].
    ///
    /// Seeds gray via slab lookup (not an O(heap) list scan).
    pub fn mark_from_roots(&mut self, root_addrs: &[u64]) {
        self.gc_gray.clear();
        self.remark_roots(root_addrs);
        while !self.mark_quantum(usize::MAX) {}
    }

    /// Complete collect with no host's roots: mark `extra_roots` plus immortal
    /// enums, clear dead weaks, sweep.
    pub fn collect(&mut self, extra_roots: &[u64]) {
        if self.epoch_stw {
            return;
        }
        if self.gc_phase == GcPhase::Sweeping {
            self.finish_sweep();
        }
        let mut roots = self.take_gc_roots();
        roots.extend_from_slice(extra_roots);
        if self.gc_phase == GcPhase::Idle {
            self.begin_mark(&roots);
        } else {
            self.mark_from_roots(&roots);
        }
        while !self.mark_quantum(usize::MAX) {}
        self.clear_dead_weaks();
        self.begin_sweep();
        self.finish_sweep();
        self.restore_gc_roots(roots);
    }

    /// Clear [`Object::Weak`] handles whose referents were not marked.
    ///
    /// Must run after the mark phase and before [`Self::sweep`] so upgrades
    /// never observe a recycled address (ABA).
    pub fn clear_dead_weaks(&mut self) {
        if self.weaks.is_empty() {
            return;
        }
        let mut list = std::mem::take(&mut self.weaks);
        list.sort_unstable();
        list.dedup();
        // A cleared weak stays cleared: only live, uncleared ones stay listed.
        list.retain(|&addr| {
            let Some(Object::Weak(gc)) = self.find_object_by_addr(addr) else {
                return false;
            };
            let weak = gc.as_ref();
            if weak.cleared.get() {
                return false;
            }
            let target = weak.target.get().heap_addr();
            if target != 0
                && let Some(referent) = self.find_object_by_addr(target)
                && !referent.is_marked()
            {
                weak.cleared.set(true);
                weak.target.set(Value::from(0i64));
                return false;
            }
            true
        });
        self.weaks = list;
    }

    /// Follow an evacuation in the side lists (before its old slots free).
    fn forward_side_lists(&mut self, fwd: &AddrMap) {
        for addr in self.weaks.iter_mut().chain(self.finalizable.iter_mut()) {
            if let Some(&new) = fwd.get(addr) {
                *addr = new;
            }
        }
    }

    /// Take the reusable GC root address buffer (caller must restore via [`Self::restore_gc_roots`]).
    /// Immortal arity-0 enum singletons are always seeded as roots.
    pub fn take_gc_roots(&mut self) -> Vec<u64> {
        let mut roots = std::mem::take(&mut self.gc_roots);
        roots.clear();
        for obj in self.immortal_enums.values() {
            roots.push(obj.addr());
        }
        roots.extend(self.immortal_fns.values().map(Object::addr));
        roots
    }

    /// The shared function value at `entry` that captures and fills
    /// nothing. Such a value holds only its entry and arity, so every
    /// evaluation of a lambda without captures (one per loop pass) can be
    /// one object instead of a nursery allocation per pass (#2251).
    pub fn immortal_plain_fn(&mut self, entry: u32, arity: u32, is_rest: bool) -> Object {
        if let Some(obj) = self.immortal_fns.get(&(entry, arity, is_rest)) {
            return *obj;
        }
        let _epoch_guard = self.epoch_guard();
        let pfn = ObjFn { entry, arity, is_rest, filled_mask: 0, captured_args: Vec::new(), captures: Vec::new(), dicts: None };
        // Old: a root for the runtime's lifetime, never worth a nursery copy.
        let (object, _) = self.alloc_unlocked_in(pfn, Object::Fn, false);
        self.immortal_fns.insert((entry, arity, is_rest), object);
        object
    }

    /// The shared empty tuple `()`. It has no elements to change, so every
    /// `()` (a `Result<(), E>` `Ok`, a unit return) can be one object.
    pub fn immortal_empty_tuple(&mut self) -> Object {
        self.immortal_unit_enum(common::PRODUCT_TAG)
    }

    /// Return a shared arity-0 enum for `tag`, allocating once per tag.
    pub fn immortal_unit_enum(&mut self, tag: u32) -> Object {
        let _epoch_guard = self.epoch_guard();
        if let Some((cached, obj)) = self.unit_enum
            && cached == tag
        {
            return obj;
        }
        if let Some(obj) = self.immortal_enums.get(&tag) {
            self.unit_enum = Some((tag, *obj));
            return *obj;
        }
        let obj_enum = crate::memory::ObjEnum::new(tag, EnumPayload::empty());
        let (object, _) = self.alloc_unlocked(obj_enum, Object::Enum);
        self.immortal_enums.insert(tag, object);
        self.unit_enum = Some((tag, object));
        object
    }

    pub fn restore_gc_roots(&mut self, roots: Vec<u64>) {
        self.gc_roots = roots;
    }

    pub fn take_gc_worklists(&mut self) -> (Vec<Object>, Vec<Object>) {
        let mut gray = std::mem::take(&mut self.gc_gray);
        let mut root_objects = std::mem::take(&mut self.gc_root_objects);
        gray.clear();
        root_objects.clear();
        (gray, root_objects)
    }

    pub fn restore_gc_worklists(&mut self, gray: Vec<Object>, root_objects: Vec<Object>) {
        self.gc_gray = gray;
        self.gc_root_objects = root_objects;
    }

    /// Head of the intrusive object list (for address lookup).
    /// One line of moving-GC feasibility numbers for the current mark
    /// (`gc-stats` feature, `docs/internals/gc-evacuation.md`). Call
    /// after marking completes: marked objects are the live set.
    #[cfg(feature = "gc-stats")]
    pub fn census(&self, roots: &[(u64, RootKind)]) -> String {
        use std::collections::{HashMap, HashSet};
        // chunk -> (live slots, slots, slot size)
        let mut chunks: HashMap<usize, (usize, usize, usize)> = HashMap::new();
        let (mut live, mut live_bytes) = (0usize, 0usize);
        let mut pinned: HashSet<u64> = HashSet::new();
        let (mut precise_refs, mut ambiguous_refs) = (0usize, 0usize);
        for obj in self.objects() {
            if obj.is_marked() {
                live += 1;
                if let Some((i, size, slots)) = self.slab.slot_meta(obj.addr()) {
                    live_bytes += size;
                    let e = chunks.entry(i).or_insert((0, slots, size));
                    e.0 += 1;
                }
                obj.for_each_reference(self, &mut |addr, precise| {
                    if precise {
                        precise_refs += 1;
                    } else {
                        ambiguous_refs += 1;
                        pinned.insert(addr);
                    }
                });
            }
        }
        let interior_pinned = pinned.len();
        let (mut root_precise, mut root_ambiguous, mut root_pinned) = (0usize, 0usize, 0usize);
        for &(addr, kind) in roots {
            match kind {
                RootKind::Precise => root_precise += 1,
                RootKind::Ambiguous => {
                    root_ambiguous += 1;
                    pinned.insert(addr);
                }
                RootKind::Pinned => {
                    root_pinned += 1;
                    pinned.insert(addr);
                }
            }
        }
        let mapped = self.slab.mapped_bytes();
        let total_chunks = self.slab.chunk_count();
        let empty = total_chunks.saturating_sub(chunks.len());
        // Per size class: chunks in use vs chunks the live slots would fill.
        let mut by_size: HashMap<usize, (usize, usize, usize)> = HashMap::new();
        for &(n, slots, size) in chunks.values() {
            let e = by_size.entry(size).or_insert((0, 0, slots));
            e.0 += 1;
            e.1 += n;
        }
        let packable: usize = by_size
            .values()
            .map(|&(used, live_slots, per)| used - live_slots.div_ceil(per.max(1)).min(used))
            .sum();
        let chunk = mapped.checked_div(total_chunks).unwrap_or(0);
        let pct = |a: usize, b: usize| if b == 0 { 0.0 } else { 100.0 * a as f64 / b as f64 };
        format!(
            "gc-stats: rss={}KiB mapped={}KiB released={}KiB chunks={} live={} ({}KiB, {:.1}% of mapped) | \
             reclaim unmap-empty={}KiB compact={}KiB | \
             roots precise={} ambiguous={} pinned={} | \
             interior refs precise={} ambiguous={} | pinned objs={} ({:.1}% of live; {} via interior)",
            resident_kib(),
            mapped / 1024,
            self.slab.released_bytes() / 1024,
            total_chunks,
            live,
            live_bytes / 1024,
            pct(live_bytes, mapped),
            empty * chunk / 1024,
            packable * chunk / 1024,
            root_precise,
            root_ambiguous,
            root_pinned,
            precise_refs,
            ambiguous_refs,
            pinned.len(),
            pct(pinned.len(), live),
            interior_pinned,
        )
    }

    /// Install the program's per-class field word kinds.
    pub fn set_class_word_kinds(&mut self, table: std::sync::Arc<Vec<Box<[u8]>>>) {
        self.class_kinds = table;
    }

    /// Word kinds of `type_id`'s typed fields (empty = all unknown).
    #[inline]
    pub fn class_field_kinds(&self, type_id: u32) -> &[u8] {
        self.class_kinds
            .get(common::type_kinds_index(type_id))
            .map_or(&[][..], |row| &row[..])
    }

    /// `gc-stress`: a word's declared kind must match what it holds.
    #[cfg(feature = "gc-stress")]
    fn verify_kinded_word(&self, what: &str, index: usize, kind: u8, v: Value) {
        let addr = v.heap_addr();
        let resolves = addr != 0 && self.find_object_by_addr(addr).is_some();
        if kind == common::WORD_SCALAR && resolves {
            panic!("gc-stress: scalar word {index} of {what} holds a heap reference");
        }
        if kind == common::WORD_POINTER && addr != 0 && !resolves && !self.early_freed_slot(addr) {
            panic!("gc-stress: pointer word {index} of {what} holds a non-object");
        }
    }

    /// Every live object, in slab order.
    pub fn objects(&self) -> HeapIter<'_> {
        HeapIter {
            slab: &self.slab,
            cur: super::slab::SlotCursor::default(),
        }
    }

    /// Find a heap object by its address (mapped slot + header kind).
    #[inline]
    pub fn find_object_by_addr(&self, addr: u64) -> Option<Object> {
        if addr == 0 || !self.slab.contains_slot(addr) {
            return None;
        }
        unsafe { Object::from_header(addr) }
    }

    /// The function object at `addr`, for a word whose static type is a
    /// function (a code offset or an `ObjFn`): a range check and the
    /// header kind, without [`Self::find_object_by_addr`]'s slot search,
    /// which a typed word does not need. `None` for anything else.
    #[inline]
    pub fn fn_at(&self, addr: u64) -> Option<Gc<ObjFn>> {
        if addr == 0 || !addr.is_multiple_of(8) || !self.slab.may_contain(addr) {
            return None;
        }
        // SAFETY: the slab's range stays mapped and a function value's
        // word is a slot start; a header kind is read either way.
        let header = unsafe { &*(addr as *const GcHeader) };
        // Kind 12 is `Object::Fn` (see `Object::from_header`).
        (header.kind.get() == 12).then(|| unsafe { Gc::from_addr(addr) })
    }

    /// Classify a raw payload / field word as an object or an immediate.
    #[cfg(test)]
    pub(crate) fn member_of(&self, v: Value) -> Member {
        match self.find_object_by_addr(v.bits()) {
            Some(o) => Member::Object(o),
            None => Member::Value(v),
        }
    }

    /// Write back scratch-buffer values into a live `ObjArray`.
    pub fn update_array_elements(&mut self, addr: u64, values: &[i64]) {
        let Some(Object::Array(mut gc)) = self.find_object_by_addr(addr) else {
            return;
        };
        gc.as_mut().overwrite_prefix(values.iter().map(|&v| Value::from(v)));
    }

    /// Write `bytes` back into the array at `addr`, one byte per element
    /// (an FFI `Bytes` buffer after the call). Extra bytes are dropped.
    pub fn update_array_bytes(&mut self, addr: u64, bytes: &[u8]) {
        let Some(Object::Array(mut gc)) = self.find_object_by_addr(addr) else {
            return;
        };
        gc.as_mut().write_bytes_at(0, bytes);
    }

    /// True if `addr` is a live heap object.
    /// True when `v` could be a heap reference: its address lies in the
    /// slab's mapped range. False proves it is not one.
    #[inline]
    pub fn may_be_ref(&self, v: Value) -> bool {
        let addr = v.heap_addr();
        addr != 0 && self.slab.may_contain(addr)
    }

    pub fn contains_addr(&self, addr: u64) -> bool {
        self.find_object_by_addr(addr).is_some()
    }

    #[cfg(test)]
    fn slot_mapped_for_test(&self, addr: u64) -> bool {
        self.slab.contains_slot(addr)
    }
}

impl Drop for Heap {
    fn drop(&mut self) {
        // Drop payloads in place; the slab (and its free lists) goes away
        // next, so slots are not returned and nothing is collected first.
        self.forget_remembered();
        if self.slab.young_chunks_in_use() != 0 {
            note_young_heap(false);
        }
        let mut cur = super::slab::SlotCursor::default();
        while let Some(addr) = self.slab.next_slot(&mut cur) {
            if let Some(object) = unsafe { Object::from_header(addr) } {
                self.counts.alloc_bytes -= object.size();
                self.counts.live_count -= 1;
                unsafe { object.recycle_payload() };
            }
        }

        debug_assert_eq!(0, self.counts.alloc_bytes);
    }
}

impl<'a> IntoIterator for &'a Heap {
    type Item = Object;

    type IntoIter = HeapIter<'a>;

    fn into_iter(self) -> Self::IntoIter {
        self.objects()
    }
}

/// An iterator through all currently allocated objects (slab slots whose
/// header is live).
pub struct HeapIter<'a> {
    slab: &'a super::slab::Slab,
    cur: super::slab::SlotCursor,
}

impl Iterator for HeapIter<'_> {
    type Item = Object;

    fn next(&mut self) -> Option<Self::Item> {
        while let Some(addr) = self.slab.next_slot(&mut self.cur) {
            if let Some(obj) = unsafe { Object::from_header(addr) } {
                return Some(obj);
            }
        }
        None
    }
}

#[cfg(debug_assertions)]
use std::fmt::Debug;

use std::{
    cell::Cell,
    error, fmt, mem,
    ops::{self, BitXor, Deref},
};

pub type RefString = Gc<ObjString>;
pub type RefInstance = Gc<ObjInstance>;
pub type RefEnum = Gc<ObjEnum>;
pub type RefLibrary = Gc<ObjLibrary>;
pub type RefCoroutine = Gc<ObjCoroutine>;

/// Lifecycle of a heap-allocated coroutine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoroState {
    /// Created but never resumed, or suspended at a `yield`.
    Suspended,
    /// Body returned; further `resume` is a no-op (returns default).
    Done,
}

/// An enumeration of all potential errors that occur when working with objects.
#[derive(Debug)]
pub enum Error {
    InvalidCast,
}

impl error::Error for Error {}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidCast => write!(f, "Invalid cast."),
        }
    }
}

pub type RefRoot = Gc<ObjRoot>;
pub type RefWeak = Gc<ObjWeak>;
pub type RefFn = Gc<ObjFn>;
pub type RefStream = Gc<ObjStream>;
pub type RefThread = Gc<ObjThread>;
pub type RefSender = Gc<ObjSender>;
pub type RefReceiver = Gc<ObjReceiver>;
pub type RefThreadMutex = Gc<ObjThreadMutex>;
pub type RefRwLock = Gc<ObjRwLock>;

/// Kind of host-backed IO stream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamKind {
    Stdin,
    Stdout,
    Stderr,
    File,
    Tcp,
    TcpListener,
    /// Datagram socket (`io::net::udp::bind` / `connect`).
    Udp,
    /// Package IO attached in place (`Stream.attach`).
    Attached,
    /// A file or socket stream a `thread::spawn` worker read from a static
    /// of its spawner: the handle stays with the spawner, so using it in
    /// the worker panics (#1684).
    Unsent,
}

#[derive(Clone, Copy)]
pub enum Object {
    String(RefString),
    Instance(RefInstance),
    Enum(RefEnum),
    Library(RefLibrary),
    Array(crate::memory::Gc<ObjArray>),
    Coroutine(RefCoroutine),
    /// Strong GC pin: marks `payload` while this handle is reachable.
    Root(RefRoot),
    /// Non-rooting handle; cleared when the referent is unmarked.
    Weak(RefWeak),
    Fn(RefFn),
    Stream(RefStream),
    Thread(RefThread),
    Sender(RefSender),
    Receiver(RefReceiver),
    Mutex(RefThreadMutex),
    RwLock(RefRwLock),
}

/// `$body` with `$r` bound to `$obj`'s handle, whatever its kind.
macro_rules! each_kind {
    ($obj:expr, $r:ident => $body:expr) => {
        match $obj {
            Object::String($r) => $body,
            Object::Instance($r) => $body,
            Object::Enum($r) => $body,
            Object::Library($r) => $body,
            Object::Array($r) => $body,
            Object::Coroutine($r) => $body,
            Object::Root($r) => $body,
            Object::Weak($r) => $body,
            Object::Fn($r) => $body,
            Object::Stream($r) => $body,
            Object::Thread($r) => $body,
            Object::Sender($r) => $body,
            Object::Receiver($r) => $body,
            Object::Mutex($r) => $body,
            Object::RwLock($r) => $body,
        }
    };
}

/// What [`Object::visit_slots`] calls for each slot that may hold a
/// reference: a raw word with its kind (`common::WORD_*`), or a typed
/// member. Implementations mark their methods `#[inline(always)]`: a walk
/// calls them from every arm, and the mark and nursery paths are hot.
pub(crate) trait SlotVisitor {
    fn word(&mut self, v: &Value, kind: u8);
    fn member(&mut self, m: &Member);
}

/// [`SlotVisitor`] with the slots writable ([`Object::visit_slots_mut`]).
pub(crate) trait SlotVisitorMut {
    fn word(&mut self, v: &mut Value, kind: u8);
    fn member(&mut self, m: &mut Member);
}

/// The one walk of an object's slots, behind [`Object::visit_slots`] (reads)
/// and [`Object::visit_slots_mut`] (rewrites). It says which words of each
/// kind of object may hold a reference and how sure that is:
///
/// - instance slots and enum payload (tuple) words: every word, with the
///   kind its class row or construction site recorded (scalars included);
/// - array words, when [`ObjArray::may_hold_refs`]: a word array's words
///   with its element kind, a lane array's pointer lanes only;
/// - coroutine words: [`common::WORD_POINTER`] where `saved_live_mask`
///   names them, [`common::WORD_UNKNOWN`] where there is no mask;
/// - closure captures: [`common::WORD_UNKNOWN`]; `Root` payloads
///   and closure dicts: members.
///
/// The two expansions differ only in how they borrow the payloads.
macro_rules! walk_slots {
    ($obj:expr, $heap:expr, $f:expr, $get:ident, $slice:ident, $gc_words:ident, $iter:ident $(, $m:tt)?) => {{
        let f = $f;
        match $obj {
            Object::Instance(i) => {
                let inst = i.$get();
                let mut kinds = common::ClassSlotKinds::new($heap.class_field_kinds(inst.type_id), inst.storage.as_slice().len());
                let slots = & $($m)? *inst.storage.$slice();
                for i in 0..slots.len() {
                    let kind = kinds.next_kind(slots);
                    f.word(& $($m)? slots[i], kind);
                }
            }
            Object::Enum(e) => {
                let payload = & $($m)? e.$get().payload;
                let kinds = payload.kinds();
                for (i, v) in payload.$slice().$iter().enumerate() {
                    f.word(v, common::packed_word_kind(kinds, i));
                }
            }
            Object::Array(a) => {
                let a = a.$get();
                if a.may_hold_refs() {
                    let (k, mask, kind) = (a.lanes(), a.tag_mask, a.elem_kind());
                    if let Some(words) = a.$gc_words() {
                        if k > 1 {
                            for i in 0..words.len() {
                                if ObjArray::lane_pointer(words, k, mask, i) {
                                    f.word(& $($m)? words[i], common::WORD_POINTER);
                                }
                            }
                        } else {
                            for v in words.$iter() {
                                f.word(v, kind);
                            }
                        }
                    }
                }
            }
            Object::Coroutine(c) => {
                let coro = c.$get();
                let mask = coro.saved_live_mask;
                for (i, v) in coro.saved_stack.$iter().enumerate() {
                    let masked = mask != 0 && i < 64;
                    if masked && mask & (1u64 << i) == 0 {
                        continue;
                    }
                    f.word(v, if masked { common::WORD_POINTER } else { common::WORD_UNKNOWN });
                }
            }
            Object::Root(r) => {
                if let Some(m) = & $($m)? r.$get().payload {
                    f.member(m);
                }
            }
            Object::Fn(func) => {
                let func = func.$get();
                for v in func.captures.$iter().chain(func.captured_args.$iter()) {
                    f.word(v, common::WORD_UNKNOWN);
                }
                for m in func.dicts.$iter().flatten().flatten() {
                    f.member(m);
                }
            }
            Object::String(_)
            | Object::Library(_)
            | Object::Weak(_)
            | Object::Stream(_)
            | Object::Thread(_)
            | Object::Sender(_)
            | Object::Receiver(_)
            | Object::Mutex(_)
            | Object::RwLock(_) => {}
        }
    }};
}

impl Object {
    /// Every kind starts with the `repr(C)` [`GcHeader`].
    #[inline]
    fn header(&self) -> &GcHeader {
        // SAFETY: every kind shares the `repr(C)` header at the slot start.
        unsafe { &*(self.addr() as *const GcHeader) }
    }

    /// Mark the current object reference and put it in `grey_objects` if its has not been marked.
    #[inline]
    pub fn mark(&self, grey_objects: &mut Vec<Self>) {
        let header = self.header();
        if !header.marked.get() {
            header.marked.set(true);
            grey_objects.push(*self);
        }
    }

    /// Unmark the object.
    #[inline]
    pub fn unmark(&self) {
        self.header().marked.set(false);
    }

    #[must_use]
    #[inline]
    pub fn is_marked(&self) -> bool {
        self.header().marked.get()
    }

    /// Call `f` on each slot of `self` that may hold a reference
    /// ([`walk_slots`]).
    #[inline]
    pub(crate) fn visit_slots(&self, heap: &Heap, f: &mut impl SlotVisitor) {
        walk_slots!(self, heap, f, as_ref, as_slice, gc_words, iter)
    }

    /// `gc-stats`: how many slots [`Self::visit_slots`] walks.
    #[cfg(feature = "gc-stats")]
    pub(crate) fn slot_words(&self, heap: &Heap) -> usize {
        struct Count(usize);
        impl SlotVisitor for Count {
            fn word(&mut self, _: &Value, _: u8) {
                self.0 += 1;
            }

            fn member(&mut self, _: &Member) {
                self.0 += 1;
            }
        }
        let mut count = Count(0);
        self.visit_slots(heap, &mut count);
        count.0
    }

    /// [`Self::visit_slots`] with the slots writable, for a moving
    /// collector. Takes each payload as a write does ([`Gc::payload_mut`]).
    pub(crate) fn visit_slots_mut(&self, heap: &Heap, f: &mut impl SlotVisitorMut) {
        walk_slots!(self, heap, f, payload_mut, as_mut_slice, gc_words_mut, iter_mut, mut)
    }

    /// Mark a stored member, including Result `Err` tagged pointers in `Value`.
    fn mark_member(heap: &Heap, member: &Member, grey_objects: &mut Vec<Self>) {
        match member {
            // A dead object may still name a slot an early free gave back.
            Member::Object(o) if heap.freed_early => heap.mark_value(Value::from(o.addr()), grey_objects),
            Member::Object(o) => o.mark(grey_objects),
            Member::Value(v) => heap.mark_value(*v, grey_objects),
        }
    }

    /// Mark direct heap references held by this object.
    ///
    /// Aggregates that store raw [`Value`]s (arrays, tuples, fn captures,
    /// coroutine stacks) resolve those addresses through `heap`. Stream attach
    /// state is host-side (A4); channel payloads are PortableValue copies,
    /// not this-heap pointers. An array found to hold none is flagged so the
    /// next mark skips it until a write.
    #[inline]
    pub fn mark_references(&self, heap: &Heap, grey_objects: &mut Vec<Self>) {
        struct Marker<'a> {
            heap: &'a Heap,
            grey: &'a mut Vec<Object>,
            any: bool,
            #[cfg(feature = "gc-stress")]
            check: Option<(String, usize)>,
        }
        impl SlotVisitor for Marker<'_> {
            #[inline(always)]
            fn word(&mut self, &v: &Value, kind: u8) {
                #[cfg(feature = "gc-stress")]
                if let Some((what, i)) = &mut self.check {
                    if kind != common::WORD_UNKNOWN {
                        self.heap.verify_kinded_word(what, *i, kind, v);
                    }
                    *i += 1;
                }
                let reference = match kind {
                    common::WORD_SCALAR => false,
                    // `0` or an object: no classification.
                    common::WORD_POINTER => v.heap_addr() != 0,
                    _ => self.heap.may_be_ref(v),
                };
                if reference {
                    self.any = true;
                    self.heap.mark_value(v, self.grey);
                }
            }

            #[inline(always)]
            fn member(&mut self, m: &Member) {
                Object::mark_member(self.heap, m, self.grey);
            }
        }
        let mut marker = Marker {
            heap,
            grey: grey_objects,
            any: false,
            // Coroutine words are not checked.
            #[cfg(feature = "gc-stress")]
            check: (!matches!(self, Self::Coroutine(_))).then(|| (self.slot_owner(), 0)),
        };
        self.visit_slots(heap, &mut marker);
        // Clear flag: scanned clean at this mark, unwritten since.
        if let Self::Array(a) = self
            && !marker.any
            && a.as_ref().may_hold_refs()
        {
            a.payload_mut().may_hold_refs = false;
        }
    }

    /// What a gc-stress check names the slots of `self` by.
    #[cfg(feature = "gc-stress")]
    fn slot_owner(&self) -> String {
        match self {
            Self::Instance(i) => format!("class {}", i.as_ref().type_id),
            Self::Array(a) if a.as_ref().lanes() > 1 => "a lane array's pointer lane".into(),
            Self::Array(_) => "a pointer-kind array".into(),
            _ => "an enum payload / tuple".into(),
        }
    }

    /// Every heap reference `self` holds, with whether the word is precise.
    ///
    /// Precise: `Member::Object` fields, words a class row, construction
    /// site or element kind records as pointers, and coroutine stack words
    /// named by a nonzero `saved_live_mask`. Everything else is a raw
    /// `Value` word that is traced by address lookup and may be an immediate
    /// (unknown-kind words, closure captures, unmasked coroutine words,
    /// `Member::Value` fields): a moving collector cannot rewrite those
    /// (`docs/internals/gc-evacuation.md`). Not on the mark path.
    pub fn for_each_reference(&self, heap: &Heap, visit: &mut dyn FnMut(u64, bool)) {
        struct Tracer<'a, 'v> {
            heap: &'a Heap,
            visit: &'v mut dyn FnMut(u64, bool),
        }
        impl Tracer<'_, '_> {
            #[inline(always)]
            fn resolve(&mut self, v: Value, precise: bool) {
                let addr = v.heap_addr();
                if addr != 0 && self.heap.find_object_by_addr(addr).is_some() {
                    (self.visit)(addr, precise);
                }
            }
        }
        impl SlotVisitor for Tracer<'_, '_> {
            #[inline(always)]
            fn word(&mut self, &v: &Value, kind: u8) {
                if kind != common::WORD_SCALAR {
                    self.resolve(v, kind == common::WORD_POINTER);
                }
            }

            #[inline(always)]
            fn member(&mut self, m: &Member) {
                match m {
                    Member::Object(o) => (self.visit)(o.addr(), true),
                    Member::Value(v) => self.resolve(*v, false),
                }
            }
        }
        self.visit_slots(heap, &mut Tracer { heap, visit });
    }

    /// Header nursery bits ([`GEN_YOUNG`] and friends).
    #[inline]
    pub(crate) fn generation(&self) -> u8 {
        // SAFETY: every kind shares the `repr(C)` header at the slot start.
        unsafe { &*(self.addr() as *const GcHeader) }.generation.get()
    }

    #[inline]
    pub(crate) fn set_generation(&self, bits: u8) {
        unsafe { &*(self.addr() as *const GcHeader) }.generation.set(bits);
    }

    /// Copy-on-write: whether a second place may name this object.
    #[inline]
    pub fn is_shared(&self) -> bool {
        // SAFETY: every kind shares the `repr(C)` header at the slot start.
        unsafe { &*(self.addr() as *const GcHeader) }.shared.get()
    }

    /// This object as a tuple: an enum object of [`common::PRODUCT_TAG`].
    #[inline]
    pub fn as_tuple(self) -> Option<RefEnum> {
        match self {
            Self::Enum(e) if e.as_ref().is_tuple() => Some(e),
            _ => None,
        }
    }

    /// Whether this object's `drop` has been claimed.
    #[inline]
    pub fn is_finalized(&self) -> bool {
        // SAFETY: every kind shares the `repr(C)` header at the slot start.
        unsafe { &*(self.addr() as *const GcHeader) }.finalized.get()
    }

    /// Claim this object's `drop`: `false` when it was claimed already.
    #[inline]
    pub fn claim_finalizer(&self) -> bool {
        // SAFETY: as in `is_finalized`.
        !unsafe { &*(self.addr() as *const GcHeader) }.finalized.replace(true)
    }

    #[inline]
    pub(crate) fn set_shared(&self) {
        // SAFETY: a live object's header; the flag is a `Cell<bool>` at
        // [`COW_SHARED_OFFSET`], which native code's inline share writes the
        // same way.
        unsafe { *((self.addr() + COW_SHARED_OFFSET as u64) as *mut u8) = COW_SHARED_SET };
    }

    /// Copy-on-write: whether a name writes this object in place without a
    /// test while it is pinned, so a share hands out a copy of it instead
    /// of marking it ([`Heap::cow_share`]).
    #[inline]
    pub fn is_pinned(&self) -> bool {
        // SAFETY: every kind shares the `repr(C)` header at the slot start.
        unsafe { &*(self.addr() as *const GcHeader) }.pinned.get()
    }

    /// Set or clear the pin ([`Self::is_pinned`]): a byte at
    /// [`COW_PINNED_OFFSET`], which native code's `CowPin` / `CowUnpin`
    /// write the same way.
    #[inline]
    pub fn set_pinned(&self, on: bool) {
        // SAFETY: a live object's header; the bit is a `Cell<bool>` at
        // [`COW_PINNED_OFFSET`].
        unsafe { *((self.addr() + COW_PINNED_OFFSET as u64) as *mut u8) = if on { COW_PINNED_SET } else { 0 } };
    }

    /// Point every word of this instance's slots or array's elements that
    /// names `from` at `to` instead ([`Heap::cow_copy`]: a pinned child's
    /// copy in a fresh copy of its holder). Writes go through the
    /// payload's barrier.
    fn replace_reference(&self, from: u64, to: Value) {
        match self {
            Self::Instance(gc) => {
                let hits: Vec<usize> = gc.as_ref().storage.as_slice().iter().enumerate().filter(|(_, v)| v.heap_addr() == from).map(|(i, _)| i).collect();
                let mut gc = *gc;
                for i in hits {
                    gc.as_mut().set_slot(i, to);
                }
            }
            Self::Array(gc) => {
                let hits: Vec<usize> = (0..gc.as_ref().word_count()).filter(|&i| gc.as_ref().word_at(i).is_some_and(|v| v.heap_addr() == from)).collect();
                let mut gc = *gc;
                for i in hits {
                    gc.as_mut().set_word_at(i, to);
                }
            }
            _ => {}
        }
    }

    #[must_use]
    #[inline]
    pub fn addr(&self) -> u64 {
        each_kind!(self, r => r.as_ptr() as u64)
    }

    /// The header kind byte. 5 (a tuple, now an enum object of
    /// [`common::PRODUCT_TAG`]), 8 (a boxed scalar) and 11 are free; no kind
    /// takes them, so a stale number never reads as another kind.
    fn kind(self) -> u8 {
        match self {
            Self::String(_) => 1,
            Self::Instance(_) => KIND_INSTANCE,
            Self::Enum(_) => KIND_ENUM,
            Self::Library(_) => 4,
            Self::Array(_) => 6,
            Self::Coroutine(_) => 7,
            Self::Root(_) => 9,
            Self::Weak(_) => 10,
            Self::Fn(_) => 12,
            Self::Stream(_) => 13,
            Self::Thread(_) => 14,
            Self::Sender(_) => 15,
            Self::Receiver(_) => 16,
            Self::Mutex(_) => 17,
            Self::RwLock(_) => 18,
        }
    }

    /// What a collector check names an object by.
    pub(crate) fn kind_name(self) -> &'static str {
        match self {
            Self::String(_) => "a string",
            Self::Instance(_) => "an instance",
            Self::Enum(e) if e.as_ref().is_tuple() => "a tuple",
            Self::Enum(_) => "an enum",
            Self::Library(_) => "a library",
            Self::Array(a) if a.as_ref().elem_kind() == common::WORD_POINTER => "a pointer-kind array",
            Self::Array(_) => "an array",
            Self::Coroutine(_) => "a coroutine",
            Self::Root(_) => "a root",
            Self::Weak(_) => "a weak",
            Self::Fn(_) => "a closure",
            Self::Stream(_) => "a stream",
            Self::Thread(_) => "a thread",
            Self::Sender(_) => "a sender",
            Self::Receiver(_) => "a receiver",
            Self::Mutex(_) => "a mutex",
            Self::RwLock(_) => "a rwlock",
        }
    }

    /// Drop the payload and poison `kind = 0`. Slot memory stays mapped.
    unsafe fn recycle_payload(self) {
        each_kind!(self, r => unsafe { r.recycle() })
    }

    /// Rebuild a typed handle from a live allocation. The address must be a
    /// current [`Heap::alloc`] result (non-moving); kind is the header byte.
    pub(crate) unsafe fn from_header(addr: u64) -> Option<Self> {
        let header = unsafe { &*(addr as *const GcHeader) };
        Some(match header.kind.get() {
            1 => Self::String(unsafe { Gc::from_addr(addr) }),
            KIND_INSTANCE => Self::Instance(unsafe { Gc::from_addr(addr) }),
            3 => Self::Enum(unsafe { Gc::from_addr(addr) }),
            4 => Self::Library(unsafe { Gc::from_addr(addr) }),
            KIND_ARRAY => Self::Array(unsafe { Gc::from_addr(addr) }),
            7 => Self::Coroutine(unsafe { Gc::from_addr(addr) }),
            9 => Self::Root(unsafe { Gc::from_addr(addr) }),
            10 => Self::Weak(unsafe { Gc::from_addr(addr) }),
            12 => Self::Fn(unsafe { Gc::from_addr(addr) }),
            13 => Self::Stream(unsafe { Gc::from_addr(addr) }),
            14 => Self::Thread(unsafe { Gc::from_addr(addr) }),
            15 => Self::Sender(unsafe { Gc::from_addr(addr) }),
            16 => Self::Receiver(unsafe { Gc::from_addr(addr) }),
            17 => Self::Mutex(unsafe { Gc::from_addr(addr) }),
            18 => Self::RwLock(unsafe { Gc::from_addr(addr) }),
            _ => return None,
        })
    }
}

impl GcSized for Object {
    fn size(&self) -> usize {
        each_kind!(self, r => r.size())
    }
}

impl fmt::Display for Object {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::String(s) => write!(f, "{}", s.as_ref()),
            Self::Instance(_) => write!(f, "0x{:08x}", self.addr()),
            Self::Enum(e) if e.as_ref().is_tuple() => write!(f, "{}", e.as_ref()),
            Self::Enum(_) => write!(f, "0x{:08x}", self.addr()),
            Self::Library(_) => write!(f, "0x{:08x}", self.addr()),
            Self::Array(a) => write!(f, "{}", a.as_ref()),
            Self::Coroutine(c) => write!(f, "{}", c.as_ref()),
            Self::Root(_) => write!(f, "<root 0x{:08x}>", self.addr()),
            Self::Weak(_) => write!(f, "<weak 0x{:08x}>", self.addr()),
            Self::Fn(_) => write!(f, "<fn 0x{:08x}>", self.addr()),
            Self::Stream(_) => write!(f, "<stream 0x{:08x}>", self.addr()),
            Self::Thread(_) => write!(f, "<thread 0x{:08x}>", self.addr()),
            Self::Sender(_) => write!(f, "<sender 0x{:08x}>", self.addr()),
            Self::Receiver(_) => write!(f, "<receiver 0x{:08x}>", self.addr()),
            Self::Mutex(_) => write!(f, "<mutex 0x{:08x}>", self.addr()),
            Self::RwLock(_) => write!(f, "<rwlock 0x{:08x}>", self.addr()),
        }
    }
}

impl Object {
}

/// How a moving collector may treat a root (`docs/internals/gc-evacuation.md`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RootKind {
    /// A word the runtime knows is a heap reference; could be rewritten.
    Precise,
    /// A word that may be an immediate (conservative frame, static); the
    /// object it hits must not move.
    Ambiguous,
    /// Keyed by address outside the heap (FFI library handles); never moves.
    Pinned,
}

#[derive(Clone, Copy)]
pub enum Member {
    Value(Value),
    Object(Object),
}

/// Max typed field count stored inside [`ObjInstance`] without a Rust `Vec`:
/// the inline width of the store every product object shares
/// ([`EnumPayload`]).
pub const INSTANCE_INLINE_FIELDS: usize = ENUM_INLINE_ARITY;

/// `type_id` first, so the slots sit at the offset of an [`ObjEnum`]'s
/// payload: one inline read serves both ([`product_slots`]).
#[repr(C)]
pub struct ObjInstance {
    /// Compile-time class identity (`0` = none).
    pub type_id: u32,
    /// The fields' words; their kinds come from the class row, so the
    /// store's own packed kinds stay `0`.
    storage: EnumPayload,
}

impl Default for ObjInstance {
    fn default() -> Self {
        Self {
            storage: EnumPayload::empty(),
            type_id: 0,
        }
    }
}

impl ObjInstance {
    /// `nfields` dense slots, zeroed. Counts ≤ [`INSTANCE_INLINE_FIELDS`]
    /// stay in the header; larger spill.
    #[must_use]
    pub fn with_type_id_and_fields(type_id: u32, nfields: usize) -> Self {
        Self {
            storage: EnumPayload::zeros(nfields),
            type_id,
        }
    }

    /// Named read of a typed slot by its known name.
    pub fn get(&self, key: RefString) -> Option<Value> {
        let slot = common::range_heap_field_slot(self.type_id, &key.as_ref().data)?;
        self.slot(slot)
    }

    pub fn slot(&self, index: usize) -> Option<Value> {
        self.storage.as_slice().get(index).copied()
    }

    pub fn set_slot(&mut self, index: usize, value: Value) {
        if let Some(slot) = self.storage.as_mut_slice().get_mut(index) {
            *slot = value;
        }
    }

    pub fn slot_len(&self) -> usize {
        self.storage.as_slice().len()
    }

    pub fn slots(&self) -> &[Value] {
        self.storage.as_slice()
    }

    /// True when typed fields live in the object header (no spill `Vec`).
    #[inline]
    pub fn slots_are_inline(&self) -> bool {
        self.storage.is_inline()
    }

    #[must_use]
    pub fn with_slots(type_id: u32, slots: Vec<Value>) -> Self {
        Self {
            storage: EnumPayload::from_vec(slots),
            type_id,
        }
    }
}

impl GcSized for ObjInstance {
    const NURSERY: bool = true;

    fn finalizer_type_id(&self) -> Option<u32> {
        Some(self.type_id)
    }

    fn size(&self) -> usize {
        // Inline slots live in the object; only a spill `Vec` is extra.
        std::mem::size_of::<Self>()
            + self.storage.spill_capacity() * std::mem::size_of::<Value>()
    }
}

/// Max payload arity stored inside [`ObjEnum`] without a Rust `Vec`.
///
/// Payload words are raw [`Value`]s (the GC resolves them through the slab),
/// so four fit where two tagged [`Member`]s used to; larger variants spill.
pub const ENUM_INLINE_ARITY: usize = 4;

/// The fixed-slot store of every product object: a variant's payload, a
/// tuple's items ([`ObjEnum`]) and an instance's fields ([`ObjInstance`]).
/// Inline words up to [`ENUM_INLINE_ARITY`], else a `Vec`.
#[repr(C)]
pub struct EnumPayload {
    inner: EnumPayloadInner,
}

/// `repr(u8)`: fixed layout, so native code fills an inline payload in
/// place ([`InlineLayout`]).
#[repr(u8)]
enum EnumPayloadInner {
    /// `kinds`: packed word kinds (`common::packed_word_kind`), in padding.
    Inline {
        len: u8,
        kinds: u8,
        slots: [Value; ENUM_INLINE_ARITY],
    },
    Spill {
        words: Vec<Value>,
        kinds: u8,
    },
}

impl EnumPayload {
    /// Empty payload (arity-0). Immortal unit enums use this, not a `Vec`.
    #[inline]
    pub fn empty() -> Self {
        Self {
            inner: EnumPayloadInner::Inline {
                len: 0,
                kinds: 0,
                slots: [Value::default(); ENUM_INLINE_ARITY],
            },
        }
    }

    /// `n` zero words: an instance's fields before their stores.
    pub fn zeros(n: usize) -> Self {
        if n <= ENUM_INLINE_ARITY {
            Self {
                inner: EnumPayloadInner::Inline {
                    len: n as u8,
                    kinds: 0,
                    slots: [Value::from(0i64); ENUM_INLINE_ARITY],
                },
            }
        } else {
            Self::from_vec(vec![Value::from(0i64); n])
        }
    }

    /// Unary payload without a heap `Vec` (Option/Result).
    #[inline]
    pub fn one(v: Value) -> Self {
        let mut slots = [Value::default(); ENUM_INLINE_ARITY];
        slots[0] = v;
        Self {
            inner: EnumPayloadInner::Inline {
                len: 1,
                kinds: 0,
                slots,
            },
        }
    }

    /// Arity-2 payload without a heap `Vec` (`Tree::Node`).
    #[inline]
    pub fn two(a: Value, b: Value) -> Self {
        let mut slots = [Value::default(); ENUM_INLINE_ARITY];
        slots[0] = a;
        slots[1] = b;
        Self {
            inner: EnumPayloadInner::Inline {
                len: 2,
                kinds: 0,
                slots,
            },
        }
    }

    /// Build from declaration-order words; spills past the inline cap.
    pub fn from_slice(words: &[Value]) -> Self {
        if words.len() <= ENUM_INLINE_ARITY {
            let mut slots = [Value::default(); ENUM_INLINE_ARITY];
            slots[..words.len()].copy_from_slice(words);
            Self {
                inner: EnumPayloadInner::Inline {
                    len: words.len() as u8,
                    kinds: 0,
                    slots,
                },
            }
        } else {
            Self {
                inner: EnumPayloadInner::Spill {
                    words: words.to_vec(),
                    kinds: 0,
                },
            }
        }
    }

    /// Build from owned words; spills only when `words.len()` exceeds the cap.
    pub fn from_vec(words: Vec<Value>) -> Self {
        if words.len() <= ENUM_INLINE_ARITY {
            Self::from_slice(&words)
        } else {
            Self {
                inner: EnumPayloadInner::Spill { words, kinds: 0 },
            }
        }
    }

    #[inline]
    fn as_slice(&self) -> &[Value] {
        match &self.inner {
            EnumPayloadInner::Inline { len, slots, .. } => &slots[..*len as usize],
            EnumPayloadInner::Spill { words, .. } => words.as_slice(),
        }
    }

    /// Payload words for in-place reference rewrites (evacuation).
    fn as_mut_slice(&mut self) -> &mut [Value] {
        match &mut self.inner {
            EnumPayloadInner::Inline { len, slots, .. } => &mut slots[..*len as usize],
            EnumPayloadInner::Spill { words, .. } => words.as_mut_slice(),
        }
    }

    /// Packed word kinds of the first words (`common::packed_word_kind`).
    #[inline]
    pub fn kinds(&self) -> u8 {
        match &self.inner {
            EnumPayloadInner::Inline { kinds, .. } | EnumPayloadInner::Spill { kinds, .. } => {
                *kinds
            }
        }
    }

    /// Record the construction site's word kinds.
    #[inline]
    pub fn with_kinds(mut self, k: u8) -> Self {
        match &mut self.inner {
            EnumPayloadInner::Inline { kinds, .. } | EnumPayloadInner::Spill { kinds, .. } => {
                *kinds = k;
            }
        }
        self
    }

    /// True when payload lives in the object header (no spill `Vec`).
    #[inline]
    pub fn is_inline(&self) -> bool {
        matches!(self.inner, EnumPayloadInner::Inline { .. })
    }

    fn spill_capacity(&self) -> usize {
        match &self.inner {
            EnumPayloadInner::Inline { .. } => 0,
            EnumPayloadInner::Spill { words, .. } => words.capacity(),
        }
    }
}

impl From<Vec<Value>> for EnumPayload {
    fn from(words: Vec<Value>) -> Self {
        Self::from_vec(words)
    }
}

impl ops::Deref for EnumPayload {
    type Target = [Value];

    #[inline]
    fn deref(&self) -> &[Value] {
        self.as_slice()
    }
}

impl<'a> IntoIterator for &'a EnumPayload {
    type Item = &'a Value;
    type IntoIter = std::slice::Iter<'a, Value>;

    fn into_iter(self) -> Self::IntoIter {
        self.as_slice().iter()
    }
}

/// Heap-allocated enum variant (`tag` + inline-or-spill word payload).
#[repr(C)]
pub struct ObjEnum {
    pub tag: u32,
    /// Finalizer key for an enum with `fn drop()` (`TagEnumType`); `0` = none.
    /// Unit variants are shared immortals and are never tagged.
    pub type_id: u32,
    pub payload: EnumPayload,
}

impl ObjEnum {
    pub fn new(tag: u32, payload: EnumPayload) -> Self {
        Self {
            tag,
            payload,
            type_id: 0,
        }
    }

    /// A tuple of `elements`, `kinds` their packed word kinds: a product,
    /// an enum object of [`common::PRODUCT_TAG`]. Immutable; up to
    /// [`ENUM_INLINE_ARITY`] words live in the object, wider ones spill.
    #[inline]
    pub fn tuple(elements: &[Value], kinds: u8) -> Self {
        Self::new(common::PRODUCT_TAG, EnumPayload::from_slice(elements).with_kinds(kinds))
    }

    /// Whether this object is a tuple ([`Self::tuple`]), not a variant.
    #[inline]
    pub fn is_tuple(&self) -> bool {
        self.tag == common::PRODUCT_TAG
    }

    /// A tuple's elements (a variant's payload words).
    #[inline]
    pub fn elements(&self) -> &[Value] {
        &self.payload
    }
}

impl GcSized for ObjEnum {
    const NURSERY: bool = true;

    fn finalizer_type_id(&self) -> Option<u32> {
        Some(self.type_id)
    }

    fn size(&self) -> usize {
        std::mem::size_of::<Self>() + self.payload.spill_capacity() * std::mem::size_of::<Value>()
    }
}

/// The content of a heap-allocated string object.
///
/// The content hash is computed on first use ([`Self::hash_code`]): only
/// intern-table keys and `Hash` on `string` need it, so a string built at
/// runtime (concat, format, `from_bytes`) never pays for a full-length hash.
pub struct ObjString {
    pub data: super::StrData,
    /// `HASH_SET | hash` once computed, 0 before. Atomic (relaxed) because
    /// steal-epoch workers may hash the same shared string concurrently.
    hash: AtomicU64,
}

const HASH_SET: u64 = 1 << 32;

impl ObjString {
    /// A string whose hash is computed lazily.
    #[must_use]
    pub fn new(data: String) -> Self {
        Self::from_data(data.into())
    }

    pub(crate) fn from_data(data: super::StrData) -> Self {
        Self {
            data,
            hash: AtomicU64::new(0),
        }
    }

    /// `self + tail` (see [`super::StrData::concat`]).
    #[must_use]
    pub fn concat(&self, tail: &str) -> Self {
        Self::from_data(self.data.concat(tail))
    }

    fn with_hash(data: String, hash: u32) -> Self {
        Self {
            data: data.into(),
            hash: AtomicU64::new(HASH_SET | u64::from(hash)),
        }
    }

    /// Content hash ([`Self::hash`] of `data`), cached after the first call.
    #[inline]
    /// Whether [`Self::hash_code`] ran (interning computes it first).
    fn hash_set(&self) -> bool {
        self.hash.load(Ordering::Relaxed) & HASH_SET != 0
    }

    pub fn hash_code(&self) -> u32 {
        let cached = self.hash.load(Ordering::Relaxed);
        if cached & HASH_SET != 0 {
            return cached as u32;
        }
        let h = Self::hash(&self.data);
        self.hash.store(HASH_SET | u64::from(h), Ordering::Relaxed);
        h
    }

    #[must_use]
    pub fn hash(s: &str) -> u32 {
        let mut hash = 2_166_136_261;
        for b in s.bytes() {
            hash = hash.bitxor(u32::from(b));
            hash = hash.wrapping_mul(16_777_619);
        }
        hash
    }
}

impl GcSized for ObjString {
    const HOLDS_REFS: bool = false;
    const REGION: bool = true;

    fn size(&self) -> usize {
        mem::size_of::<Self>() + self.data.accounted_bytes()
    }
}

impl From<&str> for ObjString {
    fn from(value: &str) -> Self {
        Self::from_data(value.into())
    }
}

/// Element storage of an [`ObjArray`].
///
/// `Bytes` holds one byte per element and is lossless: it only ever holds
/// words whose raw value is `0..=255` (a `byte`, a `bool`, or a small int),
/// and a read widens the byte back to that same word. A store that does not
/// fit (a larger int, a boxed value from a shared generic body, any heap
/// reference) unpacks the array to `Words` first, so the packing is a pure
/// space optimisation and never depends on the static element type.
enum ArrayData {
    /// One word per element, and the card table of a large old array
    /// ([`CardTable`]).
    Words(Vec<Value>, Option<Box<CardTable>>),
    Bytes(Vec<u8>),
    /// `k` words per element, element `i` at `words[i * k..][..k]`: a
    /// `Vec` of a two-word type (`Option<int>`, `(int, float)`) keeps
    /// both words in place (`MakeArrayK` stamps it from the element
    /// layout). One-word reads and writes refuse it: only the lane API
    /// below and code that knows the stride touch it.
    /// `k` is [`ObjArray::stride`]: kept out of the variant so that
    /// [`Self::Words`]'s card table fits in the same size.
    Lanes { words: Vec<Value> },
}

/// Elements per card of a [`CardTable`]: `1 << CARD_SHIFT`. Eight: a sweep
/// of {8, 16, 64} found 8 cheapest on map updates (most cards dirty) and 64
/// on a sparse store loop (#2687).
pub const CARD_SHIFT: u32 = 3;
/// Elements a remembered word array has when a minor gives it a
/// [`CardTable`]; a smaller one is scanned whole.
pub const CARD_MIN_WORDS: usize = 1024;

/// Which parts of a large old word array were written since the last minor
/// collection (#2687): one byte per card of `1 << CARD_SHIFT` elements,
/// non-zero when a write may have stored a reference there, so a minor
/// that finds the array remembered scans only those elements. `all` covers
/// a write that names no index (raw word access). A minor gives a
/// remembered array its table ([`ObjArray::attach_cards`]) and clears
/// what it scanned. A minor's safepoint drops every native view, so no
/// plain view taken before the table exists outlives it, and every store
/// view taken after it is [`VIEW_TRACKED`]: native code marks the card of
/// each element it stores in place.
pub(crate) struct CardTable {
    pub(crate) all: bool,
    pub(crate) cards: Vec<u8>,
}

impl CardTable {
    fn new(capacity: usize) -> Self {
        Self { all: false, cards: vec![0; Self::cards_for(capacity)] }
    }

    /// Cards that cover `n` elements.
    #[inline]
    pub(crate) fn cards_for(n: usize) -> usize {
        n.div_ceil(1 << CARD_SHIFT)
    }

    #[inline(always)]
    fn mark(&mut self, i: usize) {
        match self.cards.get_mut(i >> CARD_SHIFT) {
            Some(c) => *c = 1,
            None => self.grow_mark(i, i + 1),
        }
    }

    /// Mark the cards of elements `lo..hi`.
    fn mark_range(&mut self, lo: usize, hi: usize) {
        if lo >= hi {
            return;
        }
        let (a, b) = (lo >> CARD_SHIFT, (hi - 1) >> CARD_SHIFT);
        if b >= self.cards.len() {
            self.grow_mark(lo, hi);
            return;
        }
        self.cards[a..=b].fill(1);
    }

    /// The array grew past the cards: cover twice as many, then mark.
    #[cold]
    #[inline(never)]
    fn grow_mark(&mut self, lo: usize, hi: usize) {
        let need = Self::cards_for(hi).max(2 * self.cards.len());
        self.cards.resize(need, 0);
        self.mark_range(lo, hi);
    }

    /// Bytes the table takes, for heap accounting.
    fn bytes(&self) -> usize {
        mem::size_of::<Self>() + self.cards.capacity()
    }
}

/// `coil_native_view`'s states (`0`: no view): `VIEW_PLAIN` loads and stores
/// in place; `VIEW_TRACKED` does too, and each store also marks its
/// element's card in the array's table, whose bytes the view carries.
pub const VIEW_PLAIN: u64 = 1;
pub const VIEW_TRACKED: u64 = 2;

/// The word stored as one byte, when it fits.
#[inline(always)]
fn byte_of(v: Value) -> Option<u8> {
    let raw = v.bits();
    if raw <= u8::MAX as u64 { Some(raw as u8) } else { None }
}

#[inline(always)]
fn word_of(b: u8) -> Value {
    Value::from(i64::from(b))
}

/// Element iterator of an [`ObjArray`] (bytes widen to words).
pub enum ArrayIter<'a> {
    Words(std::iter::Copied<std::slice::Iter<'a, Value>>),
    Bytes(std::slice::Iter<'a, u8>),
}

impl Iterator for ArrayIter<'_> {
    type Item = Value;

    #[inline]
    fn next(&mut self) -> Option<Value> {
        match self {
            Self::Words(it) => it.next(),
            Self::Bytes(it) => it.next().map(|b| word_of(*b)),
        }
    }

    #[inline]
    fn size_hint(&self) -> (usize, Option<usize>) {
        match self {
            Self::Words(it) => it.size_hint(),
            Self::Bytes(it) => it.size_hint(),
        }
    }
}

impl ExactSizeIterator for ArrayIter<'_> {}

/// A `Vec` / array object.
///
/// Elements are words, or packed bytes (see [`ArrayData`]) once the array is
/// stamped [`common::WORD_BYTE`] (`Vec<byte>` / `Vec<bool>`) or built with
/// [`Self::from_bytes`]. Everything outside this type goes through the
/// width-aware API below; only [`Self::words`] / [`Self::bytes`] expose the
/// storage, for fast paths that handle both.
///
/// `may_hold_refs` is false only when the last mark scanned every element,
/// found none that could be a heap reference, and nothing was written since.
/// Marking skips such an array (a live `Vec<int>` of millions of words is
/// scanned once, then costs nothing per collection). Every word write goes
/// through [`Self::push`] / [`Self::set`] / [`Self::store_indexed`] /
/// [`Self::words_mut`], which set the flag with one byte store — no
/// compare on the hot path. Not type-based, so boxed values from a generic
/// shared body are covered too. Packed bytes never hold references, so the
/// flag stays false for them.
pub struct ObjArray {
    data: ArrayData,
    may_hold_refs: bool,
    /// Element word kind (`common::WORD_*`) the array was made with
    /// (`MakeArrayK`, or a copy of such an array). Only `WORD_POINTER` is
    /// ever kept: each element is `0` or an object address, so marking
    /// treats them as precise references. `WORD_BYTE` packs the storage
    /// instead of being kept.
    elem_kind: u8,
    /// Words per element: `k` of a lane array, `1` otherwise.
    stride: u8,
    /// A lane array's pointer tags: element lane 1 is a tag, and lane 0
    /// is a heap reference exactly when bit `tag` is set (`0` for a
    /// product of scalars). Unused by other storage.
    tag_mask: u64,
    /// Storage bytes the heap has counted for this array (see
    /// [`Self::take_resize`]). [`GcSized::size`] reports this, not the live
    /// storage, so a grow or a pack / unpack the heap was not told about
    /// can never make the heap's byte count underflow at sweep.
    accounted: usize,
}

// The card table fits where the slot had room (#2687): a word array's
// table sits in its variant, and a lane array's stride left its variant.
#[cfg(target_pointer_width = "64")]
const _: () = assert!(mem::size_of::<GcData<ObjArray>>() == 64);

impl ObjArray {
    /// Conservative: any element may be a reference.
    pub fn new(elements: Vec<Value>) -> Self {
        let may_hold_refs = !elements.is_empty();
        Self {
            accounted: elements.capacity() * mem::size_of::<Value>(),
            data: ArrayData::Words(elements, None),
            may_hold_refs,
            elem_kind: common::WORD_UNKNOWN,
            stride: 1,
            tag_mask: 0,
        }
    }

    /// Classify `elements` against `heap` up front.
    pub fn from_values(elements: Vec<Value>, heap: &Heap) -> Self {
        let may_hold_refs = elements.iter().any(|v| heap.may_be_ref(*v));
        Self {
            accounted: elements.capacity() * mem::size_of::<Value>(),
            data: ArrayData::Words(elements, None),
            may_hold_refs,
            elem_kind: common::WORD_UNKNOWN,
            stride: 1,
            tag_mask: 0,
        }
    }

    pub fn with_capacity(n: usize) -> Self {
        Self {
            accounted: n * mem::size_of::<Value>(),
            data: ArrayData::Words(Vec::with_capacity(n), None),
            may_hold_refs: false,
            elem_kind: common::WORD_UNKNOWN,
            stride: 1,
            tag_mask: 0,
        }
    }

    /// A packed byte array (one byte per element).
    pub fn from_bytes(bytes: Vec<u8>) -> Self {
        Self {
            accounted: bytes.capacity(),
            data: ArrayData::Bytes(bytes),
            may_hold_refs: false,
            elem_kind: common::WORD_UNKNOWN,
            stride: 1,
            tag_mask: 0,
        }
    }

    /// An empty array of `k`-word elements with room for `n`, whose
    /// lane 0 is a reference under the tags in `tag_mask`.
    pub fn with_lanes(k: usize, n: usize, tag_mask: u64) -> Self {
        assert!(k >= 2, "a lane array has two or more words per element");
        Self {
            accounted: n * k * mem::size_of::<Value>(),
            data: ArrayData::Lanes { words: Vec::with_capacity(n * k) },
            may_hold_refs: tag_mask != 0,
            elem_kind: common::WORD_UNKNOWN,
            stride: Self::stride_of(k),
            tag_mask,
        }
    }

    /// A lane array of the elements laid out in `words` (`k` each).
    pub fn from_lanes(words: Vec<Value>, k: usize, tag_mask: u64) -> Self {
        assert!(k >= 2 && words.len().is_multiple_of(k), "lane words are whole elements");
        Self {
            accounted: words.capacity() * mem::size_of::<Value>(),
            may_hold_refs: tag_mask != 0 && !words.is_empty(),
            data: ArrayData::Lanes { words },
            elem_kind: common::WORD_UNKNOWN,
            stride: Self::stride_of(k),
            tag_mask,
        }
    }

    /// A lane array's `k` as [`Self::stride`].
    fn stride_of(k: usize) -> u8 {
        u8::try_from(k).expect("a lane array has at most 255 words per element")
    }

    /// Turn a lane array into one word per element in place: `boxes` holds
    /// each element's object, in order. The array stays the same object;
    /// its element kind is pointer, as a boxed two-word element's
    /// (`array_elem_stamp`).
    pub fn lanes_into_words(&mut self, boxes: Vec<Value>) {
        debug_assert_eq!(boxes.len(), self.len(), "a box per element");
        self.may_hold_refs = !boxes.is_empty();
        self.data = ArrayData::Words(boxes, None);
        self.stride = 1;
        self.elem_kind = common::WORD_POINTER;
        self.tag_mask = 0;
    }

    /// Words per element: `1` but for a lane array.
    #[inline]
    pub fn lanes(&self) -> usize {
        match &self.data {
            ArrayData::Lanes { .. } => usize::from(self.stride),
            _ => 1,
        }
    }

    /// A lane array's pointer tags ([`Self::with_lanes`]).
    #[inline]
    pub fn tag_mask(&self) -> u64 {
        self.tag_mask
    }

    /// A lane array's words, `k` per element.
    #[inline]
    pub fn lane_words(&self) -> Option<&[Value]> {
        match &self.data {
            ArrayData::Lanes { words, .. } => Some(words),
            _ => None,
        }
    }

    /// Element `i`'s words in a lane array.
    #[inline]
    pub fn get_lanes(&self, i: usize) -> Option<&[Value]> {
        let k = &usize::from(self.stride);
        match &self.data {
            ArrayData::Lanes { words } => words.get(i * k..(i + 1) * k),
            _ => None,
        }
    }

    /// Overwrite element `i` of a lane array (`false` out of range).
    #[inline]
    pub fn set_lanes(&mut self, i: usize, lanes: &[Value]) -> bool {
        let k = &usize::from(self.stride);
        let mask = self.tag_mask;
        let ArrayData::Lanes { words } = &mut self.data else {
            panic!("set_lanes on an array of one-word elements");
        };
        let Some(slot) = words.get_mut(i * *k..(i + 1) * *k) else {
            return false;
        };
        slot.copy_from_slice(lanes);
        if mask != 0 {
            self.note();
        }
        true
    }

    /// Append one element to a lane array.
    #[inline]
    pub fn push_lanes(&mut self, lanes: &[Value]) {
        let k = &usize::from(self.stride);
        let mask = self.tag_mask;
        let ArrayData::Lanes { words } = &mut self.data else {
            panic!("push_lanes on an array of one-word elements");
        };
        debug_assert_eq!(lanes.len(), *k);
        words.extend_from_slice(lanes);
        if mask != 0 {
            self.note();
        }
    }

    /// Remove the last element of a lane array into `out`.
    #[inline]
    pub fn pop_lanes(&mut self, out: &mut [Value]) -> bool {
        let k = &usize::from(self.stride);
        let ArrayData::Lanes { words } = &mut self.data else {
            panic!("pop_lanes on an array of one-word elements");
        };
        if words.len() < *k {
            return false;
        }
        let at = words.len() - *k;
        out.copy_from_slice(&words[at..]);
        words.truncate(at);
        true
    }

    /// Insert one element at `i` of a lane array (caller checks `i <= len`).
    pub fn insert_lanes(&mut self, i: usize, lanes: &[Value]) {
        let k = &usize::from(self.stride);
        let mask = self.tag_mask;
        let ArrayData::Lanes { words } = &mut self.data else {
            panic!("insert_lanes on an array of one-word elements");
        };
        let at = i * *k;
        words.splice(at..at, lanes.iter().copied());
        if mask != 0 {
            self.note();
        }
    }

    /// Remove element `i` of a lane array into `out` (caller checks `i < len`).
    pub fn remove_lanes(&mut self, i: usize, out: &mut [Value]) {
        let k = &usize::from(self.stride);
        let ArrayData::Lanes { words } = &mut self.data else {
            panic!("remove_lanes on an array of one-word elements");
        };
        let at = i * *k;
        out.copy_from_slice(&words[at..at + *k]);
        words.drain(at..at + *k);
    }

    /// Each stored word with the kind the collector reads it as: an
    /// array's element kind, or a lane array's per lane (lane 0 a pointer
    /// or a scalar by the element's tag, the rest scalars).
    pub(crate) fn kinded_words(&self) -> impl Iterator<Item = (Value, u8)> + '_ {
        let k = &usize::from(self.stride);
        let (words, k, kind): (&[Value], usize, u8) = match &self.data {
            ArrayData::Words(w, ..) => (w, 1, self.elem_kind),
            ArrayData::Bytes(_) => (&[], 1, common::WORD_SCALAR),
            ArrayData::Lanes { words } => (words, *k, common::WORD_SCALAR),
        };
        let mask = self.tag_mask;
        words.iter().enumerate().map(move |(i, &v)| {
            if k == 1 {
                return (v, kind);
            }
            (v, if Self::lane_pointer(words, k, mask, i) { common::WORD_POINTER } else { common::WORD_SCALAR })
        })
    }

    /// Whether word `i` of lane words `words` (`k` per element) is a
    /// reference: lane 0 of an element whose tag (lane 1) is in `mask`.
    #[inline]
    fn lane_pointer(words: &[Value], k: usize, mask: u64, i: usize) -> bool {
        let lane = i % k;
        lane == 0 && words.get(i + 1).is_some_and(|t| t.bits() < 64 && mask >> t.bits() & 1 == 1)
    }

    /// Element word kind (`common::WORD_*`).
    #[inline]
    pub fn elem_kind(&self) -> u8 {
        self.elem_kind
    }

    /// Stamp the static element kind. A pointer kind is kept; a byte kind
    /// packs the elements when they all fit. Other scalar stamps are dropped:
    /// they would be unsound for generic bodies that box values.
    #[inline]
    pub fn set_elem_kind(&mut self, kind: u8) {
        if kind == common::WORD_POINTER {
            self.elem_kind = kind;
        } else if kind == common::WORD_BYTE {
            self.pack();
        }
    }

    /// True when elements are stored one byte each.
    #[inline]
    pub fn is_packed(&self) -> bool {
        matches!(self.data, ArrayData::Bytes(_))
    }

    /// Switch to byte storage when every element fits; otherwise no-op.
    pub fn pack(&mut self) {
        let ArrayData::Words(words, ..) = &self.data else {
            return;
        };
        let mut bytes = Vec::with_capacity(words.capacity());
        for v in words {
            match byte_of(*v) {
                Some(b) => bytes.push(b),
                None => return,
            }
        }
        self.data = ArrayData::Bytes(bytes);
        self.may_hold_refs = false;
    }

    /// Switch to word storage (a store that does not fit in a byte).
    #[cold]
    #[inline(never)]
    fn unpack(&mut self) {
        if let ArrayData::Bytes(bytes) = &self.data {
            let mut words = Vec::with_capacity(bytes.capacity());
            words.extend(bytes.iter().map(|b| word_of(*b)));
            self.data = ArrayData::Words(words, None);
        }
    }

    /// Word storage, unpacking first when needed.
    #[inline]
    fn words_vec(&mut self) -> &mut Vec<Value> {
        if let ArrayData::Bytes(_) = self.data {
            self.unpack();
        }
        match &mut self.data {
            ArrayData::Words(w, ..) => w,
            ArrayData::Bytes(_) => unreachable!(),
            ArrayData::Lanes { .. } => lane_misuse(),
        }
    }

    /// The elements as words, when not packed.
    #[inline]
    pub fn words(&self) -> Option<&[Value]> {
        match &self.data {
            ArrayData::Words(w, ..) => Some(w),
            ArrayData::Bytes(_) | ArrayData::Lanes { .. } => None,
        }
    }

    /// The elements as bytes, when packed.
    #[inline]
    pub fn bytes(&self) -> Option<&[u8]> {
        match &self.data {
            ArrayData::Words(..) | ArrayData::Lanes { .. } => None,
            ArrayData::Bytes(b) => Some(b),
        }
    }

    /// Packed bytes for in-place byte writes (no unpack, no new references).
    #[inline]
    pub fn bytes_mut(&mut self) -> Option<&mut Vec<u8>> {
        match &mut self.data {
            ArrayData::Words(..) | ArrayData::Lanes { .. } => None,
            ArrayData::Bytes(b) => Some(b),
        }
    }

    /// Iterate the elements as words.
    #[inline]
    pub fn iter(&self) -> ArrayIter<'_> {
        match &self.data {
            ArrayData::Words(w, ..) => ArrayIter::Words(w.iter().copied()),
            ArrayData::Bytes(b) => ArrayIter::Bytes(b.iter()),
            ArrayData::Lanes { .. } => lane_misuse(),
        }
    }

    /// A fresh array with a copy of these elements in the same storage and
    /// with the same element kind (the same words, so a pointer kind still
    /// holds).
    pub fn copy(&self) -> Self {
        let k = &usize::from(self.stride);
        let mut copy = match &self.data {
            ArrayData::Words(w, ..) => Self::new(w.clone()),
            ArrayData::Bytes(b) => Self::from_bytes(b.clone()),
            ArrayData::Lanes { words } => Self::from_lanes(words.clone(), *k, self.tag_mask),
        };
        copy.elem_kind = self.elem_kind;
        copy
    }

    /// Copy of the elements as words.
    pub fn to_vec(&self) -> Vec<Value> {
        match &self.data {
            ArrayData::Words(w, ..) => w.clone(),
            ArrayData::Bytes(b) => b.iter().map(|b| word_of(*b)).collect(),
            ArrayData::Lanes { .. } => lane_misuse(),
        }
    }

    /// Copy of elements `start..end` as words (caller checks bounds).
    pub fn slice_to_vec(&self, start: usize, end: usize) -> Vec<Value> {
        match &self.data {
            ArrayData::Words(w, ..) => w[start..end].to_vec(),
            ArrayData::Bytes(b) => b[start..end].iter().map(|b| word_of(*b)).collect(),
            ArrayData::Lanes { .. } => lane_misuse(),
        }
    }

    /// Element `i` as a word.
    #[inline]
    pub fn get(&self, i: usize) -> Option<Value> {
        match &self.data {
            ArrayData::Words(w, ..) => w.get(i).copied(),
            ArrayData::Bytes(b) => b.get(i).map(|b| word_of(*b)),
            ArrayData::Lanes { .. } => lane_misuse(),
        }
    }

    /// `elements[index]` with the runtime's bounds rules: `unchecked` is a proven
    /// index; otherwise out of range returns `None`.
    #[inline(always)]
    pub fn read_indexed(&self, index: i64, unchecked: bool) -> Option<Value> {
        match &self.data {
            ArrayData::Words(w, ..) => read_indexed_in(w, index, unchecked),
            ArrayData::Bytes(b) => read_indexed_in(b, index, unchecked).map(word_of),
            ArrayData::Lanes { .. } => lane_misuse(),
        }
    }

    #[inline]
    pub fn len(&self) -> usize {
        match &self.data {
            ArrayData::Words(w, ..) => w.len(),
            ArrayData::Bytes(b) => b.len(),
            ArrayData::Lanes { words } => words.len() / usize::from(self.stride),
        }
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Element capacity.
    #[inline]
    pub fn capacity(&self) -> usize {
        match &self.data {
            ArrayData::Words(w, ..) => w.capacity(),
            ArrayData::Bytes(b) => b.capacity(),
            ArrayData::Lanes { words } => words.capacity() / usize::from(self.stride),
        }
    }

    /// Bytes of element storage (capacity × element width), for heap
    /// accounting.
    #[inline]
    pub fn storage_bytes(&self) -> usize {
        match &self.data {
            ArrayData::Words(w, cards) => w.capacity() * mem::size_of::<Value>() + cards.as_ref().map_or(0, |t| t.bytes()),
            ArrayData::Lanes { words: w, .. } => w.capacity() * mem::size_of::<Value>(),
            ArrayData::Bytes(b) => b.capacity(),
        }
    }

    /// Bring the counted storage size up to date: `(old, new)` for
    /// [`Heap::account_resize`] when it changed (a grow, a pack, an unpack).
    #[inline]
    pub fn take_resize(&mut self) -> Option<(usize, usize)> {
        let now = self.storage_bytes();
        if now == self.accounted {
            return None;
        }
        Some((mem::replace(&mut self.accounted, now), now))
    }

    /// False when no element can be a heap reference (marking skips them).
    #[inline]
    pub fn may_hold_refs(&self) -> bool {
        self.may_hold_refs
    }

    /// The element words in place, for native code (`None` for a byte
    /// array). `for_store` marks the array as a runtime store would.
    /// [`Self::words_view`] for loads only: no write to the object, so a
    /// shared-heap worker may view an array from before its epoch.
    pub(crate) fn words_view_for_load(&self) -> Option<(*mut Value, usize)> {
        match &self.data {
            ArrayData::Words(w, ..) => Some((w.as_ptr().cast_mut(), w.len())),
            ArrayData::Bytes(_) | ArrayData::Lanes { .. } => None,
        }
    }

    /// The words in place for native code's stores, marked as a runtime
    /// store would, with the first card byte of the array's card table
    /// (null without one).
    #[inline]
    pub(crate) fn words_view_for_store(&mut self) -> Option<(*mut Value, usize, *mut u8)> {
        self.note();
        match &mut self.data {
            ArrayData::Words(w, cards) => Some((w.as_mut_ptr(), w.len(), cards.as_mut().map_or(std::ptr::null_mut(), |t| t.cards.as_mut_ptr()))),
            ArrayData::Bytes(_) | ArrayData::Lanes { .. } => None,
        }
    }

    /// A two-word lane array's words in place, for native code, with its
    /// element count (`None` for any other array). Loads only: no write to
    /// the object ([`Self::words_view_for_load`]).
    pub(crate) fn lanes_view_for_load(&self) -> Option<(*mut Value, usize)> {
        match &self.data {
            ArrayData::Lanes { words } if self.stride == 2 => Some((words.as_ptr().cast_mut(), words.len() / 2)),
            _ => None,
        }
    }

    /// [`Self::lanes_view_for_load`] for stores: marks the array as
    /// [`Self::set_lanes`] does when an element may hold a reference.
    pub(crate) fn lanes_view(&mut self) -> Option<(*mut Value, usize)> {
        if self.tag_mask != 0 {
            self.note();
        }
        self.lanes_view_for_load()
    }

    /// A write may add a reference: the next mark rescans.
    #[inline(always)]
    fn note(&mut self) {
        // Test first: rewriting the byte on every store costs more than the
        // (predictable) branch in hot store loops.
        if !self.may_hold_refs {
            self.may_hold_refs = true;
        }
    }

    /// [`Self::note`] for a write of elements `lo..hi`.
    #[inline]
    fn note_range(&mut self, lo: usize, hi: usize) {
        self.note();
        if let ArrayData::Words(_, Some(t)) = &mut self.data {
            t.mark_range(lo, hi);
        }
    }

    /// Whether the array has a card table: native code's stores into it
    /// mark cards ([`VIEW_TRACKED`]).
    #[inline]
    #[cfg(test)]
    pub(crate) fn has_cards(&self) -> bool {
        matches!(self.data, ArrayData::Words(_, Some(_)))
    }

    /// A minor found this array remembered and scanned it whole: give a
    /// large one a clean card table. Returns whether it did.
    pub(crate) fn attach_cards(&mut self) -> bool {
        match &mut self.data {
            ArrayData::Words(w, cards @ None) if w.len() >= CARD_MIN_WORDS => {
                *cards = Some(Box::new(CardTable::new(w.capacity())));
                true
            }
            _ => false,
        }
    }

    /// The words and the card table, for a minor's card scan.
    #[inline]
    pub(crate) fn words_and_cards_mut(&mut self) -> Option<(&mut [Value], Option<&mut CardTable>)> {
        match &mut self.data {
            ArrayData::Words(w, cards) => Some((w, cards.as_deref_mut())),
            _ => None,
        }
    }

    /// The card table, for the missed-barrier check.
    #[inline]
    pub(crate) fn cards(&self) -> Option<&CardTable> {
        match &self.data {
            ArrayData::Words(_, cards) => cards.as_deref(),
            _ => None,
        }
    }

    /// Store a numeric SIMD lane (never a reference) without classifying.
    ///
    /// # Safety
    /// `i` must be in bounds and `v` must not be a heap reference.
    #[inline(always)]
    pub unsafe fn set_numeric_unchecked(&mut self, i: usize, v: Value) {
        if let ArrayData::Bytes(b) = &mut self.data
            && let Some(byte) = byte_of(v)
        {
            unsafe { *b.get_unchecked_mut(i) = byte };
            return;
        }
        unsafe { *self.words_vec().get_unchecked_mut(i) = v };
    }

    #[inline(always)]
    pub fn push(&mut self, v: Value) {
        if let ArrayData::Words(w, cards) = &mut self.data {
            if !self.may_hold_refs {
                self.may_hold_refs = true;
            }
            if let Some(t) = cards {
                t.mark(w.len());
            }
            w.push(v);
        } else {
            self.push_packed(v);
        }
    }

    /// [`Self::push`] onto byte storage (unpacking makes word storage with
    /// no card table).
    #[inline(never)]
    fn push_packed(&mut self, v: Value) {
        if let ArrayData::Bytes(b) = &mut self.data
            && let Some(byte) = byte_of(v)
        {
            b.push(byte);
            return;
        }
        self.note();
        self.words_vec().push(v);
    }

    /// How many words the elements take: one per element, `k` per element
    /// of a lane array, none for packed bytes ([`Self::word_at`]).
    pub(crate) fn word_count(&self) -> usize {
        match &self.data {
            ArrayData::Words(w, ..) => w.len(),
            ArrayData::Bytes(_) => 0,
            ArrayData::Lanes { words } => words.len(),
        }
    }

    /// Word `i` of the elements' storage, lanes included ([`Self::word_count`]).
    pub(crate) fn word_at(&self, i: usize) -> Option<Value> {
        match &self.data {
            ArrayData::Words(w, ..) => w.get(i).copied(),
            ArrayData::Bytes(_) => None,
            ArrayData::Lanes { words } => words.get(i).copied(),
        }
    }

    /// Overwrite word `i` of the elements' storage ([`Self::word_at`]).
    pub(crate) fn set_word_at(&mut self, i: usize, v: Value) {
        if i >= self.word_count() {
            return;
        }
        if matches!(self.data, ArrayData::Words(..)) {
            self.set(i, v);
        } else if let ArrayData::Lanes { words } = &mut self.data {
            words[i] = v;
            self.note();
        }
    }

    /// Overwrite element `i` (caller checks bounds).
    #[inline]
    pub fn set(&mut self, i: usize, v: Value) {
        match &mut self.data {
            ArrayData::Words(w, cards) => {
                if !self.may_hold_refs {
                    self.may_hold_refs = true;
                }
                if let Some(t) = cards {
                    t.mark(i);
                }
                w[i] = v;
            }
            ArrayData::Bytes(b) if let Some(byte) = byte_of(v) => b[i] = byte,
            _ => {
                self.note();
                self.words_vec()[i] = v;
            }
        }
    }

    /// Unchecked [`Self::set`]; `i < len` is the caller's proof.
    ///
    /// # Safety
    /// `i` must be in bounds.
    #[inline]
    pub unsafe fn set_unchecked(&mut self, i: usize, v: Value) {
        match &mut self.data {
            ArrayData::Words(w, cards) => {
                if !self.may_hold_refs {
                    self.may_hold_refs = true;
                }
                if let Some(t) = cards {
                    t.mark(i);
                }
                unsafe { *w.get_unchecked_mut(i) = v };
            }
            ArrayData::Bytes(b) if let Some(byte) = byte_of(v) => unsafe { *b.get_unchecked_mut(i) = byte },
            _ => {
                self.note();
                unsafe { *self.words_vec().get_unchecked_mut(i) = v };
            }
        }
    }

    /// `elements[index] = value` with the runtime's bounds rules: `unchecked` is a
    /// proven index; otherwise out of range returns `false`.
    #[inline(always)]
    pub fn store_indexed(&mut self, index: i64, value: Value, unchecked: bool) -> bool {
        let len = self.len();
        let idx = index as usize;
        if unchecked {
            promise!(index >= 0);
            promise!(idx < len);
        } else if index < 0 || idx >= len {
            return false;
        }
        match &mut self.data {
            ArrayData::Words(elements, cards) => {
                unsafe {
                    *elements.get_unchecked_mut(idx) = value;
                }
                if !self.may_hold_refs {
                    self.may_hold_refs = true;
                }
                if let Some(t) = cards {
                    t.mark(idx);
                }
            }
            ArrayData::Bytes(b) => match byte_of(value) {
                Some(byte) => unsafe { *b.get_unchecked_mut(idx) = byte },
                None => {
                    // Unpacking makes word storage with no card table.
                    self.note();
                    unsafe { *self.words_vec().get_unchecked_mut(idx) = value };
                }
            },
            ArrayData::Lanes { .. } => lane_misuse(),
        }
        true
    }

    /// Remove and return the last element.
    #[inline]
    pub fn pop(&mut self) -> Option<Value> {
        match &mut self.data {
            ArrayData::Words(w, ..) => w.pop(),
            ArrayData::Bytes(b) => b.pop().map(word_of),
            ArrayData::Lanes { .. } => lane_misuse(),
        }
    }

    /// Insert `v` at `i` (caller checks `i <= len`).
    pub fn insert(&mut self, i: usize, v: Value) {
        if let ArrayData::Bytes(b) = &mut self.data
            && let Some(byte) = byte_of(v)
        {
            b.insert(i, byte);
            return;
        }
        let len = self.len();
        self.note_range(i, len + 1);
        self.words_vec().insert(i, v);
    }

    /// Remove element `i` (caller checks `i < len`).
    pub fn remove(&mut self, i: usize) -> Value {
        // The elements after `i` move down a card boundary.
        if let ArrayData::Words(w, Some(t)) = &mut self.data {
            t.mark_range(i, w.len());
        }
        match &mut self.data {
            ArrayData::Words(w, ..) => w.remove(i),
            ArrayData::Bytes(b) => word_of(b.remove(i)),
            ArrayData::Lanes { .. } => lane_misuse(),
        }
    }

    /// Drop elements `from..to`, the ones after them moving down (the
    /// range cut to the length).
    pub fn drain_range(&mut self, from: usize, to: usize) {
        let k = &usize::from(self.stride);
        let to = to.min(self.len());
        let from = from.min(to);
        if let ArrayData::Words(w, Some(t)) = &mut self.data {
            t.mark_range(from, w.len());
        }
        match &mut self.data {
            ArrayData::Words(w, ..) => drop(w.drain(from..to)),
            ArrayData::Bytes(b) => drop(b.drain(from..to)),
            ArrayData::Lanes { words } => drop(words.drain(from * *k..to * *k)),
        }
    }

    /// Move the last element to `i`, elements `i..` moving up one (nothing
    /// when `i` is not below the length).
    pub fn rotate_last_to(&mut self, i: usize) {
        let k = &usize::from(self.stride);
        if i >= self.len() {
            return;
        }
        if let ArrayData::Words(w, Some(t)) = &mut self.data {
            t.mark_range(i, w.len());
        }
        match &mut self.data {
            ArrayData::Words(w, ..) => w[i..].rotate_right(1),
            ArrayData::Bytes(b) => b[i..].rotate_right(1),
            ArrayData::Lanes { words } => words[i * *k..].rotate_right(*k),
        }
    }

    pub fn clear(&mut self) {
        match &mut self.data {
            ArrayData::Words(w, ..) | ArrayData::Lanes { words: w, .. } => w.clear(),
            ArrayData::Bytes(b) => b.clear(),
        }
    }

    /// Append elements `lo..hi` of `src` (caller checks bounds; `src` is
    /// another array). Packed into packed is one copy.
    pub fn extend_from(&mut self, src: &Self, lo: usize, hi: usize) {
        match (&mut self.data, &src.data) {
            (ArrayData::Bytes(out), ArrayData::Bytes(inp)) => out.extend_from_slice(&inp[lo..hi]),
            (ArrayData::Words(out, ..), ArrayData::Words(inp, ..)) => {
                let at = out.len();
                out.extend_from_slice(&inp[lo..hi]);
                if src.may_hold_refs {
                    self.note_range(at, at + (hi - lo));
                }
            }
            (ArrayData::Lanes { words: out }, ArrayData::Lanes { words: inp }) if self.stride == src.stride => {
                let k = &usize::from(self.stride);
                out.extend_from_slice(&inp[lo * *k..hi * *k]);
                if src.may_hold_refs {
                    self.note();
                }
            }
            (ArrayData::Lanes { .. }, _) | (_, ArrayData::Lanes { .. }) => lane_misuse(),
            _ => {
                self.reserve(hi - lo);
                for i in lo..hi {
                    self.push(src.get(i).expect("in bounds"));
                }
            }
        }
    }

    /// Append `bytes`, each as an `int` element.
    pub fn extend_bytes(&mut self, bytes: &[u8]) {
        match &mut self.data {
            ArrayData::Bytes(out) => out.extend_from_slice(bytes),
            ArrayData::Words(out, ..) => out.extend(bytes.iter().map(|&b| Value::from(i64::from(b)))),
            ArrayData::Lanes { .. } => lane_misuse(),
        }
    }

    /// Push `v` `n` times.
    pub fn fill_push(&mut self, v: Value, n: usize) {
        if let ArrayData::Bytes(b) = &mut self.data
            && let Some(byte) = byte_of(v)
        {
            b.resize(b.len() + n, byte);
            return;
        }
        let len = self.len();
        self.note_range(len, len + n);
        let w = self.words_vec();
        w.resize(w.len() + n, v);
    }

    pub fn reserve(&mut self, extra: usize) {
        let k = &usize::from(self.stride);
        match &mut self.data {
            ArrayData::Words(w, ..) => w.reserve(extra),
            ArrayData::Bytes(b) => b.reserve(extra),
            ArrayData::Lanes { words } => words.reserve(extra * *k),
        }
    }

    /// The elements narrowed to bytes (`as u8`): one copy when packed.
    /// Packed bytes moved out, leaving the array empty
    /// ([`Self::restore_bytes`] puts them back); `None` for word storage.
    pub fn take_bytes(&mut self) -> Option<Vec<u8>> {
        match &mut self.data {
            ArrayData::Words(..) | ArrayData::Lanes { .. } => None,
            ArrayData::Bytes(b) => Some(std::mem::take(b)),
        }
    }

    /// Put back the bytes [`Self::take_bytes`] took.
    pub fn restore_bytes(&mut self, bytes: Vec<u8>) {
        self.data = ArrayData::Bytes(bytes);
    }

    pub fn to_byte_vec(&self) -> Vec<u8> {
        match &self.data {
            ArrayData::Words(w, ..) => w.iter().map(|v| v.as_int() as u8).collect(),
            ArrayData::Bytes(b) => b.clone(),
            ArrayData::Lanes { .. } => lane_misuse(),
        }
    }

    /// Overwrite elements `at..` with `bytes` (bytes past the end are
    /// dropped): one copy when packed.
    pub fn write_bytes_at(&mut self, at: usize, bytes: &[u8]) {
        let n = bytes.len().min(self.len().saturating_sub(at));
        match &mut self.data {
            ArrayData::Bytes(b) => b[at..at + n].copy_from_slice(&bytes[..n]),
            ArrayData::Words(w, ..) => {
                for (slot, &b) in w[at..at + n].iter_mut().zip(bytes) {
                    *slot = word_of(b);
                }
            }
            ArrayData::Lanes { .. } => lane_misuse(),
        }
    }

    /// Overwrite elements from the start with `values` (extra values are
    /// dropped).
    pub fn overwrite_prefix(&mut self, values: impl IntoIterator<Item = Value>) {
        let len = self.len();
        for (i, v) in values.into_iter().take(len).enumerate() {
            self.set(i, v);
        }
    }

    /// Raw word access for writes the classifier does not see. Unpacks a
    /// byte array and marks the array as possibly holding references
    /// (always safe).
    #[inline(always)]
    pub fn words_mut(&mut self) -> &mut Vec<Value> {
        self.may_hold_refs = true;
        if let ArrayData::Bytes(_) = self.data {
            self.unpack();
        }
        match &mut self.data {
            ArrayData::Words(w, cards) => {
                if let Some(t) = cards {
                    t.all = true;
                }
                w
            }
            ArrayData::Bytes(_) => unreachable!(),
            ArrayData::Lanes { .. } => lane_misuse(),
        }
    }

    /// Word elements for the collector's in-place fixups (compaction); no
    /// flag change, `None` for packed bytes (never references).
    #[inline]
    pub(crate) fn gc_words(&self) -> Option<&[Value]> {
        match &self.data {
            ArrayData::Words(w, ..) | ArrayData::Lanes { words: w, .. } => Some(w),
            ArrayData::Bytes(_) => None,
        }
    }

    pub(crate) fn gc_words_mut(&mut self) -> Option<&mut [Value]> {
        match &mut self.data {
            ArrayData::Words(w, ..) | ArrayData::Lanes { words: w, .. } => Some(w),
            ArrayData::Bytes(_) => None,
        }
    }
}

/// A one-word element read or write on a lane array: a code path that
/// does not know the array's stride. Loud, never a misread.
#[cold]
#[inline(never)]
#[track_caller]
fn lane_misuse() -> ! {
    panic!("an array of two-word elements read or written one word per element")
}

/// Shared bounds rule of [`ObjArray::read_indexed`].
#[inline(always)]
fn read_indexed_in<T: Copy>(elements: &[T], index: i64, unchecked: bool) -> Option<T> {
    let len = elements.len();
    if unchecked {
        let idx = index as usize;
        promise!(index >= 0);
        promise!(idx < len);
        Some(unsafe { *elements.get_unchecked(idx) })
    } else if index >= 0 && (index as usize) < len {
        Some(unsafe { *elements.get_unchecked(index as usize) })
    } else {
        None
    }
}

/// Suspended async function state: saved stack segment + call frames.
pub struct ObjCoroutine {
    pub state: CoroState,
    pub resume_ip: usize,
    /// Stack segment (args + locals + operands) relative to segment base 0.
    pub saved_stack: Vec<Value>,
    /// Bitmask of `saved_stack` slots that hold heap pointers (for precise GC).
    /// When zero, GC conservatively scans every slot.
    pub saved_live_mask: u64,
    /// The body has run: a close has `defer`s to run only after this.
    pub started: bool,
    /// Listed for a close by the collector: it suspended with a `defer`
    /// armed ([`Heap::note_closeable`]).
    pub closeable: bool,
    /// Its body is native code, suspended on this fiber (`gen_fiber.rs`):
    /// `saved_stack` then holds that frame's root words.
    pub native: Option<Box<crate::native::GenFiber>>,
}

/// Explicit strong GC pin: keeps `payload` alive while this object is reachable.
pub struct ObjRoot {
    /// Rooted value; `None` after [`crate::gc_handles`] unroot.
    pub payload: Option<Member>,
}

/// Non-rooting handle to a value; cleared when the referent is unmarked.
pub struct ObjWeak {
    /// Referent (immediate or heap address). Not traced by mark-sweep.
    pub target: Cell<Value>,
    /// Set when the referent dies (or after an explicit clear).
    pub cleared: Cell<bool>,
}

/// First-class function value: a monomorphic function, partial application
/// or explicit-capture lambda (`MakeFn`), or a generic function value
/// (`MakePolyFn*`, which sets [`Self::dicts`]).
pub struct ObjFn {
    /// Entry id of the body.
    pub entry: u32,
    /// Fixed arity, or rest `nfixed` when `is_rest`.
    pub arity: u32,
    /// Trailing rest parameter packs extra args into `[T]`.
    pub is_rest: bool,
    /// Bitmask of which fixed param slots are already filled (partial apply).
    pub filled_mask: u64,
    /// Values for filled param slots (decl order among filled bits).
    pub captured_args: Vec<Value>,
    /// Explicit `use (x, y)` capture snapshot (leading frame locals).
    pub captures: Vec<Value>,
    /// Generic function value: dictionary evidence captured when the value
    /// escaped a constrained scope, merged with the call site's dictionaries
    /// by `CallIndirect`. A `None` slot takes application-time evidence.
    /// `None` for a monomorphic function.
    pub dicts: Option<Vec<Option<Member>>>,
}

impl ObjFn {
    /// Generic function value at `entry` declaring `arity` parameters (0:
    /// unrecorded) with captured evidence `dicts`.
    pub fn poly(entry: u32, arity: u32, dicts: Vec<Option<Member>>) -> Self {
        Self {
            entry,
            arity,
            is_rest: false,
            filled_mask: 0,
            captured_args: Vec::new(),
            captures: Vec::new(),
            dicts: Some(dicts),
        }
    }
}

/// Host-backed non-blocking IO stream (file / stdio / TCP / UDP / attached).
pub struct ObjStream {
    pub handle: Option<crate::io_handle::NativeHandle>,
    pub kind: StreamKind,
    pub closed: bool,
    /// Soft deadline for sync read adapters / handshake reads (`None` = wait forever).
    pub read_timeout: Option<std::time::Duration>,
    /// Soft deadline for sync write adapters / handshake writes (`None` = wait forever).
    pub write_timeout: Option<std::time::Duration>,
    /// Package session pointer + C vtable (`Stream.attach`).
    pub attached: Option<crate::stream_attach::AttachedIo>,
}

impl Drop for ObjStream {
    fn drop(&mut self) {
        if let Some(slot) = self.attached.take() {
            // shutdown when the fd is still here, then free.
            slot.shutdown_then_free(self.handle.as_mut());
        }
        // NativeHandle closes on drop; clear so Drop does not double-close.
        self.handle.take();
        self.closed = true;
    }
}

impl GcSized for ObjStream {
    fn size(&self) -> usize {
        std::mem::size_of::<Self>()
    }
}

impl fmt::Display for ObjStream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<stream {:?}>", self.kind)
    }
}

/// Join handle for a spawned OS thread (host `JoinState` lives outside the heap).
pub struct ObjThread {
    pub state: std::sync::Arc<crate::thread::JoinState>,
}

impl GcSized for ObjThread {
    fn size(&self) -> usize {
        std::mem::size_of::<Self>()
    }
}

impl fmt::Display for ObjThread {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<thread>")
    }
}

pub struct ObjSender {
    pub inner: std::sync::Arc<crate::thread::ChannelInner>,
}

impl GcSized for ObjSender {
    fn size(&self) -> usize {
        std::mem::size_of::<Self>()
    }
}

impl fmt::Display for ObjSender {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<sender>")
    }
}

pub struct ObjReceiver {
    pub inner: std::sync::Arc<crate::thread::ChannelInner>,
}

impl GcSized for ObjReceiver {
    fn size(&self) -> usize {
        std::mem::size_of::<Self>()
    }
}

impl fmt::Display for ObjReceiver {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<receiver>")
    }
}

pub struct ObjThreadMutex {
    pub inner: std::sync::Arc<crate::thread::MutexInner>,
}

impl GcSized for ObjThreadMutex {
    fn size(&self) -> usize {
        std::mem::size_of::<Self>()
    }
}

impl fmt::Display for ObjThreadMutex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<mutex>")
    }
}

pub struct ObjRwLock {
    pub inner: std::sync::Arc<crate::thread::RwLockInner>,
}

impl GcSized for ObjRwLock {
    fn size(&self) -> usize {
        std::mem::size_of::<Self>()
    }
}

impl fmt::Display for ObjRwLock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<rwlock>")
    }
}

impl GcSized for ObjArray {
    const NURSERY: bool = true;

    fn size(&self) -> usize {
        mem::size_of::<Self>() + self.accounted
    }
}

impl GcSized for ObjCoroutine {
    fn size(&self) -> usize {
        // `saved_stack` uses Rust's allocator, not the runtime
        // heap byte counter (same contract as `ObjInstance`).
        mem::size_of::<Self>()
    }
}

impl GcSized for ObjRoot {
    fn size(&self) -> usize {
        mem::size_of::<Self>()
    }
}

impl GcSized for ObjWeak {
    fn size(&self) -> usize {
        mem::size_of::<Self>()
    }
}

impl GcSized for ObjFn {
    const NURSERY: bool = true;

    fn size(&self) -> usize {
        // Vec payloads use Rust's allocator (same as ObjArray elements).
        mem::size_of::<Self>()
    }
}

impl fmt::Display for ObjEnum {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if !self.is_tuple() {
            return write!(f, "<variant {}>", self.tag);
        }
        write!(
            f,
            "({})",
            self.payload
                .iter()
                .map(|v| format!("{}", v.as_int()))
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}

impl fmt::Display for ObjArray {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "[{}]",
            self.iter()
                .map(|v| format!("{}", v.as_int()))
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}

impl fmt::Display for ObjCoroutine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "<coroutine {:?}>", self.state)
    }
}

impl fmt::Display for ObjString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.data)
    }
}

/// Loaded shared library plus cached FFI signatures.
pub struct ObjLibrary {
    pub library: std::sync::Arc<crate::ffi::Library>,
    pub signatures: Vec<RegisteredFunction>,
    pub by_name: std::collections::HashMap<String, usize>,
    /// libffi closures registered for callbacks (keeps trampolines alive).
    pub closures: Vec<crate::ffi::OwnedClosure>,
    /// A library linked into the executable: its functions' addresses
    /// (`library` is then the process itself).
    pub static_symbols: Option<&'static crate::ffi::StaticLibrary>,
}

/// A declared FFI function with a prepared libffi call interface.
pub struct RegisteredFunction {
    /// The signature the call path marshals against, kept so an invoke
    /// does not rebuild (and allocate) it.
    pub ffi_sig: crate::ffi::FfiSignature,
    pub prepared: crate::ffi::PreparedCall,
}

impl RegisteredFunction {
    pub fn ffi_signature(&self) -> &crate::ffi::FfiSignature {
        &self.ffi_sig
    }
}

/// C ABI type tags for FFI marshalling.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FfiType {
    Int,
    Float,
    String,
    Void,
    Bool,
    Int8,
    Int16,
    Int32,
    UInt8,
    UInt16,
    UInt32,
    UInt64,
    Ptr,
    Callback(u32),
    Struct(u32),
    /// `Vec<byte>` buffer, passed as `uint8_t *` and copied back.
    Bytes,
    /// Read-only `string` / `Vec<byte>` view, passed as `const uint8_t *`.
    ConstBytes,
}

impl FfiType {
    pub fn from_tag(tag: u32, aux: u32) -> Self {
        use common::tag as t;
        match tag {
            x if x == t::FLOAT => Self::Float,
            x if x == t::STRING => Self::String,
            x if x == t::VOID => Self::Void,
            x if x == t::BOOL => Self::Bool,
            x if x == t::INT8 => Self::Int8,
            x if x == t::INT16 => Self::Int16,
            x if x == t::INT32 => Self::Int32,
            x if x == t::UINT8 => Self::UInt8,
            x if x == t::UINT16 => Self::UInt16,
            x if x == t::UINT32 => Self::UInt32,
            x if x == t::UINT64 => Self::UInt64,
            x if x == t::PTR => Self::Ptr,
            x if x == t::CALLBACK => Self::Callback(aux),
            x if x == t::STRUCT => Self::Struct(aux),
            x if x == t::BYTES => Self::Bytes,
            x if x == t::CONST_BYTES => Self::ConstBytes,
            _ => Self::Int,
        }
    }

    pub fn tag(&self) -> u32 {
        use common::tag as t;
        match self {
            Self::Int => t::INT,
            Self::Float => t::FLOAT,
            Self::String => t::STRING,
            Self::Void => t::VOID,
            Self::Bool => t::BOOL,
            Self::Int8 => t::INT8,
            Self::Int16 => t::INT16,
            Self::Int32 => t::INT32,
            Self::UInt8 => t::UINT8,
            Self::UInt16 => t::UINT16,
            Self::UInt32 => t::UINT32,
            Self::UInt64 => t::UINT64,
            Self::Ptr => t::PTR,
            Self::Callback(_) => t::CALLBACK,
            Self::Struct(_) => t::STRUCT,
            Self::Bytes => t::BYTES,
            Self::ConstBytes => t::CONST_BYTES,
        }
    }

    pub fn aux(&self) -> u32 {
        match self {
            Self::Callback(id) | Self::Struct(id) => *id,
            _ => 0,
        }
    }

    pub fn is_void(self) -> bool {
        matches!(self, Self::Void)
    }
}

/// C-layout struct descriptor for pass-by-value FFI.
#[derive(Clone, Debug)]
pub struct CStructLayout {
    pub name: String,
    pub fields: Vec<(String, FfiType)>,
    pub offsets: Vec<usize>,
    pub size: usize,
    pub align: usize,
}

impl CStructLayout {
    pub fn from_archive(layout: &common::CStructLayout) -> Self {
        Self {
            name: layout.name.clone(),
            fields: layout
                .fields
                .iter()
                .map(|(n, enc)| {
                    let (tag, aux) = common::decode_tag_operand(*enc);
                    (n.clone(), FfiType::from_tag(tag, aux))
                })
                .collect(),
            offsets: layout.offsets.iter().map(|&o| o as usize).collect(),
            size: layout.size as usize,
            align: layout.align as usize,
        }
    }
}

impl GcSized for ObjLibrary {
    fn size(&self) -> usize {
        mem::size_of::<Self>() + mem::size_of_val(&*self.library)
    }
}

impl fmt::Display for ObjLibrary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "<library at 0x{:x}, {} function(s)>",
            std::sync::Arc::as_ptr(&self.library) as u64,
            self.signatures.len()
        )
    }
}

pub trait GcSized {
    fn size(&self) -> usize;
    /// Allocated in the nursery when it is on: a kind a minor collection
    /// can move ([`Object::movable_layout`]).
    const NURSERY: bool = false;
    /// Allocated in the nursery inside a `region` block: the movable kinds,
    /// and strings, which a minor collection frees but never moves (a
    /// surviving one is held in place).
    const REGION: bool = Self::NURSERY;
    /// Can hold heap references: an old object of this kind is remembered
    /// when it is born, since it may start out naming young objects.
    const HOLDS_REFS: bool = true;
    /// Class or enum type id, so an object whose type has a `drop` stays
    /// out of the nursery.
    fn finalizer_type_id(&self) -> Option<u32> {
        None
    }
}

/// [`GcHeader::kind`] of the objects native code allocates inline, and of
/// those a share marks (an instance, an array).
pub const KIND_INSTANCE: u8 = 2;
const KIND_ENUM: u8 = 3;
pub const KIND_ARRAY: u8 = 6;

/// [`GcHeader::generation`]: the object lives in a young (nursery) chunk.
pub(crate) const GEN_YOUNG: u8 = 1;
/// [`GcHeader::generation`]: an old object not in the remembered set; its
/// next payload write records it ([`remember_written`]).
pub(crate) const GEN_UNLOGGED: u8 = 2;
/// [`GcHeader::generation`]: a young object a minor collection may not
/// move (an ambiguous word names it). Only set during a minor collection.
pub(crate) const GEN_PINNED: u8 = 4;
/// [`GcHeader::generation`]: a young object a minor pinned, held in its
/// chunk until a major frees it. Minors treat it as old: they do not trace
/// it, and the write barrier records it ([`GEN_UNLOGGED`]) like any old
/// object (#2255).
pub(crate) const GEN_HELD: u8 = 8;

/// [`GcHeader::generation`]: an old object the barrier logged as quiet
/// ([`QUIET`]), whose barrier the nursery's restart turns back on
/// ([`relog_quiet`]). A store view of it must remember it at once instead
/// ([`GcData::note_store_view`]).
pub(crate) const GEN_QUIET: u8 = 16;

/// Generation bits of an object the write barrier covers: an old one or a
/// held one.
#[inline]
pub(crate) fn barriered(generation: u8) -> bool {
    generation & GEN_YOUNG == 0 || generation & GEN_HELD != 0
}

thread_local! {
    /// Old objects written since the last minor collection (the nursery's
    /// remembered set). The barrier has no heap at hand, so the set lives
    /// with the thread; a heap takes only its own entries
    /// ([`Heap::take_remembered`]).
    static REMEMBERED: std::cell::RefCell<Vec<u64>> = const { std::cell::RefCell::new(Vec::new()) };
    /// Heaps on this thread with nursery chunks in use. While none has, no
    /// young object exists for a write to store, and the barrier records
    /// nothing (pretenured objects are written while the nursery is empty).
    static YOUNG_HEAPS: Cell<u32> = const { Cell::new(0) };
    /// Old objects written while no heap here had young objects: their
    /// barrier is off (nothing young to record) until their heap's nursery
    /// starts again ([`relog_quiet`]), so a write loop pays it once.
    static QUIET: std::cell::RefCell<Vec<u64>> = const { std::cell::RefCell::new(Vec::new()) };
    /// Times [`QUIET`] outgrew [`QUIET_CAP`] and was dropped: a heap that
    /// has not seen the latest count relogs all its old objects instead
    /// ([`relog_all`]).
    static QUIET_OVERFLOWS: Cell<u64> = const { Cell::new(0) };
}

/// Entries [`QUIET`] holds before it is dropped for a whole-heap relog. A
/// long pretenured stretch writes every new object once; a list per write
/// would grow with the program (and every compaction forwards it).
const QUIET_CAP: usize = 1 << 16;

/// [`QUIET_OVERFLOWS`] now.
pub(super) fn quiet_overflows() -> u64 {
    QUIET_OVERFLOWS.with(Cell::get)
}

/// [`relog_quiet`] for a heap whose entries [`QUIET`] dropped: log every
/// old object of `slab` again.
pub(super) fn relog_all(slab: &super::slab::Slab) {
    let mut cur = super::slab::SlotCursor::default();
    while let Some(addr) = slab.next_slot(&mut cur) {
        // SAFETY: `next_slot` yields slot starts in mapped chunks.
        relog(unsafe { &*(addr as *const GcHeader) });
    }
}

/// `slab`'s nursery starts again (no young object exists yet): turn the
/// barrier back on for its old objects written while it was empty.
pub(super) fn relog_quiet(slab: &super::slab::Slab) {
    QUIET.with_borrow_mut(|q| {
        q.retain(|&addr| {
            if !slab.contains_slot(addr) {
                return true;
            }
            // SAFETY: a slot start in a mapped chunk holds a header; a
            // freed or re-carved slot reads as free or as another old
            // object, and an extra log bit only costs one more record.
            relog(unsafe { &*(addr as *const GcHeader) });
            false
        });
    });
}

/// The nursery starts again: an old object written while it was empty is
/// logged again on its next write.
fn relog(header: &GcHeader) {
    let g = header.generation.get();
    if header.kind.get() != 0 && barriered(g) {
        header.generation.set(g & !GEN_QUIET | GEN_UNLOGGED);
    }
}

/// A heap on this thread started (`true`) or stopped using nursery chunks.
pub(super) fn note_young_heap(on: bool) {
    YOUNG_HEAPS.with(|n| n.set(if on { n.get() + 1 } else { n.get().saturating_sub(1) }));
}

/// Slow path of the write barrier in [`GcData::as_mut`].
#[cold]
#[inline(never)]
fn remember_written(header: &GcHeader) {
    let addr = header as *const GcHeader as u64;
    if YOUNG_HEAPS.with(Cell::get) == 0 {
        header.generation.set(header.generation.get() & !GEN_UNLOGGED | GEN_QUIET);
        QUIET.with_borrow_mut(|q| {
            q.push(addr);
            if q.len() >= QUIET_CAP {
                q.clear();
                QUIET_OVERFLOWS.with(|n| n.set(n.get() + 1));
            }
        });
        return;
    }
    header.generation.set(header.generation.get() & !GEN_UNLOGGED);
    REMEMBERED.with_borrow_mut(|r| r.push(addr));
}

/// Slow path of [`GcData::note_store_view`].
#[cold]
#[inline(never)]
fn remember_viewed(addr: u64) {
    REMEMBERED.with_borrow_mut(|r| r.push(addr));
}

/// Prefix of every managed allocation. Kind reconstructs [`Object`] without
/// a live-address HashSet (`kind == 0` is a free slot).
#[repr(C)]
struct GcHeader {
    kind: Cell<u8>,
    marked: Cell<bool>,
    /// Allocated while a sweep was running; that sweep skips it once.
    fresh: Cell<bool>,
    /// Nursery bits ([`GEN_YOUNG`], [`GEN_UNLOGGED`], [`GEN_PINNED`]).
    generation: Cell<u8>,
    /// Copy-on-write: a second place may name this object, so a write
    /// copies it first ([`Heap::cow_unique`]). Sticky until the slot is
    /// reused.
    shared: Cell<bool>,
    /// An instance's or enum's `drop` has been claimed: it never runs
    /// twice ([`Object::claim_finalizer`]).
    finalized: Cell<bool>,
    /// Copy-on-write: a name writes this object in place without a test
    /// (an alias the compiler pinned it for, `CowPin`), so a share copies
    /// it instead of setting [`Self::shared`] ([`Heap::cow_share`]). A
    /// pinned object is never shared. Header state, like `shared`: no
    /// barrier, and an evacuation copies it with the slot.
    pinned: Cell<bool>,
    /// Debug / gc-stress: generation of the steal epoch the object was
    /// allocated in, 0 outside any epoch ([`EpochWriteScope`]). It takes a
    /// byte of what is header padding before an 8-aligned payload; release
    /// builds do not have it.
    #[cfg(any(debug_assertions, feature = "gc-stress"))]
    epoch: Cell<u8>,
}


/// What [`Heap::cow_share`] did with a word.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CowShare {
    /// Not a class instance or an array: left as it is.
    Untouched,
    /// The object's shared flag is set; the word names it still.
    Marked,
    /// The object is pinned: the new name takes this copy of it.
    Copied(Value),
}

/// Byte offset of a header's kind byte ([`GcHeader::kind`]): what
/// [`Object::from_header`] reads, and native code's inline share
/// (#2506, `coil-native`).
pub const HEADER_KIND_OFFSET: i32 = std::mem::offset_of!(GcHeader, kind) as i32;
/// Byte offset of a header's copy-on-write flag ([`GcHeader::shared`]).
/// A share stores [`COW_SHARED_SET`] there: [`Object::set_shared`], and
/// native code's inline share of a class instance or an array (#2506).
pub const COW_SHARED_OFFSET: i32 = std::mem::offset_of!(GcHeader, shared) as i32;
/// The byte a share stores at [`COW_SHARED_OFFSET`]: `true`.
pub const COW_SHARED_SET: u8 = 1;
/// Byte offset of a header's copy-on-write pin ([`GcHeader::pinned`]):
/// what native code's inline share tests, and its `CowPin` / `CowUnpin`
/// write ([`COW_PINNED_SET`] or `0`).
pub const COW_PINNED_OFFSET: i32 = std::mem::offset_of!(GcHeader, pinned) as i32;
/// The byte a pin stores at [`COW_PINNED_OFFSET`]: `true`.
pub const COW_PINNED_SET: u8 = 1;

// The flag and the kind are one byte each, so native code reads and writes
// them as bytes.
const _: () = assert!(std::mem::size_of::<Cell<bool>>() == 1 && std::mem::size_of::<Cell<u8>>() == 1);
const _: () = assert!(COW_SHARED_SET == true as u8 && COW_PINNED_SET == true as u8);
// The pin takes a byte of what was padding before an 8-aligned payload: the
// header stays within one word, so no slot grows.
const _: () = assert!(std::mem::size_of::<GcHeader>() <= 8);
const _: () = assert!(std::mem::offset_of!(GcData<u64>, data) == 8);
const _: () = assert!(COW_PINNED_OFFSET != COW_SHARED_OFFSET && COW_PINNED_OFFSET != HEADER_KIND_OFFSET);

#[repr(C)]
pub struct GcData<T> {
    header: GcHeader,
    data: T,
}

impl GcHeader {
    const fn new() -> Self {
        Self {
            kind: Cell::new(0),
            marked: Cell::new(false),
            fresh: Cell::new(false),
            generation: Cell::new(0),
            shared: Cell::new(false),
            finalized: Cell::new(false),
            pinned: Cell::new(false),
            #[cfg(any(debug_assertions, feature = "gc-stress"))]
            epoch: Cell::new(0),
        }
    }
}

impl<T> GcData<T> {
    pub const fn new(data: T) -> Self {
        Self {
            header: GcHeader::new(),
            data,
        }
    }

    fn set_kind(&self, kind: u8) {
        self.header.kind.set(kind);
    }

    fn set_fresh(&self) {
        self.header.fresh.set(true);
    }

    pub const fn is_marked(&self) -> bool {
        self.header.marked.get()
    }

    pub fn mark(&self) -> bool {
        let is_not_marked = !self.header.marked.get();
        if is_not_marked {
            self.header.marked.set(true);
        }
        is_not_marked
    }

    pub fn unmark(&self) {
        self.header.marked.set(false);
    }
}

impl<T> AsRef<T> for GcData<T> {
    fn as_ref(&self) -> &T {
        &self.data
    }
}

impl<T> AsMut<T> for GcData<T> {
    fn as_mut(&mut self) -> &mut T {
        #[cfg(any(debug_assertions, feature = "gc-stress"))]
        self.check_epoch_write();
        // Write barrier: the first write to an old object since the last
        // minor collection records it, so the nursery finds a young object
        // it may now name ([`remember_written`]).
        if unlikely(self.header.generation.get() & GEN_UNLOGGED != 0) {
            remember_written(&self.header);
        }
        &mut self.data
    }
}

impl GcData<ObjArray> {
    /// Native code took a store view of this array, after
    /// [`AsMut::as_mut`] ran its barrier: stores through the view skip the
    /// barrier until native code drops it. An array the barrier logged as
    /// quiet is remembered now. Its barrier would come back on when the
    /// nursery starts again, which an allocation does with the view still
    /// live, and the young objects stored through the view would go
    /// unseen (#2702).
    #[inline]
    pub(crate) fn note_store_view(&self) {
        let g = self.header.generation.get();
        if unlikely(g & GEN_QUIET != 0) {
            self.header.generation.set(g & !GEN_QUIET);
            remember_viewed(self as *const Self as u64);
        }
    }
}

#[cfg(any(debug_assertions, feature = "gc-stress"))]
impl<T> GcData<T> {
    /// Inside a shared-heap job, a payload write must target an object
    /// allocated in that job's epoch ([`EpochWriteScope`]). Every runtime
    /// payload write reaches the object through [`AsMut::as_mut`] (directly
    /// or via [`Gc::payload_mut`]), so this one check covers field stores,
    /// index stores, pushes, `may_hold_refs` and resize bookkeeping.
    #[inline]
    fn check_epoch_write(&self) {
        let job = EPOCH_WRITE_GEN.try_with(std::cell::Cell::get).unwrap_or(0);
        if job != 0 {
            let own = self.header.epoch.get();
            if own != job {
                epoch_write_violation(self as *const Self as u64, own, job);
            }
        }
    }
}

/// Header bytes charged to the collection trigger per object. The header
/// shrank to one word when the intrusive `next` link went away; charging
/// the old three words keeps collection pacing (cycles per object) as it
/// was, so the smaller header lowers the peak instead of delaying GC.
const PACED_HEADER_BYTES: usize = 24;

impl<T: GcSized> GcSized for GcData<T> {
    fn size(&self) -> usize {
        PACED_HEADER_BYTES + self.data.size()
    }
}

impl<T: GcSized + Copy> GcSized for Cell<T> {
    fn size(&self) -> usize {
        self.get().size()
    }
}

/// `CString::new` rejected an interior NUL. No extra payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InteriorNul;

pub struct Gc<T> {
    ptr: NonNull<GcData<T>>,
}

impl<T> Gc<T> {
    fn from_slot(ptr: NonNull<GcData<T>>) -> Self {
        Self { ptr }
    }

    /// Drop `T` and poison the header. The slot stays mapped for reuse.
    unsafe fn recycle(self) {
        let p = self.ptr.as_ptr();
        unsafe {
            ptr::drop_in_place(&mut (*p).data);
            (*p).header.kind.set(0);
            (*p).header.marked.set(false);
            (*p).header.fresh.set(false);
            (*p).header.generation.set(0);
            (*p).header.shared.set(false);
            (*p).header.pinned.set(false);
        }
    }

    #[must_use]
    pub fn ptr_eq(lhs: Self, rhs: Self) -> bool {
        lhs.ptr.eq(&rhs.ptr)
    }

    #[must_use]
    pub const fn as_ptr(&self) -> *const GcData<T> {
        self.ptr.as_ptr()
    }

    unsafe fn from_addr(addr: u64) -> Self {
        Self {
            ptr: unsafe { NonNull::new_unchecked(addr as *mut GcData<T>) },
        }
    }

    /// Mutable access to the inner payload (single-threaded runtime only).
    #[allow(clippy::mut_from_ref)] // `Gc<T>` is a copyable slot handle; the runtime mutates the payload through shared `&self`
    pub fn payload_mut(&self) -> &mut T {
        unsafe {
            let ptr = self.ptr.as_ptr().cast::<GcData<T>>();
            (*ptr).as_mut()
        }
    }
}

impl<T: GcSized> GcSized for Gc<T> {
    fn size(&self) -> usize {
        self.deref().size()
    }
}

impl<T> ops::Deref for Gc<T> {
    type Target = GcData<T>;

    fn deref(&self) -> &Self::Target {
        unsafe { self.ptr.as_ref() }
    }
}

impl<T> ops::DerefMut for Gc<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { self.ptr.as_mut() }
    }
}

impl<T> Copy for Gc<T> {}
impl<T> Clone for Gc<T> {
    fn clone(&self) -> Self {
        *self
    }
}

// Open-addressing hash table keyed by interned strings.

use std::{alloc, cell::UnsafeCell, marker::PhantomData};

use common::Value;

pub struct Table<V>(UnsafeCell<Store<V>>);

impl<V> Default for Table<V> {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl<V> Table<V> {
    #[inline]
    #[must_use]
    pub const fn new() -> Self {
        Self(UnsafeCell::new(Store::new()))
    }

    #[inline]
    pub fn len(&self) -> usize {
        let store = unsafe { &*self.0.get() };
        store.lives
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    #[inline]
    pub fn capacity(&self) -> usize {
        let store = unsafe { &*self.0.get() };
        store.cap
    }

    #[inline]
    pub fn get(&self, key: RefString) -> Option<V>
    where
        V: Copy,
    {
        let store = unsafe { &*self.0.get() };
        store.get(key)
    }

    #[inline]
    pub fn find(&self, s: &str, hash: u32) -> Option<RefString> {
        let store = unsafe { &*self.0.get() };
        store.find(s, hash)
    }

    #[inline]
    pub fn insert(&self, key: RefString, val: V) -> Option<V> {
        let store = unsafe { &mut *self.0.get() };
        store.insert(key, val)
    }

    #[inline]
    pub fn remove(&self, key: RefString) -> Option<V> {
        let store = unsafe { &mut *self.0.get() };
        store.remove(key)
    }

    #[inline]
    pub fn iter(&self) -> Iter<'_, V>
    where
        V: Copy,
    {
        let store = unsafe { &*self.0.get() };
        store.into_iter()
    }
}

pub struct Iter<'store, V> {
    ptr: NonNull<Entry<V>>,
    idx: usize,
    cap: usize,
    marker: PhantomData<&'store Store<V>>,
}

impl<V> Iterator for Iter<'_, V>
where
    V: Copy,
{
    type Item = (RefString, V);

    fn next(&mut self) -> Option<Self::Item> {
        while self.idx < self.cap {
            let entry = unsafe { &*self.ptr.as_ptr().add(self.idx) };
            self.idx += 1;
            if let Entry::Live(x) = entry {
                return Some((x.key, x.val));
            }
        }
        None
    }
}

struct Store<V> {
    lives: usize,
    deads: usize,
    cap: usize,
    ptr: NonNull<Entry<V>>,
}

impl<V> Drop for Store<V> {
    fn drop(&mut self) {
        if self.cap > 0 {
            let entries = NonNull::slice_from_raw_parts(self.ptr, self.cap);
            unsafe {
                NonNull::drop_in_place(entries);
                Self::dealloc(self.ptr, self.cap);
            }
        }
    }
}

impl<'store, V> IntoIterator for &'store Store<V>
where
    V: Copy,
{
    type Item = (RefString, V);

    type IntoIter = Iter<'store, V>;

    fn into_iter(self) -> Self::IntoIter {
        Self::IntoIter {
            ptr: self.ptr,
            idx: 0,
            cap: self.cap,
            marker: PhantomData,
        }
    }
}

impl<V> Store<V> {
    const fn new() -> Self {
        Self {
            ptr: NonNull::dangling(),
            cap: 0,
            lives: 0,
            deads: 0,
        }
    }

    fn get(&self, key: RefString) -> Option<V>
    where
        V: Copy,
    {
        if self.lives == 0 {
            return None;
        }
        let entry_ptr = unsafe { Self::probe(self.cap, self.ptr, key) };
        let entry = unsafe { entry_ptr.as_ref() };
        let Entry::Live(e) = entry else {
            return None;
        };
        Some(e.val)
    }

    fn find(&self, s: &str, hash: u32) -> Option<RefString> {
        if self.lives == 0 {
            return None;
        }
        let mut index = hash as usize & (self.cap - 1);
        loop {
            let entry_ptr = unsafe { self.ptr.add(index) };
            let entry = unsafe { entry_ptr.as_ref() };
            match entry {
                Entry::Free => return None,
                Entry::Live(entry) if coil_simd::bytes::eq(entry.key.as_ref().data.as_bytes(), s.as_bytes()) => {
                    return Some(entry.key);
                }
                _ => {}
            }
            index = (index + 1) & (self.cap - 1);
        }
    }

    fn insert(&mut self, key: RefString, val: V) -> Option<V> {
        if self.lives + self.deads >= self.cap * 3 / 4 {
            self.resize();
        }
        let mut entry_ptr = unsafe { Self::probe(self.cap, self.ptr, key) };
        let entry = unsafe { entry_ptr.as_mut() };
        match mem::replace(entry, Entry::Live(EntryInner { key, val })) {
            Entry::Free => {
                self.lives += 1;
                None
            }
            Entry::Dead => {
                self.lives += 1;
                self.deads -= 1;
                None
            }
            Entry::Live(e) => Some(e.val),
        }
    }

    fn remove(&mut self, key: RefString) -> Option<V> {
        if self.lives == 0 {
            return None;
        }
        let mut entry_ptr = unsafe { Self::probe(self.cap, self.ptr, key) };
        let entry = unsafe { entry_ptr.as_mut() };
        let Entry::Live(entry_old) = mem::replace(entry, Entry::Dead) else {
            return None;
        };
        self.lives -= 1;
        self.deads += 1;
        Some(entry_old.val)
    }

    unsafe fn probe(cap: usize, ptr: NonNull<Entry<V>>, key: RefString) -> NonNull<Entry<V>> {
        let mut dead = None;
        let mut index = key.as_ref().hash_code() as usize & (cap - 1);
        loop {
            let entry_ptr = unsafe { ptr.add(index) };
            match unsafe { entry_ptr.as_ref() } {
                Entry::Free => {
                    return dead.unwrap_or(entry_ptr);
                }
                Entry::Dead if dead.is_none() => {
                    dead = Some(entry_ptr);
                }
                Entry::Live(e) if Gc::ptr_eq(e.key, key) => {
                    return entry_ptr;
                }
                _ => {}
            }
            index = (index + 1) & (cap - 1);
        }
    }

    fn resize(&mut self) {
        let new_cap = self
            .cap
            .checked_mul(2)
            .expect("capacity does not overflow")
            .max(8);

        let new_ptr = Self::alloc(new_cap);
        if self.cap > 0 {
            for i in 0..self.cap {
                let old_entry_ptr = unsafe { self.ptr.add(i) };
                if let Entry::Live(e) = unsafe { old_entry_ptr.as_ref() } {
                    let new_entry_ptr = unsafe { Self::probe(new_cap, new_ptr, e.key) };
                    unsafe {
                        NonNull::swap(old_entry_ptr, new_entry_ptr);
                    }
                }
            }
            unsafe {
                Self::dealloc(self.ptr, self.cap);
            }
        }
        self.deads = 0;
        self.cap = new_cap;
        self.ptr = new_ptr;
    }

    fn layout(cap: usize) -> alloc::Layout {
        alloc::Layout::array::<Entry<V>>(cap).expect("a valid array layout")
    }

    fn alloc(cap: usize) -> NonNull<Entry<V>> {
        let layout = Self::layout(cap);
        let nullable = unsafe { alloc::alloc(layout) };
        let Some(ptr) = NonNull::new(nullable.cast()) else {
            alloc::handle_alloc_error(layout);
        };
        for i in 0..cap {
            unsafe {
                ptr.add(i).write(Entry::Free);
            }
        }
        ptr
    }

    unsafe fn dealloc(ptr: NonNull<Entry<V>>, cap: usize) {
        unsafe {
            std::alloc::dealloc(ptr.as_ptr().cast(), Self::layout(cap));
        }
    }
}

enum Entry<V> {
    Free,
    Dead,
    Live(EntryInner<V>),
}

struct EntryInner<V> {
    key: RefString,
    val: V,
}

/// Resident set size of this process in KiB (`gc-stats`; 0 off Linux).
#[cfg(feature = "gc-stats")]
fn resident_kib() -> usize {
    #[cfg(target_os = "linux")]
    {
        let statm = std::fs::read_to_string("/proc/self/statm").unwrap_or_default();
        let pages: usize = statm.split_whitespace().nth(1).and_then(|p| p.parse().ok()).unwrap_or(0);
        pages * 4
    }
    #[cfg(not(target_os = "linux"))]
    {
        0
    }
}


#[path = "compact.rs"]
mod compact;
pub use compact::{AddrMap, EvacPlan, Evacuation};

#[path = "nursery.rs"]
mod nursery;

#[path = "inline.rs"]
mod inline;
pub use inline::{InlineObject, InlineShape, ProductSlots, inline_object, product_slots, layout_fingerprint};
pub use nursery::MinorPlan;

#[path = "snapshot.rs"]
mod snapshot;
pub use snapshot::SnapRoot;

#[cfg(test)]
mod tests {
    use super::*;

    /// An instance of a class with no `drop` holding `slots`.
    fn cow_instance(heap: &mut Heap, slots: Vec<Value>) -> Object {
        heap.alloc(ObjInstance::with_slots(5, slots), Object::Instance).0
    }

    fn slot_of(heap: &Heap, obj: u64, i: usize) -> Value {
        let Some(Object::Instance(gc)) = heap.find_object_by_addr(obj) else { panic!("an instance") };
        gc.as_ref().slot(i).expect("a slot")
    }

    #[test]
    fn a_share_marks_an_unpinned_object_and_copies_a_pinned_one() {
        let mut heap = Heap::default();
        let a = cow_instance(&mut heap, vec![Value::from(1i64)]);
        assert_eq!(heap.cow_share(Value::from(a.addr())), CowShare::Marked);
        assert!(a.is_shared());
        let b = cow_instance(&mut heap, vec![Value::from(2i64)]);
        b.set_pinned(true);
        let CowShare::Copied(copy) = heap.cow_share(Value::from(b.addr())) else { panic!("a pinned object is copied") };
        assert_ne!(copy.heap_addr(), b.addr());
        assert!(!b.is_shared(), "a pinned object is never shared");
        let c = heap.find_object_by_addr(copy.heap_addr()).expect("the copy");
        assert!(!c.is_pinned() && !c.is_shared(), "the copy is the new name's own");
        assert_eq!(slot_of(&heap, copy.heap_addr(), 0).as_int(), 2);
        // Not a class instance or an array: left alone.
        let (s, _) = heap.alloc(ObjString::from("x"), Object::String);
        s.set_pinned(true);
        assert_eq!(heap.cow_share(Value::from(s.addr())), CowShare::Untouched);
    }

    #[test]
    fn a_pinned_objects_copy_copies_its_pinned_children_and_shares_the_rest() {
        let mut heap = Heap::default();
        let pinned_child = cow_instance(&mut heap, vec![Value::from(3i64)]);
        pinned_child.set_pinned(true);
        let (plain_child, _) = heap.alloc(ObjArray::new(vec![Value::from(4i64)]), Object::Array);
        let grand = cow_instance(&mut heap, vec![Value::from(9i64)]);
        grand.set_pinned(true);
        let (pinned_arr, _) = heap.alloc(ObjArray::new(vec![Value::from(grand.addr()), Value::from(grand.addr())]), Object::Array);
        pinned_arr.set_pinned(true);
        let root = cow_instance(&mut heap, vec![Value::from(pinned_child.addr()), Value::from(plain_child.addr()), Value::from(pinned_arr.addr())]);
        root.set_pinned(true);
        let CowShare::Copied(copy) = heap.cow_share(Value::from(root.addr())) else { panic!("copied") };
        let copy = copy.heap_addr();
        let c0 = slot_of(&heap, copy, 0).heap_addr();
        assert_ne!(c0, pinned_child.addr(), "a pinned child is copied for the copy");
        assert_eq!(slot_of(&heap, c0, 0).as_int(), 3);
        assert_eq!(slot_of(&heap, copy, 1).heap_addr(), plain_child.addr(), "an unpinned child is held by both");
        assert!(plain_child.is_shared());
        let arr = slot_of(&heap, copy, 2).heap_addr();
        assert_ne!(arr, pinned_arr.addr());
        let Some(Object::Array(a)) = heap.find_object_by_addr(arr) else { panic!("an array") };
        let (g0, g1) = (a.as_ref().get(0).expect("0").heap_addr(), a.as_ref().get(1).expect("1").heap_addr());
        assert_ne!(g0, grand.addr(), "pinned through a pinned array");
        assert_eq!(g0, g1, "an object held twice is copied once");
        for o in [root, pinned_child, pinned_arr, grand] {
            assert!(o.is_pinned() && !o.is_shared(), "the originals stay pinned and unshared");
        }
        // The originals still hold the originals.
        assert_eq!(slot_of(&heap, root.addr(), 0).heap_addr(), pinned_child.addr());
    }

    #[test]
    fn unique_copies_a_shared_objects_pinned_children() {
        let mut heap = Heap::default();
        let pinned_child = cow_instance(&mut heap, vec![Value::from(3i64)]);
        pinned_child.set_pinned(true);
        let plain_child = cow_instance(&mut heap, vec![Value::from(4i64)]);
        let root = cow_instance(&mut heap, vec![Value::from(pinned_child.addr()), Value::from(plain_child.addr())]);
        assert_eq!(heap.cow_unique(Value::from(root.addr())), None, "not shared: written in place");
        assert_eq!(heap.cow_share(Value::from(root.addr())), CowShare::Marked);
        let copy = heap.cow_unique(Value::from(root.addr())).expect("a copy").heap_addr();
        assert_ne!(slot_of(&heap, copy, 0).heap_addr(), pinned_child.addr(), "the copy gets its own pinned child");
        assert!(!pinned_child.is_shared());
        assert_eq!(slot_of(&heap, copy, 1).heap_addr(), plain_child.addr());
        assert!(plain_child.is_shared());
    }

    #[test]
    fn recycling_a_slot_clears_its_pin() {
        let mut heap = Heap::default();
        let a = cow_instance(&mut heap, vec![Value::from(1i64)]);
        a.set_pinned(true);
        let addr = a.addr();
        heap.begin_mark(&[]);
        while !heap.mark_quantum(usize::MAX) {}
        unsafe { heap.sweep() };
        assert!(heap.find_object_by_addr(addr).is_none(), "swept");
        // SAFETY: a swept slot stays mapped.
        let pin = unsafe { *((addr + COW_PINNED_OFFSET as u64) as *const u8) };
        assert_eq!(pin, 0, "the recycled header holds no pin");
    }

    fn live_object_addrs(heap: &Heap) -> std::collections::HashSet<u64> {
        let mut addrs = std::collections::HashSet::new();
        for obj in heap {
            addrs.insert(obj.addr());
        }
        addrs
    }

    #[test]
    fn lane_array_keeps_each_elements_words() {
        let mut a = ObjArray::with_lanes(2, 2, 0);
        for i in 0..5_i64 {
            a.push_lanes(&[Value::from(i), Value::from(i * 10)]);
        }
        assert_eq!((a.len(), a.lanes()), (5, 2));
        assert!(a.set_lanes(1, &[Value::from(7_i64), Value::from(70_i64)]));
        assert!(!a.set_lanes(5, &[Value::from(0_i64), Value::from(0_i64)]));
        a.insert_lanes(0, &[Value::from(-1_i64), Value::from(-10_i64)]);
        let mut out = [Value::from(0_i64); 2];
        a.remove_lanes(2, &mut out);
        assert_eq!(out.map(|v| v.as_int()), [7, 70]);
        assert!(a.pop_lanes(&mut out));
        assert_eq!(out.map(|v| v.as_int()), [4, 40]);
        let all: Vec<i64> = a.lane_words().unwrap().iter().map(|v| v.as_int()).collect();
        assert_eq!(all, [-1, -10, 0, 0, 2, 20, 3, 30]);
        assert_eq!(a.copy().get_lanes(2).unwrap()[1].as_int(), 20);
        assert!(a.words().is_none(), "no one-word view of a lane array");
    }

    #[test]
    #[should_panic(expected = "two-word elements")]
    fn lane_array_refuses_a_one_word_read() {
        let a = ObjArray::from_lanes(vec![Value::from(1_i64), Value::from(2_i64)], 2, 0);
        let _ = a.get(0);
    }

    #[test]
    fn fresh_string_table_is_empty() {
        let table: Table<u32> = Table::new();
        assert!(table.is_empty());
        assert_eq!(table.len(), 0);
    }

    #[test]
    fn enum_gc_marks_payload_pointers() {
        let mut heap = Heap::default();

        // 1. Allocate the inner object (a string).
        let (string_obj, string_ref) = heap.alloc(ObjString::from("inner"), Object::String);
        let string_addr = string_obj.addr();
        let string_member = Value::from(string_obj.addr());

        // 2. Allocate the enum with the string in its payload.
        let enum_value = ObjEnum::new(0, EnumPayload::one(string_member));
        let (enum_obj, _enum_ref) = heap.alloc(enum_value, Object::Enum);
        let enum_addr = enum_obj.addr();

        // 3. Mark the enum as a root and propagate the mark to its
        //    payload (which holds the string pointer).
        let mut gray = Vec::new();
        heap.trace(&[enum_addr]);
        enum_obj.mark_references(&heap, &mut gray);

        // 4. Sweep, anything not marked is deallocated.
        unsafe { heap.sweep() };

        // 5. Both objects must still be alive.
        let live = live_object_addrs(&heap);
        assert!(
            live.contains(&string_addr),
            "string at 0x{:x} was collected despite being reachable from enum payload",
            string_addr
        );
        assert!(
            live.contains(&enum_addr),
            "enum at 0x{:x} was collected despite being a GC root",
            enum_addr
        );
        // Sanity: the string ref is still dereferenceable.
        let _ = string_ref.as_ref();
    }

    #[test]
    fn enum_gc_marks_nested_enum_payloads() {
        let mut heap = Heap::default();

        // Inner enum: empty payload.
        let (inner_obj, _inner_ref) = heap.alloc(
            ObjEnum::new(1, EnumPayload::empty()),
            Object::Enum,
        );
        let inner_addr = inner_obj.addr();

        // Outer enum: payload contains the inner enum as a
        // `Member::Object`.
        let outer = ObjEnum::new(0, EnumPayload::one(Value::from(inner_obj.addr())));
        let (outer_obj, _outer_ref) = heap.alloc(outer, Object::Enum);
        let outer_addr = outer_obj.addr();

        // Mark outer as root, propagate through its payload to mark
        // the inner enum.
        let mut gray = Vec::new();
        heap.trace(&[outer_addr]);
        outer_obj.mark_references(&heap, &mut gray);

        // Drain grey stack (inner enum has empty payload; still exercise the arm).
        while let Some(obj) = gray.pop() {
            obj.mark_references(&heap, &mut gray);
        }

        unsafe { heap.sweep() };

        // Both must survive.
        let live = live_object_addrs(&heap);
        assert!(
            live.contains(&inner_addr),
            "inner enum at 0x{:x} was collected despite being reachable from outer enum payload",
            inner_addr
        );
        assert!(
            live.contains(&outer_addr),
            "outer enum at 0x{:x} was collected despite being a GC root",
            outer_addr
        );
    }

    #[test]
    fn enum_payload_inlines_upto_cap_and_spills_above() {
        assert!(EnumPayload::empty().is_inline());
        assert!(EnumPayload::one(Value::from(1i64)).is_inline());
        assert!(
            EnumPayload::two(
                Value::from(1i64),
                Value::from(2i64),
            )
            .is_inline()
        );
        let four: Vec<Value> = (0..ENUM_INLINE_ARITY as i64).map(Value::from).collect();
        assert!(EnumPayload::from_vec(four).is_inline());
        let spilled =
            EnumPayload::from_vec((0..=ENUM_INLINE_ARITY as i64).map(Value::from).collect());
        assert!(!spilled.is_inline());
        assert_eq!(spilled.len(), ENUM_INLINE_ARITY + 1);
        assert_eq!(spilled[2].as_int(), 2);
    }

    #[test]
    fn instance_slots_inlines_upto_cap_and_spills_above() {
        let one = ObjInstance::with_type_id_and_fields(1, 1);
        assert!(one.slots_are_inline());
        assert_eq!(one.slot_len(), 1);

        let two = ObjInstance::with_type_id_and_fields(1, INSTANCE_INLINE_FIELDS);
        assert!(two.slots_are_inline());
        assert_eq!(two.slot_len(), INSTANCE_INLINE_FIELDS);

        let spilled = ObjInstance::with_type_id_and_fields(1, INSTANCE_INLINE_FIELDS + 1);
        assert!(!spilled.slots_are_inline());
        assert_eq!(spilled.slot_len(), INSTANCE_INLINE_FIELDS + 1);

        let from_vec = ObjInstance::with_slots(
            2,
            vec![Value::from(1i64), Value::from(2i64)],
        );
        assert!(from_vec.slots_are_inline());
        assert_eq!(from_vec.slot(1).map(|v| v.as_int()), Some(2));
    }

    #[test]
    fn instance_gc_marks_inline_slot_pointers() {
        let mut heap = Heap::default();
        let (a, _) = heap.alloc(ObjString::from("a"), Object::String);
        let (b, _) = heap.alloc(ObjString::from("b"), Object::String);
        let inst = ObjInstance::with_slots(
            3,
            vec![Value::from(a.addr()), Value::from(b.addr())],
        );
        assert!(inst.slots_are_inline());
        let (obj, _) = heap.alloc(inst, Object::Instance);

        let mut gray = Vec::new();
        heap.trace(&[obj.addr()]);
        obj.mark_references(&heap, &mut gray);
        while let Some(next) = gray.pop() {
            next.mark_references(&heap, &mut gray);
        }
        unsafe { heap.sweep() };

        let live = live_object_addrs(&heap);
        assert!(live.contains(&a.addr()));
        assert!(live.contains(&b.addr()));
        assert!(live.contains(&obj.addr()));
    }

    /// Field word kinds: pointer fields are traced and reported precise;
    /// a scalar field is skipped even if its bits look like an address.
    #[test]
    fn class_word_kinds_skip_scalars_and_trace_pointers() {
        let mut heap = Heap::default();
        heap.set_class_word_kinds(crate::class_kind_table(&[common::ClassWordKinds {
            type_id: 7,
            kinds: vec![common::WORD_POINTER, common::WORD_SCALAR],
        }]));
        let (kept, _) = heap.alloc(ObjString::from("kept"), Object::String);
        let (lookalike, _) = heap.alloc(ObjString::from("lookalike"), Object::String);
        let inst = ObjInstance::with_slots(
            7,
            vec![Value::from(kept.addr()), Value::from(lookalike.addr())],
        );
        let (obj, _) = heap.alloc(inst, Object::Instance);

        let mut seen = Vec::new();
        obj.for_each_reference(&heap, &mut |a, precise| seen.push((a, precise)));
        assert_eq!(seen, vec![(kept.addr(), true)]);

        let mut gray = Vec::new();
        heap.trace(&[obj.addr()]);
        obj.mark_references(&heap, &mut gray);
        while let Some(next) = gray.pop() {
            next.mark_references(&heap, &mut gray);
        }
        unsafe { heap.sweep() };
        let live = live_object_addrs(&heap);
        assert!(live.contains(&kept.addr()));
        assert!(!live.contains(&lookalike.addr()), "a scalar field is not a root");
    }

    /// Payload / element word kinds from the construction site: a scalar
    /// word is not traced even if its bits look like an address.
    #[test]
    fn payload_and_tuple_kinds_skip_scalars() {
        let mut heap = Heap::default();
        let (kept, _) = heap.alloc(ObjString::from("kept"), Object::String);
        let (lookalike, _) = heap.alloc(ObjString::from("lookalike"), Object::String);
        let kinds = common::pack_word_kinds([common::WORD_POINTER, common::WORD_SCALAR]);
        let words = [Value::from(kept.addr()), Value::from(lookalike.addr())];
        let (en, _) = heap.alloc(
            ObjEnum::new(1, EnumPayload::from_slice(&words).with_kinds(kinds)),
            Object::Enum,
        );
        let (tup, _) = heap.alloc(ObjEnum::tuple(&words, kinds), Object::Enum);

        for obj in [en, tup] {
            let mut seen = Vec::new();
            obj.for_each_reference(&heap, &mut |a, precise| seen.push((a, precise)));
            assert_eq!(seen, vec![(kept.addr(), true)]);
        }

        let mut gray = Vec::new();
        heap.trace(&[en.addr(), tup.addr()]);
        for obj in [en, tup] {
            obj.mark_references(&heap, &mut gray);
        }
        while let Some(next) = gray.pop() {
            next.mark_references(&heap, &mut gray);
        }
        unsafe { heap.sweep() };
        let live = live_object_addrs(&heap);
        assert!(live.contains(&kept.addr()));
        assert!(!live.contains(&lookalike.addr()), "a scalar payload word is not a root");
    }

    #[test]
    fn instance_gc_marks_spilled_slot_pointers() {
        let mut heap = Heap::default();
        let (a, _) = heap.alloc(ObjString::from("a"), Object::String);
        let (b, _) = heap.alloc(ObjString::from("b"), Object::String);
        let (c, _) = heap.alloc(ObjString::from("c"), Object::String);
        let mut words = vec![Value::from(a.addr()), Value::from(b.addr()), Value::from(c.addr())];
        words.resize(INSTANCE_INLINE_FIELDS + 1, Value::from(7i64));
        let inst = ObjInstance::with_slots(3, words);
        assert!(!inst.slots_are_inline());
        let (obj, _) = heap.alloc(inst, Object::Instance);

        let mut gray = Vec::new();
        heap.trace(&[obj.addr()]);
        obj.mark_references(&heap, &mut gray);
        while let Some(next) = gray.pop() {
            next.mark_references(&heap, &mut gray);
        }
        unsafe { heap.sweep() };

        let live = live_object_addrs(&heap);
        assert!(live.contains(&a.addr()));
        assert!(live.contains(&b.addr()));
        assert!(live.contains(&c.addr()));
        assert!(live.contains(&obj.addr()));
    }

    #[test]
    fn enum_gc_marks_spilled_payload_pointers() {
        let mut heap = Heap::default();
        let (a, _) = heap.alloc(ObjString::from("a"), Object::String);
        let (b, _) = heap.alloc(ObjString::from("b"), Object::String);
        let (c, _) = heap.alloc(ObjString::from("c"), Object::String);
        let mut words = vec![Value::from(a.addr()), Value::from(b.addr()), Value::from(c.addr())];
        words.resize(ENUM_INLINE_ARITY + 1, Value::from(9i64));
        let payload = EnumPayload::from_vec(words);
        assert!(!payload.is_inline());
        let (enum_obj, enum_ref) = heap.alloc(ObjEnum::new(0, payload), Object::Enum);
        assert_eq!(enum_ref.as_ref().payload.len(), ENUM_INLINE_ARITY + 1);

        let mut gray = Vec::new();
        heap.trace(&[enum_obj.addr()]);
        enum_obj.mark_references(&heap, &mut gray);
        while let Some(obj) = gray.pop() {
            obj.mark_references(&heap, &mut gray);
        }
        unsafe { heap.sweep() };

        let live = live_object_addrs(&heap);
        assert!(live.contains(&a.addr()));
        assert!(live.contains(&b.addr()));
        assert!(live.contains(&c.addr()));
        assert!(live.contains(&enum_obj.addr()));
    }

    #[test]
    fn find_object_by_addr_hit_and_miss() {
        let mut heap = Heap::default();
        let (obj, _) = heap.alloc(ObjString::from("hi"), Object::String);
        let addr = obj.addr();
        assert!(matches!(
            heap.find_object_by_addr(addr),
            Some(Object::String(_))
        ));
        assert!(heap.find_object_by_addr(addr.wrapping_add(1)).is_none());
    }

    #[test]
    fn mark_value_strips_result_err_low_bit() {
        let mut heap = Heap::default();
        let (obj, _) = heap.alloc(ObjString::from("err"), Object::String);
        let tagged = Value::from(obj.addr() | 1);
        assert!(heap.find_object_by_addr(tagged.bits()).is_none());
        let mut gray = Vec::new();
        heap.mark_value(tagged, &mut gray);
        assert_eq!(gray.len(), 1);
        heap.mark_from_roots(&[tagged.heap_addr()]);
        unsafe { heap.sweep() };
        assert!(heap.find_object_by_addr(obj.addr()).is_some());
    }

    #[test]
    fn borrowed_intern_reuses_existing_string_without_heap_growth() {
        let mut heap = Heap::default();
        let first = heap.intern("literal".to_owned());
        let size = heap.size();
        let second = heap.intern_str("literal");

        assert!(Gc::ptr_eq(first, second));
        assert_eq!(heap.size(), size);
        assert_eq!(
            heap.into_iter()
                .filter(|obj| matches!(obj, Object::String(_)))
                .count(),
            1
        );
    }

    #[test]
    fn intern_ref_registers_an_existing_string_without_copying() {
        let mut heap = Heap::default();
        let (object, string) = heap.alloc(ObjString::from("raw"), Object::String);
        let resolved = heap.intern_ref(string);
        let found = heap.intern_str("raw");

        assert_eq!(object.addr(), resolved.as_ptr() as u64);
        assert!(Gc::ptr_eq(resolved, found));
    }

    #[test]
    fn sweep_reuses_dangling_string_scratch() {
        let mut heap = Heap::default();
        let _ = heap.intern("first".to_owned());
        unsafe { heap.sweep() };
        let capacity = heap.gc_dangling_strings.capacity();
        assert!(capacity >= 1);

        let _ = heap.intern("second".to_owned());
        unsafe { heap.sweep() };
        assert_eq!(heap.gc_dangling_strings.capacity(), capacity);
    }

    /// Byte-threshold GC: under threshold → no collect; after a rooted sweep the
    /// threshold scales with live bytes so a single surviving object does not
    /// trigger on every subsequent alloc.
    #[test]
    fn should_collect_uses_byte_threshold_rescaled_by_sweep() {
        let mut heap = Heap::default();
        assert!(
            !heap.should_collect(),
            "fresh heap must sit under the default threshold"
        );

        let (keep, _) = heap.alloc(ObjString::from("keep"), Object::String);
        let keep_addr = keep.addr();
        // Force a collection, then root `keep` so it survives.
        heap.set_gc_threshold_for_test(0);
        assert!(heap.should_collect());
        heap.trace(&[keep_addr]);
        keep.mark_references(&heap, &mut Vec::new());
        unsafe { heap.sweep() };

        assert!(
            !heap.should_collect(),
            "after sweep, threshold must be max(live*growth, budget) so one survivor is quiet"
        );
        let quiet_size = heap.size();
        // Grow past the rescaled (floored) threshold without roots, should_collect again.
        while !heap.should_collect() {
            let _ = heap.alloc(ObjString::from("pressure"), Object::String);
            // Guard against runaway if rescale broke (would never trip).
            assert!(
                heap.size() <= quiet_size.saturating_mul(8).max(GC_NEXT_THRESHOLD * 2),
                "alloc_bytes grew without tripping should_collect"
            );
        }
        assert!(
            heap.size() > GC_NEXT_THRESHOLD,
            "a tiny live set must not collect below the initial budget"
        );
    }

    /// The next budget grows 4× when most of the heap survived (a growing live
    /// set) and 2× when most of it was garbage, never below the initial budget.
    #[test]
    fn gc_budget_grows_faster_when_most_of_the_heap_survives() {
        let mut heap = Heap::default();
        let mut keep = Vec::new();
        while heap.size() < GC_NEXT_THRESHOLD / 2 {
            keep.push(heap.alloc(ObjString::from("live"), Object::String).0.addr());
        }
        heap.trace(&keep);
        unsafe { heap.sweep() };
        assert_eq!(heap.gc_next_threshold, heap.size() * GC_GROWTH_FACTOR_SURVIVING);

        let live = heap.size();
        while heap.size() < live * 4 {
            let _ = heap.alloc(ObjString::from("garbage"), Object::String);
        }
        heap.trace(&keep);
        unsafe { heap.sweep() };
        assert_eq!(heap.gc_next_threshold, (heap.size() * GC_GROWTH_FACTOR).max(GC_NEXT_THRESHOLD));
    }

    /// Immortal arity-0 enums are seeded as GC roots and must not be swept,
    /// even when nothing else references them.
    #[test]
    fn immortal_unit_enums_survive_sweep_as_roots() {
        let mut heap = Heap::default();
        let immortal = heap.immortal_unit_enum(7);
        let immortal_addr = immortal.addr();
        let again = heap.immortal_unit_enum(7);
        assert_eq!(immortal_addr, again.addr());

        let (junk, _) = heap.alloc(ObjString::from("junk"), Object::String);
        let junk_addr = junk.addr();

        let roots = heap.take_gc_roots();
        assert!(
            roots.contains(&immortal_addr),
            "take_gc_roots must seed immortal enum addresses"
        );
        heap.trace(&roots);
        unsafe { heap.sweep() };
        heap.restore_gc_roots(roots);

        assert!(
            heap.find_object_by_addr(immortal_addr).is_some(),
            "immortal enum must survive an otherwise empty-root sweep"
        );
        assert!(
            heap.find_object_by_addr(junk_addr).is_none(),
            "unrooted junk must still be collected"
        );
        assert_eq!(
            heap.immortal_unit_enum(7).addr(),
            immortal_addr,
            "post-sweep lookup must reuse the same singleton"
        );
    }

    #[test]
    fn empty_tuple_is_one_immortal_object() {
        let mut heap = Heap::default();
        let first = heap.immortal_empty_tuple().addr();
        assert_eq!(first, heap.immortal_empty_tuple().addr());
        let roots = heap.take_gc_roots();
        assert!(roots.contains(&first), "`()` is always a root");
        heap.trace(&roots);
        unsafe { heap.sweep() };
        heap.restore_gc_roots(roots);
        match heap.find_object_by_addr(first) {
            Some(Object::Enum(t)) => assert!(t.as_ref().is_tuple() && t.as_ref().elements().is_empty()),
            _ => panic!("`()` must survive a sweep"),
        }
    }

    #[test]
    fn alloc_enum_value_reuses_unit_singletons() {
        let mut heap = Heap::default();
        let first = heap.alloc_enum_value(3, Vec::new());
        let second = heap.alloc_enum_value(3, Vec::new());

        assert_eq!(first.raw(), second.raw());
        assert_eq!(
            heap.into_iter()
                .filter(|obj| matches!(obj, Object::Enum(_)))
                .count(),
            1
        );
    }

    #[test]
    fn find_object_by_addr_clears_after_sweep() {
        let mut heap = Heap::default();
        let (obj, _) = heap.alloc(ObjString::from("gone"), Object::String);
        let addr = obj.addr();
        assert!(heap.find_object_by_addr(addr).is_some());
        // No roots → sweep poisons the header; the slot stays mapped.
        unsafe { heap.sweep() };
        assert!(
            heap.slot_mapped_for_test(addr),
            "swept slot must stay mapped"
        );
        assert!(
            heap.find_object_by_addr(addr).is_none(),
            "swept object must be poisoned (kind 0), not a HashSet miss"
        );
    }

    /// Many strings, all swept: after the release window their chunks' pages
    /// go back, stale lookups still read a poisoned header, and new
    /// allocations reuse the released chunks instead of mapping more.
    // Release is `madvise`; elsewhere it is a no-op and nothing is released.
    #[cfg(unix)]
    #[test]
    fn idle_chunks_release_and_are_reused() {
        let mut heap = Heap::default();
        let mut addrs = Vec::new();
        for i in 0..20_000 {
            let (obj, _) = heap.alloc(ObjString::from(format!("s{i}").as_str()), Object::String);
            addrs.push(obj.addr());
        }
        let mapped = heap.slab.mapped_bytes();
        assert!(mapped >= 4 * 64 * 1024, "test needs several chunks: {mapped}");
        // Sweep everything, then idle: the first window still saw the
        // allocation phase drain the list, the second finds it idle.
        for _ in 0..2 * super::super::slab::RELEASE_WINDOW_FOR_TEST {
            unsafe { heap.sweep() };
        }
        assert!(heap.slab.released_bytes() > 0, "idle chunks must be released");
        for &a in addrs.iter().step_by(997) {
            assert!(heap.slot_mapped_for_test(a), "released chunk stays mapped");
            assert!(heap.find_object_by_addr(a).is_none(), "released slot reads poisoned");
        }
        for i in 0..20_000 {
            let _ = heap.alloc(ObjString::from(format!("t{i}").as_str()), Object::String);
        }
        assert_eq!(heap.slab.mapped_bytes(), mapped, "reuse released chunks, map nothing new");
        assert_eq!(heap.slab.released_bytes(), 0);
    }

    /// Steady churn drains the free list every cycle: nothing is released.
    #[test]
    fn busy_chunks_are_not_released() {
        let mut heap = Heap::default();
        for _ in 0..(2 * super::super::slab::RELEASE_WINDOW_FOR_TEST) {
            for i in 0..20_000 {
                let _ = heap.alloc(ObjString::from(format!("s{i}").as_str()), Object::String);
            }
            unsafe { heap.sweep() };
        }
        assert_eq!(heap.slab.released_bytes(), 0);
    }

    /// An int-only array is scanned once, then skipped; a reference stored
    /// after that must still keep its target alive.
    #[test]
    fn clean_array_is_skipped_until_written() {
        let mut heap = Heap::default();
        let (arr, gc) = heap.alloc(
            ObjArray::new((0..1000).map(Value::from).collect()),
            Object::Array,
        );
        heap.gc_roots.push(arr.addr());
        let root = |heap: &mut Heap| {
            heap.begin_mark(&[arr.addr()]);
            while !heap.mark_quantum(usize::MAX) {}
            unsafe { heap.sweep() };
        };
        root(&mut heap);
        assert!(!gc.as_ref().may_hold_refs(), "no element looked like a reference");
        let (s, _) = heap.alloc(ObjString::from("kept"), Object::String);
        let mut gc = gc;
        gc.as_mut().set(3, Value::from(s.addr()));
        assert!(gc.as_ref().may_hold_refs(), "a write reopens the scan");
        root(&mut heap);
        assert!(heap.find_object_by_addr(s.addr()).is_some(), "stored reference survives");
        assert!(gc.as_ref().may_hold_refs());
    }

    /// A pointer element kind makes array words precise references; a scalar
    /// stamp is ignored (generic bodies may store boxed values).
    #[test]
    fn pointer_kind_array_words_are_precise() {
        let mut heap = Heap::default();
        let (s, _) = heap.alloc(ObjString::from("x"), Object::String);
        let (arr, mut gc) = heap.alloc(
            ObjArray::new(vec![Value::from(s.addr()), Value::from(0i64)]),
            Object::Array,
        );
        gc.as_mut().set_elem_kind(common::WORD_SCALAR);
        assert_eq!(gc.as_ref().elem_kind(), common::WORD_UNKNOWN);
        gc.as_mut().set_elem_kind(common::WORD_POINTER);
        let mut seen = Vec::new();
        arr.for_each_reference(&heap, &mut |a, precise| seen.push((a, precise)));
        assert_eq!(seen, vec![(s.addr(), true)], "`0` is a hole, not a reference");
        heap.begin_mark(&[arr.addr()]);
        while !heap.mark_quantum(usize::MAX) {}
        unsafe { heap.sweep() };
        assert!(heap.find_object_by_addr(s.addr()).is_some(), "element survives");
    }

    /// Array elements and enum payloads are raw words (ambiguous).
    #[test]
    fn for_each_reference_marks_array_words_ambiguous() {
        let mut heap = Heap::default();
        let (s, _) = heap.alloc(ObjString::from("x"), Object::String);
        let (arr, _) = heap.alloc(
            ObjArray::new(vec![Value::from(s.addr()), Value::from(7i64)]),
            Object::Array,
        );
        let (en, _) = heap.alloc(ObjEnum::new(0, EnumPayload::one(Value::from(s.addr()))), Object::Enum);
        let mut seen = Vec::new();
        arr.for_each_reference(&heap, &mut |a, precise| seen.push((a, precise)));
        assert_eq!(seen, vec![(s.addr(), false)], "immediate 7 is not a reference");
        seen.clear();
        en.for_each_reference(&heap, &mut |a, precise| seen.push((a, precise)));
        assert_eq!(seen, vec![(s.addr(), false)]);
    }

    #[test]
    fn cstr_from_addr_string_hit_and_type_miss() {
        let mut heap = Heap::default();
        let (s_obj, _) = heap.alloc(ObjString::from("coil"), Object::String);
        let (arr_obj, _) = heap.alloc(
            ObjArray::new(vec![Value::from(1i64)]),
            Object::Array,
        );

        let ptr = heap
            .cstr_from_addr(s_obj.addr())
            .expect("string addr must intern")
            .expect("string addr must yield a cstr");
        let got = unsafe { std::ffi::CStr::from_ptr(ptr) }
            .to_str()
            .expect("utf8");
        assert_eq!(got, "coil");
        assert_eq!(heap.ffi_string_live_count(), 1);
        heap.reset_ffi_strings();
        assert_eq!(heap.ffi_string_live_count(), 0);

        assert!(
            heap.cstr_from_addr(arr_obj.addr())
                .expect("type miss is Ok")
                .is_none(),
            "non-string live addr must miss"
        );
        assert!(heap.cstr_from_addr(0).expect("null is Ok").is_none());
    }

    #[test]
    fn cstr_from_addr_rejects_embedded_nul() {
        let mut heap = Heap::default();
        let (obj, _) = heap.alloc(ObjString::from("a\0b"), Object::String);
        assert!(
            heap.cstr_from_addr(obj.addr()).is_err(),
            "embedded NUL cannot become a CString"
        );
        assert_eq!(heap.ffi_string_live_count(), 0);
    }

    #[test]
    fn update_array_elements_writes_and_truncates_excess() {
        let mut heap = Heap::default();
        let (obj, gc) = heap.alloc(
            ObjArray::new(vec![Value::from(0i64), Value::from(0i64)]),
            Object::Array,
        );
        let addr = obj.addr();
        heap.update_array_elements(addr, &[10, 20, 30]);
        assert_eq!(gc.as_ref().get(0).unwrap().as_int(), 10);
        assert_eq!(gc.as_ref().get(1).unwrap().as_int(), 20);
        assert_eq!(gc.as_ref().len(), 2);
    }

    #[test]
    fn update_array_elements_noops_missing_or_wrong_type() {
        let mut heap = Heap::default();
        let (s_obj, _) = heap.alloc(ObjString::from("x"), Object::String);
        heap.update_array_elements(s_obj.addr(), &[1]);
        heap.update_array_elements(0, &[1]);
        assert!(matches!(
            heap.find_object_by_addr(s_obj.addr()),
            Some(Object::String(_))
        ));
    }

    #[test]
    fn gc_scratch_buffers_round_trip_take_restore() {
        let mut heap = Heap::default();
        let mut roots = heap.take_gc_roots();
        roots.push(1);
        roots.push(2);
        heap.restore_gc_roots(roots);

        let (mut gray, mut root_objects) = heap.take_gc_worklists();
        assert!(gray.is_empty());
        assert!(root_objects.is_empty());
        // Capacity may be retained after clear; restore must not panic.
        gray.reserve(4);
        root_objects.reserve(4);
        heap.restore_gc_worklists(gray, root_objects);

        let roots2 = heap.take_gc_roots();
        // Prior contents were cleared on take; buffer is reusable.
        assert!(roots2.is_empty());
        heap.restore_gc_roots(roots2);
    }

    #[test]
    fn repeated_trace_reuses_mark_set_without_leaking_prior_roots() {
        let mut heap = Heap::default();
        let (keep, _) = heap.alloc(ObjString::from("keep"), Object::String);
        let (drop_me, _) = heap.alloc(ObjString::from("drop"), Object::String);
        let keep_addr = keep.addr();
        let drop_addr = drop_me.addr();

        // First collection: only `keep` is a root.
        heap.trace(&[keep_addr]);
        let mut gray = Vec::new();
        keep.mark_references(&heap, &mut gray);
        unsafe { heap.sweep() };

        let live = live_object_addrs(&heap);
        assert!(live.contains(&keep_addr));
        assert!(!live.contains(&drop_addr));

        // Second collection with empty roots must not resurrect drop_me via a
        // stale mark-set entry from the previous trace.
        let (orphan, _) = heap.alloc(ObjString::from("orphan"), Object::String);
        let orphan_addr = orphan.addr();
        heap.trace(&[]);
        unsafe { heap.sweep() };
        let live = live_object_addrs(&heap);
        assert!(
            !live.contains(&orphan_addr),
            "empty-root trace must not keep prior mark-set addresses alive"
        );
        // `keep` was unmarked after sweep and not re-rooted, also gone.
        assert!(!live.contains(&keep_addr));
    }

    fn weak_to(heap: &mut Heap, target: Object) -> Object {
        let weak = ObjWeak { target: Cell::new(Value::from(target.addr())), cleared: Cell::new(false) };
        heap.alloc(weak, Object::Weak).0
    }

    /// A cycle visits the weak list, not the heap: cleared and swept weaks
    /// leave it, live ones stay (#1870).
    #[test]
    fn weak_list_drops_cleared_and_swept_weaks() {
        let mut heap = Heap::default();
        let (keep, _) = heap.alloc(ObjString::from("keep"), Object::String);
        let (dies, _) = heap.alloc(ObjString::from("dies"), Object::String);
        let live_weak = weak_to(&mut heap, keep);
        let dead_target = weak_to(&mut heap, dies);
        let unreached = weak_to(&mut heap, keep);
        assert_eq!(heap.weaks.len(), 3);
        heap.collect(&[keep.addr(), live_weak.addr(), dead_target.addr()]);
        let Some(Object::Weak(gc)) = heap.find_object_by_addr(dead_target.addr()) else {
            panic!("rooted weak swept");
        };
        assert!(gc.as_ref().cleared.get());
        // The unreached weak was still listed when its cycle cleared weaks.
        assert!(heap.find_object_by_addr(unreached.addr()).is_none());
        heap.collect(&[keep.addr(), live_weak.addr()]);
        assert_eq!(heap.weaks, vec![live_weak.addr()]);
    }

    /// One `drop` type does not stop early freeing of other types' objects;
    /// an object of a `drop` type, or one reaching it, is left to the
    /// collector, which runs the `drop`.
    #[test]
    fn early_free_is_per_drop_type() {
        let mut heap = Heap::default();
        heap.register_finalizer_type(7);
        let keep = |_: u64| false;
        let (plain, _) = heap.alloc(ObjInstance::with_type_id_and_fields(8, 1), Object::Instance);
        assert_eq!(heap.free_early(Value::from(plain.addr()), false, false, &keep), 1);
        assert!(heap.find_object_by_addr(plain.addr()).is_none());

        let (tagged, tagged_gc) = heap.alloc(ObjEnum::new(1, EnumPayload::one(Value::from(1i64))), Object::Enum);
        tagged_gc.payload_mut().type_id = 9;
        assert_eq!(heap.free_early(Value::from(tagged.addr()), true, false, &keep), 1);

        let (dropped, _) = heap.alloc(ObjInstance::with_type_id_and_fields(7, 1), Object::Instance);
        assert_eq!(heap.free_early(Value::from(dropped.addr()), true, false, &keep), 0);
        assert!(heap.find_object_by_addr(dropped.addr()).is_some());

        // A parent reaching a `drop` object keeps both: the `drop` runs
        // later and may read what the walk would free.
        let (child, child_gc) = heap.alloc(ObjEnum::new(0, EnumPayload::one(Value::from(2i64))), Object::Enum);
        child_gc.payload_mut().type_id = 7;
        heap.note_enum_type(child, 7);
        let mut kinds: Vec<Box<[u8]>> = vec![Box::new([]); common::type_kinds_index(10) + 1];
        kinds[common::type_kinds_index(10)] = Box::new([common::WORD_POINTER]);
        heap.set_class_word_kinds(std::sync::Arc::new(kinds));
        let (parent, _) = heap.alloc(ObjInstance::with_slots(10, vec![Value::from(child.addr())]), Object::Instance);
        let mut seen = Vec::new();
        parent.for_each_reference(&heap, &mut |a, precise| seen.push((a, precise)));
        assert_eq!(seen, vec![(child.addr(), true)]);
        assert_eq!(heap.free_early(Value::from(parent.addr()), true, false, &keep), 0);
        for obj in [parent, child] {
            assert!(heap.find_object_by_addr(obj.addr()).is_some());
        }
    }

    /// A `drop` that may run at an early free is listed parent first; once
    /// it has run (the object is finalized) the free gives the object back.
    #[test]
    fn early_drops_lists_pending_drops_in_walk_order() {
        let mut heap = Heap::default();
        heap.register_finalizer_type(7);
        let keep = |_: u64| false;
        let (late, _) = heap.alloc(ObjInstance::with_type_id_and_fields(7, 1), Object::Instance);
        assert!(heap.early_drops(Value::from(late.addr()), false, &keep).is_none(), "not an early drop type");
        heap.register_early_drop_type(7);
        let drops = heap.early_drops(Value::from(late.addr()), false, &keep).expect("listed");
        assert_eq!(drops.iter().map(|o| o.addr()).collect::<Vec<_>>(), vec![late.addr()]);
        assert_eq!(heap.free_early(Value::from(late.addr()), false, false, &keep), 0, "its drop has not run");
        late.claim_finalizer();
        assert!(heap.early_drops(Value::from(late.addr()), false, &keep).is_some_and(|d| d.is_empty()));
        assert_eq!(heap.free_early(Value::from(late.addr()), false, false, &keep), 1);
    }

    /// Objects of a `drop` type are listed from their allocation (or from
    /// the registration, for ones made before it) until they are finalized.
    #[test]
    fn finalizable_list_tracks_drop_types() {
        let mut heap = Heap::default();
        let (early, _) = heap.alloc(ObjInstance::with_type_id_and_fields(7, 1), Object::Instance);
        let (_other, _) = heap.alloc(ObjInstance::with_type_id_and_fields(8, 1), Object::Instance);
        heap.register_finalizer_type(7);
        let (late, _) = heap.alloc(ObjInstance::with_type_id_and_fields(7, 1), Object::Instance);
        let (tagged, tagged_gc) = heap.alloc(ObjEnum::new(1, EnumPayload::one(Value::from(1i64))), Object::Enum);
        tagged_gc.payload_mut().type_id = 7;
        heap.note_enum_type(tagged, 7);
        heap.begin_mark(&[late.addr()]);
        while !heap.mark_quantum(usize::MAX) {}
        let mut got: Vec<u64> = heap.unmarked_finalizable().iter().map(|o| o.addr()).collect();
        got.sort_unstable();
        let mut want = vec![early.addr(), tagged.addr()];
        want.sort_unstable();
        assert_eq!(got, want);
        early.claim_finalizer();
        let left: Vec<u64> = heap.unmarked_finalizable().iter().map(|o| o.addr()).collect();
        assert_eq!(left, vec![tagged.addr()]);
        assert_eq!(heap.finalizable.len(), 2, "finalized entries leave the list");
    }

    #[test]
    fn incremental_mark_seeds_via_lookup_not_list_scan() {
        let mut heap = Heap::default();
        let (keep, _) = heap.alloc(ObjString::from("keep"), Object::String);
        let (drop_me, _) = heap.alloc(ObjString::from("drop"), Object::String);
        heap.begin_mark(&[keep.addr()]);
        assert_eq!(heap.gc_phase(), GcPhase::Marking);
        assert!(keep.is_marked());
        assert!(!drop_me.is_marked());
        while !heap.mark_quantum(1) {}
        heap.clear_dead_weaks();
        heap.begin_sweep();
        while !heap.sweep_quantum(1) {}
        assert_eq!(heap.gc_phase(), GcPhase::Idle);
        assert!(heap.find_object_by_addr(keep.addr()).is_some());
        assert!(heap.find_object_by_addr(drop_me.addr()).is_none());
    }

    #[test]
    fn alloc_during_mark_is_black() {
        let mut heap = Heap::default();
        let (root, _) = heap.alloc(ObjString::from("root"), Object::String);
        heap.begin_mark(&[root.addr()]);
        let (baby, baby_gc) = heap.alloc(ObjString::from("baby"), Object::String);
        assert!(baby_gc.is_marked());
        while !heap.mark_quantum(64) {}
        heap.clear_dead_weaks();
        heap.begin_sweep();
        heap.finish_sweep();
        assert!(heap.find_object_by_addr(baby.addr()).is_some());
        assert!(heap.find_object_by_addr(root.addr()).is_some());
    }

    #[test]
    fn object_allocated_behind_the_sweep_is_traced_next_cycles() {
        let mut heap = Heap::default();
        // Garbage in `P`'s size class, swept first so its slot is reused.
        let (garbage, _) = heap.alloc(ObjInstance::with_slots(1, vec![Value::from(0i64)]), Object::Instance);
        heap.mark_from_roots(&[]);
        heap.begin_sweep();
        while heap.find_object_by_addr(garbage.addr()).is_some() {
            assert!(!heap.sweep_quantum(1), "sweep ended before freeing the garbage");
        }
        // Allocated mid-sweep into the freed slot, behind the cursor.
        let (parent, mut parent_gc) =
            heap.alloc(ObjInstance::with_slots(1, vec![Value::from(0i64)]), Object::Instance);
        assert_eq!(parent.addr(), garbage.addr());
        heap.finish_sweep();

        let (child, _) = heap.alloc(ObjString::from("child"), Object::String);
        parent_gc.as_mut().set_slot(0, Value::from(child.addr()));
        for cycle in 0..3 {
            heap.mark_from_roots(&[parent.addr()]);
            heap.begin_sweep();
            heap.finish_sweep();
            assert!(
                heap.find_object_by_addr(child.addr()).is_some(),
                "cycle {cycle}: child of a once-fresh object was freed"
            );
        }
    }

    #[test]
    fn lazy_sweep_reclaims_across_quanta() {
        let mut heap = Heap::default();
        let (keep, _) = heap.alloc(ObjString::from("keep"), Object::String);
        let mut dead = Vec::new();
        for i in 0..8 {
            let (o, _) = heap.alloc(ObjString::from(format!("d{i}").as_str()), Object::String);
            dead.push(o.addr());
        }
        heap.mark_from_roots(&[keep.addr()]);
        heap.begin_sweep();
        assert_eq!(heap.gc_phase(), GcPhase::Sweeping);
        let mut steps = 0;
        while !heap.sweep_quantum(1) {
            steps += 1;
            assert!(steps < 64);
        }
        assert!(steps >= 1, "sweep must take more than one quantum");
        assert_eq!(heap.gc_phase(), GcPhase::Idle);
        assert!(heap.find_object_by_addr(keep.addr()).is_some());
        for addr in dead {
            assert!(heap.find_object_by_addr(addr).is_none());
        }
    }

    /// Shared-heap workers intern and allocate concurrently inside a steal
    /// epoch; the intern table and slab must stay consistent.
    #[test]
    fn epoch_workers_intern_and_alloc_concurrently() {
        struct HeapPtr(*mut Heap);
        unsafe impl Send for HeapPtr {}
        unsafe impl Sync for HeapPtr {}
        let lock = Mutex::new(());
        let mut heap = Heap::default();
        heap.enter_epoch_stw(&lock);
        let shared = HeapPtr(&mut heap);
        std::thread::scope(|scope| {
            for t in 0..4 {
                let shared = &shared;
                scope.spawn(move || {
                    let heap = unsafe { &mut *shared.0 };
                    for i in 0..2000 {
                        let s = heap.intern_str(&format!("k{}", (i * 7 + t) % 500));
                        assert!(heap.find_object_by_addr(s.as_ptr() as u64).is_some());
                        let (o, _) = heap.alloc(ObjString::from("x"), Object::String);
                        assert!(heap.find_object_by_addr(o.addr()).is_some());
                    }
                    // A worker hands its batch back before it publishes.
                    heap.flush_epoch_batch();
                });
            }
        });
        heap.exit_epoch_stw();
        // 500 interned keys plus 4 × 2000 batch allocations, all counted.
        assert_eq!(heap.live_object_count(), 500 + 4 * 2000);
        for i in 0..500 {
            let key = format!("k{i}");
            let a = heap.intern_str(&key);
            let b = heap.intern_str(&key);
            assert!(Gc::ptr_eq(a, b), "{key} interned twice");
        }
    }

    /// `alloc_string` skips the intern table; `intern_ref` later resolves it
    /// to the interned copy, and its lazy hash matches the eager one.
    #[test]
    fn alloc_string_is_not_interned_until_used_as_key() {
        let mut heap = Heap::default();
        let lit = heap.intern("same".to_owned());
        let runtime = heap.alloc_string("same".to_owned());
        assert!(!Gc::ptr_eq(lit, runtime));
        assert_eq!(runtime.as_ref().hash_code(), ObjString::hash("same"));
        assert!(Gc::ptr_eq(heap.intern_ref(runtime), lit));

        let fresh = heap.alloc_string("only-runtime".to_owned());
        assert!(heap.strings.find("only-runtime", ObjString::hash("only-runtime")).is_none());
        assert!(Gc::ptr_eq(heap.intern_ref(fresh), fresh));
    }

    /// Inside a steal epoch the normal threshold does not trigger a collect
    /// (which would abort the epoch); only the epoch ceiling does.
    #[test]
    #[cfg(not(feature = "gc-stress"))]
    fn epoch_defers_collect_until_ceiling() {
        let lock = Mutex::new(());
        let mut heap = Heap::default();
        heap.counts.alloc_bytes = GC_NEXT_THRESHOLD + 1;
        assert!(heap.should_collect());
        heap.enter_epoch_stw(&lock);
        assert!(!heap.should_collect(), "past threshold, under epoch ceiling");
        heap.counts.alloc_bytes = heap.epoch_gc_ceiling + 1;
        assert!(heap.should_collect(), "past the epoch ceiling");
        heap.exit_epoch_stw();
        heap.counts.alloc_bytes = GC_NEXT_THRESHOLD + 1;
        assert!(heap.should_collect(), "normal threshold again after the epoch");
        heap.counts.alloc_bytes = 0;
    }

    /// A thread that waits mid-epoch on heap A and helps with a job in heap
    /// B's epoch keeps the two batches apart (#766, #779): B's objects land
    /// in B, and A's batch comes back intact for A's flush.
    #[test]
    fn stashed_epoch_batch_keeps_heaps_apart() {
        let (lock_a, lock_b) = (Mutex::new(()), Mutex::new(()));
        let (mut a, mut b) = (Heap::default(), Heap::default());
        a.enter_epoch_stw(&lock_a);
        let in_a = a.alloc(ObjString::from("a"), Object::String).0.addr();
        {
            let _stash = stash_epoch_batch();
            b.enter_epoch_stw(&lock_b);
            let in_b = b.alloc(ObjString::from("b"), Object::String).0.addr();
            assert!(b.find_object_by_addr(in_b).is_some());
            assert!(a.find_object_by_addr(in_b).is_none());
            b.flush_epoch_batch();
            b.exit_epoch_stw();
        }
        let again = a.alloc(ObjString::from("a2"), Object::String).0.addr();
        assert!(a.find_object_by_addr(in_a).is_some() && a.find_object_by_addr(again).is_some());
        a.flush_epoch_batch();
        a.exit_epoch_stw();
        assert_eq!(a.live_object_count(), 2);
        assert_eq!(b.live_object_count(), 1);
    }

    /// An epoch batch hands out the same slots a plain alloc would, and its
    /// unused slots go back to the free list on flush.
    #[test]
    fn epoch_batch_matches_plain_alloc_and_returns_leftovers() {
        let mut plain = Heap::default();
        let plain_addrs: Vec<u64> = (0..3)
            .map(|_| plain.alloc(ObjString::from("p"), Object::String).0.addr())
            .collect();
        let next_plain = plain.alloc(ObjString::from("p"), Object::String).0.addr();

        let lock = Mutex::new(());
        let mut heap = Heap::default();
        heap.enter_epoch_stw(&lock);
        let addrs: Vec<u64> = (0..3)
            .map(|_| heap.alloc(ObjString::from("p"), Object::String).0.addr())
            .collect();
        heap.flush_epoch_batch();
        heap.exit_epoch_stw();
        assert_eq!(heap.live_object_count(), 3);
        // Same carve, so the same offsets inside each heap's first chunk.
        let rel = |v: &[u64]| v.iter().map(|a| a - v[0]).collect::<Vec<_>>();
        assert_eq!(rel(&addrs), rel(&plain_addrs));
        let next = heap.alloc(ObjString::from("p"), Object::String).0.addr();
        assert_eq!(next - addrs[0], next_plain - plain_addrs[0]);
        assert_eq!(heap.live_object_count(), 4);
    }

    /// The shared-heap write guard (#1892) lets a job write what it
    /// allocated in its epoch, and lets an unchecked scope (a help-stolen
    /// job) write anything.
    #[cfg(any(debug_assertions, feature = "gc-stress"))]
    #[test]
    fn epoch_write_guard_allows_epoch_objects() {
        let lock = Mutex::new(());
        let mut heap = Heap::default();
        let (_, mut old) = heap.alloc(ObjArray::new(vec![Value::from(0_i64)]), Object::Array);
        heap.enter_epoch_stw(&lock);
        {
            let _scope = EpochWriteScope::enter(&heap, true);
            let (_, mut fresh) = heap.alloc(ObjArray::with_capacity(4), Object::Array);
            fresh.as_mut().push(Value::from(1_i64));
            heap.account_array(fresh.as_mut());
            {
                let _help = EpochWriteScope::unchecked();
                old.as_mut().set(0, Value::from(2_i64));
            }
        }
        heap.flush_epoch_batch();
        heap.exit_epoch_stw();
        // Outside any job the guard is off.
        old.as_mut().set(0, Value::from(3_i64));
        assert_eq!(old.as_ref().get(0), Some(Value::from(3_i64)));
    }

    /// A shared-heap job that writes an object from before its epoch (a
    /// wrong purity proof) is caught.
    #[cfg(any(debug_assertions, feature = "gc-stress"))]
    #[test]
    #[should_panic(expected = "shared-heap epoch guard")]
    fn epoch_write_guard_rejects_pre_epoch_object() {
        let lock = Mutex::new(());
        let mut heap = Heap::default();
        let (_, mut old) = heap.alloc(ObjArray::new(vec![Value::from(0_i64)]), Object::Array);
        heap.enter_epoch_stw(&lock);
        let _scope = EpochWriteScope::enter(&heap, true);
        old.as_mut().set(0, Value::from(1_i64));
    }

    /// Allocs between sweep quanta prepend to `head`; freeing the old dead
    /// head must not unlink them (leaked bytes tripped the Drop assert).
    #[test]
    fn alloc_during_sweep_stays_linked() {
        let mut heap = Heap::default();
        let (keep, _) = heap.alloc(ObjString::from("keep"), Object::String);
        let (_dead, _) = heap.alloc(ObjString::from("dead"), Object::String);
        heap.mark_from_roots(&[keep.addr()]);
        heap.begin_sweep();
        let (baby, _) = heap.alloc(ObjString::from("baby"), Object::String);
        while !heap.sweep_quantum(1) {}
        assert!(heap.find_object_by_addr(baby.addr()).is_some());
        assert!((&heap).into_iter().any(|o| o.addr() == baby.addr()));
        assert!((&heap).into_iter().any(|o| o.addr() == keep.addr()));
        assert_eq!((&heap).into_iter().count(), 2);
    }

    fn words(vals: &[i64]) -> Vec<Value> {
        vals.iter().map(|&v| Value::from(v)).collect()
    }

    fn ints(a: &ObjArray) -> Vec<i64> {
        a.iter().map(|v| v.as_int()).collect()
    }

    #[test]
    fn byte_stamp_packs_and_reads_back_the_same_words() {
        let mut a = ObjArray::new(words(&[0, 1, 200, 255]));
        a.set_elem_kind(common::WORD_BYTE);
        assert!(a.is_packed());
        assert_eq!(a.bytes(), Some(&[0u8, 1, 200, 255][..]));
        assert_eq!(ints(&a), vec![0, 1, 200, 255]);
        assert_eq!(a.read_indexed(2, false).map(|v| v.as_int()), Some(200));
        assert_eq!(a.read_indexed(4, false), None);
        assert_eq!(a.read_indexed(-1, false), None);
        assert!(!a.may_hold_refs(), "packed bytes never hold references");
        // The stamp is not kept as an element kind.
        assert_eq!(a.elem_kind(), common::WORD_UNKNOWN);
    }

    #[test]
    fn byte_stamp_keeps_words_when_an_element_does_not_fit() {
        let mut a = ObjArray::new(words(&[1, 256]));
        a.set_elem_kind(common::WORD_BYTE);
        assert!(!a.is_packed());
        assert_eq!(ints(&a), vec![1, 256]);
    }

    #[test]
    fn packed_stores_that_fit_stay_bytes() {
        let mut a = ObjArray::from_bytes(vec![0; 3]);
        a.set(0, Value::from(7i64));
        assert!(a.store_indexed(1, Value::from(true), false));
        assert!(!a.store_indexed(3, Value::from(1i64), false));
        unsafe { a.set_unchecked(2, Value::from(255i64)) };
        a.push(Value::from(9i64));
        a.insert(0, Value::from(4i64));
        assert!(a.is_packed());
        assert_eq!(ints(&a), vec![4, 7, 1, 255, 9]);
        assert_eq!(a.remove(0).as_int(), 4);
        assert_eq!(a.pop().map(|v| v.as_int()), Some(9));
        assert_eq!(ints(&a), vec![7, 1, 255]);
        assert!(a.get(1).unwrap().as_bool());
        assert!(!a.may_hold_refs());
    }

    #[test]
    fn packed_store_that_does_not_fit_unpacks_losslessly() {
        // A shared generic body may store a boxed word (a heap address):
        // the array falls back to words instead of truncating it.
        for store in 0..4 {
            let mut a = ObjArray::from_bytes(vec![1, 2, 3]);
            let big = Value::from(0x1_0000i64);
            match store {
                0 => a.set(1, big),
                1 => assert!(a.store_indexed(1, big, false)),
                2 => unsafe { a.set_unchecked(1, big) },
                _ => {
                    a.remove(1);
                    a.insert(1, big);
                }
            }
            assert!(!a.is_packed(), "store {store}");
            assert!(a.may_hold_refs(), "store {store}: the next mark rescans");
            assert_eq!(ints(&a), vec![1, 0x1_0000, 3], "store {store}");
        }
        let mut a = ObjArray::from_bytes(vec![1]);
        a.push(Value::from(-1i64));
        assert!(!a.is_packed());
        assert_eq!(ints(&a), vec![1, -1]);
    }

    #[test]
    fn packed_storage_accounts_one_byte_per_element() {
        let w = ObjArray::new(Vec::with_capacity(64));
        let b = ObjArray::from_bytes(Vec::with_capacity(64));
        assert_eq!(w.storage_bytes(), 64 * mem::size_of::<Value>());
        assert_eq!(b.storage_bytes(), 64);
        assert_eq!(b.capacity(), 64);
        assert!(b.size() < w.size());
    }

    #[test]
    fn storage_changes_are_counted_once_through_take_resize() {
        let mut a = ObjArray::new(words(&[1, 2]));
        assert_eq!(a.take_resize(), None);
        let before = a.size();
        a.set_elem_kind(common::WORD_BYTE);
        // Not counted until the heap is told: size stays what was counted.
        assert_eq!(a.size(), before);
        assert_eq!(a.take_resize(), Some((16, 2)));
        assert_eq!(a.take_resize(), None);
        a.set(0, Value::from(1000i64));
        assert_eq!(a.take_resize(), Some((2, 16)));
    }

    #[test]
    fn byte_copies_work_on_both_storages() {
        for packed in [false, true] {
            let mut a = if packed {
                ObjArray::from_bytes(vec![0; 4])
            } else {
                ObjArray::new(words(&[0, 0, 0, 0]))
            };
            a.write_bytes_at(1, &[7, 8, 9, 10]);
            assert_eq!(ints(&a), vec![0, 7, 8, 9], "packed {packed}");
            assert_eq!(a.to_byte_vec(), vec![0, 7, 8, 9], "packed {packed}");
            assert_eq!(a.is_packed(), packed);
        }
        let mut heap = Heap::default();
        let (obj, gc) = heap.alloc(ObjArray::from_bytes(vec![0; 2]), Object::Array);
        heap.update_array_bytes(obj.addr(), &[3, 4, 5]);
        assert_eq!(gc.as_ref().bytes(), Some(&[3u8, 4][..]));
    }

    #[test]
    fn packed_copy_stays_packed() {
        let a = ObjArray::from_bytes(vec![5, 6]);
        let c = a.copy();
        assert!(c.is_packed());
        assert_eq!(ints(&c), vec![5, 6]);
        assert_eq!(a.to_vec(), words(&[5, 6]));
        assert_eq!(a.slice_to_vec(1, 2), words(&[6]));
    }

    #[test]
    fn copy_keeps_the_pointer_kind() {
        let mut a = ObjArray::new(words(&[0]));
        a.set_elem_kind(common::WORD_POINTER);
        assert_eq!(a.copy().elem_kind(), common::WORD_POINTER);
        assert_eq!(ObjArray::new(words(&[0])).copy().elem_kind(), common::WORD_UNKNOWN);
    }

    #[test]
    fn packed_array_survives_collection() {
        let mut heap = Heap::default();
        let (arr, gc) = heap.alloc(ObjArray::from_bytes(vec![1, 2, 3]), Object::Array);
        heap.gc_roots.push(arr.addr());
        heap.begin_mark(&[arr.addr()]);
        while !heap.mark_quantum(usize::MAX) {}
        unsafe { heap.sweep() };
        assert!(gc.as_ref().is_packed());
        assert_eq!(ints(gc.as_ref()), vec![1, 2, 3]);
    }
}
