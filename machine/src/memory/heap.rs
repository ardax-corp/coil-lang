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
    weaks: usize,
}

impl EpochBatch {
    fn is_empty(&self) -> bool {
        self.classes.iter().all(|(_, v)| v.is_empty())
            && self.bytes == 0
            && self.objects == 0
            && self.weaks == 0
    }
}

thread_local! {
    static EPOCH_BATCH_TLS: std::cell::RefCell<EpochBatch> =
        std::cell::RefCell::new(EpochBatch::default());
}

/// This thread's epoch batch, set aside while it helps with a job that may
/// allocate in another heap's epoch (a join help-steals unrelated jobs: other
/// test cases, other VMs' auto-par work). Dropping it puts the batch back.
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

/// Collector phase. `Idle` means the last cycle finished.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GcPhase {
    Idle,
    Marking,
    Sweeping,
}

/// Managed heap. Objects are linked in an intrusive list for traversal.
/// `Gc<T>` handles are copyable; the VM controls when objects become unreachable.
pub struct Heap {
    alloc_bytes: usize,
    gc_next_threshold: usize,
    gc_growth_factor: usize,
    /// Heap bytes when the current sweep started (survival for the next budget).
    gc_sweep_start_bytes: usize,
    strings: Table<()>,
    slab: Slab,
    /// Live object count (alloc +1, sweep/dealloc −1). Not a membership set.
    live_count: usize,
    /// Per class `type_id`: field word kinds from static types
    /// (`common::WORD_*`). Missing rows / fields are unknown.
    class_kinds: std::sync::Arc<Vec<Box<[u8]>>>,
    /// Live `Weak` objects, so [`Self::clear_dead_weaks`] can skip its heap
    /// walk (weak handles are rare).
    weak_count: usize,
    /// Immortal arity-0 enum singletons keyed by tag (never swept).
    immortal_enums: HashMap<u32, Object, AddrHashBuilder>,
    /// Last tag returned by [`Self::immortal_unit_enum`]. Unit constructors
    /// (binary-tree leaves) hit this instead of the map.
    unit_enum: Option<(u32, Object)>,
    /// Shared `()` ([`Self::immortal_empty_tuple`]), never swept or moved.
    empty_tuple: Option<Object>,
    /// Reused gray worklist / root buffers across collections.
    gc_gray: Vec<Object>,
    gc_root_objects: Vec<Object>,
    gc_roots: Vec<u64>,
    gc_dangling_strings: Vec<RefString>,
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
}

/// Machine-owned heap that can borrow the root Heap for a C1 steal job.
#[derive(Default)]
pub struct HeapSlot {
    owned: Heap,
    borrowed: Option<NonNull<Heap>>,
}

impl Default for Heap {
    fn default() -> Self {
        Self {
            alloc_bytes: 0,
            gc_next_threshold: GC_NEXT_THRESHOLD,
            gc_growth_factor: GC_GROWTH_FACTOR,
            gc_sweep_start_bytes: 0,
            strings: Table::default(),
            slab: Slab::new(),
            live_count: 0,
            class_kinds: std::sync::Arc::default(),
            weak_count: 0,
            immortal_enums: HashMap::default(),
            unit_enum: None,
            empty_tuple: None,
            gc_gray: Vec::new(),
            gc_root_objects: Vec::new(),
            gc_roots: Vec::new(),
            gc_dangling_strings: Vec::new(),
            gc_phase: GcPhase::Idle,
            gc_sweep_cursor: None,
            ffi_strings: Vec::new(),
            epoch_stw: false,
            epoch_gc_ceiling: 0,
            alloc_lock: ptr::null(),
        }
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
        self.epoch_stw = true;
        self.epoch_gc_ceiling = self
            .gc_next_threshold
            .saturating_mul(EPOCH_GC_HEADROOM_FACTOR)
            .max(self.alloc_bytes.saturating_add(EPOCH_GC_HEADROOM_MIN));
        self.alloc_lock = lock as *const Mutex<()>;
    }

    pub fn exit_epoch_stw(&mut self) {
        self.epoch_stw = false;
        self.alloc_lock = ptr::null();
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
        EPOCH_BATCH_TLS.with(|cell| {
            let mut batch = cell.borrow_mut();
            batch.bytes += object.size();
            batch.objects += 1;
            if matches!(object, Object::Weak(_)) {
                batch.weaks += 1;
            }
        });
        crate::vm::note_heap_alloc();
        (object, content)
    }

    /// Add a batch's pending byte / object counts to the heap (lock held).
    fn fold_epoch_counts(&mut self, batch: &mut EpochBatch) {
        self.alloc_bytes += std::mem::take(&mut batch.bytes);
        self.live_count += std::mem::take(&mut batch.objects);
        self.weak_count += std::mem::take(&mut batch.weaks);
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
        let layout = Layout::new::<GcData<T>>();
        let slot = self.slab.alloc(layout).cast::<GcData<T>>();
        let (object, content) = Self::init_slot(slot, data, map);
        let size = object.size();
        self.alloc_bytes += size;
        self.live_count += 1;
        if matches!(object, Object::Weak(_)) {
            self.weak_count += 1;
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
        crate::vm::note_heap_alloc();
        debug_assert!(
            self.find_object_by_addr(object.addr()).is_some(),
            "slab alloc must be findable at {:#x}",
            object.addr()
        );

        (object, content)
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

    /// Allocate `head + tail` without interning, appending in place when
    /// `head` ends at its buffer's tail (amortized linear `s = s + x`).
    pub fn alloc_concat(&mut self, head: RefString, tail: &str) -> RefString {
        let joined = head.as_ref().concat(tail);
        self.alloc(joined, Object::String).1
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
        crate::vm::note_intern_str();
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
        self.strings.insert(string, ());
        string
    }

    fn intern_new(&mut self, data: String, hash: u32) -> RefString {
        let obj_string = ObjString::with_hash(data, hash);
        let (_, s) = self.alloc_unlocked(obj_string, Object::String);
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
        self.gc_sweep_start_bytes = self.alloc_bytes;
        self.gc_sweep_cursor = Some(super::slab::SlotCursor::default());
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
    pub fn gc_is_marking(&self) -> bool {
        self.gc_phase == GcPhase::Marking
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
        self.gc_phase = GcPhase::Sweeping;
        self.gc_sweep_start_bytes = self.alloc_bytes;
        self.gc_sweep_cursor = Some(super::slab::SlotCursor::default());
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
        self.gc_sweep_cursor = Some(cur);
        // A `fresh` object is kept whatever its mark (it may postdate the
        // mark phase) — but still unmarked: a stale mark would make the next
        // cycle skip tracing its children.
        let fresh = obj.take_fresh();
        if obj.is_marked() {
            obj.unmark();
        } else if !fresh {
            unsafe { self.dealloc(obj) };
        }
    }

    fn finish_sweep_cycle(&mut self) {
        // Floor at the initial budget: a tiny live set would otherwise
        // schedule a collection every few allocations.
        let growth = if self.alloc_bytes.saturating_mul(2) > self.gc_sweep_start_bytes {
            self.gc_growth_factor.max(GC_GROWTH_FACTOR_SURVIVING)
        } else {
            self.gc_growth_factor
        };
        self.gc_next_threshold = self
            .alloc_bytes
            .saturating_mul(growth)
            .max(GC_NEXT_THRESHOLD);
        // Empty chunks the program did not touch all cycle go back to the OS.
        self.slab.release_idle_chunks();
        self.gc_phase = GcPhase::Idle;
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

    /// Returns the number of bytes that are being allocated.
    pub const fn size(&self) -> usize {
        self.alloc_bytes
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
        self.live_count
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
            && (cfg!(feature = "gc-stress") || self.alloc_bytes > limit)
    }

    /// Objects to sweep at one safepoint (doubles under pressure).
    #[inline]
    pub fn gc_sweep_quantum(&self) -> usize {
        if self.alloc_bytes > self.gc_next_threshold {
            GC_SWEEP_QUANTUM.saturating_mul(2)
        } else {
            GC_SWEEP_QUANTUM
        }
    }

    /// Lower the byte threshold so the next [`Self::should_collect`] check
    /// fires (test helper for GC stress).
    #[cfg(any(test, feature = "debugger"))]
    pub fn set_gc_threshold_for_test(&mut self, bytes: usize) {
        self.gc_next_threshold = bytes;
    }

    /// Adjust tracked heap bytes after an in-place grow/shrink of a managed
    /// object's internal Rust allocation (for example `ObjArray.elements`).
    pub fn account_resize(&mut self, old_size: usize, new_size: usize) {
        let _epoch_guard = self.epoch_guard();
        if new_size >= old_size {
            self.alloc_bytes += new_size - old_size;
        } else {
            self.alloc_bytes -= old_size - new_size;
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
        self.alloc_bytes -= size;
        debug_assert!(self.live_count > 0);
        self.live_count -= 1;
        if matches!(object, Object::Weak(_)) {
            self.weak_count -= 1;
        }
        let ptr = unsafe { NonNull::new_unchecked(object.addr() as *mut u8) };
        unsafe { object.recycle_payload() };
        self.slab.free(ptr);
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

    /// Complete collect without a `Machine`: mark `extra_roots` plus immortal
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
    pub fn clear_dead_weaks(&self) {
        if self.weak_count == 0 {
            return;
        }
        for obj in self.objects() {
            if let Object::Weak(gc) = obj {
                let weak = gc.as_ref();
                if !weak.cleared.get() {
                    let target = weak.target.get();
                    let addr = target.heap_addr();
                    if addr != 0
                        && let Some(referent) = self.find_object_by_addr(addr)
                        && !referent.is_marked()
                    {
                        weak.cleared.set(true);
                        weak.target.set(Value::from(0i64));
                    }
                }
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
        if let Some(obj) = self.empty_tuple {
            roots.push(obj.addr());
        }
        roots
    }

    /// The shared empty tuple `()`. It has no elements to change, so every
    /// `()` (a `Result<(), E>` `Ok`, a unit return) can be one object.
    pub fn immortal_empty_tuple(&mut self) -> Object {
        if let Some(obj) = self.empty_tuple {
            return obj;
        }
        let _epoch_guard = self.epoch_guard();
        let (object, _) = self.alloc_unlocked(ObjTuple::from_slice(&[]), Object::Tuple);
        self.empty_tuple = Some(object);
        object
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

    pub fn class_word_kinds_table(&self) -> std::sync::Arc<Vec<Box<[u8]>>> {
        std::sync::Arc::clone(&self.class_kinds)
    }

    /// Word kinds of `type_id`'s typed fields (empty = all unknown).
    #[inline]
    pub fn class_field_kinds(&self, type_id: u32) -> &[u8] {
        self.class_kinds
            .get(type_id as usize)
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
        if kind == common::WORD_POINTER && addr != 0 && !resolves {
            panic!("gc-stress: pointer word {index} of {what} holds a non-object");
        }
    }

    /// Mark enum payload / tuple words, skipping those the construction
    /// site proved scalar.
    #[inline]
    fn mark_kinded_words(&self, words: &[Value], kinds: u8, grey_objects: &mut Vec<Object>) {
        for (i, v) in words.iter().enumerate() {
            let kind = common::packed_word_kind(kinds, i);
            #[cfg(feature = "gc-stress")]
            self.verify_kinded_word("an enum payload / tuple", i, kind, *v);
            if kind != common::WORD_SCALAR {
                self.mark_value(*v, grey_objects);
            }
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
    pub fn find_object_by_addr(&self, addr: u64) -> Option<Object> {
        if addr == 0 || !self.slab.contains_slot(addr) {
            return None;
        }
        unsafe { Object::from_header(addr) }
    }

    /// Classify a raw payload / field word as an object or an immediate.
    #[cfg(test)]
    pub(crate) fn member_of(&self, v: Value) -> Member {
        match self.find_object_by_addr(v.raw() as u64) {
            Some(o) => Member::Object(o),
            None => Member::Value(v),
        }
    }

    /// Write back scratch-buffer values into a live `ObjArray`.
    pub fn update_array_elements(&mut self, addr: u64, values: &[i64]) {
        let Some(Object::Array(mut gc)) = self.find_object_by_addr(addr) else {
            return;
        };
        let n = gc.as_ref().elements().len().min(values.len());
        let arr = gc.as_mut();
        for (i, &v) in values.iter().take(n).enumerate() {
            arr.elements[i] = Value::from(v);
        }
    }

    /// Write `bytes` back into the array at `addr`, one byte per element
    /// (an FFI `Bytes` buffer after the call). Extra bytes are dropped.
    pub fn update_array_bytes(&mut self, addr: u64, bytes: &[u8]) {
        let Some(Object::Array(mut gc)) = self.find_object_by_addr(addr) else {
            return;
        };
        let arr = gc.as_mut();
        for (slot, &b) in arr.elements.iter_mut().zip(bytes) {
            *slot = Value::from(i64::from(b));
        }
    }

    /// True if `addr` is a live heap object.
    /// True when `v` could be a heap reference: its address lies in the
    /// slab's mapped range. False proves it is not one.
    #[inline]
    pub fn may_be_ref(&self, v: Value) -> bool {
        let addr = v.heap_addr();
        addr != 0 && self.slab.may_contain(addr)
    }

    pub fn contains_addr(&self, addr: *mut u8) -> bool {
        self.find_object_by_addr(addr as u64).is_some()
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
        let mut cur = super::slab::SlotCursor::default();
        while let Some(addr) = self.slab.next_slot(&mut cur) {
            if let Some(object) = unsafe { Object::from_header(addr) } {
                self.alloc_bytes -= object.size();
                self.live_count -= 1;
                unsafe { object.recycle_payload() };
            }
        }

        debug_assert_eq!(0, self.alloc_bytes);
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

pub type RefBoxed = Gc<ObjBoxed>;
pub type RefRoot = Gc<ObjRoot>;
pub type RefWeak = Gc<ObjWeak>;
pub type RefPolyFn = Gc<ObjPolyFn>;
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
}

#[derive(Clone, Copy)]
pub enum Object {
    String(RefString),
    Instance(RefInstance),
    Enum(RefEnum),
    Library(RefLibrary),
    Tuple(crate::memory::Gc<ObjTuple>),
    Array(crate::memory::Gc<ObjArray>),
    Coroutine(RefCoroutine),
    Boxed(RefBoxed),
    /// Strong GC pin: marks `payload` while this handle is reachable.
    Root(RefRoot),
    /// Non-rooting handle; cleared when the referent is unmarked.
    Weak(RefWeak),
    PolyFn(RefPolyFn),
    Fn(RefFn),
    Stream(RefStream),
    Thread(RefThread),
    Sender(RefSender),
    Receiver(RefReceiver),
    Mutex(RefThreadMutex),
    RwLock(RefRwLock),
}

impl Object {
    /// Mark the current object reference and put it in `grey_objects` if its has not been marked.
    pub fn mark(&self, grey_objects: &mut Vec<Self>) {
        let marked = match self {
            Self::String(s) => s.mark(),
            Self::Instance(i) => i.mark(),
            Self::Enum(e) => e.mark(),
            Self::Library(l) => l.mark(),
            Self::Tuple(t) => t.mark(),
            Self::Array(a) => a.mark(),
            Self::Coroutine(c) => c.mark(),
            Self::Boxed(b) => b.mark(),
            Self::Root(r) => r.mark(),
            Self::Weak(w) => w.mark(),
            Self::PolyFn(p) => p.mark(),
            Self::Fn(f) => f.mark(),
            Self::Stream(s) => s.mark(),
            Self::Thread(t) => t.mark(),
            Self::Sender(s) => s.mark(),
            Self::Receiver(r) => r.mark(),
            Self::Mutex(m) => m.mark(),
            Self::RwLock(l) => l.mark(),
        };
        if marked {
            grey_objects.push(*self);
        }
    }

    /// Unmark the object.
    pub fn unmark(&self) {
        match self {
            Self::String(s) => s.unmark(),
            Self::Instance(i) => i.unmark(),
            Self::Enum(e) => e.unmark(),
            Self::Library(l) => l.unmark(),
            Self::Tuple(t) => t.unmark(),
            Self::Array(a) => a.unmark(),
            Self::Coroutine(c) => c.unmark(),
            Self::Boxed(b) => b.unmark(),
            Self::Root(r) => r.unmark(),
            Self::Weak(w) => w.unmark(),
            Self::PolyFn(p) => p.unmark(),
            Self::Fn(f) => f.unmark(),
            Self::Stream(s) => s.unmark(),
            Self::Thread(t) => t.unmark(),
            Self::Sender(s) => s.unmark(),
            Self::Receiver(r) => r.unmark(),
            Self::Mutex(m) => m.unmark(),
            Self::RwLock(l) => l.unmark(),
        }
    }

    #[must_use]
    pub fn is_marked(&self) -> bool {
        match self {
            Self::String(s) => s.is_marked(),
            Self::Instance(i) => i.is_marked(),
            Self::Enum(e) => e.is_marked(),
            Self::Library(l) => l.is_marked(),
            Self::Tuple(t) => t.is_marked(),
            Self::Array(a) => a.is_marked(),
            Self::Coroutine(c) => c.is_marked(),
            Self::Boxed(b) => b.is_marked(),
            Self::Root(r) => r.is_marked(),
            Self::Weak(w) => w.is_marked(),
            Self::PolyFn(p) => p.is_marked(),
            Self::Fn(f) => f.is_marked(),
            Self::Stream(s) => s.is_marked(),
            Self::Thread(t) => t.is_marked(),
            Self::Sender(s) => s.is_marked(),
            Self::Receiver(r) => r.is_marked(),
            Self::Mutex(m) => m.is_marked(),
            Self::RwLock(l) => l.is_marked(),
        }
    }

    /// Mark a stored member, including Result `Err` tagged pointers in `Value`.
    fn mark_member(heap: &Heap, member: &Member, grey_objects: &mut Vec<Self>) {
        match member {
            Member::Object(o) => o.mark(grey_objects),
            Member::Value(v) => heap.mark_value(*v, grey_objects),
        }
    }

    /// Mark direct heap references held by this object.
    ///
    /// Aggregates that store raw [`Value`]s (arrays, tuples, fn captures,
    /// coroutine stacks) resolve those addresses through `heap`. Stream attach
    /// state is host-side (A4); channel payloads are PortableValue copies,
    /// not this-heap pointers.
    pub fn mark_references(&self, heap: &Heap, grey_objects: &mut Vec<Self>) {
        match self {
            Self::String(_) => {}
            Self::Instance(i) => i.as_ref().mark_members(heap, grey_objects),
            Self::Enum(e) => {
                let payload = &e.as_ref().payload;
                heap.mark_kinded_words(payload, payload.kinds(), grey_objects);
            }
            Self::Library(_) => {}
            Self::Tuple(t) => {
                let t = t.as_ref();
                heap.mark_kinded_words(t.elements(), t.kinds(), grey_objects);
            }
            Self::Array(a) => {
                // Clear flag: scanned clean at a previous mark, unwritten since.
                if a.as_ref().may_hold_refs() {
                    let pointers = a.as_ref().elem_kind() == common::WORD_POINTER;
                    let mut any = false;
                    for v in a.as_ref().elements() {
                        #[cfg(feature = "gc-stress")]
                        if pointers {
                            heap.verify_kinded_word("a pointer-kind array", 0, common::WORD_POINTER, *v);
                        }
                        // Pointer kind: `0` or an object, no classification.
                        if (pointers && v.heap_addr() != 0) || (!pointers && heap.may_be_ref(*v)) {
                            any = true;
                            heap.mark_value(*v, grey_objects);
                        }
                    }
                    if !any {
                        a.payload_mut().may_hold_refs = false;
                    }
                }
            }
            Self::Coroutine(c) => {
                let coro = c.as_ref();
                let mask = coro.saved_live_mask;
                for (i, v) in coro.saved_stack.iter().enumerate() {
                    if mask != 0 && i < 64 && mask & (1u64 << i) == 0 {
                        continue;
                    }
                    heap.mark_value(*v, grey_objects);
                }
                heap.mark_value(coro.pending_send, grey_objects);
                for link in [coro.yield_from, coro.delegator].into_iter().flatten() {
                    Object::Coroutine(link).mark(grey_objects);
                }
            }
            Self::Boxed(b) => Self::mark_member(heap, &b.as_ref().payload, grey_objects),
            Self::Root(r) => {
                if let Some(member) = &r.as_ref().payload {
                    Self::mark_member(heap, member, grey_objects);
                }
            }
            Self::Weak(_) => {}
            Self::PolyFn(p) => {
                for captured in p.as_ref().captured_dicts.iter().flatten() {
                    Self::mark_member(heap, captured, grey_objects);
                }
            }
            Self::Fn(f) => {
                let f = f.as_ref();
                for v in f.captures.iter().chain(f.captured_args.iter()) {
                    heap.mark_value(*v, grey_objects);
                }
            }
            Self::Stream(_) => {}
            Self::Thread(_) => {}
            Self::Sender(_) => {}
            Self::Receiver(_) => {}
            Self::Mutex(_) => {}
            Self::RwLock(_) => {}
        }
    }

    /// Every heap reference `self` holds, with whether the word is precise.
    ///
    /// Precise: `Member::Object` fields, coroutine links, and coroutine stack
    /// words named by a nonzero `saved_live_mask`. Everything else is a raw
    /// `Value` word that is traced by address lookup and may be an immediate
    /// (tuple / array elements, closure captures, unmasked coroutine words,
    /// `Member::Value` fields) — a moving collector cannot rewrite those
    /// (`docs/internals/gc-evacuation.md`). Not on the mark path.
    pub fn for_each_reference(&self, heap: &Heap, visit: &mut dyn FnMut(u64, bool)) {
        let word = |v: Value, precise: bool, visit: &mut dyn FnMut(u64, bool)| {
            let addr = v.heap_addr();
            if addr != 0 && heap.find_object_by_addr(addr).is_some() {
                visit(addr, precise);
            }
        };
        let kinded = |words: &[Value], kinds: u8, visit: &mut dyn FnMut(u64, bool)| {
            for (i, v) in words.iter().enumerate() {
                match common::packed_word_kind(kinds, i) {
                    common::WORD_SCALAR => {}
                    kind => word(*v, kind == common::WORD_POINTER, visit),
                }
            }
        };
        let member = |m: &Member, visit: &mut dyn FnMut(u64, bool)| match m {
            Member::Object(o) => visit(o.addr(), true),
            Member::Value(v) => word(*v, false, visit),
        };
        match self {
            Self::Instance(i) => {
                let inst = i.as_ref();
                match &inst.storage {
                    InstanceStorage::Table(table) => {
                        table.iter().for_each(|(_, v)| member(&v, visit));
                    }
                    InstanceStorage::Inline { .. } | InstanceStorage::Spill(_) => {
                        let kinds = heap.class_field_kinds(inst.type_id);
                        for (i, v) in inst.storage.as_slice().into_iter().flatten().enumerate() {
                            match kinds.get(i).copied().unwrap_or(common::WORD_UNKNOWN) {
                                common::WORD_SCALAR => {}
                                kind => word(*v, kind == common::WORD_POINTER, visit),
                            }
                        }
                    }
                }
            }
            Self::Enum(e) => {
                let payload = &e.as_ref().payload;
                kinded(payload, payload.kinds(), visit);
            }
            Self::Tuple(t) => {
                let t = t.as_ref();
                kinded(t.elements(), t.kinds(), visit);
            }
            Self::Array(a) => {
                let a = a.as_ref();
                if a.may_hold_refs() {
                    let precise = a.elem_kind() == common::WORD_POINTER;
                    a.elements().iter().for_each(|v| word(*v, precise, visit));
                }
            }
            Self::Coroutine(c) => {
                let coro = c.as_ref();
                let mask = coro.saved_live_mask;
                for (i, v) in coro.saved_stack.iter().enumerate() {
                    let masked = mask != 0 && i < 64;
                    if masked && mask & (1u64 << i) == 0 {
                        continue;
                    }
                    word(*v, masked, visit);
                }
                word(coro.pending_send, false, visit);
                for link in [coro.yield_from, coro.delegator].into_iter().flatten() {
                    visit(Object::Coroutine(link).addr(), true);
                }
            }
            Self::Boxed(b) => member(&b.as_ref().payload, visit),
            Self::Root(r) => {
                if let Some(m) = &r.as_ref().payload {
                    member(m, visit);
                }
            }
            Self::PolyFn(p) => p.as_ref().captured_dicts.iter().flatten().for_each(|m| member(m, visit)),
            Self::Fn(f) => {
                let f = f.as_ref();
                for v in f.captures.iter().chain(f.captured_args.iter()) {
                    word(*v, false, visit);
                }
            }
            Self::String(_)
            | Self::Library(_)
            | Self::Weak(_)
            | Self::Stream(_)
            | Self::Thread(_)
            | Self::Sender(_)
            | Self::Receiver(_)
            | Self::Mutex(_)
            | Self::RwLock(_) => {}
        }
    }

    /// Clear the header's `fresh` bit (allocated during a sweep), returning
    /// whether it was set. Every kind shares the `repr(C)` header layout.
    fn take_fresh(&self) -> bool {
        unsafe { &*(self.addr() as *const GcHeader) }.fresh.replace(false)
    }

    #[must_use]
    pub fn addr(&self) -> u64 {
        match self {
            Self::String(s) => s.as_ptr() as u64,
            Self::Instance(i) => i.as_ptr() as u64,
            Self::Enum(e) => e.as_ptr() as u64,
            Self::Library(l) => l.as_ptr() as u64,
            Self::Tuple(t) => t.as_ptr() as u64,
            Self::Array(a) => a.as_ptr() as u64,
            Self::Coroutine(c) => c.as_ptr() as u64,
            Self::Boxed(b) => b.as_ptr() as u64,
            Self::Root(r) => r.as_ptr() as u64,
            Self::Weak(w) => w.as_ptr() as u64,
            Self::PolyFn(p) => p.as_ptr() as u64,
            Self::Fn(f) => f.as_ptr() as u64,
            Self::Stream(s) => s.as_ptr() as u64,
            Self::Thread(t) => t.as_ptr() as u64,
            Self::Sender(s) => s.as_ptr() as u64,
            Self::Receiver(r) => r.as_ptr() as u64,
            Self::Mutex(m) => m.as_ptr() as u64,
            Self::RwLock(l) => l.as_ptr() as u64,
        }
    }

    fn kind(self) -> u8 {
        match self {
            Self::String(_) => 1,
            Self::Instance(_) => 2,
            Self::Enum(_) => 3,
            Self::Library(_) => 4,
            Self::Tuple(_) => 5,
            Self::Array(_) => 6,
            Self::Coroutine(_) => 7,
            Self::Boxed(_) => 8,
            Self::Root(_) => 9,
            Self::Weak(_) => 10,
            Self::PolyFn(_) => 11,
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
        match self {
            Self::String(s) => unsafe { s.recycle() },
            Self::Instance(i) => unsafe { i.recycle() },
            Self::Enum(e) => unsafe { e.recycle() },
            Self::Library(l) => unsafe { l.recycle() },
            Self::Tuple(t) => unsafe { t.recycle() },
            Self::Array(a) => unsafe { a.recycle() },
            Self::Coroutine(c) => unsafe { c.recycle() },
            Self::Boxed(b) => unsafe { b.recycle() },
            Self::Root(r) => unsafe { r.recycle() },
            Self::Weak(w) => unsafe { w.recycle() },
            Self::PolyFn(p) => unsafe { p.recycle() },
            Self::Fn(f) => unsafe { f.recycle() },
            Self::Stream(s) => unsafe { s.recycle() },
            Self::Thread(t) => unsafe { t.recycle() },
            Self::Sender(s) => unsafe { s.recycle() },
            Self::Receiver(r) => unsafe { r.recycle() },
            Self::Mutex(m) => unsafe { m.recycle() },
            Self::RwLock(l) => unsafe { l.recycle() },
        }
    }

    /// Rebuild a typed handle from a live allocation. The address must be a
    /// current [`Heap::alloc`] result (non-moving); kind is the header byte.
    unsafe fn from_header(addr: u64) -> Option<Self> {
        let header = unsafe { &*(addr as *const GcHeader) };
        Some(match header.kind.get() {
            1 => Self::String(unsafe { Gc::from_addr(addr) }),
            2 => Self::Instance(unsafe { Gc::from_addr(addr) }),
            3 => Self::Enum(unsafe { Gc::from_addr(addr) }),
            4 => Self::Library(unsafe { Gc::from_addr(addr) }),
            5 => Self::Tuple(unsafe { Gc::from_addr(addr) }),
            6 => Self::Array(unsafe { Gc::from_addr(addr) }),
            7 => Self::Coroutine(unsafe { Gc::from_addr(addr) }),
            8 => Self::Boxed(unsafe { Gc::from_addr(addr) }),
            9 => Self::Root(unsafe { Gc::from_addr(addr) }),
            10 => Self::Weak(unsafe { Gc::from_addr(addr) }),
            11 => Self::PolyFn(unsafe { Gc::from_addr(addr) }),
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
        match self {
            Self::String(s) => s.size(),
            Self::Instance(i) => i.size(),
            Self::Enum(e) => e.size(),
            Self::Library(l) => l.size(),
            Self::Tuple(t) => t.size(),
            Self::Array(a) => a.size(),
            Self::Coroutine(c) => c.size(),
            Self::Boxed(b) => b.size(),
            Self::Root(r) => r.size(),
            Self::Weak(w) => w.size(),
            Self::PolyFn(p) => p.size(),
            Self::Fn(f) => f.size(),
            Self::Stream(s) => s.size(),
            Self::Thread(t) => t.size(),
            Self::Sender(s) => s.size(),
            Self::Receiver(r) => r.size(),
            Self::Mutex(m) => m.size(),
            Self::RwLock(l) => l.size(),
        }
    }
}

impl fmt::Display for Object {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::String(s) => write!(f, "{}", s.as_ref()),
            Self::Instance(_) => write!(f, "0x{:08x}", self.addr()),
            Self::Enum(_) => write!(f, "0x{:08x}", self.addr()),
            Self::Library(_) => write!(f, "0x{:08x}", self.addr()),
            Self::Tuple(t) => write!(f, "{}", t.as_ref()),
            Self::Array(a) => write!(f, "{}", a.as_ref()),
            Self::Coroutine(c) => write!(f, "{}", c.as_ref()),
            Self::Boxed(_) => write!(f, "<boxed 0x{:08x}>", self.addr()),
            Self::Root(_) => write!(f, "<root 0x{:08x}>", self.addr()),
            Self::Weak(_) => write!(f, "<weak 0x{:08x}>", self.addr()),
            Self::PolyFn(_) => write!(f, "<polyfn 0x{:08x}>", self.addr()),
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
    /// C string pointer for FFI; non-strings return null.
    pub fn as_cstr(&self) -> *const std::os::raw::c_char {
        match self {
            Self::String(s) => s.data.data.as_str().as_ptr() as *const std::os::raw::c_char,
            Self::Instance(_)
            | Self::Enum(_)
            | Self::Library(_)
            | Self::Tuple(_)
            | Self::Array(_)
            | Self::Coroutine(_)
            | Self::Boxed(_)
            | Self::Root(_)
            | Self::Weak(_)
            | Self::PolyFn(_)
            | Self::Fn(_)
            | Self::Stream(_)
            | Self::Thread(_)
            | Self::Sender(_)
            | Self::Receiver(_)
            | Self::Mutex(_)
            | Self::RwLock(_) => std::ptr::null(),
        }
    }
}

/// How a moving collector may treat a root (`docs/internals/gc-evacuation.md`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RootKind {
    /// A word the VM knows is a heap reference; could be rewritten.
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

/// Max typed field count stored inside [`ObjInstance`] without a Rust `Vec`.
///
/// Typed fields are raw words (the GC resolves them like tuple elements), so
/// four fit where two tagged [`Member`]s used to; larger classes spill.
pub const INSTANCE_INLINE_FIELDS: usize = 4;

/// Named intern table (`type_id == 0` / `INIT`) or dense typed slots.
enum InstanceStorage {
    Table(Table<Member>),
    Inline {
        len: u8,
        slots: [Value; INSTANCE_INLINE_FIELDS],
    },
    Spill(Vec<Value>),
}

impl InstanceStorage {
    fn typed_slots(nfields: usize) -> Self {
        if nfields <= INSTANCE_INLINE_FIELDS {
            Self::Inline {
                len: nfields as u8,
                slots: [Value::from(0i64); INSTANCE_INLINE_FIELDS],
            }
        } else {
            Self::Spill(vec![Value::from(0i64); nfields])
        }
    }

    fn from_vec(slots: Vec<Value>) -> Self {
        let n = slots.len();
        if n <= INSTANCE_INLINE_FIELDS {
            let mut inline = [Value::from(0i64); INSTANCE_INLINE_FIELDS];
            inline[..n].copy_from_slice(&slots);
            Self::Inline {
                len: n as u8,
                slots: inline,
            }
        } else {
            Self::Spill(slots)
        }
    }

    fn as_slice(&self) -> Option<&[Value]> {
        match self {
            Self::Inline { len, slots } => Some(&slots[..*len as usize]),
            Self::Spill(slots) => Some(slots.as_slice()),
            Self::Table(_) => None,
        }
    }

    fn as_mut_slice(&mut self) -> Option<&mut [Value]> {
        match self {
            Self::Inline { len, slots } => Some(&mut slots[..*len as usize]),
            Self::Spill(slots) => Some(slots.as_mut_slice()),
            Self::Table(_) => None,
        }
    }

    fn is_inline(&self) -> bool {
        matches!(self, Self::Inline { .. })
    }

    fn spill_capacity(&self) -> usize {
        match self {
            Self::Spill(slots) => slots.capacity(),
            Self::Inline { .. } | Self::Table(_) => 0,
        }
    }
}

pub struct ObjInstance {
    storage: InstanceStorage,
    /// Compile-time class identity (`0` = none / dict / legacy `INIT`).
    pub type_id: u32,
    /// Set when `drop` has run (GC or explicit); drop must not run twice.
    pub finalized: bool,
}

impl Default for ObjInstance {
    fn default() -> Self {
        Self {
            storage: InstanceStorage::Table(Table::default()),
            type_id: 0,
            finalized: false,
        }
    }
}

impl ObjInstance {

    #[must_use]
    pub fn with_type_id(type_id: u32) -> Self {
        Self::with_type_id_and_fields(type_id, 0)
    }

    /// Typed instances (`type_id != 0`) use dense slots of `nfields`.
    /// Counts ≤ [`INSTANCE_INLINE_FIELDS`] stay in the header; larger spill.
    /// `nfields == 0` keeps a named table so legacy `InitTyped` + GetField still works.
    #[must_use]
    pub fn with_type_id_and_fields(type_id: u32, nfields: usize) -> Self {
        let storage = if type_id != 0 && nfields > 0 {
            InstanceStorage::typed_slots(nfields)
        } else {
            InstanceStorage::Table(Table::default())
        };
        Self {
            storage,
            type_id,
            finalized: false,
        }
    }

    pub fn set(&mut self, key: RefString, value: Member) {
        if let InstanceStorage::Table(table) = &mut self.storage {
            table.insert(key, value);
        }
    }

    /// Named read: the table entry, or a typed slot by its known name.
    pub fn get(&self, key: RefString) -> Option<Value> {
        match &self.storage {
            InstanceStorage::Table(table) => table.get(key).map(|m| match m {
                Member::Value(v) => v,
                Member::Object(o) => Value::from(o.addr()),
            }),
            InstanceStorage::Inline { .. } | InstanceStorage::Spill(_) => {
                let slot = common::range_heap_field_slot(self.type_id, &key.as_ref().data)?;
                self.slot(slot)
            }
        }
    }

    pub fn slot(&self, index: usize) -> Option<Value> {
        self.storage.as_slice()?.get(index).copied()
    }

    pub fn set_slot(&mut self, index: usize, value: Value) {
        if let Some(slots) = self.storage.as_mut_slice()
            && let Some(slot) = slots.get_mut(index) {
                *slot = value;
            }
    }

    pub fn slot_len(&self) -> Option<usize> {
        self.storage.as_slice().map(|s| s.len())
    }

    pub fn slots(&self) -> Option<&[Value]> {
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
            storage: InstanceStorage::from_vec(slots),
            type_id,
            finalized: false,
        }
    }

    /// Iterate live `(key, value)` entries in table order (DictEntries).
    /// Typed slot instances have no interned names.
    pub fn iter_fields(&self) -> InstanceFieldIter<'_> {
        match &self.storage {
            InstanceStorage::Table(table) => InstanceFieldIter::Table(table.iter()),
            InstanceStorage::Inline { .. } | InstanceStorage::Spill(_) => InstanceFieldIter::Empty,
        }
    }

    fn mark_members(&self, heap: &Heap, grey_objects: &mut Vec<Object>) {
        match &self.storage {
            InstanceStorage::Table(table) => {
                table.iter().for_each(|(k, v)| {
                    k.mark();
                    Object::mark_member(heap, &v, grey_objects);
                });
            }
            InstanceStorage::Inline { .. } | InstanceStorage::Spill(_) => {
                if let Some(slots) = self.storage.as_slice() {
                    let kinds = heap.class_field_kinds(self.type_id);
                    for (i, v) in slots.iter().enumerate() {
                        let kind = kinds.get(i).copied().unwrap_or(common::WORD_UNKNOWN);
                        #[cfg(feature = "gc-stress")]
                        heap.verify_kinded_word(&format!("class {}", self.type_id), i, kind, *v);
                        if kind != common::WORD_SCALAR {
                            heap.mark_value(*v, grey_objects);
                        }
                    }
                }
            }
        }
    }
}

/// Named-field walk for dicts; empty for dense typed slots.
pub enum InstanceFieldIter<'a> {
    Table(Iter<'a, Member>),
    Empty,
}

impl Iterator for InstanceFieldIter<'_> {
    type Item = (RefString, Member);

    fn next(&mut self) -> Option<Self::Item> {
        match self {
            Self::Table(it) => it.next(),
            Self::Empty => None,
        }
    }
}

impl GcSized for ObjInstance {
    fn size(&self) -> usize {
        // `Table` / inline slots live in the object; only a spill `Vec` is extra.
        std::mem::size_of::<Self>()
            + self.storage.spill_capacity() * std::mem::size_of::<Value>()
    }
}

/// Max payload arity stored inside [`ObjEnum`] without a Rust `Vec`.
///
/// Payload words are raw [`Value`]s (the GC resolves them through the slab),
/// so four fit where two tagged [`Member`]s used to; larger variants spill.
pub const ENUM_INLINE_ARITY: usize = 4;

/// Flat enum payload: inline words up to [`ENUM_INLINE_ARITY`], else a `Vec`.
pub struct EnumPayload {
    inner: EnumPayloadInner,
}

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
pub struct ObjEnum {
    pub tag: u32,
    pub payload: EnumPayload,
    /// Finalizer key for an enum with `fn drop()` (`TagEnumType`); `0` = none.
    /// Unit variants are shared immortals and are never tagged.
    pub type_id: u32,
    /// Set once the finalizer has been claimed, like [`ObjInstance::finalized`].
    pub finalized: bool,
}

impl ObjEnum {
    pub fn new(tag: u32, payload: EnumPayload) -> Self {
        Self {
            tag,
            payload,
            type_id: 0,
            finalized: false,
        }
    }
}

impl GcSized for ObjEnum {
    fn size(&self) -> usize {
        std::mem::size_of::<Self>() + self.payload.spill_capacity() * std::mem::size_of::<Member>()
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
    fn size(&self) -> usize {
        mem::size_of::<Self>() + self.data.accounted_bytes()
    }
}

impl From<&str> for ObjString {
    fn from(value: &str) -> Self {
        Self::new(String::from(value))
    }
}

/// Immutable tuple: up to [`ENUM_INLINE_ARITY`] words live in the object,
/// wider tuples spill (same storage as enum payloads).
pub struct ObjTuple {
    elements: EnumPayload,
}

impl ObjTuple {
    pub fn new(elements: Vec<Value>) -> Self {
        Self {
            elements: EnumPayload::from_vec(elements),
        }
    }

    #[inline]
    pub fn from_slice(elements: &[Value]) -> Self {
        Self {
            elements: EnumPayload::from_slice(elements),
        }
    }

    /// Record the construction site's element word kinds.
    #[inline]
    pub fn with_kinds(self, kinds: u8) -> Self {
        Self {
            elements: self.elements.with_kinds(kinds),
        }
    }

    #[inline]
    pub fn kinds(&self) -> u8 {
        self.elements.kinds()
    }

    #[inline]
    pub fn elements(&self) -> &[Value] {
        &self.elements
    }
}

/// A `Vec` / array object.
///
/// `may_hold_refs` is false only when the last mark scanned every element,
/// found none that could be a heap reference, and nothing was written since.
/// Marking skips such an array (a live `Vec<int>` of millions of words is
/// scanned once, then costs nothing per collection). Every write goes
/// through [`Self::push`] / [`Self::set`] / [`Self::store_indexed`] /
/// [`Self::elements_mut`], which set the flag with one byte store — no
/// compare on the hot path. Not type-based, so boxed values from a generic
/// shared body are covered too.
pub struct ObjArray {
    elements: Vec<Value>,
    may_hold_refs: bool,
    /// Element word kind (`common::WORD_*`) stamped by a typed constructor
    /// (`TagArrayKind`). Only `WORD_POINTER` is ever set: each element is `0`
    /// or an object address, so marking treats them as precise references.
    elem_kind: u8,
}

impl ObjArray {
    /// Conservative: any element may be a reference.
    pub fn new(elements: Vec<Value>) -> Self {
        let may_hold_refs = !elements.is_empty();
        Self {
            elements,
            may_hold_refs,
            elem_kind: common::WORD_UNKNOWN,
        }
    }

    /// Classify `elements` against `heap` up front.
    pub fn from_values(elements: Vec<Value>, heap: &Heap) -> Self {
        let may_hold_refs = elements.iter().any(|v| heap.may_be_ref(*v));
        Self {
            elements,
            may_hold_refs,
            elem_kind: common::WORD_UNKNOWN,
        }
    }

    pub fn with_capacity(n: usize) -> Self {
        Self {
            elements: Vec::with_capacity(n),
            may_hold_refs: false,
            elem_kind: common::WORD_UNKNOWN,
        }
    }

    /// Element word kind (`common::WORD_*`).
    #[inline]
    pub fn elem_kind(&self) -> u8 {
        self.elem_kind
    }

    /// Stamp the static element kind. Only a pointer kind is kept: a scalar
    /// stamp would be unsound for generic bodies that box values.
    #[inline]
    pub fn set_elem_kind(&mut self, kind: u8) {
        if kind == common::WORD_POINTER {
            self.elem_kind = kind;
        }
    }

    #[inline]
    pub fn elements(&self) -> &Vec<Value> {
        &self.elements
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.elements.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.elements.is_empty()
    }

    /// False when no element can be a heap reference (marking skips them).
    #[inline]
    pub fn may_hold_refs(&self) -> bool {
        self.may_hold_refs
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
        unsafe { *self.elements.get_unchecked_mut(i) = v };
    }

    #[inline]
    pub fn push(&mut self, v: Value) {
        self.note();
        self.elements.push(v);
    }

    /// Overwrite element `i` (caller checks bounds).
    #[inline]
    pub fn set(&mut self, i: usize, v: Value) {
        self.note();
        self.elements[i] = v;
    }

    /// Unchecked [`Self::set`]; `i < len` is the caller's proof.
    ///
    /// # Safety
    /// `i` must be in bounds.
    #[inline]
    pub unsafe fn set_unchecked(&mut self, i: usize, v: Value) {
        self.note();
        unsafe { *self.elements.get_unchecked_mut(i) = v };
    }

    /// `elements[index] = value` with the VM's bounds rules: `unchecked` is a
    /// proven index; otherwise out of range returns `false`.
    #[inline(always)]
    pub fn store_indexed(&mut self, index: i64, value: Value, unchecked: bool) -> bool {
        let elements: &mut [Value] = &mut self.elements;
        let len = elements.len();
        if unchecked {
            let idx = index as usize;
            promise!(index >= 0);
            promise!(idx < len);
            unsafe {
                *elements.get_unchecked_mut(idx) = value;
            }
        } else if index >= 0 && (index as usize) < len {
            unsafe {
                *elements.get_unchecked_mut(index as usize) = value;
            }
        } else {
            return false;
        }
        self.note();
        true
    }

    /// Raw element access for writes the classifier does not see. Marks the
    /// array as possibly holding references (always safe).
    #[inline]
    pub fn elements_mut(&mut self) -> &mut Vec<Value> {
        self.may_hold_refs = true;
        &mut self.elements
    }

    /// Element access for writes that add no new values (pop, remove,
    /// truncate, clear, reorder): the flag stays as is.
    #[inline]
    pub fn elements_mut_no_new_values(&mut self) -> &mut Vec<Value> {
        &mut self.elements
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
    /// `(ip, sp_offset)` pairs; `sp_offset` is relative to the coroutine segment base.
    pub saved_frames: Vec<(usize, usize)>,
    /// Value from the resumer's `resume h with v` (delivered at the next binding yield).
    pub pending_send: Value,
    /// Active `yield from` delegate, if any.
    pub yield_from: Option<RefCoroutine>,
    /// Coroutine delegating to this one via `yield from` (back edge of
    /// `yield_from`). Traced: a running delegate keeps its parent alive.
    pub delegator: Option<RefCoroutine>,
    /// Outer continuation IP when the delegate completes.
    pub yield_from_resume_ip: usize,
}

/// Heap-allocated boxed value for the generics runtime.
pub struct ObjBoxed {
    /// `ValueTag` discriminant stored as a raw `u16`.
    pub tag: u16,
    /// The wrapped payload.
    pub payload: Member,
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

/// Heap-allocated polymorphic function descriptor.
pub struct ObjPolyFn {
    /// Bytecode entry offset of the monomorphised body.
    pub entry: u32,
    /// Number of type parameters expected (reserved for future use).
    pub type_arity: u8,
    /// Dictionary evidence captured when this value escaped a constrained
    /// scope. `None` leaves the position for application-time evidence.
    pub captured_dicts: Vec<Option<Member>>,
}

/// First-class monomorphic function / partial / explicit-capture lambda.
pub struct ObjFn {
    /// Bytecode entry of the body.
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

/// Join handle for a spawned OS thread (host `JoinState` lives outside the VM heap).
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

impl GcSized for ObjTuple {
    fn size(&self) -> usize {
        mem::size_of::<Self>() + self.elements.spill_capacity() * mem::size_of::<Value>()
    }
}

impl GcSized for ObjArray {
    fn size(&self) -> usize {
        mem::size_of::<Self>() + self.elements.capacity() * mem::size_of::<Value>()
    }
}

impl GcSized for ObjCoroutine {
    fn size(&self) -> usize {
        // `saved_stack` / `saved_frames` use Rust's allocator, not the VM
        // heap byte counter (same contract as `ObjInstance`).
        mem::size_of::<Self>()
    }
}

impl GcSized for ObjBoxed {
    fn size(&self) -> usize {
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

impl GcSized for ObjPolyFn {
    fn size(&self) -> usize {
        mem::size_of::<Self>()
    }
}

impl GcSized for ObjFn {
    fn size(&self) -> usize {
        // Vec payloads use Rust's allocator (same as ObjArray elements).
        mem::size_of::<Self>()
    }
}

impl fmt::Display for ObjTuple {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "({})",
            self.elements
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
            self.elements
                .iter()
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
}

/// C signature metadata for an FFI function.
#[derive(Clone, Debug)]
pub struct FunctionSig {
    pub name: String,
    /// Fixed-prefix arity (`nfixed` when [`Self::variadic`]).
    pub arity: usize,
    pub arg_types: Vec<FfiType>,
    pub ret_type: FfiType,
    /// C-style varargs, CIF rebuilt per invoke with `Cif::new_variadic`.
    pub variadic: bool,
}

impl FunctionSig {
    pub fn from_ffi_signature(sig: &crate::ffi::FfiSignature) -> Self {
        Self {
            name: sig.name.clone(),
            arity: sig.arity(),
            arg_types: sig.args.clone(),
            ret_type: sig.ret,
            variadic: sig.variadic,
        }
    }
}

/// A declared FFI function with a prepared libffi call interface.
pub struct RegisteredFunction {
    pub sig: FunctionSig,
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
}

/// Prefix of every managed allocation. Kind reconstructs [`Object`] without
/// a live-address HashSet (`kind == 0` is a free slot).
#[repr(C)]
struct GcHeader {
    kind: Cell<u8>,
    marked: Cell<bool>,
    /// Allocated while a sweep was running; that sweep skips it once.
    fresh: Cell<bool>,
}

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
        &mut self.data
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

    /// Mutable access to the inner payload (single-threaded VM only).
    #[allow(clippy::mut_from_ref)] // `Gc<T>` is a copyable slot handle; the VM mutates the payload through shared `&self`
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

#[cfg(test)]
mod tests {
    use super::*;

    fn live_object_addrs(heap: &Heap) -> std::collections::HashSet<u64> {
        let mut addrs = std::collections::HashSet::new();
        for obj in heap {
            addrs.insert(obj.addr());
        }
        addrs
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
        assert_eq!(one.slot_len(), Some(1));

        let two = ObjInstance::with_type_id_and_fields(1, INSTANCE_INLINE_FIELDS);
        assert!(two.slots_are_inline());
        assert_eq!(two.slot_len(), Some(INSTANCE_INLINE_FIELDS));

        let spilled = ObjInstance::with_type_id_and_fields(1, INSTANCE_INLINE_FIELDS + 1);
        assert!(!spilled.slots_are_inline());
        assert_eq!(spilled.slot_len(), Some(INSTANCE_INLINE_FIELDS + 1));

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
        let (tup, _) = heap.alloc(ObjTuple::from_slice(&words).with_kinds(kinds), Object::Tuple);

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
        assert!(heap.find_object_by_addr(tagged.raw() as u64).is_none());
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
            Some(Object::Tuple(t)) => assert!(t.as_ref().elements().is_empty()),
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
        assert_eq!(gc.as_ref().elements()[0].as_int(), 10);
        assert_eq!(gc.as_ref().elements()[1].as_int(), 20);
        assert_eq!(gc.as_ref().elements().len(), 2);
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
        heap.alloc_bytes = GC_NEXT_THRESHOLD + 1;
        assert!(heap.should_collect());
        heap.enter_epoch_stw(&lock);
        assert!(!heap.should_collect(), "past threshold, under epoch ceiling");
        heap.alloc_bytes = heap.epoch_gc_ceiling + 1;
        assert!(heap.should_collect(), "past the epoch ceiling");
        heap.exit_epoch_stw();
        heap.alloc_bytes = GC_NEXT_THRESHOLD + 1;
        assert!(heap.should_collect(), "normal threshold again after the epoch");
        heap.alloc_bytes = 0;
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
}

