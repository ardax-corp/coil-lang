//! The heap: slab-allocated objects with reference counts, string
//! interning, and the cycle collector's state. Objects do not move.

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
/// Unmarked objects considered at one alloc safepoint during lazy sweep.
pub const GC_SWEEP_QUANTUM: usize = 128;

/// Slots one steal-epoch mutator takes per size class under the epoch lock.
const EPOCH_BATCH: usize = 64;

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
    /// Count changes made in the epoch, for [`Heap::epoch_rc`]: an object's
    /// address, with bit 0 set for a decrement (reference-count plan step 8).
    rc: Vec<u64>,
}

impl EpochBatch {
    fn is_empty(&self) -> bool {
        self.classes.iter().all(|(_, v)| v.is_empty())
            && self.bytes == 0
            && self.objects == 0
            && self.weaks.is_empty()
            && self.finalizable.is_empty()
            && self.rc.is_empty()
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

// Shared-heap epoch write guard (debug / rc-check only, #1892): see
// `docs/internals/shared-heap-guard.md`.
#[cfg(any(debug_assertions, feature = "rc-check"))]
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
/// `debug_assertions` or the `rc-check` feature is on.
#[must_use]
pub struct EpochWriteScope {
    #[cfg(any(debug_assertions, feature = "rc-check"))]
    prev: u8,
}

impl EpochWriteScope {
    /// Enter a job on `heap` (the heap the job's runtime is bound to): a
    /// shared-heap job of `heap`'s open epoch when `shared`, otherwise a job
    /// that may write anything ([`Self::unchecked`]).
    #[cfg(any(debug_assertions, feature = "rc-check"))]
    #[inline]
    pub fn enter(heap: &Heap, shared: bool) -> Self {
        Self::set(if shared { heap.epoch_gen } else { 0 })
    }

    /// Lift the guard until dropped: a join that help-steals runs the stolen
    /// job (and the reset of its helper runtime's own heap) on this thread.
    #[cfg(any(debug_assertions, feature = "rc-check"))]
    #[inline]
    pub fn unchecked() -> Self {
        Self::set(0)
    }

    #[cfg(any(debug_assertions, feature = "rc-check"))]
    fn set(generation: u8) -> Self {
        Self {
            prev: EPOCH_WRITE_GEN.with(|g| g.replace(generation)),
        }
    }

    /// Release build: no guard.
    #[cfg(not(any(debug_assertions, feature = "rc-check")))]
    #[inline(always)]
    pub fn enter(_heap: &Heap, _shared: bool) -> Self {
        Self {}
    }

    /// Release build: no guard.
    #[cfg(not(any(debug_assertions, feature = "rc-check")))]
    #[inline(always)]
    pub fn unchecked() -> Self {
        Self {}
    }
}

#[cfg(any(debug_assertions, feature = "rc-check"))]
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
#[cfg(any(debug_assertions, feature = "rc-check"))]
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
    strings: Table<()>,
    slab: Slab,
    /// Per class `type_id`: field word kinds from static types
    /// (`common::WORD_*`). Missing rows / fields are unknown.
    class_kinds: std::sync::Arc<Vec<Box<[u8]>>>,
    /// Per row of [`Self::class_kinds`]: how many leading slots are numbers
    /// (`WORD_SCALAR` / `WORD_BYTE`). An instance with no more slots than
    /// that names no object ([`Self::rc_leaf`]).
    class_leaf: Vec<u32>,
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
    /// What the last [`Self::free_early`] gave back.
    early_freed: Vec<u64>,
    /// Scratch of a deep [`Heap::free_early`] walk (found, mark work,
    /// children), kept so a free does not allocate.
    early_scratch: (Vec<Object>, Vec<Object>, Vec<u64>),
    /// [`Self::free_early`] freed something: a dead frame word or a dead
    /// object may now point at a freed or reused slot, so a typed handle
    /// is looked up before it is marked ([`Self::early_freed_slot`]).
    freed_early: bool,
    /// Reference counts are kept (always, but in tests that turn them off,
    /// [`Self::set_rc_counting`]).
    rc_on: bool,
    /// A count that reaches zero frees its object ([`Self::set_rc_frees`]).
    rc_frees: bool,
    /// Objects a host-side diff ([`Self::rc_after`]) took to count zero:
    /// released at the next `RcDec`, once the code has counted what the
    /// host handed back (a popped element). A collection drops the list
    /// (the sweep frees what is still unowned; a minor moves objects).
    pub(crate) rc_pending: std::cell::RefCell<Vec<u64>>,
    /// [`Heap::rc_release`]'s worklist, kept so a release allocates nothing.
    pub(crate) rc_stack: Vec<Object>,
    /// Objects whose count reached zero with their `drop` still owed
    /// (reference-count plan step 7): roots until the runtime runs it
    /// ([`Heap::take_rc_due`]) and releases them.
    pub(crate) rc_due: Vec<u64>,
    /// What the program's statics still hold as it ends: owned for the
    /// program's life, so the leak report leaves them out
    /// ([`Heap::rc_leaks_exempt`]).
    pub(super) leak_exempt: std::collections::HashSet<u64>,
    /// The count changes of the open steal epoch's mutators, folded in from
    /// their batches ([`Heap::rc_log`]).
    epoch_rc: Vec<u64>,
    /// The class type ids whose objects can close a reference cycle, a
    /// bitset ([`Heap::register_cyclic_type`]): the cycle collector's starts.
    pub(crate) cyclic_types: Vec<u64>,
    /// Allocate movable kinds in the nursery ([`Self::set_nursery`]).
    nursery: bool,
    /// CString arena for the current FFI invoke (reset after each call).
    ffi_strings: Vec<std::ffi::CString>,
    /// Layer A steal epoch: no collect; `alloc` takes [`Self::alloc_lock`].
    epoch_stw: bool,
    /// Non-null while a C1 steal epoch is live (points at the epoch mutex).
    alloc_lock: *const Mutex<()>,
    /// `alloc_bytes` past which a steal epoch aborts (see
    /// [`EPOCH_GC_HEADROOM_FACTOR`]); only meaningful while `epoch_stw`.
    epoch_gc_ceiling: usize,
    /// Debug / rc-check: generation of the current (or last) steal epoch,
    /// never 0 once an epoch opened; stamped on objects allocated in it
    /// ([`EpochWriteScope`]).
    #[cfg(any(debug_assertions, feature = "rc-check"))]
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
            strings: Table::default(),
            slab: Slab::new(),
            class_kinds: std::sync::Arc::default(),
            class_leaf: Vec::new(),
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
            early_freed: Vec::new(),
            early_scratch: (Vec::new(), Vec::new(), Vec::new()),
            freed_early: false,
            rc_on: true,
            rc_frees: true,
            rc_pending: std::cell::RefCell::new(Vec::new()),
            rc_stack: Vec::new(),
            rc_due: Vec::new(),
            leak_exempt: std::collections::HashSet::new(),
            epoch_rc: Vec::new(),
            cyclic_types: Vec::new(),
            nursery: false,
            ffi_strings: Vec::new(),
            epoch_stw: false,
            epoch_gc_ceiling: 0,
            alloc_lock: ptr::null(),
            #[cfg(any(debug_assertions, feature = "rc-check"))]
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
        #[cfg(any(debug_assertions, feature = "rc-check"))]
        {
            // Skip 0, which marks objects allocated outside any epoch.
            self.epoch_gen = self.epoch_gen.wrapping_add(1).max(1);
        }
    }

    pub fn exit_epoch_stw(&mut self) {
        self.epoch_stw = false;
        self.sync_inline_gate();
        self.alloc_lock = ptr::null();
        self.apply_epoch_rc();
    }

    /// Allocates an object and returns its handle. The object is pushed to the
    /// front of the list of allocated objects.
    pub fn alloc<T: GcSized, F>(&mut self, data: T, map: F) -> (Object, Gc<T>)
    where
        F: Fn(Gc<T>) -> Object,
    {
        let out = if unlikely(!self.alloc_lock.is_null()) { self.alloc_from_epoch_batch(data, map) } else { self.alloc_unlocked(data, map) };
        // Its first words gain their owner (reference-count plan step 2).
        if T::HOLDS_REFS && unlikely(self.rc_counting()) {
            self.rc_inc_slots(out.0);
        }
        out
    }

    /// Steal-epoch alloc: write into this thread's batch, taking the epoch
    /// lock only to refill it (see [`EpochBatch`]).
    #[inline(never)]
    fn alloc_from_epoch_batch<T: GcSized, F>(&mut self, data: T, map: F) -> (Object, Gc<T>)
    where
        F: Fn(Gc<T>) -> Object,
    {
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
        #[cfg(any(debug_assertions, feature = "rc-check"))]
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
        });
        (object, content)
    }

    /// Add a batch's pending byte / object counts to the heap (lock held).
    fn fold_epoch_counts(&mut self, batch: &mut EpochBatch) {
        self.counts.alloc_bytes += std::mem::take(&mut batch.bytes);
        self.counts.live_count += std::mem::take(&mut batch.objects);
        self.weaks.append(&mut batch.weaks);
        self.finalizable.append(&mut batch.finalizable);
        self.epoch_rc.append(&mut batch.rc);
    }

    /// A count change while a steal epoch is open (reference-count plan
    /// step 8): mutators share the heap and its objects, so no header count
    /// is written. This thread's batch logs the change; the epoch's end
    /// sums the logs into the counts ([`Self::apply_epoch_rc`]).
    pub(crate) fn rc_log(&self, o: Object, dec: bool) {
        let me: *const Heap = self;
        EPOCH_BATCH_TLS.with(|cell| {
            let mut batch = cell.borrow_mut();
            debug_assert!(batch.heap.is_null() || batch.heap == me, "epoch batch left over from another heap");
            batch.heap = me;
            batch.rc.push(o.addr() | u64::from(dec));
        });
    }

    /// Sum the epoch's count changes into the counts, once it ended, and
    /// release what reached zero.
    fn apply_epoch_rc(&mut self) {
        if self.epoch_rc.is_empty() {
            return;
        }
        let mut delta: HashMap<u64, i64> = HashMap::new();
        for w in std::mem::take(&mut self.epoch_rc) {
            *delta.entry(w & !1).or_default() += if w & 1 == 0 { 1 } else { -1 };
        }
        let mut zero = Vec::new();
        for (addr, d) in delta {
            // SAFETY: nothing is freed while an epoch is open: a logged
            // address still names its object.
            let Some(o) = (unsafe { Object::from_header(addr) }) else { continue };
            let c = o.rc_count();
            if c == COUNT_IMMORTAL {
                continue;
            }
            let n = (i64::from(c) + d).clamp(0, i64::from(COUNT_IMMORTAL - 1)) as u32;
            o.rc_set_count(n);
            // At zero with changes logged: its last owner went in the
            // epoch, also when it was born there and its owners came and
            // went (a net change of nothing from a count of zero).
            if n == 0 && d <= 0 {
                zero.push(addr);
            }
        }
        if self.rc_frees() {
            self.rc_pending.get_mut().extend(zero);
            self.rc_release_pending();
        }
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
        let young = T::NURSERY
            && may_young
            && self.nursery
            && !self.epoch_stw
            && (self.finalizer_types.is_empty()
                || data.finalizer_type_id().is_none_or(|t| !self.has_finalizer(t)));
        let slot = if young { self.slab.alloc_young(layout) } else { self.slab.alloc(layout) };
        let (object, content) = Self::init_slot(slot.cast::<GcData<T>>(), data, map);
        let size = object.size();
        self.counts.alloc_bytes += size;
        self.counts.live_count += 1;
        if young {
            content.header.generation.set(GEN_YOUNG);
            return (object, content);
        }
        match self.side_list_of(object) {
            Some(SideList::Weak) => self.weaks.push(object.addr()),
            Some(SideList::Finalizable) => self.finalizable.push(object.addr()),
            None => {}
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
        let open = !cfg!(feature = "rc-check")
            && self.nursery
            && !self.epoch_stw;
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
            Some(obj @ (Object::Instance(_) | Object::Array(_))) if obj.is_pinned() => self.cow_share_pinned(obj),
            Some(obj @ (Object::Instance(_) | Object::Array(_))) => {
                obj.set_shared();
                CowShare::Marked
            }
            _ => CowShare::Untouched,
        }
    }

    /// Whether a write through `obj` must copy it first: a value share
    /// marked it, and, with counts kept, an owner besides the writer's is
    /// still there (reference-count plan step 5: the mark is no longer
    /// sticky once the other names are gone). The mark stays the test of
    /// whether a share happened at all: a name that writes in place by
    /// design (a `mut` capture, a view) counts an owner but shares nothing.
    #[inline]
    fn cow_is_shared(&self, obj: Object) -> bool {
        obj.is_shared() && (!self.rc_counts_live() || obj.rc_count() > 1)
    }

    /// A copy's child now has a second owner: marked shared.
    #[inline]
    fn cow_share_child(&self, child: Object) {
        child.set_shared();
    }

    /// [`Self::cow_share`] of a pinned object: a copy of it.
    #[cold]
    #[inline(never)]
    fn cow_share_pinned(&mut self, obj: Object) -> CowShare {
        match self.cow_copy(obj, &mut Vec::new()) {
            Some(copy) => CowShare::Copied(copy),
            // An identity class is never copied: marked as before.
            None => {
                obj.set_shared();
                CowShare::Marked
            }
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
        if !self.cow_is_shared(obj) {
            return None;
        }
        let copy = match obj {
            Object::Instance(gc) => {
                let inst = gc.as_ref();
                if inst.type_id == 0 || self.is_identity_type(inst.type_id) {
                    return None;
                }
                let slots = inst.storage.as_slice().to_vec();
                let own = inst.own_kinds();
                self.alloc(
                    ObjInstance::with_slots(inst.type_id, slots).with_own_kinds(own),
                    Object::Instance,
                )
                .0
            }
            Object::Array(gc) => {
                let copy = gc.as_ref().copy();
                self.alloc(copy, Object::Array).0
            }
            _ => return None,
        };
        let mut pinned = false;
        copy.for_each_reference(self, &mut |addr, _| {
            if let Some(child @ (Object::Instance(_) | Object::Array(_))) = self.find_object_by_addr(addr) {
                if child.is_pinned() {
                    pinned = true;
                } else {
                    self.cow_share_child(child);
                }
            }
        });
        if pinned {
            self.cow_copy_pinned_children(obj, copy, &mut Vec::new());
        }
        self.cow_copies += 1;
        Some(Value::from(copy.addr()))
    }

    /// A shallow copy of `obj` (a class instance or an array), whose
    /// children both now name: each is marked shared, or, pinned, copied
    /// in turn for the copy (deep through pinned objects; `done` pairs what
    /// this copy made already with its copy, so a child held twice is
    /// copied once: written only below a pinned child, so a plain copy
    /// allocates nothing more than it did).
    /// `None` for an identity class or another kind of object. Allocates
    /// and never collects, as the `Cow` helpers do.
    fn cow_copy(&mut self, obj: Object, done: &mut Vec<(u64, u64)>) -> Option<Value> {
        let copy = match obj {
            Object::Instance(gc) => {
                let inst = gc.as_ref();
                if inst.type_id == 0 || self.is_identity_type(inst.type_id) {
                    return None;
                }
                let slots = inst.storage.as_slice().to_vec();
                let own = inst.own_kinds();
                self.alloc(
                    ObjInstance::with_slots(inst.type_id, slots).with_own_kinds(own),
                    Object::Instance,
                )
                .0
            }
            Object::Array(gc) => {
                let copy = gc.as_ref().copy();
                self.alloc(copy, Object::Array).0
            }
            _ => return None,
        };
        let mut pinned = false;
        copy.for_each_reference(self, &mut |addr, _| {
            if let Some(child @ (Object::Instance(_) | Object::Array(_))) = self.find_object_by_addr(addr) {
                if child.is_pinned() {
                    pinned = true;
                } else {
                    self.cow_share_child(child);
                }
            }
        });
        if pinned {
            self.cow_copy_pinned_children(obj, copy, done);
        }
        self.cow_copies += 1;
        Some(Value::from(copy.addr()))
    }

    /// [`Self::cow_copy`] below a pinned child: each pinned child of `copy`
    /// (a fresh copy of `obj`) is copied for it in turn.
    #[cold]
    #[inline(never)]
    fn cow_copy_pinned_children(&mut self, obj: Object, copy: Object, done: &mut Vec<(u64, u64)>) {
        let mut pinned = Vec::new();
        copy.for_each_reference(self, &mut |addr, _| {
            if let Some(child @ (Object::Instance(_) | Object::Array(_))) = self.find_object_by_addr(addr)
                && child.is_pinned()
            {
                pinned.push(child);
            }
        });
        done.push((obj.addr(), copy.addr()));
        for child in pinned {
            let to = match done.iter().find(|&&(from, _)| from == child.addr()) {
                Some(&(_, to)) => Some(Value::from(to)),
                None => {
                    let to = self.cow_copy(child, done);
                    if let Some(to) = to {
                        done.push((child.addr(), to.heap_addr()));
                    }
                    to
                }
            };
            if let Some(to) = to {
                let before = self.rc_before(Value::from(copy.addr()));
                copy.replace_reference(child.addr(), to);
                self.rc_after(before);
            }
        }
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
        // A young string's slot goes back to its bump chunk's free list;
        // the table keeps an old copy instead.
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

    /// Free the object `v` points at before a collection would, because
    /// the compiler proved nothing else reaches it (`feature/escape-memory`
    /// step 2). `deep` also frees every object it reaches through precise
    /// pointer words. `keep` names objects the runtime still holds itself (frame
    /// pins); those and anything this cannot free safely stay for the GC:
    /// interned strings, the shared unit variants and `()`, objects whose
    /// `drop` has not run (the runtime runs it first when [`Self::early_drops`]
    /// lists it), handles, and everything while a cycle or a steal epoch is
    /// open. `check` frees nothing and checks the free instead
    /// ([`Self::check_early_free`]). Returns how many objects it freed.
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
                self.check_early_free(&[root]);
                return 0;
            }
            self.freed_early = true;
            self.early_freed.push(root.addr());
            unsafe { self.dealloc(root) };
            return 1;
        }
        if !check && self.finalizer_types.is_empty() {
            return self.free_deep_unchecked(v, keep);
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
            self.check_early_free(&found);
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

    /// A deep [`Self::free_early`] with no `drop` type registered and no
    /// check (#2881): each object is freed as soon as its precise children
    /// are listed, so the walk needs no mark bits and no second pass. A DAG
    /// child named twice is freed once: its slot is poisoned (kind 0) after
    /// the first, and the second visit finds no object there.
    fn free_deep_unchecked(&mut self, v: Value, keep: &dyn Fn(u64) -> bool) -> usize {
        let Some(root) = self.early_root(v, keep) else {
            return 0;
        };
        self.freed_early = true;
        let (found, work, mut children) = std::mem::take(&mut self.early_scratch);
        let mut stack = work;
        stack.clear();
        stack.push(root);
        let mut n = 0;
        while let Some(obj) = stack.pop() {
            children.clear();
            obj.precise_children(self, &mut children);
            // SAFETY: nothing else reaches `obj` (the compiler's proof);
            // its children were read above.
            unsafe { self.dealloc(obj) };
            n += 1;
            for &addr in &children {
                // SAFETY: a precise word of a live object names a slot.
                let Some(child) = (unsafe { Object::from_header(addr) }) else {
                    continue;
                };
                if self.early_freeable(child) && !keep(addr) {
                    // Poisoned at once, so a second name of it in this walk
                    // finds nothing.
                    child.header().kind.set(0);
                    stack.push(child);
                }
            }
        }
        children.clear();
        self.early_scratch = (found, stack, children);
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
            obj.precise_children(self, &mut children);
            for addr in children.drain(..) {
                // SAFETY: a precise pointer word of a live object names a
                // live object (`precise_children` keeps slab addresses only).
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
        if self.epoch_stw || !self.alloc_lock.is_null() {
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

    /// `--early-free check`: abort when an early free of `found` (its
    /// root first) would free an object something outside it still owns
    /// ([`Self::early_free_violation`]).
    fn check_early_free(&self, found: &[Object]) {
        if let Some(why) = self.early_free_violation(found) {
            eprintln!("early free check: {why}");
            std::process::abort();
        }
    }

    /// The root of `found` may be owned once, by the frame that frees it;
    /// every other object only by the pointer words of the objects freed
    /// with it. A count above that is an owner the free would leave
    /// naming a freed slot.
    pub(crate) fn early_free_violation(&self, found: &[Object]) -> Option<String> {
        let mut inside: HashMap<u64, u32> = HashMap::new();
        let mut children = Vec::new();
        for obj in found {
            children.clear();
            obj.precise_children(self, &mut children);
            for &a in &children {
                *inside.entry(a).or_insert(0) += 1;
            }
        }
        found.iter().enumerate().find_map(|(i, obj)| {
            let count = obj.rc_count();
            let owned = inside.get(&obj.addr()).copied().unwrap_or(0) + u32::from(i == 0);
            (count != COUNT_IMMORTAL && count > owned).then(|| {
                format!(
                    "{} at {:#x} was freed while something else owns it (count {count}, {owned} owners freed with it)",
                    rc::kind_name(*obj),
                    obj.addr()
                )
            })
        })
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
        heap.rc_on = self.rc_on;
        heap.rc_frees = self.rc_frees;
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

    /// Whether an allocation's safepoint has work: `drop`s counts left due,
    /// the cycle collector on allocation pressure, or a heap over its cap
    /// ([`crate::collect::safepoint`]).
    #[inline]
    pub fn safepoint_due(&self) -> bool {
        !self.rc_due.is_empty() || self.cycles_due() || self.over_byte_cap()
    }

    /// The heap grew past its threshold since the last cycle collection
    /// (in a steal epoch, past the epoch's ceiling): run the cycle
    /// collector.
    #[inline]
    pub fn cycles_due(&self) -> bool {
        let limit = if self.epoch_stw {
            self.epoch_gc_ceiling
        } else {
            self.gc_next_threshold
        };
        cfg!(feature = "rc-check") || self.counts.alloc_bytes > limit
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

    /// Lower the byte threshold so the next [`Self::cycles_due`]
    /// check fires (test helper).
    #[cfg(test)]
    pub fn set_gc_threshold_for_test(&mut self, bytes: usize) {
        self.gc_next_threshold = bytes;
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
            self.slab.free_young(ptr);
        } else {
            self.slab.free(ptr);
        }
    }

    /// Clear the [`Object::Weak`] handles that name `addr`, an object whose
    /// last strong reference just went (reference-count plan step 7).
    pub fn clear_weaks_to(&self, addr: u64) {
        for &w in &self.weaks {
            if let Some(Object::Weak(gc)) = self.find_object_by_addr(w) {
                let weak = gc.as_ref();
                if !weak.cleared.get() && weak.target.get().heap_addr() == addr {
                    weak.cleared.set(true);
                    weak.target.set(Value::from(0i64));
                }
            }
        }
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
        let pfn = ObjFn { entry, arity, is_rest, filled_mask: 0, captured_args: Vec::new(), captures: Vec::new(), dicts: None, word_kinds: 0 };
        // Old: a root for the runtime's lifetime, never worth a nursery copy.
        let (object, _) = self.alloc_unlocked_in(pfn, Object::Fn, false);
        object.rc_set_immortal();
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
        object.rc_set_immortal();
        self.immortal_enums.insert(tag, object);
        self.unit_enum = Some((tag, object));
        object
    }

    /// Install the program's per-class field word kinds.
    pub fn set_class_word_kinds(&mut self, table: std::sync::Arc<Vec<Box<[u8]>>>) {
        self.class_leaf = table
            .iter()
            .map(|row| row.iter().take_while(|&&k| matches!(k, common::WORD_SCALAR | common::WORD_BYTE)).count() as u32)
            .collect();
        self.class_kinds = table;
    }

    /// True when an instance of `type_id` with `n` slots holds numbers only.
    #[inline]
    pub fn class_leaf(&self, type_id: u32, n: usize) -> bool {
        self.class_leaf.get(common::type_kinds_index(type_id)).is_some_and(|&p| n <= p as usize)
    }

    /// True when `type_id`'s row has a tagged slot ([`common::WORD_TAGGED`]).
    #[inline]
    pub fn class_has_tagged(&self, type_id: u32) -> bool {
        self.class_field_kinds(type_id).contains(&common::WORD_TAGGED)
    }

    /// Word kinds of `type_id`'s typed fields (empty = all unknown).
    #[inline]
    pub fn class_field_kinds(&self, type_id: u32) -> &[u8] {
        self.class_kinds
            .get(common::type_kinds_index(type_id))
            .map_or(&[][..], |row| &row[..])
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

}

impl Drop for Heap {
    fn drop(&mut self) {
        // Drop payloads in place; the slab (and its free lists) goes away
        // next, so slots are not returned and nothing is collected first.
        if rc::rc_leaks_shown() {
            self.report_leaks();
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
                let mut kinds = common::ClassSlotKinds::new($heap.class_field_kinds(inst.type_id), inst.storage.as_slice().len())
                    .with_own(inst.storage.kinds());
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
                if !a.holds_no_refs() {
                    let (k, mask, kind) = (a.lanes(), a.tag_mask, a.gc_elem_kind());
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
                let kinds = func.word_kinds;
                for (i, v) in func.captures.$iter().chain(func.captured_args.$iter()).enumerate() {
                    f.word(v, common::packed_word_kind(kinds, i));
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
        if !header.flag(FLAG_MARKED) {
            header.set_flag(FLAG_MARKED, true);
            grey_objects.push(*self);
        }
    }

    /// Unmark the object.
    #[inline]
    pub fn unmark(&self) {
        self.header().set_flag(FLAG_MARKED, false);
    }

    #[must_use]
    #[inline]
    pub fn is_marked(&self) -> bool {
        self.header().flag(FLAG_MARKED)
    }

    /// Call `f` on each slot of `self` that may hold a reference
    /// ([`walk_slots`]).
    #[inline]
    pub(crate) fn visit_slots(&self, heap: &Heap, f: &mut impl SlotVisitor) {
        walk_slots!(self, heap, f, as_ref, as_slice, gc_words, iter)
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
    /// The addresses of the objects this one names through precise
    /// pointer words, for an early free's walk (#2881): a precise word
    /// always names a live object, so unlike [`Self::for_each_reference`]
    /// no word is looked up in the slab, only range-checked.
    pub(crate) fn precise_children(&self, heap: &Heap, out: &mut Vec<u64>) {
        struct Precise<'a> {
            lo: u64,
            hi: u64,
            out: &'a mut Vec<u64>,
        }
        impl SlotVisitor for Precise<'_> {
            #[inline(always)]
            fn word(&mut self, &v: &Value, kind: u8) {
                if kind == common::WORD_POINTER {
                    let addr = v.heap_addr();
                    if addr >= self.lo && addr < self.hi {
                        self.out.push(addr);
                    }
                }
            }

            #[inline(always)]
            fn member(&mut self, m: &Member) {
                if let Member::Object(o) = m {
                    self.out.push(o.addr());
                }
            }
        }
        let (lo, hi) = heap.slab.bounds();
        self.visit_slots(heap, &mut Precise { lo, hi, out });
    }

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

    /// Copy-on-write: whether a second place may name this object.
    #[inline]
    pub fn is_shared(&self) -> bool {
        // SAFETY: every kind shares the `repr(C)` header at the slot start.
        unsafe { &*(self.addr() as *const GcHeader) }.flag(FLAG_SHARED)
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
        unsafe { &*(self.addr() as *const GcHeader) }.flag(FLAG_FINALIZED)
    }

    /// Claim this object's `drop`: `false` when it was claimed already.
    #[inline]
    pub fn claim_finalizer(&self) -> bool {
        // SAFETY: as in `is_finalized`.
        !unsafe { &*(self.addr() as *const GcHeader) }.replace_flag(FLAG_FINALIZED, true)
    }

    #[inline]
    pub(crate) fn set_shared(&self) {
        // SAFETY: every kind shares the `repr(C)` header at the slot start.
        unsafe { &*(self.addr() as *const GcHeader) }.set_flag(FLAG_SHARED, true);
    }

    /// Whether a `gc::weak` handle names this object ([`FLAG_WEAK_TARGET`]).
    #[inline]
    pub fn is_weak_target(&self) -> bool {
        // SAFETY: every kind shares the `repr(C)` header at the slot start.
        unsafe { &*(self.addr() as *const GcHeader) }.flag(FLAG_WEAK_TARGET)
    }

    /// Note that a `gc::weak` handle names this object ([`Self::is_weak_target`]).
    #[inline]
    pub fn set_weak_target(&self) {
        // SAFETY: as in `is_weak_target`.
        unsafe { &*(self.addr() as *const GcHeader) }.set_flag(FLAG_WEAK_TARGET, true);
    }

    /// Copy-on-write: whether a name writes this object in place without a
    /// test while it is pinned, so a share hands out a copy of it instead
    /// of marking it ([`Heap::cow_share`]).
    #[inline]
    pub fn is_pinned(&self) -> bool {
        // SAFETY: every kind shares the `repr(C)` header at the slot start.
        unsafe { &*(self.addr() as *const GcHeader) }.flag(FLAG_PINNED)
    }

    /// Set or clear the pin ([`Self::is_pinned`]): [`FLAG_PINNED`] at
    /// [`HEADER_FLAGS_OFFSET`], which native code's `CowPin` / `CowUnpin`
    /// write the same way.
    #[inline]
    pub fn set_pinned(&self, on: bool) {
        // SAFETY: every kind shares the `repr(C)` header at the slot start.
        unsafe { &*(self.addr() as *const GcHeader) }.set_flag(FLAG_PINNED, on);
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
    /// The fields' words. Their kinds come from the class row; the store's
    /// own packed kinds are those of the slots the row leaves unknown
    /// (generic fields), from the type arguments where the instance was
    /// made (`common::ClassSlotKinds::with_own`).
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

    /// Packed kinds of the slots the class row leaves unknown, by slot.
    #[inline]
    pub fn own_kinds(&self) -> common::PackedKinds {
        self.storage.kinds()
    }

    /// `self` with own slot kinds `k` ([`Self::own_kinds`]).
    #[must_use]
    pub fn with_own_kinds(self, k: common::PackedKinds) -> Self {
        Self { storage: self.storage.with_kinds(k), type_id: self.type_id }
    }

    /// Add own slot kinds `k` to those recorded (`vec_stamp`).
    #[inline]
    pub fn add_own_kinds(&mut self, k: common::PackedKinds) {
        self.storage.add_kinds(k);
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
        kinds: common::PackedKinds,
        slots: [Value; ENUM_INLINE_ARITY],
    },
    Spill {
        words: Vec<Value>,
        kinds: common::PackedKinds,
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
    pub fn kinds(&self) -> common::PackedKinds {
        match &self.inner {
            EnumPayloadInner::Inline { kinds, .. } | EnumPayloadInner::Spill { kinds, .. } => {
                *kinds
            }
        }
    }

    /// Record the construction site's word kinds.
    #[inline]
    pub fn with_kinds(mut self, k: common::PackedKinds) -> Self {
        match &mut self.inner {
            EnumPayloadInner::Inline { kinds, .. } | EnumPayloadInner::Spill { kinds, .. } => {
                *kinds = k;
            }
        }
        self
    }

    /// Add word kinds `k` (packed, as [`Self::with_kinds`]) to those the
    /// construction site recorded: a shared body's, for a type parameter's
    /// words (`vec_stamp`).
    #[inline]
    pub fn add_kinds(&mut self, k: common::PackedKinds) {
        match &mut self.inner {
            EnumPayloadInner::Inline { kinds, .. } | EnumPayloadInner::Spill { kinds, .. } => {
                *kinds |= k;
            }
        }
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
    pub fn tuple(elements: &[Value], kinds: common::PackedKinds) -> Self {
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
    /// One word per element.
    Words(Vec<Value>),
    Bytes(Vec<u8>),
    /// `k` words per element, element `i` at `words[i * k..][..k]`: a
    /// `Vec` of a two-word type (`Option<int>`, `(int, float)`) keeps
    /// both words in place (`MakeArrayK` stamps it from the element
    /// layout). One-word reads and writes refuse it: only the lane API
    /// below and code that knows the stride touch it.
    /// `k` is [`ObjArray::stride`].
    Lanes { words: Vec<Value> },
}

/// `coil_native_view`'s state for a view (`0`: no view): loads and
/// stores go in place.
pub const VIEW_PLAIN: u64 = 1;

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

#[cfg(target_pointer_width = "64")]
const _: () = assert!(mem::size_of::<GcData<ObjArray>>() <= 64);

impl ObjArray {
    /// Conservative: any element may be a reference.
    pub fn new(elements: Vec<Value>) -> Self {
        let may_hold_refs = !elements.is_empty();
        Self {
            accounted: elements.capacity() * mem::size_of::<Value>(),
            data: ArrayData::Words(elements),
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
            data: ArrayData::Words(elements),
            may_hold_refs,
            elem_kind: common::WORD_UNKNOWN,
            stride: 1,
            tag_mask: 0,
        }
    }

    /// `self` stamped with element kind `kind` ([`Self::set_elem_kind`]),
    /// for arrays the runtime makes of words it knows.
    pub fn of_kind(mut self, kind: u8) -> Self {
        self.set_elem_kind(kind);
        self
    }

    pub fn with_capacity(n: usize) -> Self {
        Self {
            accounted: n * mem::size_of::<Value>(),
            data: ArrayData::Words(Vec::with_capacity(n)),
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
        self.data = ArrayData::Words(boxes);
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
            ArrayData::Words(w) => (w, 1, self.elem_kind),
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
    pub(crate) fn lane_pointer(words: &[Value], k: usize, mask: u64, i: usize) -> bool {
        let lane = i % k;
        lane == 0 && words.get(i + 1).is_some_and(|t| t.bits() < 64 && mask >> t.bits() & 1 == 1)
    }

    /// Element word kind (`common::WORD_*`).
    #[inline]
    pub fn elem_kind(&self) -> u8 {
        self.elem_kind
    }

    /// The element kind a walk reads the words by: a pointer or scalar
    /// stamp, else unknown. A scalar stamp names no object: its words are
    /// not walked (a wrong one could only keep objects alive, never free
    /// one early).
    #[inline]
    pub fn gc_elem_kind(&self) -> u8 {
        match self.elem_kind {
            common::WORD_POINTER | common::WORD_SCALAR => self.elem_kind,
            _ => common::WORD_UNKNOWN,
        }
    }

    /// Whether no element word names an object by the array's stamp or
    /// storage: nothing written that may, bytes, or one-word elements
    /// stamped scalar.
    #[inline]
    pub fn holds_no_refs(&self) -> bool {
        !self.may_hold_refs || matches!(self.data, ArrayData::Bytes(_)) || (self.lanes() == 1 && self.elem_kind == common::WORD_SCALAR)
    }

    /// Stamp the static element kind. A pointer or scalar kind is kept
    /// (the collector reads a scalar one as unknown, [`Self::gc_elem_kind`]);
    /// a byte kind packs the elements when they all fit.
    #[inline]
    pub fn set_elem_kind(&mut self, kind: u8) {
        if kind == common::WORD_POINTER || kind == common::WORD_SCALAR {
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
        let ArrayData::Words(words) = &self.data else {
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
            self.data = ArrayData::Words(words);
        }
    }

    /// Word storage, unpacking first when needed.
    #[inline]
    fn words_vec(&mut self) -> &mut Vec<Value> {
        if let ArrayData::Bytes(_) = self.data {
            self.unpack();
        }
        match &mut self.data {
            ArrayData::Words(w) => w,
            ArrayData::Bytes(_) => unreachable!(),
            ArrayData::Lanes { .. } => lane_misuse(),
        }
    }

    /// The elements as words, when not packed.
    #[inline]
    pub fn words(&self) -> Option<&[Value]> {
        match &self.data {
            ArrayData::Words(w) => Some(w),
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
            ArrayData::Words(w) => ArrayIter::Words(w.iter().copied()),
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
            ArrayData::Words(w) => Self::new(w.clone()),
            ArrayData::Bytes(b) => Self::from_bytes(b.clone()),
            ArrayData::Lanes { words } => Self::from_lanes(words.clone(), *k, self.tag_mask),
        };
        copy.elem_kind = self.elem_kind;
        copy
    }

    /// Copy of the elements as words.
    pub fn to_vec(&self) -> Vec<Value> {
        match &self.data {
            ArrayData::Words(w) => w.clone(),
            ArrayData::Bytes(b) => b.iter().map(|b| word_of(*b)).collect(),
            ArrayData::Lanes { .. } => lane_misuse(),
        }
    }

    /// Copy of elements `start..end` as words (caller checks bounds).
    pub fn slice_to_vec(&self, start: usize, end: usize) -> Vec<Value> {
        match &self.data {
            ArrayData::Words(w) => w[start..end].to_vec(),
            ArrayData::Bytes(b) => b[start..end].iter().map(|b| word_of(*b)).collect(),
            ArrayData::Lanes { .. } => lane_misuse(),
        }
    }

    /// Element `i` as a word.
    #[inline]
    pub fn get(&self, i: usize) -> Option<Value> {
        match &self.data {
            ArrayData::Words(w) => w.get(i).copied(),
            ArrayData::Bytes(b) => b.get(i).map(|b| word_of(*b)),
            ArrayData::Lanes { .. } => lane_misuse(),
        }
    }

    /// `elements[index]` with the runtime's bounds rules: `unchecked` is a proven
    /// index; otherwise out of range returns `None`.
    #[inline(always)]
    pub fn read_indexed(&self, index: i64, unchecked: bool) -> Option<Value> {
        match &self.data {
            ArrayData::Words(w) => read_indexed_in(w, index, unchecked),
            ArrayData::Bytes(b) => read_indexed_in(b, index, unchecked).map(word_of),
            ArrayData::Lanes { .. } => lane_misuse(),
        }
    }

    #[inline]
    pub fn len(&self) -> usize {
        match &self.data {
            ArrayData::Words(w) => w.len(),
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
            ArrayData::Words(w) => w.capacity(),
            ArrayData::Bytes(b) => b.capacity(),
            ArrayData::Lanes { words } => words.capacity() / usize::from(self.stride),
        }
    }

    /// Bytes of element storage (capacity × element width), for heap
    /// accounting.
    #[inline]
    pub fn storage_bytes(&self) -> usize {
        match &self.data {
            ArrayData::Words(w) => w.capacity() * mem::size_of::<Value>(),
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
            ArrayData::Words(w) => Some((w.as_ptr().cast_mut(), w.len())),
            ArrayData::Bytes(_) | ArrayData::Lanes { .. } => None,
        }
    }

    /// The words in place for native code's stores, marked as a runtime
    /// store would.
    #[inline]
    pub(crate) fn words_view_for_store(&mut self) -> Option<(*mut Value, usize)> {
        self.note();
        match &mut self.data {
            ArrayData::Words(w) => Some((w.as_mut_ptr(), w.len())),
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
        if let ArrayData::Words(w) = &mut self.data {
            if !self.may_hold_refs {
                self.may_hold_refs = true;
            }
            w.push(v);
        } else {
            self.push_packed(v);
        }
    }

    /// [`Self::push`] onto byte storage.
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
            ArrayData::Words(w) => w.len(),
            ArrayData::Bytes(_) => 0,
            ArrayData::Lanes { words } => words.len(),
        }
    }

    /// Word `i` of the elements' storage, lanes included ([`Self::word_count`]).
    pub(crate) fn word_at(&self, i: usize) -> Option<Value> {
        match &self.data {
            ArrayData::Words(w) => w.get(i).copied(),
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
            ArrayData::Words(w) => {
                if !self.may_hold_refs {
                    self.may_hold_refs = true;
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
            ArrayData::Words(w) => {
                if !self.may_hold_refs {
                    self.may_hold_refs = true;
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
            ArrayData::Words(elements) => {
                unsafe {
                    *elements.get_unchecked_mut(idx) = value;
                }
                if !self.may_hold_refs {
                    self.may_hold_refs = true;
                }
            }
            ArrayData::Bytes(b) => match byte_of(value) {
                Some(byte) => unsafe { *b.get_unchecked_mut(idx) = byte },
                None => {
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
            ArrayData::Words(w) => w.pop(),
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
        self.note();
        self.words_vec().insert(i, v);
    }

    /// Remove element `i` (caller checks `i < len`).
    pub fn remove(&mut self, i: usize) -> Value {
        match &mut self.data {
            ArrayData::Words(w) => w.remove(i),
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
        match &mut self.data {
            ArrayData::Words(w) => drop(w.drain(from..to)),
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
        match &mut self.data {
            ArrayData::Words(w) => w[i..].rotate_right(1),
            ArrayData::Bytes(b) => b[i..].rotate_right(1),
            ArrayData::Lanes { words } => words[i * *k..].rotate_right(*k),
        }
    }

    pub fn clear(&mut self) {
        match &mut self.data {
            ArrayData::Words(w) | ArrayData::Lanes { words: w, .. } => w.clear(),
            ArrayData::Bytes(b) => b.clear(),
        }
    }

    /// Append elements `lo..hi` of `src` (caller checks bounds; `src` is
    /// another array). Packed into packed is one copy.
    pub fn extend_from(&mut self, src: &Self, lo: usize, hi: usize) {
        match (&mut self.data, &src.data) {
            (ArrayData::Bytes(out), ArrayData::Bytes(inp)) => out.extend_from_slice(&inp[lo..hi]),
            (ArrayData::Words(out), ArrayData::Words(inp)) => {
                out.extend_from_slice(&inp[lo..hi]);
                if src.may_hold_refs {
                    self.note();
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
            ArrayData::Words(out) => out.extend(bytes.iter().map(|&b| Value::from(i64::from(b)))),
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
        self.note();
        let w = self.words_vec();
        w.resize(w.len() + n, v);
    }

    pub fn reserve(&mut self, extra: usize) {
        let k = &usize::from(self.stride);
        match &mut self.data {
            ArrayData::Words(w) => w.reserve(extra),
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
            ArrayData::Words(w) => w.iter().map(|v| v.as_int() as u8).collect(),
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
            ArrayData::Words(w) => {
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
            ArrayData::Words(w) => w,
            ArrayData::Bytes(_) => unreachable!(),
            ArrayData::Lanes { .. } => lane_misuse(),
        }
    }

    /// Word elements for the collector's in-place fixups (compaction); no
    /// flag change, `None` for packed bytes (never references).
    #[inline]
    pub(crate) fn gc_words(&self) -> Option<&[Value]> {
        match &self.data {
            ArrayData::Words(w) | ArrayData::Lanes { words: w, .. } => Some(w),
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
    /// Packed kinds (`common::PackedKinds`) of [`Self::captures`] then
    /// [`Self::captured_args`], as the value's construction recorded them;
    /// `0` where it recorded none.
    pub word_kinds: common::PackedKinds,
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
            word_kinds: 0,
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
    /// The record slot of each field: a struct value is a record, its
    /// slots its fields sorted by name.
    pub slots: Vec<usize>,
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
            slots: layout
                .fields
                .iter()
                .map(|(n, _)| layout.fields.iter().filter(|(f, _)| f < n).count())
                .collect(),
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
    /// Bump-allocated in a young chunk when the nursery is on.
    const NURSERY: bool = false;
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


/// Prefix of every managed allocation. Kind reconstructs [`Object`] without
/// a live-address HashSet (`kind == 0` is a free slot). One word: kind,
/// flags and generation bytes, a debug byte, then the reference count.
#[repr(C)]
struct GcHeader {
    kind: Cell<u8>,
    /// [`FLAG_MARKED`], [`FLAG_FRESH`], [`FLAG_SHARED`],
    /// [`FLAG_FINALIZED`], [`FLAG_PINNED`]. Header state: no barrier, and
    /// an evacuation copies it with the slot.
    flags: Cell<u8>,
    /// Nursery bits ([`GEN_YOUNG`], [`GEN_PINNED`]).
    generation: Cell<u8>,
    /// Debug / rc-check: generation of the steal epoch the object was
    /// allocated in, 0 outside any epoch ([`EpochWriteScope`]). It takes the
    /// byte before the count; release builds leave it as padding.
    #[cfg(any(debug_assertions, feature = "rc-check"))]
    epoch: Cell<u8>,
    /// Reference count: the number of places that own this object.
    /// [`COUNT_IMMORTAL`] never changes. Not maintained yet (#2881 plan
    /// step 2).
    count: Cell<u32>,
}

/// Marked by the collector's current mark.
pub(super) const FLAG_MARKED: u8 = 1;
/// Copy-on-write: a second place may name this object, so a write copies it
/// first ([`Heap::cow_unique`]). Sticky until the slot is reused.
pub const FLAG_SHARED: u8 = 1 << 2;
/// An instance's or enum's `drop` has been claimed: it never runs twice
/// ([`Object::claim_finalizer`]).
const FLAG_FINALIZED: u8 = 1 << 3;
/// Copy-on-write: a name writes this object in place without a test (an
/// alias the compiler pinned it for, `CowPin`), so a share copies it
/// instead of setting [`FLAG_SHARED`] ([`Heap::cow_share`]). A pinned
/// object is never shared.
pub const FLAG_PINNED: u8 = 1 << 4;
/// A `gc::weak` handle names this object: its release clears them
/// ([`Heap::clear_weaks_to`]).
const FLAG_WEAK_TARGET: u8 = 1 << 5;

/// A count that never changes: interned and constant objects that live as
/// long as the program.
pub const COUNT_IMMORTAL: u32 = u32::MAX;

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
/// Byte offset of a header's flags byte ([`GcHeader::flags`]): native code's
/// inline share sets [`FLAG_SHARED`] there, and its `CowPin` / `CowUnpin`
/// set and clear [`FLAG_PINNED`].
pub const HEADER_FLAGS_OFFSET: i32 = std::mem::offset_of!(GcHeader, flags) as i32;
/// Byte offset of [`Heap`]'s steal-epoch allocation lock: non-null while
/// counts are not kept ([`Heap::rc_counting`]); native code's inline count
/// ops test it.
pub const HEAP_ALLOC_LOCK_OFFSET: i32 = std::mem::offset_of!(Heap, alloc_lock) as i32;
/// Offsets of the heap's chunk span `[lo, hi)`: every object lies in it.
pub const HEAP_SLAB_LO_OFFSET: i32 = (std::mem::offset_of!(Heap, slab) + super::slab::SLAB_LO_OFFSET) as i32;
pub const HEAP_SLAB_HI_OFFSET: i32 = (std::mem::offset_of!(Heap, slab) + super::slab::SLAB_HI_OFFSET) as i32;

/// Byte offset of a header's reference count ([`GcHeader::count`]), a
/// 32-bit word.
pub const HEADER_COUNT_OFFSET: i32 = std::mem::offset_of!(GcHeader, count) as i32;

// Native code reads and writes the kind and the flags as bytes.
const _: () = assert!(std::mem::size_of::<Cell<u8>>() == 1);
// The header stays one word, so no slot grows.
const _: () = assert!(std::mem::size_of::<GcHeader>() == 8);
const _: () = assert!(HEADER_COUNT_OFFSET == 4);
const _: () = assert!(std::mem::offset_of!(GcData<u64>, data) == 8);
const _: () = assert!(HEADER_FLAGS_OFFSET != HEADER_KIND_OFFSET);

#[repr(C)]
pub struct GcData<T> {
    header: GcHeader,
    data: T,
}

impl GcHeader {
    const fn new() -> Self {
        Self {
            kind: Cell::new(0),
            flags: Cell::new(0),
            generation: Cell::new(0),
            #[cfg(any(debug_assertions, feature = "rc-check"))]
            epoch: Cell::new(0),
            count: Cell::new(0),
        }
    }

    #[inline]
    const fn flag(&self, f: u8) -> bool {
        self.flags.get() & f != 0
    }

    #[inline]
    fn set_flag(&self, f: u8, on: bool) {
        let v = self.flags.get();
        self.flags.set(if on { v | f } else { v & !f });
    }

    /// Set or clear `f`, returning whether it was set.
    #[inline]
    fn replace_flag(&self, f: u8, on: bool) -> bool {
        let was = self.flag(f);
        self.set_flag(f, on);
        was
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

    pub const fn is_marked(&self) -> bool {
        self.header.flag(FLAG_MARKED)
    }

    pub fn mark(&self) -> bool {
        let is_not_marked = !self.header.flag(FLAG_MARKED);
        if is_not_marked {
            self.header.set_flag(FLAG_MARKED, true);
        }
        is_not_marked
    }

    pub fn unmark(&self) {
        self.header.set_flag(FLAG_MARKED, false);
    }
}

impl<T> AsRef<T> for GcData<T> {
    fn as_ref(&self) -> &T {
        &self.data
    }
}

impl<T> AsMut<T> for GcData<T> {
    fn as_mut(&mut self) -> &mut T {
        #[cfg(any(debug_assertions, feature = "rc-check"))]
        self.check_epoch_write();
        &mut self.data
    }
}

#[cfg(any(debug_assertions, feature = "rc-check"))]
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
            // Every flag but the finalizer's claim, as before the flags
            // shared a byte.
            (*p).header.flags.set((*p).header.flags.get() & FLAG_FINALIZED);
            (*p).header.generation.set(0);
            (*p).header.count.set(0);
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



#[path = "nursery.rs"]
mod nursery;

#[path = "inline.rs"]
mod inline;
pub use inline::{InlineObject, InlineShape, ProductSlots, inline_object, product_slots, layout_fingerprint};

#[path = "rc.rs"]
mod rc;
pub(crate) use rc::rc_leaks_shown;

#[path = "cycles.rs"]
mod cycles;

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
        // Sharing by the shared bit, as with no counts.
        heap.set_rc_counting(false);
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
    #[cfg(not(feature = "rc-check"))]
    fn epoch_defers_collect_until_ceiling() {
        let lock = Mutex::new(());
        let mut heap = Heap::default();
        heap.counts.alloc_bytes = GC_NEXT_THRESHOLD + 1;
        assert!(heap.cycles_due());
        heap.enter_epoch_stw(&lock);
        assert!(!heap.cycles_due(), "past threshold, under epoch ceiling");
        heap.counts.alloc_bytes = heap.epoch_gc_ceiling + 1;
        assert!(heap.cycles_due(), "past the epoch ceiling");
        heap.exit_epoch_stw();
        heap.counts.alloc_bytes = GC_NEXT_THRESHOLD + 1;
        assert!(heap.cycles_due(), "normal threshold again after the epoch");
        heap.counts.alloc_bytes = 0;
    }

    /// A thread that waits mid-epoch on heap A and helps with a job in heap
    /// B's epoch keeps the two batches apart (#766, #779): B's objects land
    /// in B, and A's batch comes back intact for A's flush.
    /// In a steal epoch a count change is logged, not written: the header
    /// stays as it was until the epoch ends and the logs are summed in
    /// (reference-count plan step 8). One released to zero is freed then.
    #[test]
    fn epoch_count_changes_apply_at_its_end() {
        let lock = Mutex::new(());
        let mut heap = Heap::default();
        heap.set_rc_counting(true);
        heap.set_rc_frees(true);
        let old = heap.alloc(ObjString::from("old"), Object::String).0;
        let dropped = heap.alloc(ObjString::from("dropped"), Object::String).0;
        old.rc_inc();
        dropped.rc_inc();
        heap.enter_epoch_stw(&lock);
        let born = heap.alloc(ObjString::from("born"), Object::String).0;
        let gone = heap.alloc(ObjString::from("gone"), Object::String).0;
        heap.rc_inc_obj(old);
        heap.rc_inc_obj(born);
        heap.rc_inc_obj(gone);
        assert!(!heap.rc_dec_obj(gone), "no release inside the epoch");
        heap.rc_inc_obj(born);
        assert!(!heap.rc_dec_obj(dropped));
        assert_eq!((old.rc_count(), born.rc_count(), dropped.rc_count()), (1, 0, 1), "headers untouched in the epoch");
        heap.flush_epoch_batch();
        heap.exit_epoch_stw();
        assert_eq!((old.rc_count(), born.rc_count()), (2, 2));
        // `dropped` reached zero and was freed; so was `gone`, born in the
        // epoch, whose owners came and went (changes that sum to nothing
        // from a count of zero).
        assert_eq!(heap.live_object_count(), 2);
    }

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
    #[cfg(any(debug_assertions, feature = "rc-check"))]
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
    #[cfg(any(debug_assertions, feature = "rc-check"))]
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

}
