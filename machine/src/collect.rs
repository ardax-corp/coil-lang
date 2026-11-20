//! The collector's root walk. A host (the native driver, or the MIR
//! interpreter) reports the roots it holds itself; the [`Runtime`] adds its own (statics, the
//! scheduler's tasks, FFI handles and arguments, native frames' root
//! records). Minor collections and evacuation forward the precise ones
//! and pin the rest (`docs/internals/gc-evacuation.md`).

use std::sync::Arc;

use common::Value;

use crate::memory::RootKind;
use crate::runtime::Runtime;

thread_local! {
    /// Per static slot word kind of the program installed on this thread
    /// (archive minor 29). Only GC root walks read it; kept out of the
    /// `Heap` so its hot-loop layout is unchanged.
    pub(crate) static STATIC_KINDS: std::cell::RefCell<Arc<Vec<u8>>> = std::cell::RefCell::new(Arc::default());
    /// Evacuation pacing for this thread's collector: `(cycles since the
    /// last attempt, collections to wait)`.
    static COMPACT_PACING: std::cell::Cell<(u32, u32)> = const { std::cell::Cell::new((0, COMPACT_WINDOW)) };
    /// Off with `COIL_GC_COMPACT=0` (debugging a suspected move bug), in
    /// this crate's unit tests only; other builds always evacuate.
    static COMPACT_ENABLED: bool = !cfg!(test) || std::env::var_os("COIL_GC_COMPACT").is_none_or(|v| v != "0");
}

/// Install `kinds` as the static word kinds of the program this thread
/// runs (what a GC root walk reads static slots by) and return the ones
/// it replaces. A runtime that runs on a thread already serving another program
/// (the compile-time runtime) installs its own and puts the old ones back.
pub fn swap_static_word_kinds(kinds: Arc<Vec<u8>>) -> Arc<Vec<u8>> {
    STATIC_KINDS.with_borrow_mut(|k| std::mem::replace(k, kinds))
}

/// Collections between evacuation attempts outside stress mode, and the
/// ceiling unproductive attempts back off to.
const COMPACT_WINDOW: u32 = 8;
const COMPACT_WINDOW_MAX: u32 = 256;

/// A root the host holds itself: a heap address it cannot rewrite.
#[derive(Clone, Copy)]
pub(crate) struct HostRoot(pub(crate) u64, pub(crate) RootKind);

/// What the collector needs from the host that runs the program.
pub(crate) trait GcHost {
    fn rt(&self) -> &Runtime;
    fn rt_mut(&mut self) -> &mut Runtime;
    /// Every root the host holds besides the Runtime's.
    fn host_roots(&self, visit: &mut dyn FnMut(HostRoot));
    /// Whether objects may move now (no raw heap word outside the roots).
    fn can_move(&self) -> bool;
    /// Drop caches that name objects which may move or die.
    fn clear_caches(&mut self) {}
    /// Before roots are collected for a mark.
    fn before_mark(&mut self) {}
    /// Run what an unreachable object owes: its `drop` at `pc`, or the
    /// close of a generator (`None`).
    fn run_pending(&mut self, val: Value, pc: Option<u32>);
    /// A collection was asked for in a shared-heap epoch: end the epoch.
    fn epoch_abort(&mut self) {
        if let Some(e) = &self.rt().shared_epoch {
            e.abort();
        }
    }
    /// A safepoint wanted a collection while the heap is borrowed.
    fn borrowed_heap_wants_collect(&mut self) {}
    /// The heap is over its byte cap after a safepoint.
    /// A cycle starts: a major one, or a minor one that runs.
    fn note_cycle(&self, _minor: bool) {}
}

impl Runtime {
    /// Static slots by their compile-time word kind (archive minor 29): a
    /// scalar static is no root, a pointer static is a precise root (a
    /// moving collector rewrites it), anything else stays ambiguous.
    pub(crate) fn for_each_static_root(&self, visit: &mut dyn FnMut(usize, u64, RootKind)) {
        let heap = &self.heap;
        let kinds = STATIC_KINDS.with_borrow(Arc::clone);
        for (i, v) in self.statics.iter().enumerate() {
            let kind = match kinds.get(i).copied().unwrap_or(common::WORD_UNKNOWN) {
                common::WORD_SCALAR => continue,
                common::WORD_POINTER => RootKind::Precise,
                _ => RootKind::Ambiguous,
            };
            let addr = v.heap_addr();
            let live = addr != 0 && heap.find_object_by_addr(addr).is_some();
            #[cfg(feature = "gc-stress")]
            assert!(addr == 0 || live || kind != RootKind::Precise, "gc-stress: pointer static {i} holds a non-object");
            if live {
                visit(i, addr, kind);
            }
        }
    }

    /// The Runtime's roots outside the statics and native frames. None can
    /// be rewritten by a moving collector.
    pub(crate) fn for_each_handle_root(&self, visit: &mut dyn FnMut(u64, RootKind)) {
        if let Some(sched) = &self.sched {
            for rec in sched.tasks.values() {
                let inner = rec.inner.iter().map(|(c, _, _)| *c);
                for coro in rec.coro.into_iter().chain(inner) {
                    visit(coro.as_ptr() as u64, RootKind::Precise);
                }
            }
        }
        // `FfiLoad` keeps `ObjLibrary` in `userland_libraries` for the
        // program's lifetime; the Coil handle is only an addr. Root those
        // keys so a collection cannot sweep a live dload. Keyed by address,
        // so they must not move.
        for &addr in self.ffi.userland_libraries.keys() {
            visit(addr, RootKind::Pinned);
        }
        // In-flight FFI arguments: C may hold pointers into them.
        for &v in &self.ffi_arg_roots {
            let addr = v.heap_addr();
            if addr != 0 && self.heap.find_object_by_addr(addr).is_some() {
                visit(addr, RootKind::Pinned);
            }
        }
    }

    /// Heap words native frames hold across a runtime call or a safepoint, as
    /// slot pointers: `true` when the record cannot say the word is one.
    pub(crate) fn for_each_native_word(&self, visit: &mut dyn FnMut(*mut u64, bool)) {
        for &(ctx, _) in &self.native_roots {
            // SAFETY: a native call in a collection keeps its context alive.
            unsafe { &*ctx }.for_each_root(&mut *visit);
        }
    }

    /// The native frames' heap words, as roots.
    pub(crate) fn for_each_native_root(&self, visit: &mut dyn FnMut(u64, RootKind)) {
        let heap = &self.heap;
        self.for_each_native_word(&mut |w, ambiguous| {
            // SAFETY: a root slot of a live record.
            let addr = Value::from(unsafe { *w }).heap_addr();
            if addr != 0 && heap.find_object_by_addr(addr).is_some() {
                visit(addr, if ambiguous { RootKind::Ambiguous } else { RootKind::Precise });
            }
        });
    }

    /// Every Runtime root that resolves to a live object.
    pub(crate) fn for_each_root(&self, visit: &mut dyn FnMut(u64, RootKind)) {
        self.for_each_static_root(&mut |_, addr, kind| visit(addr, kind));
        self.for_each_handle_root(visit);
        self.for_each_native_root(visit);
    }
}

impl Runtime {
    /// Drop PC for a tagged instance or enum whose finalizer has not run.
    pub(crate) fn pending_finalizer(&self, obj: crate::memory::Object) -> Option<u32> {
        use crate::memory::Object;
        let type_id = match obj {
            Object::Instance(gc) => gc.as_ref().type_id,
            Object::Enum(gc) => gc.as_ref().type_id,
            _ => return None,
        };
        if type_id == 0 || obj.is_finalized() {
            return None;
        }
        self.finalizer_by_type.get(&type_id).copied()
    }

    /// What `obj` still owes: its `drop` pc, or a close for a generator.
    pub(crate) fn pending_cleanup(&self, obj: crate::memory::Object) -> Option<Option<u32>> {
        match obj {
            crate::memory::Object::Coroutine(gc) => {
                let c = gc.as_ref();
                (c.closeable && c.state == crate::memory::CoroState::Suspended).then_some(None)
            }
            _ => self.pending_finalizer(obj).map(Some),
        }
    }

    /// The unmarked objects that owe a `drop` (its pc) or, for a generator
    /// suspended with a `defer` armed, a close (`None`).
    pub(crate) fn queue_unmarked_finalizers(&mut self) -> Vec<(Value, Option<u32>)> {
        if self.finalizer_by_type.is_empty() && !self.closeables {
            return Vec::new();
        }
        // The heap lists its finalizable objects: no walk of the heap (#1870).
        let unmarked = self.heap.unmarked_finalizable();
        unmarked
            .into_iter()
            .filter_map(|obj| Some((Value::from(obj.addr()), self.pending_cleanup(obj)?)))
            .collect()
    }

    /// Mark the finalizer of `v` as run; `false` when it ran already.
    pub(crate) fn claim_finalizer(&self, v: Value) -> bool {
        use crate::memory::Object;
        match self.heap.find_object_by_addr(v.bits()) {
            Some(obj @ Object::Instance(_)) => obj.claim_finalizer(),
            // Untagged enums (unit variants: shared immortals) are never
            // finalized, so an explicit `drop()` on one just runs the body.
            Some(obj @ Object::Enum(gc)) => gc.as_ref().type_id == 0 || obj.claim_finalizer(),
            _ => false,
        }
    }

    /// Interned string handles are not roots: a sweep may have freed them.
    pub(crate) fn invalidate_program_string_cache(&mut self) {
        self.program_string_cache.fill(Value::default());
    }

    fn unmark_heap(&self) {
        for obj in self.heap.objects() {
            obj.unmark();
        }
    }
}

/// Every root of `h` that resolves to a live heap object (the host's, then
/// the Runtime's), tagged with how a moving collector could treat it.
/// Heap-owned roots are seeded by [`crate::memory::Heap::take_gc_roots`].
pub(crate) fn for_each_root<H: GcHost + ?Sized>(h: &H, visit: &mut dyn FnMut(u64, RootKind)) {
    let heap = &h.rt().heap;
    h.host_roots(&mut |HostRoot(addr, kind)| {
        if addr != 0 && heap.find_object_by_addr(addr).is_some() {
            visit(addr, kind);
        }
    });
    h.rt().for_each_root(visit);
}

/// The heap's own roots plus every root of `h`, for a mark.
pub(crate) fn root_addrs<H: GcHost + ?Sized>(h: &mut H) -> Vec<u64> {
    h.before_mark();
    let mut roots = h.rt_mut().heap.take_gc_roots();
    for_each_root(h, &mut |addr, _| roots.push(addr));
    roots
}

/// Mark the heap from every root of `h`.
pub(crate) fn mark<H: GcHost + ?Sized>(h: &mut H) {
    let roots = root_addrs(h);
    h.rt_mut().heap.mark_from_roots(&roots);
    h.rt_mut().heap.restore_gc_roots(roots);
}

/// The roots certainly live: the host's precise slots and handles and the
/// Runtime's roots, not words a conservative scan finds (they may be stale
/// copies). `--early-free check` marks from these.
pub(crate) fn certain_root_addrs<H: GcHost + ?Sized>(h: &mut H) -> Vec<u64> {
    let mut roots = h.rt_mut().heap.take_gc_roots();
    let heap = &h.rt().heap;
    h.host_roots(&mut |HostRoot(addr, _)| {
        if addr != 0 && heap.find_object_by_addr(addr).is_some() {
            roots.push(addr);
        }
    });
    h.rt().for_each_root(&mut |addr, _| roots.push(addr));
    roots
}

/// Roots split for a moving collection: statics to rewrite, and the
/// addresses that must stay where they are.
struct MoveRoots {
    must_statics: Vec<usize>,
    pins: Vec<u64>,
}

/// Split the roots of `h` for a move.
fn move_roots<H: GcHost + ?Sized>(h: &mut H) -> MoveRoots {
    let mut pins = h.rt_mut().heap.take_gc_roots();
    h.host_roots(&mut |HostRoot(addr, _)| pins.push(addr));
    let mut must_statics = Vec::new();
    h.rt().for_each_static_root(&mut |i, addr, kind| match kind {
        RootKind::Precise => must_statics.push(i),
        _ => pins.push(addr),
    });
    h.rt().for_each_handle_root(&mut |addr, _| pins.push(addr));
    MoveRoots { must_statics, pins }
}

/// Minor collection: copy the surviving young objects out of the nursery
/// and reset it. Where nothing may move it promotes them in place instead.
/// Native frames reload their heap words after a collection: their
/// precise words move like precise slots.
pub(crate) fn minor<H: GcHost + ?Sized>(h: &mut H) {
    if !h.rt().heap.has_young_in_use() || !h.rt().heap.gc_is_idle() {
        return;
    }
    let can_move = h.can_move() && !h.rt().heap.epoch_stw() && !h.rt().heap.is_borrowed();
    if !can_move {
        h.rt_mut().heap.promote_young();
        h.rt_mut().heap.pretenure_after_promote();
        return;
    }
    // A cache may name an object that moves.
    h.clear_caches();
    let MoveRoots { must_statics, mut pins } = move_roots(h);
    let rt = h.rt();
    let mut must_native: Vec<*mut u64> = Vec::new();
    rt.for_each_native_word(&mut |w, ambiguous| {
        if ambiguous {
            // SAFETY: the slot is a live word of a native root record.
            let addr = Value::from(unsafe { *w }).heap_addr();
            if addr != 0 && rt.heap.find_object_by_addr(addr).is_some() {
                pins.push(addr);
            }
        } else {
            must_native.push(w);
        }
    });
    let precise: Vec<u64> = must_statics
        .iter()
        .map(|&i| rt.statics[i].heap_addr())
        .chain(must_native.iter().filter_map(|&w| {
            // SAFETY: the slot is a live word of a native root record.
            let addr = Value::from(unsafe { *w }).heap_addr();
            (addr != 0 && rt.heap.find_object_by_addr(addr).is_some()).then_some(addr)
        }))
        .collect();
    let rt = h.rt_mut();
    let plan = rt.heap.minor_plan(&pins, &precise);
    rt.heap.restore_gc_roots(pins);
    for w in must_native {
        // SAFETY: as above.
        unsafe {
            if let Some(v) = plan.forward(Value::from(*w)) {
                *w = v.bits();
            }
        }
    }
    for i in must_statics {
        if let Some(v) = plan.forward(rt.statics[i]) {
            rt.statics[i] = v;
        }
    }
    #[cfg(feature = "gc-stress")]
    for_each_root(h, &mut |addr, kind| {
        assert!(
            !plan.moved().contains_key(&addr),
            "gc-stress: {kind:?} root still names young {addr:#x} after a minor collection"
        );
    });
    h.rt_mut().heap.minor_finish(plan);
}

/// Evacuate sparse chunks after a finished cycle
/// (`docs/internals/gc-evacuation.md`). Precise slots and statics are
/// rewritten; every other root pins its target, native frames' words
/// included. `gc-stress` moves everything movable each cycle and checks
/// that no root still names an old slot.
pub(crate) fn compact<H: GcHost + ?Sized>(h: &mut H) {
    let heap = &h.rt().heap;
    if !COMPACT_ENABLED.with(|e| *e) || !h.can_move() || heap.epoch_stw() || heap.is_borrowed() || !heap.gc_is_idle() {
        return;
    }
    // Outside stress mode, wait `COMPACT_WINDOW` collections between
    // attempts; unproductive attempts double it (the analysis walks the
    // whole heap), a productive one resets it.
    let stress = cfg!(feature = "gc-stress");
    let (cycles, window) = COMPACT_PACING.get();
    if !stress && cycles + 1 < window {
        COMPACT_PACING.set((cycles + 1, window));
        return;
    }
    COMPACT_PACING.set((0, window));
    let MoveRoots { must_statics, mut pins } = move_roots(h);
    h.rt().for_each_native_root(&mut |addr, _| pins.push(addr));
    let rt = h.rt_mut();
    let Some(plan) = rt.heap.evacuate_plan(&pins, stress) else {
        rt.heap.restore_gc_roots(pins);
        COMPACT_PACING.set((0, (window * 2).min(COMPACT_WINDOW_MAX)));
        return;
    };
    rt.heap.restore_gc_roots(pins);
    for i in must_statics {
        if let Some(v) = plan.forward(rt.statics[i]) {
            rt.statics[i] = v;
        }
    }
    #[cfg(feature = "gc-stress")]
    {
        h.rt().heap.verify_evacuation(&plan);
        for_each_root(h, &mut |addr, kind| {
            assert!(!plan.moved().contains_key(&addr), "gc-stress: {kind:?} root still names moved {addr:#x}");
        });
    }
    let evacuation = h.rt_mut().heap.evacuate_finish(plan);
    h.clear_caches();
    COMPACT_PACING.set((0, if evacuation.capped { 1 } else { COMPACT_WINDOW }));
    #[cfg(feature = "gc-stats")]
    eprintln!("gc-stats evacuate: {evacuation:?}");
    let _ = evacuation;
}

/// Complete a cycle (explicit `gc::collect`, a heap over its cap, tests).
/// Finishes any in-flight incremental sweep first so unmarked survivors
/// are not freed. One asked for while a finalizer runs is run after.
pub(crate) fn collect<H: GcHost + ?Sized>(h: &mut H) {
    if h.rt().heap.epoch_stw() {
        h.epoch_abort();
        return;
    }
    if h.rt().gc_in_progress {
        h.rt_mut().gc_deferred = true;
        return;
    }
    h.rt_mut().gc_in_progress = true;
    loop {
        h.rt_mut().gc_deferred = false;
        h.note_cycle(false);
        if h.rt().heap.gc_is_sweeping() {
            h.rt_mut().heap.finish_sweep();
        }
        minor_counted(h);
        h.clear_caches();
        mark(h);
        let queue = h.rt_mut().queue_unmarked_finalizers();
        if !queue.is_empty() {
            let heap = &h.rt().heap;
            let mut gray = Vec::new();
            for (val, _) in &queue {
                if let Some(obj) = heap.find_object_by_addr(val.bits()) {
                    obj.mark(&mut gray);
                    obj.mark_references(heap, &mut gray);
                }
            }
            while let Some(obj) = gray.pop() {
                obj.mark_references(heap, &mut gray);
            }
            for (val, pc) in queue {
                h.run_pending(val, pc);
            }
            h.rt().unmark_heap();
            mark(h);
        }
        #[cfg(feature = "gc-stats")]
        note_census(h);
        check_early_frees(h);
        let rt = h.rt_mut();
        rt.heap.clear_dead_weaks();
        // SAFETY: all reachable objects were marked above; dead weaks cleared.
        unsafe { rt.heap.sweep() };
        rt.invalidate_program_string_cache();
        if !rt.gc_deferred {
            break;
        }
    }
    compact(h);
    h.rt_mut().gc_in_progress = false;
}

/// `gc-stats`: census of the live set just marked, before weaks clear.
#[cfg(feature = "gc-stats")]
fn note_census<H: GcHost + ?Sized>(h: &H) {
    let mut roots = Vec::new();
    for_each_root(h, &mut |addr, kind| roots.push((addr, kind)));
    eprintln!("{}", h.rt().heap.census(&roots));
}

/// `--early-free check`, with a complete mark: mark again from the roots
/// that are certainly live, fail if that reaches an object an early free
/// gave up, then restore the full mark.
fn check_early_frees<H: GcHost + ?Sized>(h: &mut H) {
    if !h.rt().heap.has_doomed() {
        return;
    }
    h.rt().unmark_heap();
    let roots = certain_root_addrs(h);
    let rt = h.rt_mut();
    rt.heap.mark_from_roots(&roots);
    rt.heap.restore_gc_roots(roots);
    rt.heap.check_doomed();
    rt.unmark_heap();
    mark(h);
}

/// `gc_region_enter` / `gc_region_exit` around a `region` block. The
/// outermost one starts and ends with a minor collection, so what the
/// block allocated and dropped is reclaimed when it ends.
pub(crate) fn region<H: GcHost + ?Sized>(h: &mut H, enter: bool) {
    if !h.rt().heap.nursery() {
        return;
    }
    let outermost = if enter { h.rt_mut().heap.region_enter() } else { h.rt_mut().heap.region_exit() };
    if !outermost {
        return;
    }
    if h.rt().heap.gc_phase() == crate::memory::GcPhase::Sweeping {
        finish_sweep(h);
    }
    if !enter {
        h.rt_mut().invalidate_program_string_cache();
    }
    minor_counted(h);
}

/// [`minor`], counted when it runs.
pub(crate) fn minor_counted<H: GcHost + ?Sized>(h: &mut H) {
    if h.rt().heap.has_young_in_use() && h.rt().heap.gc_is_idle() {
        h.note_cycle(true);
    }
    minor(h);
}

/// Mark to completion / lazy sweep at an allocation safepoint.
pub(crate) fn safepoint<H: GcHost + ?Sized>(h: &mut H) {
    if h.rt().gc_in_progress {
        return;
    }
    let heap = &h.rt().heap;
    if heap.epoch_stw() {
        if !heap.gc_is_idle() || heap.should_collect() {
            let borrowed = heap.is_borrowed();
            h.epoch_abort();
            if borrowed {
                h.borrowed_heap_wants_collect();
            }
        }
        return;
    }
    match heap.gc_phase() {
        crate::memory::GcPhase::Idle => {
            if h.rt().heap.should_collect_young() {
                minor_counted(h);
            }
            if h.rt().heap.should_collect_old() {
                start_incremental(h);
            }
        }
        crate::memory::GcPhase::Marking => mark_slice(h),
        crate::memory::GcPhase::Sweeping if heap.should_collect_young() => {
            finish_sweep(h);
            minor_counted(h);
        }
        crate::memory::GcPhase::Sweeping => sweep_slice(h),
    }
    if h.rt().heap.get().over_byte_cap() {
        over_cap(h);
    }
}

/// At a safepoint with the heap over its cap: collect fully, and if it
/// still is, stop at the next step charge (the one place a run looks, so
/// the hot path pays nothing).
#[cold]
#[inline(never)]
fn over_cap<H: GcHost + ?Sized>(h: &mut H) {
    if !h.rt().heap_cap_hit {
        collect(h);
        if !h.rt().heap.get().over_byte_cap() {
            return;
        }
        h.rt_mut().heap_cap_hit = true;
    }
    h.rt_mut().fuel = 1;
}

/// Open a cycle: mark to completion now, leave the sweep to later
/// safepoints.
pub(crate) fn start_incremental<H: GcHost + ?Sized>(h: &mut H) {
    h.note_cycle(false);
    // Empty the nursery first: the major cycle then sees only old objects.
    minor_counted(h);
    // A cache is not a root (the dense-index one kept the last indexed
    // array alive after its frame returned); drop it before objects can be
    // freed or moved under it.
    h.clear_caches();
    let roots = root_addrs(h);
    h.rt_mut().heap.begin_mark(&roots);
    h.rt_mut().heap.restore_gc_roots(roots);
    mark_slice(h);
}

fn remark<H: GcHost + ?Sized>(h: &mut H) {
    let roots = root_addrs(h);
    h.rt_mut().heap.remark_roots(&roots);
    h.rt_mut().heap.restore_gc_roots(roots);
}

/// Finish a pending lazy sweep in one go.
pub(crate) fn finish_sweep<H: GcHost + ?Sized>(h: &mut H) {
    h.rt_mut().heap.finish_sweep();
    h.rt_mut().invalidate_program_string_cache();
    compact(h);
}

fn mark_slice<H: GcHost + ?Sized>(h: &mut H) {
    // Drain mark at this safepoint: the mutator never runs with gray
    // objects (finalizers below run after the drain), so stores need no
    // write barrier. See `Heap::resurrect_during_mark`.
    while !h.rt_mut().heap.mark_quantum(usize::MAX) {}
    remark(h);
    while !h.rt_mut().heap.mark_quantum(usize::MAX) {}
    let queue = h.rt_mut().queue_unmarked_finalizers();
    if !queue.is_empty() {
        let heap = &mut h.rt_mut().heap;
        for (val, _) in &queue {
            if let Some(obj) = heap.find_object_by_addr(val.bits()) {
                heap.shade_for_finalizer(obj);
            }
        }
        while !h.rt_mut().heap.mark_quantum(usize::MAX) {}
        h.rt_mut().gc_in_progress = true;
        for (val, pc) in queue {
            h.run_pending(val, pc);
        }
        h.rt_mut().gc_in_progress = false;
        remark(h);
        while !h.rt_mut().heap.mark_quantum(usize::MAX) {}
    }
    #[cfg(feature = "gc-stats")]
    note_census(h);
    check_early_frees(h);
    let rt = h.rt_mut();
    rt.heap.clear_dead_weaks();
    rt.heap.begin_sweep();
    // Intern table is unlinked; unmarked literals will be freed across
    // later sweep quantums. Drop the STRING index cache now so the mutator
    // cannot reload a pointer that this cycle will dealloc (COI-410).
    rt.invalidate_program_string_cache();
    sweep_slice(h);
}

fn sweep_slice<H: GcHost + ?Sized>(h: &mut H) {
    let rt = h.rt_mut();
    let n = rt.heap.gc_sweep_quantum();
    if rt.heap.sweep_quantum(n) {
        rt.invalidate_program_string_cache();
        compact(h);
    }
}

/// Drain the collector and open a Layer A steal epoch on `h`'s heap (or
/// join the one open).
pub(crate) fn begin_shared_steal<H: GcHost + ?Sized>(
    h: &mut H,
) -> Result<Arc<crate::shared_heap::SharedHeapEpoch>, crate::thread::ThreadErrorTag> {
    if let Some(e) = &h.rt().shared_epoch {
        e.jobs.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        return Ok(Arc::clone(e));
    }
    // A lazy sweep left over from the last cycle only needs finishing;
    // a fresh full collect here would re-mark the whole heap.
    if h.rt().heap.get().gc_is_sweeping() {
        finish_sweep(h);
    }
    if !h.rt().heap.get().gc_is_idle() || h.rt().heap.get().should_collect() {
        collect(h);
    }
    if !h.rt().heap.get().gc_is_idle() {
        return Err(crate::thread::ThreadErrorTag::Other);
    }
    let rt = h.rt_mut();
    let ptr = rt.heap.owned_ptr();
    let epoch = crate::shared_heap::SharedHeapEpoch::new(ptr);
    rt.heap.owned_mut().enter_epoch_stw(epoch.alloc_lock());
    epoch.jobs.store(1, std::sync::atomic::Ordering::SeqCst);
    rt.shared_epoch = Some(Arc::clone(&epoch));
    Ok(epoch)
}

/// Close one shared-heap job. `published` is the join result, which no
/// root holds yet: a collection that runs now keeps it.
pub(crate) fn end_shared_steal<H: GcHost + ?Sized>(h: &mut H, published: Value) {
    let rt = h.rt_mut();
    let Some(e) = &rt.shared_epoch else {
        return;
    };
    if e.jobs.fetch_sub(1, std::sync::atomic::Ordering::SeqCst) != 1 {
        return;
    }
    // Exit STW on the epoch Heap (root owned slab, or the borrowed ptr).
    // Every worker flushed its batch before publishing; this thread's goes
    // back while the lock is still installed.
    unsafe {
        (*e.heap_ptr()).flush_epoch_batch();
        (*e.heap_ptr()).exit_epoch_stw();
    }
    rt.shared_epoch = None;
    if rt.heap.is_borrowed() || !rt.heap.get().should_collect() {
        return;
    }
    // Same incremental start as an alloc safepoint: mark now (with
    // `published` rooted), sweep lazily from later safepoints.
    rt.ffi_arg_roots.push(published);
    if rt.gc_in_progress {
        collect(h);
    } else {
        start_incremental(h);
    }
    h.rt_mut().ffi_arg_roots.pop();
}
