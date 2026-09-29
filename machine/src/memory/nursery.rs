//! Generational nursery (`gc-nursery`).
//!
//! Instances, payload enums, boxes, tuples and arrays are bump-allocated in
//! a small set of nursery chunks (hot memory, no free-list pops). A minor
//! collection copies the survivors into the old slab, rewrites every precise
//! reference to them and resets the nursery; dead young objects are dropped
//! without a mark or sweep. See `docs/internals/gc-nursery.md`.
//!
//! Roots of a minor collection:
//! - the VM's roots: must-pointer frame slots (rewritten through
//!   [`MinorPlan::forward`]); every other VM root pins what it hits;
//! - the remembered set: old objects written since the last minor collection
//!   (write barrier in `Gc::payload_mut` / `DerefMut`) or allocated while
//!   young objects existed;
//! - young finalizable objects (promoted, so the major collector runs their
//!   `drop`).
//!
//! An ambiguous reference to a young object pins it: its chunk is promoted
//! in place (becomes an old chunk) instead of being reset.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::ptr::NonNull;

use common::Value;

use super::super::AddrHashBuilder;
use super::compact::AddrMap;
use super::{GcHeader, GcPhase, GcSized, Heap, Object};

type AddrSet = HashSet<u64, AddrHashBuilder>;

thread_local! {
    /// Young objects exist (the barrier is only needed then).
    static YOUNG_LIVE: Cell<bool> = const { Cell::new(false) };
    /// The nursery ran out: collect at the next safepoint.
    static MINOR_REQUESTED: Cell<bool> = const { Cell::new(false) };
    /// Old objects that may hold young references.
    static REMEMBERED: RefCell<Vec<u64>> = const { RefCell::new(Vec::new()) };
    /// Off with `COIL_GC_NURSERY=0`.
    static ENABLED: bool = std::env::var_os("COIL_GC_NURSERY").is_none_or(|v| v != "0");
}

#[inline(always)]
pub fn young_live() -> bool {
    YOUNG_LIVE.get()
}

#[inline]
pub(super) fn set_young_live() {
    YOUNG_LIVE.set(true);
}

#[inline(never)]
pub(super) fn remember(addr: u64) {
    REMEMBERED.with_borrow_mut(|r| r.push(addr));
}

pub(super) fn request_minor() {
    MINOR_REQUESTED.set(true);
}

/// Nothing to collect (no young objects).
pub fn clear_minor_request() {
    MINOR_REQUESTED.set(false);
}

/// The nursery is full (or a caller asked): run a minor collection at the
/// next safepoint.
#[inline(always)]
pub fn minor_requested() -> bool {
    MINOR_REQUESTED.get()
}

pub fn enabled() -> bool {
    ENABLED.with(|e| *e)
}

#[inline]
fn header(addr: u64) -> &'static GcHeader {
    // SAFETY: callers pass the origin of a live slot (`find_object_by_addr`).
    unsafe { &*(addr as *const GcHeader) }
}

/// What one minor collection did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Minor {
    /// Young objects copied to the old slab.
    pub promoted: usize,
    /// Young objects kept in place (their chunk became old).
    pub pinned: usize,
    /// Dead young objects dropped.
    pub freed: usize,
    /// Nursery chunks promoted in place.
    pub chunks_promoted: usize,
}

/// A minor collection between its copy and its reset step.
pub struct MinorPlan {
    fwd: AddrMap,
    visited: AddrSet,
    promoted_chunks: HashSet<usize>,
    stats: Minor,
}

impl MinorPlan {
    /// New address of a word naming a copied object (keeps the `Result`
    /// `Err` tag bit), or `None`.
    #[inline]
    pub fn forward(&self, v: Value) -> Option<Value> {
        let addr = v.heap_addr();
        if addr == 0 {
            return None;
        }
        let to = *self.fwd.get(&addr)?;
        Some(Value::from(to | (v.raw() as u64 & 1)))
    }

    /// True when `addr` is a young object this collection frees or moved.
    pub fn is_stale(&self, heap: &Heap, addr: u64) -> bool {
        if self.fwd.contains_key(&addr) {
            return true;
        }
        heap.find_object_by_addr(addr).is_some_and(|_| {
            header(addr).young.get()
                && !heap
                    .slab
                    .chunk_of(addr)
                    .is_some_and(|i| self.promoted_chunks.contains(&i))
        })
    }
}

impl Heap {
    /// Allocation may use the nursery now: outside a mark phase and a
    /// shared-heap epoch.
    pub(super) fn nursery_open(&self) -> bool {
        enabled() && !self.epoch_stw && self.gc_phase != GcPhase::Marking
    }

    fn young_object(&self, addr: u64) -> Option<Object> {
        let obj = self.find_object_by_addr(addr)?;
        header(addr).young.get().then_some(obj)
    }

    /// Trace young objects from the roots, copy the unpinned survivors to the
    /// old slab and rewrite the heap's precise references. `vm_must` are the
    /// VM's rewritable roots, `vm_pins` the rest. `finalizable` says whether
    /// a young object still owes a `drop` (it is kept alive for the major
    /// collector). The nursery is reset in [`Self::minor_finish`].
    pub fn minor_plan(
        &mut self,
        vm_must: &[Value],
        vm_pins: &[u64],
        finalizable: &dyn Fn(Object) -> bool,
    ) -> MinorPlan {
        let mut stats = Minor::default();
        let mut visited = AddrSet::default();
        let mut pinned = AddrSet::default();
        let mut work: Vec<Object> = Vec::new();
        let enqueue = |heap: &Heap, addr: u64, visited: &mut AddrSet, work: &mut Vec<Object>| {
            if let Some(obj) = heap.young_object(addr)
                && visited.insert(addr)
            {
                work.push(obj);
            }
        };
        for v in vm_must {
            enqueue(self, v.heap_addr(), &mut visited, &mut work);
        }
        for &a in vm_pins {
            let a = a & !1;
            if self.young_object(a).is_some() {
                pinned.insert(a);
                enqueue(self, a, &mut visited, &mut work);
            }
        }
        let remembered: Vec<u64> = REMEMBERED.with_borrow_mut(std::mem::take);
        let mut remembered_objs: Vec<Object> = Vec::with_capacity(remembered.len());
        for addr in remembered {
            let Some(obj) = self.find_object_by_addr(addr) else {
                continue;
            };
            if header(addr).young.get() {
                continue;
            }
            header(addr).remembered.set(false);
            remembered_objs.push(obj);
        }
        for obj in &remembered_objs {
            obj.for_each_reference(self, &mut |a, precise| {
                if self.young_object(a).is_some() {
                    if !precise {
                        pinned.insert(a);
                    }
                    enqueue(self, a, &mut visited, &mut work);
                }
            });
            // Weak targets are raw words; keep their young referents.
            if let Object::Weak(w) = obj {
                let a = w.as_ref().target.get().heap_addr();
                if self.young_object(a).is_some() {
                    pinned.insert(a);
                    enqueue(self, a, &mut visited, &mut work);
                }
            }
        }
        // Young objects that still owe a `drop`: the major collector runs it.
        for (first, end, size, _) in self.slab.young_ranges() {
            let mut p = first;
            while p < end {
                if let Some(obj) = self.young_object(p)
                    && finalizable(obj)
                {
                    enqueue(self, p, &mut visited, &mut work);
                }
                p += size;
            }
        }
        while let Some(obj) = work.pop() {
            obj.for_each_reference(self, &mut |a, precise| {
                if self.young_object(a).is_some() {
                    if !precise {
                        pinned.insert(a);
                    }
                    enqueue(self, a, &mut visited, &mut work);
                }
            });
        }
        let promoted_chunks: HashSet<usize> = pinned
            .iter()
            .filter_map(|&a| self.slab.chunk_of(a))
            .collect();

        // Copy the unpinned survivors into the old slab.
        let mut fwd = AddrMap::default();
        for &addr in &visited {
            if self
                .slab
                .chunk_of(addr)
                .is_some_and(|i| promoted_chunks.contains(&i))
            {
                stats.pinned += 1;
                continue;
            }
            let obj = self.find_object_by_addr(addr).expect("visited young object");
            let layout = obj.movable_layout().expect("nursery kinds are movable");
            let to = self.slab.alloc(layout);
            // SAFETY: same size class; the young copy is poisoned below and
            // never dropped, so the payload (and any spill `Vec`) moves.
            unsafe {
                std::ptr::copy_nonoverlapping(addr as *const u8, to.as_ptr(), layout.size());
            }
            let new = to.as_ptr() as u64;
            let h = header(new);
            h.young.set(false);
            h.marked.set(false);
            h.remembered.set(false);
            h.fresh.set(false);
            if self.gc_phase == GcPhase::Sweeping
                && let Some(cur) = &self.gc_sweep_cursor
                && !self.slab.walk_passed(cur, new)
            {
                h.fresh.set(true);
            }
            header(addr).kind.set(0);
            fwd.insert(addr, new);
        }
        stats.promoted = fwd.len();

        // Rewrite precise references into copied objects.
        for obj in &remembered_objs {
            obj.rewrite_precise_refs(self, &fwd);
            if let Object::Weak(w) = obj {
                let t = w.as_ref().target.get();
                if let Some(&to) = fwd.get(&t.heap_addr()) {
                    w.as_ref().target.set(Value::from(to | (t.raw() as u64 & 1)));
                }
            }
        }
        for &new in fwd.values() {
            if let Some(obj) = self.find_object_by_addr(new) {
                obj.rewrite_precise_refs(self, &fwd);
            }
        }
        for &addr in &visited {
            if !fwd.contains_key(&addr)
                && let Some(obj) = self.find_object_by_addr(addr)
            {
                obj.rewrite_precise_refs(self, &fwd);
            }
        }
        MinorPlan {
            fwd,
            visited,
            promoted_chunks,
            stats,
        }
    }

    /// `gc-stress`: no old object still names a young object this minor
    /// collection frees or moved.
    #[cfg(feature = "gc-stress")]
    pub fn verify_minor(&self, plan: &MinorPlan) {
        for obj in self.objects() {
            let a = obj.addr();
            if header(a).young.get() {
                continue;
            }
            obj.for_each_reference(self, &mut |t, precise| {
                assert!(
                    !plan.is_stale(self, t),
                    "gc-stress: {} reference from old {a:#x} to a dying/moved young {t:#x} (missed write barrier?)",
                    if precise { "precise" } else { "ambiguous" },
                );
            });
            if let Object::Weak(w) = obj {
                let t = w.as_ref().target.get().heap_addr();
                assert!(!plan.is_stale(self, t), "gc-stress: weak target {t:#x} left stale");
            }
        }
    }

    /// Drop dead young objects, promote chunks holding pinned survivors and
    /// reset the nursery.
    pub fn minor_finish(&mut self, plan: MinorPlan) -> Minor {
        let MinorPlan {
            visited,
            promoted_chunks,
            mut stats,
            ..
        } = plan;
        for (first, end, size, chunk) in self.slab.young_ranges() {
            let promote = promoted_chunks.contains(&chunk);
            let mut p = first;
            while p < end {
                if let Some(obj) = self.find_object_by_addr(p) {
                    if promote && visited.contains(&p) {
                        let h = header(p);
                        h.young.set(false);
                        h.marked.set(false);
                    } else {
                        self.alloc_bytes -= obj.size();
                        self.live_count -= 1;
                        stats.freed += 1;
                        // SAFETY: unreachable young object (not copied: a
                        // copied slot is already poisoned).
                        unsafe { obj.recycle_payload() };
                        if promote {
                            self.slab.free(unsafe { NonNull::new_unchecked(p as *mut u8) });
                        }
                    }
                } else if promote {
                    self.slab.free(unsafe { NonNull::new_unchecked(p as *mut u8) });
                }
                p += size;
            }
            if promote {
                stats.chunks_promoted += 1;
                self.slab.promote_young_chunk(chunk, end);
            }
        }
        self.slab.reset_nursery();
        YOUNG_LIVE.set(false);
        MINOR_REQUESTED.set(false);
        stats
    }
}
