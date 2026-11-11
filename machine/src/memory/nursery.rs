//! Nursery minor collection (`feature/escape-memory` step 4).
//!
//! Instances, payload enums, boxes, tuples, arrays and closures are
//! bump-allocated into young slab chunks ([`super::Slab::alloc_young`]). A minor collection
//! traces young objects from the roots and the remembered set (old
//! objects written since the last one, see the barrier in
//! [`super::GcData`]'s `as_mut`), copies every survivor only precise words
//! name into an old slot, rewrites those words, and resets the young chunks.
//! A survivor an ambiguous word names is pinned: it stays where it is and
//! its chunk is held until a major frees it. See `docs/internals/escape-memory.md`.

use std::ptr::NonNull;

use common::Value;

use super::compact::{forward, AddrMap};
use super::{
    barriered, GcHeader, GcPhase, GcSized, Heap, Member, Object, GEN_HELD, GEN_PINNED, GEN_UNLOGGED, GEN_YOUNG, QUIET,
    REMEMBERED,
};

/// A minor collection between its copy and its reset step.
pub struct MinorPlan {
    fwd: AddrMap,
    remembered: Vec<u64>,
    /// `(chunk, first slot, end, slot size)` of every young chunk in use.
    spans: Vec<(usize, u64, u64, u64)>,
    #[cfg(feature = "gc-stats")]
    census: MinorCensus,
}

/// `gc-stats`: what one minor collection scanned and did, printed when it
/// finishes (`gc-stats minor: ...`).
#[cfg(feature = "gc-stats")]
#[derive(Default)]
struct MinorCensus {
    /// Objects the remembered set named.
    remembered: usize,
    /// Slot words the reach pass walked in them.
    remembered_words: usize,
    /// Slot words the rewrite pass walked: in the copies, the pinned
    /// survivors and the remembered objects, and in the last alone.
    rewrite_words: usize,
    rewrite_remembered_words: usize,
    /// Slots the rewrite pass forwarded one by one, without a walk.
    fixups: usize,
    /// Dirty cards over cards of the remembered arrays scanned by card.
    dirty_cards: usize,
    cards: usize,
}

#[cfg(feature = "gc-stats")]
impl MinorCensus {
    fn line(&self, copied: usize, pinned: usize, dead: usize) -> String {
        format!(
            "gc-stats minor: remembered={} remembered_words={} rewrite_words={} (remembered {}) fixups={} \
             cards={}/{} copied={copied} pinned={pinned} dead={dead}",
            self.remembered,
            self.remembered_words,
            self.rewrite_words,
            self.rewrite_remembered_words,
            self.fixups,
            self.dirty_cards,
            self.cards,
        )
    }
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
    /// The young object at `addr` (a `Result` `Err` tag bit is ignored),
    /// not counting a held one, which minors treat as old.
    #[inline]
    fn young_object(&self, addr: u64) -> Option<Object> {
        let obj = self.find_object_by_addr(addr & !1)?;
        (obj.generation() & (GEN_YOUNG | GEN_HELD) == GEN_YOUNG).then_some(obj)
    }

    /// Every live object: the old chunks' slots, then the written slots of
    /// the young ones. Unlike [`Heap::objects`] it never reads a nursery
    /// chunk's unwritten tail or a spare chunk, which under `gc-stress` (a
    /// minor after every allocation) can be hundreds of mostly empty held
    /// chunks walked at every collection.
    pub(super) fn live_objects(&self) -> Vec<Object> {
        // The sweep's walk: every chunk but the nursery's.
        let cur = crate::memory::slab::SlotCursor::sweep();
        let mut out: Vec<Object> = super::HeapIter { slab: &self.slab, cur }.collect();
        for (_, first, end, size) in self.slab.young_spans() {
            out.extend(slots(first, end, size).filter_map(|addr| unsafe { Object::from_header(addr) }));
        }
        out
    }

    /// Whether any nursery chunk holds young objects (in use or held).
    pub fn has_young(&self) -> bool {
        self.slab.has_young_chunks()
    }

    /// Whether a minor collection has work: a nursery chunk allocated into
    /// since the last one. Held chunks alone need none, since minors do not
    /// trace held objects and only a major frees them.
    pub fn has_young_in_use(&self) -> bool {
        self.slab.young_chunks_in_use() != 0
    }

    /// Take this heap's entries off the thread's remembered set: the live
    /// old and held objects among them. Entries of other heaps stay.
    fn take_remembered(&self) -> Vec<u64> {
        let mut mine = REMEMBERED.with_borrow_mut(|r| {
            let mut mine = Vec::new();
            r.retain(|&addr| {
                if !self.slab.contains_slot(addr) {
                    return true;
                }
                if self.find_object_by_addr(addr).is_some_and(|o| barriered(o.generation())) {
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
                let mut moved = false;
                for addr in r.iter_mut() {
                    if let Some(&to) = fwd.get(addr) {
                        *addr = to;
                        moved = true;
                    }
                }
                // A moved copy is unlogged again, so its next write adds
                // the new address a second time: drop the repeat, or a
                // compaction loop grows the set by one per cycle.
                if moved {
                    r.sort_unstable();
                    r.dedup();
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
                    self.counts.alloc_bytes -= bytes;
                    debug_assert!(self.counts.live_count > 0);
                    self.counts.live_count -= 1;
                    unsafe { obj.recycle_payload() };
                }
            }
            if !live {
                empty.push(chunk);
            }
        }
        self.slab.unhold(&empty);
        self.set_held_after_major(self.slab.held_chunks());
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
        self.counts.young_bytes = 0;
        self.slab.finish_young(&promoted, &[], free);
        // Held chunks were promoted with the rest: none is held now.
        self.set_held_after_major(0);
    }

    /// Free the nursery's objects: only the heap's own immortals stay. A
    /// finished job's heap has no other roots, and [`Self::collect`] does
    /// not reach young objects. Needs an idle collector.
    pub fn clear_young(&mut self) {
        if !self.has_young_in_use() {
            return;
        }
        let pins = self.take_gc_roots();
        let plan = self.minor_plan(&pins, &[]);
        self.restore_gc_roots(pins);
        self.minor_finish(plan);
    }

    /// Trace the young objects reachable from `pins` (roots that cannot be
    /// rewritten, which pin their target), `precise` (roots the runtime rewrites
    /// through [`MinorPlan::forward`]) and the remembered set; copy every
    /// survivor that is not pinned and rewrite the heap's precise words to
    /// it. The runtime then rewrites its precise roots and calls
    /// [`Self::minor_finish`].
    pub fn minor_plan(&mut self, pins: &[u64], precise: &[u64]) -> MinorPlan {
        debug_assert_eq!(self.gc_phase, GcPhase::Idle, "minor collections run between cycles");
        let spans = self.slab.young_spans();
        let remembered = self.take_remembered();
        #[cfg(feature = "gc-stats")]
        let mut census = MinorCensus { remembered: remembered.len(), ..MinorCensus::default() };
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
        for &addr in precise {
            reach(self, &mut gray, addr, true);
        }
        // A young object typed with a `drop` after its allocation
        // (`note_enum_type`, `register_finalizer_type`) owes its finalizer:
        // it survives to the old space, where a major runs it.
        for &addr in &self.finalizable {
            reach(self, &mut gray, addr, true);
        }
        // Young objects an array reached first, in index order: they are
        // copied first, so a loop over the array reads its elements' copies
        // one after another (locality step 5). The rest keep their nursery
        // order. Marking pushes only what it newly marks, so an array's
        // share of `gray` is its new elements in order.
        let mut by_index: Vec<Object> = Vec::new();
        for &addr in &remembered {
            let Some(obj) = self.find_object_by_addr(addr) else {
                continue;
            };
            let at = gray.len();
            #[cfg(feature = "gc-stats")]
            {
                census.remembered_words += obj.slot_words(self);
            }
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
            if matches!(obj, Object::Array(_)) {
                by_index.extend_from_slice(&gray[at..]);
            }
        }
        while let Some(obj) = gray.pop() {
            let at = gray.len();
            obj.for_each_reference(self, &mut |a, p| reach(self, &mut gray, a, p));
            if matches!(obj, Object::Array(_)) {
                by_index.extend_from_slice(&gray[at..]);
            }
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
        let mut ranges: Vec<(u64, u64)> = spans.iter().map(|&(_, first, end, _)| (first, end)).collect();
        ranges.sort_unstable();
        let in_use = |addr: u64| {
            let i = ranges.partition_point(|&(first, _)| first <= addr);
            i > 0 && addr < ranges[i - 1].1
        };
        for obj in by_index {
            // A held object (pinned by an earlier minor) is never marked by
            // a minor, so it stays where it is.
            if in_use(obj.addr()) {
                self.copy_survivor(obj, &mut fwd, true);
            }
        }
        let moved = !fwd.is_empty();
        for &(_, first, end, size) in &spans {
            for addr in slots(first, end, size) {
                if let Some(obj) = unsafe { Object::from_header(addr) } {
                    self.copy_survivor(obj, &mut fwd, moved);
                }
            }
        }

        // Rewrite precise words naming a moved object: in the copies, the
        // pinned survivors and the remembered objects. Nothing else old can
        // name a young object.
        if !fwd.is_empty() {
            for &to in fwd.values() {
                if let Some(obj) = unsafe { Object::from_header(to) } {
                    #[cfg(feature = "gc-stats")]
                    {
                        census.rewrite_words += obj.slot_words(self);
                    }
                    obj.rewrite_precise_refs(self, &fwd);
                }
            }
            for &(_, first, end, size) in &spans {
                for addr in slots(first, end, size) {
                    if let Some(obj) = unsafe { Object::from_header(addr) }
                        && obj.is_marked()
                        && !fwd.contains_key(&addr)
                    {
                        #[cfg(feature = "gc-stats")]
                        {
                            census.rewrite_words += obj.slot_words(self);
                        }
                        obj.rewrite_precise_refs(self, &fwd);
                    }
                }
            }
            for &addr in &remembered {
                if let Some(obj) = self.find_object_by_addr(addr) {
                    #[cfg(feature = "gc-stats")]
                    {
                        let n = obj.slot_words(self);
                        census.rewrite_words += n;
                        census.rewrite_remembered_words += n;
                    }
                    obj.rewrite_precise_refs(self, &fwd);
                }
            }
            self.forward_side_lists(&fwd);
        }
        MinorPlan {
            fwd,
            remembered,
            spans,
            #[cfg(feature = "gc-stats")]
            census,
        }
    }

    /// Copy `obj`, a young survivor, to an old slot unless something
    /// ambiguous names it or (`moved`: some object may have) it already
    /// moved.
    #[inline]
    fn copy_survivor(&mut self, obj: Object, fwd: &mut AddrMap, moved: bool) {
        let addr = obj.addr();
        if !obj.is_marked() || obj.generation() & GEN_PINNED != 0 || moved && fwd.contains_key(&addr) {
            return;
        }
        let Some(layout) = obj.movable_layout() else {
            obj.set_generation(obj.generation() | GEN_PINNED);
            return;
        };
        let to = self.slab.alloc(layout);
        // SAFETY: same size class; the young copy is poisoned in
        // `minor_finish` and never dropped, so the payload (spill `Vec`s
        // included) moves bitwise.
        unsafe {
            std::ptr::copy_nonoverlapping(addr as *const u8, to.as_ptr(), layout.size());
            let header = &*(to.as_ptr() as *const GcHeader);
            header.marked.set(false);
            header.generation.set(0);
        }
        fwd.insert(addr, to.as_ptr() as u64);
    }

    /// Poison the moved slots, reclaim the dead young objects, hold the
    /// chunks with pinned survivors in the nursery and reset the rest.
    /// Returns how many objects moved.
    pub fn minor_finish(&mut self, plan: MinorPlan) -> usize {
        let MinorPlan {
            fwd,
            remembered,
            spans,
            #[cfg(feature = "gc-stats")]
            census,
        } = plan;
        let mut held = Vec::new();
        let (mut kept, mut dead) = (0, 0);
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
                if header.generation.get() & GEN_HELD != 0 {
                    // Held by an earlier minor: not traced, kept as it is.
                    keep = true;
                } else if header.marked.get() {
                    // Pinned: held where it is until a major cycle finds it
                    // dead ([`Self::unmark_young`]); its chunk stays out of
                    // reuse until then. Later minors treat it as old, so
                    // the barrier records writes to it.
                    header.marked.set(false);
                    header.generation.set(GEN_YOUNG | GEN_HELD | GEN_UNLOGGED);
                    kept += 1;
                    keep = true;
                } else {
                    dead += 1;
                    let bytes = obj.size();
                    self.counts.alloc_bytes -= bytes;
                    debug_assert!(self.counts.live_count > 0);
                    self.counts.live_count -= 1;
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
                && barriered(obj.generation())
            {
                obj.set_generation(obj.generation() & (GEN_YOUNG | GEN_HELD) | GEN_UNLOGGED);
            }
        }
        #[cfg(feature = "gc-stats")]
        eprintln!("{}", census.line(fwd.len(), kept, dead));
        // Held objects count as old: only a major frees them.
        self.counts.young_bytes = 0;
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
        heap.sync_inline_gate();
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

    /// A pinned object that dies gives its chunk back to the nursery once a
    /// major frees it. A stale stack word pins one object per minor: if
    /// each such chunk stayed in the old space, a churn loop's footprint
    /// would grow with its run time (`result_heap_churn`, #2255).
    #[test]
    fn a_dead_pinned_objects_chunk_returns_to_the_nursery() {
        let mut heap = nursery_heap();
        let mut most = 0;
        for _ in 0..200 {
            let head = cons_list(&mut heap, 100);
            minor(&mut heap, &[head], &mut []);
            heap.begin_mark(&[]);
            while !heap.mark_quantum(usize::MAX) {}
            unsafe { heap.sweep() };
            let _ = cons_list(&mut heap, 1);
            minor(&mut heap, &[], &mut []);
            most = most.max(heap.slab.chunk_count());
        }
        assert!(most <= 8, "chunks stay bounded: {most}");
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
        assert_eq!(obj.generation(), GEN_YOUNG | GEN_HELD | GEN_UNLOGGED, "it is held, behind the barrier");
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
        heap.sync_inline_gate();
        let young = cons_list(&mut heap, 1);
        assert_eq!(heap.find_object_by_addr(young).expect("young").generation(), GEN_YOUNG);
    }

    #[test]
    fn a_minor_copies_an_arrays_young_elements_in_index_order() {
        let mut heap = nursery_heap();
        let mut made = Vec::new();
        for v in 0..64 {
            let (e, _) = heap.alloc(ObjEnum::new(0, EnumPayload::one(Value::from(v)).with_kinds(1)), Object::Enum);
            made.push(e.addr());
        }
        // The array holds them last allocated first.
        let words = made.iter().rev().map(|&a| Value::from(a)).collect();
        let (arr, mut gc) = heap.alloc(ObjArray::new(words), Object::Array);
        gc.as_mut().set_elem_kind(common::WORD_POINTER);
        let mut roots = [arr.addr()];
        assert_eq!(minor(&mut heap, &[], &mut roots), 65);
        let Some(Object::Array(a)) = heap.find_object_by_addr(roots[0]) else {
            panic!("array moved");
        };
        let addrs: Vec<u64> = a.as_ref().iter().map(|w| w.heap_addr()).collect();
        for (k, &addr) in addrs.iter().enumerate() {
            let Some(Object::Enum(e)) = heap.find_object_by_addr(addr) else {
                panic!("element {k} is not an enum");
            };
            assert_eq!(e.as_ref().payload[0].as_int(), 63 - k as i64);
        }
        assert!(addrs.windows(2).all(|w| w[0] < w[1]), "copies follow the array's order");
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

    /// Held chunks a major finds live do not ask for the next major
    /// (#2350). A map's values sit in a `Vec<V>` a shared generic body
    /// made, whose words are of unknown kind, so every minor pins and holds
    /// its survivors. Once a nursery's worth of chunks was held and still live,
    /// every safepoint started a major that marked the whole heap again.
    /// The held-chunk budget grows with what the last major kept.
    #[test]
    fn held_chunks_a_major_keeps_do_not_ask_for_the_next_major() {
        let mut heap = nursery_heap();
        let major = |heap: &mut Heap, roots: &[u64]| {
            heap.begin_mark(roots);
            while !heap.mark_quantum(usize::MAX) {}
            unsafe { heap.sweep() };
        };
        // Each minor pins one list head and holds its chunk.
        let mut pins = Vec::new();
        while heap.slab.held_chunks() < heap.nursery_chunks {
            pins.push(cons_list(&mut heap, 20));
            minor(&mut heap, &pins, &mut []);
        }
        let held = heap.slab.held_chunks();
        assert!(heap.should_collect_old(), "a nursery's worth of held chunks asks for a major");
        major(&mut heap, &pins);
        assert_eq!(heap.slab.held_chunks(), held, "the major keeps every pinned head");
        assert!(!heap.should_collect_old(), "live held chunks do not ask for another major");
        // The budget doubles: holding as many chunks again asks for one.
        while heap.slab.held_chunks() < 2 * held - 1 {
            pins.push(cons_list(&mut heap, 20));
            minor(&mut heap, &pins, &mut []);
            assert!(!heap.should_collect_old(), "under the held budget");
        }
        pins.push(cons_list(&mut heap, 20));
        minor(&mut heap, &pins, &mut []);
        assert!(heap.should_collect_old(), "twice what the last major kept asks for a major");
        // Once they die, a major frees them and the budget falls back.
        major(&mut heap, &[]);
        assert_eq!(heap.slab.held_chunks(), 0);
        pins.clear();
        while heap.slab.held_chunks() < heap.nursery_chunks - 1 {
            pins.push(cons_list(&mut heap, 20));
            minor(&mut heap, &pins, &mut []);
        }
        assert!(!heap.should_collect_old());
        pins.push(cons_list(&mut heap, 20));
        minor(&mut heap, &pins, &mut []);
        assert!(heap.should_collect_old(), "a nursery's worth of new held chunks asks for a major again");
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

    /// Minors do not trace a held object (#2255: a held 1M-element array cost
    /// a full scan per minor), so a young object stored into one later is
    /// found through the barrier, as for an old object.
    #[test]
    fn a_young_object_stored_into_a_held_one_survives_through_the_barrier() {
        let mut heap = nursery_heap();
        let (arr, _) = heap.alloc(ObjArray::new(vec![Value::from(0i64)]), Object::Array);
        let Object::Array(mut gc) = arr else { unreachable!() };
        gc.as_mut().set_elem_kind(common::WORD_POINTER);
        // An ambiguous root pins the array: it is held where it is.
        minor(&mut heap, &[arr.addr()], &mut []);
        assert_eq!(arr.generation(), GEN_YOUNG | GEN_HELD | GEN_UNLOGGED);
        let head = cons_list(&mut heap, 20);
        let before = walk(&heap, head);
        unsafe { gc.as_mut().set_unchecked(0, Value::from(head)) };
        // No root names the array or the list: the held array stays until a
        // major, and the barrier's record keeps the list.
        minor(&mut heap, &[], &mut []);
        let new_head = gc.as_ref().iter().next().expect("element").bits();
        assert_ne!(new_head, head, "the precise element was rewritten");
        assert_eq!(walk(&heap, new_head), before);
        assert_eq!(arr.generation(), GEN_YOUNG | GEN_HELD | GEN_UNLOGGED, "logged again after the minor");
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

    /// The `gc-stats` census of a minor counts what the remembered set made
    /// it scan and rewrite.
    #[cfg(feature = "gc-stats")]
    #[test]
    fn the_minor_census_counts_remembered_words() {
        let mut heap = nursery_heap();
        let (arr, _) = heap.alloc(ObjArray::new(vec![Value::from(0i64); 8]), Object::Array);
        let mut roots = [arr.addr()];
        minor(&mut heap, &[], &mut roots);
        let Some(Object::Array(mut old)) = heap.find_object_by_addr(roots[0]) else {
            panic!("array survived")
        };
        old.as_mut().set_elem_kind(common::WORD_POINTER);
        let head = cons_list(&mut heap, 3);
        unsafe { old.as_mut().set_unchecked(0, Value::from(head)) };
        let plan = heap.minor_plan(&[], &[]);
        let c = &plan.census;
        assert_eq!((c.remembered, c.remembered_words), (1, 8));
        assert_eq!(c.rewrite_remembered_words, 8, "the remembered array is walked again to rewrite it");
        let line = c.line(plan.fwd.len(), 0, 0);
        assert!(line.starts_with("gc-stats minor: remembered=1 remembered_words=8 "), "{line}");
        assert!(line.contains(" copied=3 "), "{line}");
        heap.minor_finish(plan);
    }

    /// #2687 risk 2: native code takes a store view right after a minor,
    /// while no young object exists, so the barrier logs the array as quiet.
    /// The next allocation starts the nursery again and re-arms the array,
    /// but the view stays live (an allocation drops no views), so the view
    /// then stores a young object in place. The minor must still find it.
    #[test]
    fn a_store_view_taken_while_the_heap_is_quiet_survives_a_nursery_restart() {
        let mut heap = nursery_heap();
        let (arr, _) = heap.alloc(ObjArray::new(vec![Value::from(0i64); 4]), Object::Array);
        let Object::Array(mut gc) = arr else { unreachable!() };
        gc.as_mut().set_elem_kind(common::WORD_POINTER);
        let mut roots = [arr.addr()];
        minor(&mut heap, &[], &mut roots);
        let old = roots[0];
        assert!(!heap.has_young_in_use(), "the minor left the nursery empty");
        let mut view = [0u64; 2];
        let state = unsafe { crate::native::coil_native_view(old, 1, view.as_mut_ptr()) };
        assert_eq!(state, 1, "a word array is viewed in place");
        // The nursery starts again with this allocation; the view is live.
        let head = cons_list(&mut heap, 20);
        let before = walk(&heap, head);
        assert!(heap.has_young_in_use());
        unsafe { (view[0] as *mut Value).add(1).write(Value::from(head)) };
        // Only the array names the list.
        minor(&mut heap, &[old], &mut []);
        let Some(Object::Array(a)) = heap.find_object_by_addr(old) else {
            panic!("array stays")
        };
        let new_head = a.as_ref().get(1).expect("element").bits();
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
