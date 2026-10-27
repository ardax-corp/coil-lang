//! Nursery minor collection (`feature/escape-memory` step 4).
//!
//! Instances, payload enums, boxes, tuples and arrays are bump-allocated
//! into young slab chunks ([`super::Slab::alloc_young`]). A minor collection
//! traces young objects from the VM roots and the remembered set (old
//! objects written since the last one, see the barrier in
//! [`super::GcData`]'s `as_mut`), copies every survivor only precise words
//! name into an old slot, rewrites those words, and resets the young chunks.
//! A survivor an ambiguous word names is pinned: its chunk is promoted in
//! place. See `docs/internals/escape-memory.md`.

use std::ptr::NonNull;

use common::Value;

use super::compact::{forward, AddrMap};
use super::{GcHeader, GcPhase, GcSized, Heap, Member, Object, GEN_PINNED, GEN_UNLOGGED, GEN_YOUNG, QUIET, REMEMBERED};

/// A minor collection between its copy and its reset step.
pub struct MinorPlan {
    fwd: AddrMap,
    remembered: Vec<u64>,
    /// `(chunk, first slot, end, slot size)` of every young chunk in use.
    spans: Vec<(usize, u64, u64, u64)>,
}

impl MinorPlan {
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

/// Slot addresses of one span.
fn slots(first: u64, end: u64, size: u64) -> impl Iterator<Item = u64> {
    (first..end).step_by(size as usize)
}

impl Heap {
    /// The young object at `addr` (a `Result` `Err` tag bit is ignored).
    #[inline]
    fn young_object(&self, addr: u64) -> Option<Object> {
        let obj = self.find_object_by_addr(addr & !1)?;
        (obj.generation() & GEN_YOUNG != 0).then_some(obj)
    }

    /// Whether any young chunk is in use.
    pub fn has_young(&self) -> bool {
        self.slab.has_young_chunks()
    }

    /// Take this heap's entries off the thread's remembered set: the live
    /// old objects among them. Entries of other heaps stay.
    fn take_remembered(&self) -> Vec<u64> {
        let mut mine = REMEMBERED.with_borrow_mut(|r| {
            let mut mine = Vec::new();
            r.retain(|&addr| {
                if !self.slab.contains_slot(addr) {
                    return true;
                }
                if self.find_object_by_addr(addr).is_some_and(|o| o.generation() & GEN_YOUNG == 0) {
                    mine.push(addr);
                }
                false
            });
            mine
        });
        mine.sort_unstable();
        mine.dedup();
        mine
    }

    /// Drop this heap's entries from the thread's remembered set (the heap
    /// goes away; another may map the same addresses).
    pub(super) fn forget_remembered(&self) {
        for set in [&REMEMBERED, &QUIET] {
            let _ = set.try_with(|r| {
                if let Ok(mut r) = r.try_borrow_mut() {
                    r.retain(|&addr| !self.slab.contains_slot(addr));
                }
            });
        }
    }

    /// Follow an evacuation in the remembered and quiet sets.
    pub(super) fn forward_remembered(&self, fwd: &AddrMap) {
        for set in [&REMEMBERED, &QUIET] {
            set.with_borrow_mut(|r| {
                for addr in r.iter_mut() {
                    if let Some(&to) = fwd.get(addr) {
                        *addr = to;
                    }
                }
            });
        }
    }

    /// Clear a major cycle's marks on young objects: the major sweep skips
    /// young chunks, and a minor collection reads the mark as "traced".
    /// Held objects (pinned by an earlier minor) the mark did not reach
    /// are dead: they go here, since no minor ever frees one.
    pub(super) fn unmark_young(&mut self) {
        let mut empty = Vec::new();
        for (chunk, first, end, size) in self.slab.held_spans() {
            let mut live = false;
            for addr in slots(first, end, size) {
                let header = unsafe { &*(addr as *const GcHeader) };
                live |= header.kind.get() != 0 && header.marked.get();
                if header.kind.get() != 0 && !header.marked.get() {
                    let obj = unsafe { Object::from_header(addr) }.expect("live held slot");
                    let bytes = obj.size();
                    self.alloc_bytes -= bytes;
                    self.young_bytes = self.young_bytes.saturating_sub(bytes);
                    debug_assert!(self.live_count > 0);
                    self.live_count -= 1;
                    unsafe { obj.recycle_payload() };
                }
            }
            if !live {
                empty.push(chunk);
            }
        }
        self.slab.unhold(&empty);
        for (_, first, end, size) in self.slab.young_spans() {
            for addr in slots(first, end, size) {
                // SAFETY: a slot start in a mapped chunk holds a header.
                let header = unsafe { &*(addr as *const GcHeader) };
                if header.kind.get() != 0 {
                    header.marked.set(false);
                }
            }
        }
    }

    /// Make every young object old where it is, without tracing: dead ones
    /// wait for a major cycle. For points where nothing may move (a host
    /// frame holds raw handles, a shared-heap epoch opens).
    pub fn promote_young(&mut self) {
        if !self.has_young() {
            return;
        }
        let sweeping = self.gc_phase == GcPhase::Sweeping;
        let mut promoted = Vec::new();
        let mut free = Vec::new();
        for (chunk, first, end, size) in self.slab.young_spans() {
            promoted.push(chunk);
            for addr in slots(first, end, size) {
                let header = unsafe { &*(addr as *const GcHeader) };
                if header.kind.get() == 0 {
                    free.push(unsafe { NonNull::new_unchecked(addr as *mut u8) });
                    continue;
                }
                header.generation.set(GEN_UNLOGGED);
                if sweeping {
                    // The sweep may still reach this chunk; it must keep
                    // what it never marked.
                    header.fresh.set(true);
                }
            }
            let tail = self.slab.chunk_slots_end(chunk);
            free.extend(slots(end, tail, size).map(|a| unsafe { NonNull::new_unchecked(a as *mut u8) }));
        }
        self.young_bytes = 0;
        self.slab.finish_young(&promoted, &[], free);
    }

    /// Trace the young objects reachable from `pins` (roots that cannot be
    /// rewritten, which pin their target), `precise` (roots the VM rewrites
    /// through [`MinorPlan::forward`]) and the remembered set; copy every
    /// survivor that is not pinned and rewrite the heap's precise words to
    /// it. The VM then rewrites its precise roots and calls
    /// [`Self::minor_finish`].
    pub fn minor_plan(&mut self, pins: &[u64], precise: &[u64]) -> MinorPlan {
        debug_assert_eq!(self.gc_phase, GcPhase::Idle, "minor collections run between cycles");
        let spans = self.slab.young_spans();
        let remembered = self.take_remembered();
        let mut gray: Vec<Object> = Vec::new();
        let reach = |heap: &Heap, gray: &mut Vec<Object>, addr: u64, precise: bool| {
            if let Some(obj) = heap.young_object(addr) {
                let g = obj.generation();
                if !precise && g & GEN_PINNED == 0 {
                    obj.set_generation(g | GEN_PINNED);
                }
                obj.mark(gray);
            }
        };
        for &addr in pins {
            reach(self, &mut gray, addr, false);
        }
        // Held objects stay put and live until a major cycle says otherwise;
        // they may name young objects (nothing logs a write to one).
        for (_, first, end, size) in self.slab.held_spans() {
            for addr in slots(first, end, size) {
                if unsafe { &*(addr as *const GcHeader) }.kind.get() != 0 {
                    reach(self, &mut gray, addr, false);
                }
            }
        }
        for &addr in precise {
            reach(self, &mut gray, addr, true);
        }
        // A young object typed with a `drop` after its allocation
        // (`note_enum_type`, `register_finalizer_type`) owes its finalizer:
        // it survives to the old space, where a major runs it.
        for &addr in &self.finalizable {
            reach(self, &mut gray, addr, true);
        }
        for &addr in &remembered {
            let Some(obj) = self.find_object_by_addr(addr) else {
                continue;
            };
            match obj {
                // FFI code may hold a rooted object's address.
                Object::Root(r) => {
                    if let Some(m) = &r.as_ref().payload {
                        let target = match m {
                            Member::Object(o) => o.addr(),
                            Member::Value(v) => v.heap_addr(),
                        };
                        reach(self, &mut gray, target, false);
                    }
                }
                _ => obj.for_each_reference(self, &mut |a, p| reach(self, &mut gray, a, p)),
            }
        }
        while let Some(obj) = gray.pop() {
            obj.for_each_reference(self, &mut |a, p| reach(self, &mut gray, a, p));
        }
        // Weak targets are raw words: a live one stays put, a dead one
        // clears its weak.
        for &w in &self.weaks {
            let Some(Object::Weak(gc)) = self.find_object_by_addr(w) else {
                continue;
            };
            let weak = gc.as_ref();
            if weak.cleared.get() {
                continue;
            }
            if let Some(target) = self.young_object(weak.target.get().heap_addr()) {
                if target.is_marked() {
                    target.set_generation(target.generation() | GEN_PINNED);
                } else {
                    weak.cleared.set(true);
                    weak.target.set(Value::from(0i64));
                }
            }
        }

        // Copy the survivors nothing ambiguous names.
        let mut fwd = AddrMap::default();
        for &(_, first, end, size) in &spans {
            for addr in slots(first, end, size) {
                let Some(obj) = (unsafe { Object::from_header(addr) }) else {
                    continue;
                };
                if !obj.is_marked() || obj.generation() & GEN_PINNED != 0 {
                    continue;
                }
                let Some(layout) = obj.movable_layout() else {
                    obj.set_generation(obj.generation() | GEN_PINNED);
                    continue;
                };
                let to = self.slab.alloc(layout);
                // SAFETY: same size class; the young copy is poisoned in
                // `minor_finish` and never dropped, so the payload (spill
                // `Vec`s included) moves bitwise.
                unsafe {
                    std::ptr::copy_nonoverlapping(addr as *const u8, to.as_ptr(), layout.size());
                    let header = &*(to.as_ptr() as *const GcHeader);
                    header.marked.set(false);
                    header.generation.set(0);
                }
                fwd.insert(addr, to.as_ptr() as u64);
            }
        }

        // Rewrite precise words naming a moved object: in the copies, the
        // pinned survivors and the remembered objects. Nothing else old can
        // name a young object.
        if !fwd.is_empty() {
            for &to in fwd.values() {
                if let Some(obj) = unsafe { Object::from_header(to) } {
                    obj.rewrite_precise_refs(self, &fwd);
                }
            }
            for &(_, first, end, size) in &spans {
                for addr in slots(first, end, size) {
                    if let Some(obj) = unsafe { Object::from_header(addr) }
                        && obj.is_marked()
                        && !fwd.contains_key(&addr)
                    {
                        obj.rewrite_precise_refs(self, &fwd);
                    }
                }
            }
            for &addr in &remembered {
                if let Some(obj) = self.find_object_by_addr(addr) {
                    obj.rewrite_precise_refs(self, &fwd);
                }
            }
            self.forward_side_lists(&fwd);
        }
        MinorPlan { fwd, remembered, spans }
    }

    /// Poison the moved slots, reclaim the dead young objects, hold the
    /// chunks with pinned survivors in the nursery and reset the rest.
    /// Returns how many objects moved.
    pub fn minor_finish(&mut self, plan: MinorPlan) -> usize {
        let MinorPlan { fwd, remembered, spans } = plan;
        let mut held = Vec::new();
        let (mut kept, mut kept_bytes, mut dead) = (0, 0, 0);
        for (chunk, first, end, size) in spans {
            let mut keep = false;
            for addr in slots(first, end, size) {
                // SAFETY: a slot start in a mapped chunk holds a header.
                let header = unsafe { &*(addr as *const GcHeader) };
                if header.kind.get() == 0 {
                    continue;
                }
                if fwd.contains_key(&addr) {
                    header.kind.set(0);
                    header.marked.set(false);
                    header.generation.set(0);
                    continue;
                }
                let obj = unsafe { Object::from_header(addr) }.expect("live young slot");
                if header.marked.get() {
                    // Pinned: held where it is, young, until a major cycle
                    // finds it dead ([`Self::unmark_young`]); its chunk
                    // stays out of reuse until then.
                    header.marked.set(false);
                    header.generation.set(GEN_YOUNG);
                    kept_bytes += obj.size();
                    kept += 1;
                    keep = true;
                } else {
                    dead += 1;
                    let bytes = obj.size();
                    self.alloc_bytes -= bytes;
                    debug_assert!(self.live_count > 0);
                    self.live_count -= 1;
                    unsafe { obj.recycle_payload() };
                }
            }
            if keep {
                held.push((chunk, end));
            }
        }
        for &to in fwd.values() {
            unsafe { &*(to as *const GcHeader) }.generation.set(GEN_UNLOGGED);
        }
        for addr in remembered {
            if let Some(obj) = self.find_object_by_addr(addr)
                && obj.generation() & GEN_YOUNG == 0
            {
                obj.set_generation(GEN_UNLOGGED);
            }
        }
        self.young_bytes = kept_bytes;
        self.note_survival(fwd.len() + kept, fwd.len() + kept + dead);
        self.slab.finish_young(&[], &held, Vec::new());
        fwd.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{EnumPayload, ObjArray, ObjEnum};

    fn nursery_heap() -> Heap {
        let mut heap = Heap::default();
        heap.set_nursery(true);
        heap
    }

    /// A cons list `Node(v, next)` of enums with a pointer-kind link word;
    /// returns the head.
    fn cons_list(heap: &mut Heap, n: i64) -> u64 {
        let kinds = common::pack_word_kinds([common::WORD_SCALAR, common::WORD_POINTER]);
        let mut next = 0u64;
        for v in 0..n {
            let payload = EnumPayload::two(Value::from(v), Value::from(next)).with_kinds(kinds);
            let (obj, _) = heap.alloc(ObjEnum::new(0, payload), Object::Enum);
            next = obj.addr();
        }
        next
    }

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

    fn minor(heap: &mut Heap, pins: &[u64], precise: &mut [u64]) -> usize {
        let plan = heap.minor_plan(pins, precise);
        for root in precise.iter_mut() {
            if let Some(v) = plan.forward(Value::from(*root)) {
                *root = v.bits();
            }
        }
        let moved = heap.minor_finish(plan);
        // Tests place objects themselves: no pretenuring after a minor
        // most of the nursery survived (see the test of that below).
        heap.pretenure_bytes = 0;
        moved
    }

    #[test]
    fn a_young_enum_typed_with_a_drop_survives_to_the_old_space() {
        let mut heap = nursery_heap();
        heap.register_finalizer_type(7);
        let head = cons_list(&mut heap, 1);
        let obj = heap.find_object_by_addr(head).expect("enum");
        let Object::Enum(gc) = obj else { unreachable!() };
        gc.payload_mut().type_id = 7;
        heap.note_enum_type(obj, 7);
        assert_eq!(minor(&mut heap, &[], &mut []), 1, "it moves instead of dying");
        assert_eq!(heap.live_object_count(), 1);
        let copy = heap.unmarked_finalizable();
        assert_eq!(copy.len(), 1, "the finalizable list names the copy");
        assert_eq!(copy[0].generation() & GEN_YOUNG, 0);
    }

    #[test]
    fn young_objects_are_allocated_in_the_nursery() {
        let mut heap = nursery_heap();
        let head = cons_list(&mut heap, 3);
        let obj = heap.find_object_by_addr(head).expect("head");
        assert_eq!(obj.generation() & GEN_YOUNG, GEN_YOUNG);
        assert!(heap.has_young());
    }

    #[test]
    fn a_minor_collection_moves_a_precisely_rooted_list_and_frees_the_rest() {
        let mut heap = nursery_heap();
        let head = cons_list(&mut heap, 500);
        let before = walk(&heap, head);
        let _garbage = cons_list(&mut heap, 1000);
        assert_eq!(heap.live_object_count(), 1500);
        let mut roots = [head];
        let moved = minor(&mut heap, &[], &mut roots);
        assert_eq!(moved, 500);
        assert_ne!(roots[0], head, "the root names the copy");
        assert_eq!(walk(&heap, roots[0]), before);
        assert_eq!(heap.live_object_count(), 500);
        assert!(!heap.has_young());
        let copy = heap.find_object_by_addr(roots[0]).expect("copy");
        assert_eq!(copy.generation(), GEN_UNLOGGED, "a copy is old and not remembered");
        assert!(heap.find_object_by_addr(head).is_none(), "the young slot is poisoned");
    }

    #[test]
    fn an_ambiguous_root_pins_its_target_and_holds_its_chunk() {
        let mut heap = nursery_heap();
        let head = cons_list(&mut heap, 100);
        let before = walk(&heap, head);
        let moved = minor(&mut heap, &[head], &mut []);
        assert_eq!(moved, 99, "everything but the pinned head moves");
        assert_eq!(walk(&heap, head), before, "the pinned head's link is rewritten");
        let obj = heap.find_object_by_addr(head).expect("pinned head stays");
        assert_eq!(obj.generation(), GEN_YOUNG, "it stays young in a held chunk");
        assert!(heap.has_young());
        // New young objects go to other chunks; the held one stays intact,
        // and later minors keep it where it is even unpinned.
        let _ = cons_list(&mut heap, 10);
        assert_eq!(minor(&mut heap, &[], &mut []), 0);
        assert_eq!(walk(&heap, head), before);
        // A major cycle that does not reach it frees it, and the next
        // minor resets its chunk.
        heap.begin_mark(&[]);
        while !heap.mark_quantum(usize::MAX) {}
        unsafe { heap.sweep() };
        assert!(heap.find_object_by_addr(head).is_none());
        assert_eq!(heap.live_object_count(), 0);
        minor(&mut heap, &[], &mut []);
        assert!(!heap.has_young());
    }

    #[test]
    fn a_minor_most_of_the_nursery_survives_pretenures_for_a_while() {
        let mut heap = nursery_heap();
        let head = cons_list(&mut heap, 100);
        let plan = heap.minor_plan(&[], &[head]);
        let head = plan.forward(Value::from(head)).expect("moved").bits();
        heap.minor_finish(plan);
        let old = cons_list(&mut heap, 1);
        let obj = heap.find_object_by_addr(old).expect("old");
        assert_eq!(obj.generation() & GEN_YOUNG, 0, "allocated old after a surviving minor");
        assert_eq!(obj.generation(), GEN_UNLOGGED, "nothing young to name: unlogged, not remembered");
        assert!(heap.find_object_by_addr(head).is_some());
        heap.pretenure_bytes = 0;
        let young = cons_list(&mut heap, 1);
        assert_eq!(heap.find_object_by_addr(young).expect("young").generation(), GEN_YOUNG);
    }

    #[test]
    fn a_held_object_keeps_what_it_names_alive() {
        let mut heap = nursery_heap();
        let head = cons_list(&mut heap, 50);
        let before = walk(&heap, head);
        // The head is pinned and held; the rest moves.
        assert_eq!(minor(&mut heap, &[head], &mut []), 49);
        // Unpinned it still holds the list up through later minors.
        let _garbage = cons_list(&mut heap, 100);
        assert_eq!(minor(&mut heap, &[], &mut []), 0);
        assert_eq!(walk(&heap, head), before);
        assert_eq!(heap.live_object_count(), 50);
        // A major cycle that reaches it keeps it and the list.
        heap.begin_mark(&[head]);
        while !heap.mark_quantum(usize::MAX) {}
        unsafe { heap.sweep() };
        assert_eq!(walk(&heap, head), before);
    }

    #[test]
    fn a_young_object_stored_into_an_old_one_survives_through_the_barrier() {
        let mut heap = nursery_heap();
        let (arr, _) = heap.alloc(ObjArray::new(vec![Value::from(0i64)]), Object::Array);
        let Object::Array(mut gc) = arr else { unreachable!() };
        gc.as_mut().set_elem_kind(common::WORD_POINTER);
        // The array survives a collection as a root and becomes old.
        let mut roots = [arr.addr()];
        minor(&mut heap, &[], &mut roots);
        let old = roots[0];
        let Some(Object::Array(mut old_gc)) = heap.find_object_by_addr(old) else {
            panic!("array survived")
        };
        assert_eq!(Object::Array(old_gc).generation() & GEN_YOUNG, 0);
        let head = cons_list(&mut heap, 20);
        let before = walk(&heap, head);
        // The store goes through the barrier, which remembers the array.
        unsafe { old_gc.as_mut().set_unchecked(0, Value::from(head)) };
        // No root names the list: only the remembered array keeps it.
        minor(&mut heap, &[old], &mut []);
        let Some(Object::Array(a)) = heap.find_object_by_addr(old) else {
            panic!("array stays")
        };
        let new_head = a.as_ref().iter().next().expect("element").bits();
        assert_ne!(new_head, head, "the precise element was rewritten");
        assert_eq!(walk(&heap, new_head), before);
    }

    #[test]
    fn an_old_object_written_while_the_quiet_log_overflowed_is_still_remembered() {
        let mut heap = nursery_heap();
        let (arr, _) = heap.alloc(ObjArray::new(vec![Value::from(0i64)]), Object::Array);
        let Object::Array(mut gc) = arr else { unreachable!() };
        gc.as_mut().set_elem_kind(common::WORD_POINTER);
        let mut roots = [arr.addr()];
        minor(&mut heap, &[], &mut roots);
        let old = roots[0];
        let Some(Object::Array(mut old_gc)) = heap.find_object_by_addr(old) else {
            panic!("array survived")
        };
        // No young object exists: the write turns the array's barrier off
        // and logs it as quiet; then the log overflows and is dropped.
        unsafe { old_gc.as_mut().set_unchecked(0, Value::from(0i64)) };
        assert!(QUIET.with_borrow(|q| q.contains(&old)));
        QUIET.with_borrow_mut(Vec::clear);
        super::super::QUIET_OVERFLOWS.with(|n| n.set(n.get() + 1));
        // The nursery starting again relogs the whole heap, so the next
        // store of a young list is remembered.
        let head = cons_list(&mut heap, 20);
        let before = walk(&heap, head);
        unsafe { old_gc.as_mut().set_unchecked(0, Value::from(head)) };
        minor(&mut heap, &[old], &mut []);
        let Some(Object::Array(a)) = heap.find_object_by_addr(old) else {
            panic!("array stays")
        };
        let new_head = a.as_ref().iter().next().expect("element").bits();
        assert_ne!(new_head, head, "the precise element was rewritten");
        assert_eq!(walk(&heap, new_head), before);
    }

    #[test]
    fn promote_young_keeps_every_young_object_in_place() {
        let mut heap = nursery_heap();
        let head = cons_list(&mut heap, 50);
        let before = walk(&heap, head);
        heap.promote_young();
        assert!(!heap.has_young());
        assert_eq!(walk(&heap, head), before);
        assert_eq!(heap.live_object_count(), 50);
        let obj = heap.find_object_by_addr(head).expect("head");
        assert_eq!(obj.generation(), GEN_UNLOGGED);
    }

    #[test]
    fn a_major_collection_keeps_young_objects_unmarked() {
        let mut heap = nursery_heap();
        let head = cons_list(&mut heap, 10);
        heap.begin_mark(&[head]);
        while !heap.mark_quantum(usize::MAX) {}
        unsafe { heap.sweep() };
        let obj = heap.find_object_by_addr(head).expect("young head survives a major cycle");
        assert!(!obj.is_marked(), "the sweep skipped it, so its mark was cleared");
        let mut roots = [head];
        minor(&mut heap, &[], &mut roots);
        assert_eq!(walk(&heap, roots[0]).len(), 10);
    }
}
