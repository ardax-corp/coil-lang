//! Mostly-copying evacuation of sparse slab chunks.
//!
//! Bartlett-style. An object moves only when every reference to it is
//! precise: a pointer-kind field / payload / tuple / array word, a
//! `Member::Object`, a masked coroutine slot, or a must-pointer frame slot.
//! Anything an ambiguous word can reach (the VM's non-must roots, unknown
//! kinds, closure captures, `Member::Value`), `Root` / `Weak` targets, and
//! every kind other than instances / payload enums / boxes / tuples / arrays
//! stays put. See `docs/internals/gc-evacuation.md`.
//!
//! Three steps so the VM can rewrite its own roots in between:
//! [`Heap::evacuate_plan`] copies and rewrites the heap,
//! the VM rewrites its must-pointer slots through [`EvacPlan::forward`], then
//! [`Heap::evacuate_finish`] poisons and frees the old slots and releases
//! emptied chunks. Runs only between cycles (`GcPhase::Idle`) and outside a
//! shared-heap epoch.

use std::alloc::Layout;
use std::collections::{HashMap, HashSet};
use std::ptr::NonNull;

use common::Value;

use super::super::AddrHashBuilder;
use super::{
    GcData, GcHeader, GcPhase, Heap, InstanceStorage, Member, ObjArray, ObjBoxed, ObjEnum,
    ObjInstance, ObjTuple, Object,
};

type AddrSet = HashSet<u64, AddrHashBuilder>;
/// Old object address → new address.
pub type AddrMap = HashMap<u64, u64, AddrHashBuilder>;

/// What one evacuation did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Evacuation {
    /// Objects copied to a new slot.
    pub moved: usize,
    /// Live objects in chosen chunks that could not move.
    pub pinned: usize,
    /// Chunks chosen for evacuation.
    pub chunks: usize,
    /// Bytes of emptied chunks given back to the OS.
    pub released: usize,
    /// More sparse chunks remain than one step moves.
    pub capped: bool,
}

/// An evacuation between its copy and its free step.
pub struct EvacPlan {
    fwd: AddrMap,
    parked: Vec<NonNull<u8>>,
    emptied: Vec<bool>,
    stats: Evacuation,
}

impl EvacPlan {
    /// New address of a word naming a moved object (keeps the `Result`
    /// `Err` tag bit), or `None` when it does not.
    #[inline]
    pub fn forward(&self, v: Value) -> Option<Value> {
        forward(&self.fwd, v)
    }

    /// Old address → new address of every moved object.
    pub fn moved(&self) -> &AddrMap {
        &self.fwd
    }
}

#[inline]
fn forward(fwd: &AddrMap, v: Value) -> Option<Value> {
    let addr = v.heap_addr();
    if addr == 0 {
        return None;
    }
    let to = *fwd.get(&addr)?;
    Some(Value::from(to | (v.raw() as u64 & 1)))
}

/// `(chunk, live objects, movable objects, slots)` for chunk choice.
type ChunkFill = (usize, usize, usize, usize);

/// Chunks one evacuation step may empty (4 MiB of slab).
const MAX_STEP_CHUNKS: usize = 64;

/// At least 1 MiB and a quarter of the slab.
fn worth_it(bytes: usize, slab_bytes: usize) -> bool {
    bytes >= (1 << 20) && bytes * 4 >= slab_bytes
}

impl Object {
    /// Slot layout of a kind evacuation may move; `None` for the rest.
    #[cold]
    pub(super) fn movable_layout(&self) -> Option<Layout> {
        Some(match self {
            Self::Instance(_) => Layout::new::<GcData<ObjInstance>>(),
            // Unit variants are shared immortals.
            Self::Enum(e) if !e.as_ref().payload.is_empty() => Layout::new::<GcData<ObjEnum>>(),
            Self::Boxed(_) => Layout::new::<GcData<ObjBoxed>>(),
            Self::Tuple(_) => Layout::new::<GcData<ObjTuple>>(),
            Self::Array(_) => Layout::new::<GcData<ObjArray>>(),
            _ => return None,
        })
    }

    /// Rewrite this object's precise references to moved objects. Mirrors
    /// the precise cases of [`Object::for_each_reference`].
    #[cold]
    pub(super) fn rewrite_precise_refs(&self, heap: &Heap, fwd: &AddrMap) {
        let fix = |v: &mut Value| {
            if let Some(n) = forward(fwd, *v) {
                *v = n;
            }
        };
        let fix_kinded = |words: &mut [Value], kinds: u8| {
            for (i, v) in words.iter_mut().enumerate() {
                if common::packed_word_kind(kinds, i) == common::WORD_POINTER {
                    fix(v);
                }
            }
        };
        let fix_member = |m: &mut Member| -> bool {
            if let Member::Object(o) = m
                && let Some(&to) = fwd.get(&o.addr())
                && let Some(n) = heap.find_object_by_addr(to)
            {
                *m = Member::Object(n);
                return true;
            }
            false
        };
        match self {
            Self::Instance(i) => {
                let inst = i.payload_mut_unbarriered();
                match &mut inst.storage {
                    InstanceStorage::Table(table) => {
                        let updates: Vec<_> = table
                            .iter()
                            .filter_map(|(k, v)| {
                                let mut m = v;
                                fix_member(&mut m).then_some((k, m))
                            })
                            .collect();
                        for (k, m) in updates {
                            table.insert(k, m);
                        }
                    }
                    storage => {
                        let kinds = heap.class_field_kinds(inst.type_id);
                        if let Some(words) = storage.as_mut_slice() {
                            for (i, v) in words.iter_mut().enumerate() {
                                if kinds.get(i).copied() == Some(common::WORD_POINTER) {
                                    fix(v);
                                }
                            }
                        }
                    }
                }
            }
            Self::Enum(e) => {
                let payload = &mut e.payload_mut_unbarriered().payload;
                let kinds = payload.kinds();
                fix_kinded(payload.as_mut_slice(), kinds);
            }
            Self::Tuple(t) => {
                let t = t.payload_mut_unbarriered();
                let kinds = t.kinds();
                fix_kinded(t.elements.as_mut_slice(), kinds);
            }
            Self::Array(a) => {
                let a = a.payload_mut_unbarriered();
                if a.may_hold_refs() && a.elem_kind() == common::WORD_POINTER {
                    a.elements_mut_no_new_values().iter_mut().for_each(fix);
                }
            }
            Self::Coroutine(c) => {
                let coro = c.payload_mut_unbarriered();
                let mask = coro.saved_live_mask;
                if mask != 0 {
                    for (i, v) in coro.saved_stack.iter_mut().enumerate().take(64) {
                        if mask & (1u64 << i) != 0 {
                            fix(v);
                        }
                    }
                }
            }
            Self::Boxed(b) => {
                fix_member(&mut b.payload_mut_unbarriered().payload);
            }
            Self::Root(r) => {
                if let Some(m) = &mut r.payload_mut_unbarriered().payload {
                    fix_member(m);
                }
            }
            Self::PolyFn(p) => {
                for m in p.payload_mut_unbarriered().captured_dicts.iter_mut().flatten() {
                    fix_member(m);
                }
            }
            _ => {}
        }
    }
}

impl Heap {
    /// Copy the movable objects of sparse chunks (or, with `everything`, of
    /// every chunk — the stress mode) and rewrite the heap's precise
    /// references. `vm_pins` are the VM roots it cannot rewrite; each pins
    /// its target. `None` when nothing is worth moving. The old copies stay
    /// readable until [`Self::evacuate_finish`].
    #[cold]
    pub fn evacuate_plan(&mut self, vm_pins: &[u64], everything: bool) -> Option<EvacPlan> {
        if self.gc_phase != GcPhase::Idle || self.epoch_stw {
            return None;
        }
        // Cheap gate before walking the heap: free slots must add up to a
        // real share of the slab.
        // Resident slab: released chunks stay mapped but hold no pages.
        let slab_bytes = self.slab.mapped_bytes() - self.slab.released_bytes();
        if !everything && !worth_it(self.slab.free_slot_bytes(), slab_bytes) {
            return None;
        }
        let live: Vec<Object> = self.objects().collect();

        let mut pinned: AddrSet = vm_pins.iter().map(|a| a & !1).collect();
        for obj in &live {
            match obj {
                // FFI code may hold a rooted object's address; weak targets
                // are raw words.
                Object::Root(r) => {
                    if let Some(m) = &r.as_ref().payload {
                        pinned.insert(match m {
                            Member::Object(o) => o.addr(),
                            Member::Value(v) => v.heap_addr(),
                        });
                    }
                }
                Object::Weak(w) => {
                    pinned.insert(w.as_ref().target.get().heap_addr());
                }
                _ => {}
            }
            obj.for_each_reference(self, &mut |addr, precise| {
                if !precise {
                    pinned.insert(addr);
                }
            });
        }
        let movable = |o: &Object| o.movable_layout().is_some() && !pinned.contains(&o.addr());

        // Per chunk: live objects and how many could move.
        let nchunks = self.slab.chunk_count();
        let mut per_chunk: Vec<(usize, usize)> = vec![(0, 0); nchunks];
        for obj in &live {
            if let Some(i) = self.slab.chunk_of(obj.addr()) {
                per_chunk[i].0 += 1;
                per_chunk[i].1 += usize::from(movable(obj));
            }
        }
        let mut capped = false;
        // Chunks the whole plan would empty (before the per-step cap).
        let mut planned = 0;
        let chosen: Vec<usize> = if everything {
            (0..nchunks).filter(|&i| per_chunk[i].1 > 0).collect()
        } else {
            // Per size class, the sparsest chunks whose objects can all move
            // and fit in the free slots of the chunks that stay.
            let mut by_class: HashMap<(u32, u32), Vec<ChunkFill>> = HashMap::new();
            for (i, &(n, m)) in per_chunk.iter().enumerate() {
                if n == 0 {
                    continue;
                }
                if let Some((class, slots)) = self.slab.chunk_shape(i) {
                    by_class.entry(class).or_default().push((i, n, m, slots));
                }
            }
            let mut out = Vec::new();
            for chunks in by_class.values_mut() {
                chunks.sort_unstable_by_key(|&(_, n, _, _)| n);
                let mut free_left: usize = chunks.iter().map(|&(_, n, _, slots)| slots - n).sum();
                for &(i, n, m, slots) in chunks.iter() {
                    // A chunk with a pinned object cannot empty.
                    if m != n {
                        continue;
                    }
                    free_left -= slots - n;
                    if m > free_left {
                        break;
                    }
                    free_left -= m;
                    out.push(i);
                }
            }
            planned = out.len();
            if out.len() > MAX_STEP_CHUNKS {
                out.sort_unstable_by_key(|&i| per_chunk[i].0);
                out.truncate(MAX_STEP_CHUNKS);
                capped = true;
            }
            out
        };
        // Worth it only when a real share of the slab comes back: moving a
        // few short-lived objects every cycle just refaults the chunks. The
        // bar is on the whole plan; a capped step continues next cycle.
        let freeable = planned * super::super::slab::CHUNK_BYTES;
        if chosen.is_empty() || (!everything && !worth_it(freeable, slab_bytes)) {
            return None;
        }
        let mut candidates = vec![false; nchunks];
        for &i in &chosen {
            candidates[i] = true;
        }

        // New copies land outside the chunks being emptied.
        let parked = self.slab.take_free_in(&candidates);
        let mut stats = Evacuation {
            chunks: chosen.len(),
            capped,
            ..Evacuation::default()
        };
        let mut fwd = AddrMap::default();
        for obj in &live {
            let addr = obj.addr();
            if !self.slab.chunk_of(addr).is_some_and(|i| candidates[i]) {
                continue;
            }
            let Some(layout) = obj.movable_layout().filter(|_| !pinned.contains(&addr)) else {
                stats.pinned += 1;
                continue;
            };
            let to = self.slab.alloc(layout);
            // SAFETY: same size class; the old copy is poisoned in
            // `evacuate_finish` and never dropped, so the payload (including
            // any spill `Vec`) moves bitwise.
            unsafe {
                std::ptr::copy_nonoverlapping(addr as *const u8, to.as_ptr(), layout.size());
            }
            fwd.insert(addr, to.as_ptr() as u64);
        }
        stats.moved = fwd.len();

        // Rewrite precise interior references (old copies are skipped: they
        // are garbage from here on).
        for obj in self.objects() {
            if !fwd.contains_key(&obj.addr()) {
                obj.rewrite_precise_refs(self, &fwd);
            }
        }
        let emptied = (0..nchunks)
            .map(|i| candidates[i] && per_chunk[i].0 == per_chunk[i].1)
            .collect();
        Some(EvacPlan {
            fwd,
            parked,
            emptied,
            stats,
        })
    }

    /// `gc-stress`: no live heap word still names a moved object's old slot.
    #[cfg(feature = "gc-stress")]
    #[cold]
    pub fn verify_evacuation(&self, plan: &EvacPlan) {
        for obj in self.objects() {
            if plan.fwd.contains_key(&obj.addr()) {
                continue;
            }
            obj.for_each_reference(self, &mut |addr, precise| {
                assert!(
                    !plan.fwd.contains_key(&addr),
                    "gc-stress: {} reference from {:#x} to moved {addr:#x}",
                    if precise { "precise" } else { "ambiguous" },
                    obj.addr(),
                );
            });
        }
    }

    /// Poison and free the old slots, give back parked free slots, and
    /// release chunks the plan emptied.
    #[cold]
    pub fn evacuate_finish(&mut self, plan: EvacPlan) -> Evacuation {
        let EvacPlan {
            fwd,
            parked,
            emptied,
            mut stats,
        } = plan;
        for &old in fwd.keys() {
            // SAFETY: `old` is a live slot origin copied above; poisoning the
            // header makes it a free slot without dropping the moved payload.
            unsafe { (*(old as *const GcHeader)).kind.set(0) };
            self.slab.free(unsafe { NonNull::new_unchecked(old as *mut u8) });
        }
        for p in parked {
            self.slab.free(p);
        }
        stats.released = self.slab.release_chunks(&emptied);
        stats
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{EnumPayload, ObjString};

    /// A cons list `Node(v, next)` of enums with a pointer-kind link word.
    fn cons_list(heap: &mut Heap, n: i64) -> Vec<u64> {
        let kinds = common::pack_word_kinds([common::WORD_SCALAR, common::WORD_POINTER]);
        let mut addrs = Vec::new();
        let mut next = 0u64;
        for v in 0..n {
            let payload = EnumPayload::two(Value::from(v), Value::from(next)).with_kinds(kinds);
            let (obj, _) = heap.alloc(ObjEnum::new(0, payload), Object::Enum);
            addrs.push(obj.addr());
            next = obj.addr();
        }
        addrs
    }

    /// Walk the list from `head`, returning the payload values.
    fn walk(heap: &Heap, head: u64) -> Vec<i64> {
        let mut out = Vec::new();
        let mut cur = heap.find_object_by_addr(head);
        while let Some(Object::Enum(e)) = cur {
            let p = &e.as_ref().payload;
            out.push(p[0].as_int());
            cur = heap.find_object_by_addr(p[1].heap_addr());
        }
        out
    }

    fn full_collect(heap: &mut Heap, roots: &[u64]) {
        heap.begin_mark(roots);
        while !heap.mark_quantum(usize::MAX) {}
        unsafe { heap.sweep() };
    }

    fn evacuate(heap: &mut Heap, pins: &[u64]) -> (Evacuation, AddrMap) {
        let plan = heap.evacuate_plan(pins, true).expect("something to move");
        let moved = plan.moved().clone();
        (heap.evacuate_finish(plan), moved)
    }

    #[test]
    fn evacuation_moves_a_linked_list_and_keeps_it_intact() {
        let mut heap = Heap::default();
        let mut keep = Vec::new();
        for _ in 0..4 {
            keep.extend(cons_list(&mut heap, 500));
            for i in 0..2000 {
                let _ = heap.alloc(ObjString::from(format!("g{i}").as_str()), Object::String);
            }
        }
        let head = *keep.last().unwrap();
        full_collect(&mut heap, &[head]);
        let before = walk(&heap, head);
        assert_eq!(before.len(), 500);

        // The head is a pinned root; the rest of the list may move.
        let (ev, moved) = evacuate(&mut heap, &[head]);
        assert!(ev.moved > 0, "{ev:?}");
        assert!(!moved.contains_key(&head), "a pinned root stays put");
        assert_eq!(walk(&heap, head), before, "list survives the move");
        assert!(
            moved.keys().all(|&a| heap.find_object_by_addr(a).is_none()),
            "old slots are poisoned"
        );
        full_collect(&mut heap, &[head]);
        assert_eq!(walk(&heap, head), before);
    }

    /// An object an ambiguous word references is pinned.
    #[test]
    fn ambiguously_referenced_object_stays_put() {
        let mut heap = Heap::default();
        let (s, _) = heap.alloc(ObjString::from("x"), Object::String);
        let (target, _) = heap.alloc(
            ObjEnum::new(0, EnumPayload::one(Value::from(s.addr()))),
            Object::Enum,
        );
        // Unknown-kind array words are ambiguous.
        let (arr, _) = heap.alloc(ObjArray::new(vec![Value::from(target.addr())]), Object::Array);
        full_collect(&mut heap, &[arr.addr()]);
        let plan = heap.evacuate_plan(&[arr.addr()], true);
        if let Some(plan) = plan {
            assert!(!plan.moved().contains_key(&target.addr()));
            heap.evacuate_finish(plan);
        }
        assert!(matches!(heap.find_object_by_addr(target.addr()), Some(Object::Enum(_))));
    }

    /// Pointer-kind array elements move and are rewritten, keeping the
    /// `Result` `Err` tag bit.
    #[test]
    fn pointer_kind_array_elements_are_rewritten() {
        let mut heap = Heap::default();
        let mut words = Vec::new();
        for v in 0..64 {
            let (e, _) = heap.alloc(
                ObjEnum::new(0, EnumPayload::one(Value::from(v)).with_kinds(1)),
                Object::Enum,
            );
            words.push(Value::from(e.addr() | u64::from(v % 2 == 1)));
        }
        let (arr, mut gc) = heap.alloc(ObjArray::new(words), Object::Array);
        gc.as_mut().set_elem_kind(common::WORD_POINTER);
        full_collect(&mut heap, &[arr.addr()]);
        let (ev, moved) = evacuate(&mut heap, &[arr.addr()]);
        assert_eq!(ev.moved, 64, "{ev:?}");
        let Some(Object::Array(a)) = heap.find_object_by_addr(arr.addr()) else {
            panic!("array is a pinned root");
        };
        for (v, w) in a.as_ref().elements().iter().enumerate() {
            assert_eq!(w.raw() as u64 & 1, (v % 2) as u64, "tag bit kept");
            assert!(!moved.contains_key(&w.heap_addr()));
            let Some(Object::Enum(e)) = heap.find_object_by_addr(w.heap_addr()) else {
                panic!("element {v} is not an enum");
            };
            assert_eq!(e.as_ref().payload[0].as_int(), v as i64);
        }
    }
}
