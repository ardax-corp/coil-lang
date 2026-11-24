//! The collector's root walk. A host (the native driver, or the MIR
//! interpreter) reports the roots it holds itself; the [`Runtime`] adds its own (statics, the
//! scheduler's tasks, FFI handles and arguments). The cycle collector
//! reads them ([`collect_cycles`]); counts free everything else.
//!
//! Frames are no roots: every word a frame uses is owned by a count, its
//! own or a holder's that outlives the use, so trial deletion sees it as
//! an outside owner already.

use std::sync::Arc;

use common::Value;

use crate::memory::RootKind;
use crate::runtime::Runtime;

thread_local! {
    /// Per static slot word kind of the program installed on this thread
    /// (archive minor 29). Only GC root walks read it; kept out of the
    /// `Heap` so its hot-loop layout is unchanged.
    pub(crate) static STATIC_KINDS: std::cell::RefCell<Arc<Vec<u8>>> = std::cell::RefCell::new(Arc::default());
}

/// Install `kinds` as the static word kinds of the program this thread
/// runs (what a GC root walk reads static slots by) and return the ones
/// it replaces. A runtime that runs on a thread already serving another program
/// (the compile-time runtime) installs its own and puts the old ones back.
pub fn swap_static_word_kinds(kinds: Arc<Vec<u8>>) -> Arc<Vec<u8>> {
    STATIC_KINDS.with_borrow_mut(|k| std::mem::replace(k, kinds))
}

/// A root the host holds itself: a heap address it cannot rewrite.
#[derive(Clone, Copy)]
pub(crate) struct HostRoot(pub(crate) u64, pub(crate) RootKind);

/// What the collector needs from the host that runs the program.
pub(crate) trait GcHost {
    fn rt(&self) -> &Runtime;
    fn rt_mut(&mut self) -> &mut Runtime;
    /// Every root the host holds besides the Runtime's.
    fn host_roots(&self, visit: &mut dyn FnMut(HostRoot));
    /// Run what an unreachable object owes: its `drop` at `pc`, or the
    /// close of a generator (`None`).
    fn run_pending(&mut self, val: Value, pc: Option<u32>);
    /// Run the `drop`s of the objects counts released at zero
    /// ([`crate::memory::Heap::take_rc_due`]), then free them.
    fn run_due_drops(&mut self) {}
    /// A collection was asked for in a shared-heap epoch: end the epoch.
    fn epoch_abort(&mut self) {
        if let Some(e) = &self.rt().shared_epoch {
            e.abort();
        }
    }
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
            #[cfg(feature = "rc-check")]
            assert!(addr == 0 || live || kind != RootKind::Precise, "rc-check: pointer static {i} holds a non-object");
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

    /// Every Runtime root that resolves to a live object.
    pub(crate) fn for_each_root(&self, visit: &mut dyn FnMut(u64, RootKind)) {
        self.for_each_static_root(&mut |_, addr, kind| visit(addr, kind));
        self.for_each_handle_root(visit);
        // Released at count zero, their `drop` still to run.
        for &addr in &self.heap.rc_due {
            visit(addr, RootKind::Ambiguous);
        }
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

/// Complete a cycle (explicit `gc::collect`, a heap over its cap, tests):
/// the `drop`s counts left due, then the cycle collector. One asked for
/// while a `drop` runs is run after.
pub(crate) fn collect<H: GcHost + ?Sized>(h: &mut H) {
    if h.rt().heap.epoch_stw() {
        h.epoch_abort();
        return;
    }
    if h.rt().gc_in_progress {
        h.rt_mut().gc_deferred = true;
        return;
    }
    if !h.rt().heap.rc_due.is_empty() || h.rt().task_coros_released() {
        h.run_due_drops();
    }
    collect_cycles(h);
    h.rt_mut().heap.cycles_ran();
}

/// At an allocation safepoint: the `drop`s counts left due, and the cycle
/// collector on allocation pressure.
pub(crate) fn safepoint<H: GcHost + ?Sized>(h: &mut H) {
    if h.rt().gc_in_progress {
        return;
    }
    if h.rt().heap.epoch_stw() {
        return;
    }
    if !h.rt().heap.rc_due.is_empty() || h.rt().task_coros_released() {
        h.run_due_drops();
    }
    if h.rt().heap.cycles_due() {
        collect_cycles(h);
        h.rt_mut().heap.cycles_ran();
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

/// Free the garbage cycles counts cannot ([`crate::memory::Heap::cycle_garbage`],
/// reference-count plan step 9). Their `drop`s run first, the objects owned
/// meanwhile; a cycle a `drop` let go of is then freed by its counts, and
/// what is left by a second pass.
pub(crate) fn collect_cycles<H: GcHost + ?Sized>(h: &mut H) {
    if h.rt().heap.epoch_stw() {
        return;
    }
    #[cfg(feature = "rc-check")]
    h.rt().heap.rc_check();
    for pass in 0..2 {
        let mut roots = Vec::new();
        for_each_root(h, &mut |addr, _| roots.push(addr));
        let garbage = h.rt().heap.cycle_garbage(&roots);
        if garbage.is_empty() {
            return;
        }
        // A `drop` to run, or a suspended generator to close (`None`).
        let drops: Vec<(Value, Option<u32>)> = garbage
            .iter()
            .filter_map(|&o| {
                let pc = if crate::memory::Heap::cycle_closes(o) { None } else { Some(h.rt().pending_finalizer(o)?) };
                Some((Value::from(o.addr()), pc))
            })
            .collect();
        if drops.is_empty() {
            h.rt_mut().heap.free_cycles(&garbage);
            return;
        }
        if pass == 1 {
            // A `drop` made more garbage that owes one: the next collect.
            return;
        }
        let words: Vec<Value> = garbage.iter().map(|o| Value::from(o.addr())).collect();
        for &w in &words {
            h.rt().heap.rc_retain(w);
        }
        // No collection inside the `drop`s: they run on a heap mid-pass.
        let was = std::mem::replace(&mut h.rt_mut().gc_in_progress, true);
        for (val, pc) in drops {
            h.run_pending(val, pc);
        }
        h.rt_mut().gc_in_progress = was;
        for w in words {
            h.rt_mut().heap.rc_drop(w);
        }
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
    if h.rt().heap.get().cycles_due() {
        collect(h);
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
    if rt.heap.is_borrowed() || !rt.heap.get().cycles_due() {
        return;
    }
    // The cycle collector, with `published` rooted.
    rt.ffi_arg_roots.push(published);
    collect(h);
    h.rt_mut().ffi_arg_roots.pop();
}
