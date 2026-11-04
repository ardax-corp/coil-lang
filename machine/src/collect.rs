//! The collector's root walk, apart from the interpreter. A host (the VM's
//! [`crate::Machine`], or a driver that runs native code alone) reports the
//! roots it holds itself; the [`Runtime`] adds its own (statics, the
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
    /// `Machine` / `Heap` so their hot-loop layout is unchanged.
    pub(crate) static STATIC_KINDS: std::cell::RefCell<Arc<Vec<u8>>> = std::cell::RefCell::new(Arc::default());
    /// Evacuation pacing for this thread's collector: `(cycles since the
    /// last attempt, collections to wait)`.
    static COMPACT_PACING: std::cell::Cell<(u32, u32)> = const { std::cell::Cell::new((0, COMPACT_WINDOW)) };
    /// Off with `COIL_GC_COMPACT=0` (debugging a suspected move bug), in
    /// this crate's unit tests only; other builds always evacuate.
    static COMPACT_ENABLED: bool = !cfg!(test) || std::env::var_os("COIL_GC_COMPACT").is_none_or(|v| v != "0");
}

/// Collections between evacuation attempts outside stress mode, and the
/// ceiling unproductive attempts back off to.
const COMPACT_WINDOW: u32 = 8;
const COMPACT_WINDOW_MAX: u32 = 256;

/// A root the host holds itself.
#[derive(Clone, Copy)]
pub(crate) enum HostRoot {
    /// A word the host can rewrite, by its index ([`GcHost::slot`]): a
    /// `Precise` one moves, any other is pinned where it points.
    Slot(usize, RootKind),
    /// A heap address the host holds where it cannot be rewritten.
    Handle(u64, RootKind),
}

/// What the collector needs from the host that runs the program.
pub(crate) trait GcHost {
    fn rt(&self) -> &Runtime;
    fn rt_mut(&mut self) -> &mut Runtime;
    /// Every root the host holds besides the Runtime's.
    fn host_roots(&self, visit: &mut dyn FnMut(HostRoot));
    /// The word of root slot `idx`.
    fn slot(&self, idx: usize) -> Value;
    fn set_slot(&mut self, idx: usize, v: Value);
    /// The slot an allocation just wrote, if any: a minor collection moves
    /// the object it names even where the host can only say it may be a
    /// reference.
    fn fresh_slot(&self) -> Option<usize> {
        None
    }
    /// Whether objects may move now (no raw heap word outside the roots).
    fn can_move(&self) -> bool;
    /// Drop caches that name objects which may move or die.
    fn clear_caches(&mut self) {}
    /// Before roots are collected for a mark.
    fn before_mark(&mut self) {}
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

    /// Heap words native frames hold across a VM call or a safepoint, as
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

/// Every root of `h` that resolves to a live heap object (the host's, then
/// the Runtime's), tagged with how a moving collector could treat it.
/// Heap-owned roots are seeded by [`crate::memory::Heap::take_gc_roots`].
pub(crate) fn for_each_root<H: GcHost + ?Sized>(h: &H, visit: &mut dyn FnMut(u64, RootKind)) {
    let heap = &h.rt().heap;
    h.host_roots(&mut |r| {
        let (addr, kind) = match r {
            HostRoot::Slot(idx, kind) => (h.slot(idx).heap_addr(), kind),
            HostRoot::Handle(addr, kind) => (addr, kind),
        };
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
    h.host_roots(&mut |r| {
        let addr = match r {
            HostRoot::Slot(idx, RootKind::Precise) => h.slot(idx).heap_addr(),
            HostRoot::Slot(..) => return,
            HostRoot::Handle(addr, _) => addr,
        };
        if addr != 0 && heap.find_object_by_addr(addr).is_some() {
            roots.push(addr);
        }
    });
    h.rt().for_each_root(&mut |addr, _| roots.push(addr));
    roots
}

/// Roots split for a moving collection: slots to rewrite, statics to
/// rewrite, and the addresses that must stay where they are.
struct MoveRoots {
    must: Vec<usize>,
    must_statics: Vec<usize>,
    pins: Vec<u64>,
}

/// Split the roots of `h` for a move. `fresh`: the host's fresh slot moves
/// too (a minor collection).
fn move_roots<H: GcHost + ?Sized>(h: &mut H, fresh: bool) -> MoveRoots {
    let mut pins = h.rt_mut().heap.take_gc_roots();
    let mut must = Vec::new();
    let fresh = if fresh { h.fresh_slot() } else { None };
    let mut fresh_seen = false;
    h.host_roots(&mut |r| match r {
        HostRoot::Slot(idx, _) if fresh == Some(idx) => {
            if !std::mem::replace(&mut fresh_seen, true) {
                must.push(idx);
            }
        }
        HostRoot::Slot(idx, RootKind::Precise) => must.push(idx),
        HostRoot::Slot(idx, _) => pins.push(h.slot(idx).heap_addr()),
        HostRoot::Handle(addr, _) => pins.push(addr),
    });
    let mut must_statics = Vec::new();
    h.rt().for_each_static_root(&mut |i, addr, kind| match kind {
        RootKind::Precise => must_statics.push(i),
        _ => pins.push(addr),
    });
    h.rt().for_each_handle_root(&mut |addr, _| pins.push(addr));
    MoveRoots { must, must_statics, pins }
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
    let MoveRoots { must, must_statics, mut pins } = move_roots(h, true);
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
    let precise: Vec<u64> = must
        .iter()
        .map(|&idx| h.slot(idx).heap_addr())
        .chain(must_statics.iter().map(|&i| rt.statics[i].heap_addr()))
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
    for idx in must {
        if let Some(v) = plan.forward(h.slot(idx)) {
            h.set_slot(idx, v);
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
    let MoveRoots { must, must_statics, mut pins } = move_roots(h, false);
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
    for idx in must {
        if let Some(v) = plan.forward(h.slot(idx)) {
            h.set_slot(idx, v);
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
