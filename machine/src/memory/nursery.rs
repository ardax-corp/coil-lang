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
    barriered, ObjArray, GcHeader, GcPhase, GcSized, Heap, Member, Object, GEN_HELD, GEN_PINNED, GEN_UNLOGGED, GEN_YOUNG, QUIET,
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
        if self.heap_checks {
            self.check_barrier(&remembered);
        }
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
        let mut fixups: Vec<*mut Value> = Vec::new();
        for &addr in &remembered {
            let Some(obj) = self.find_object_by_addr(addr) else {
                continue;
            };
            let at = gray.len();
            #[cfg(feature = "gc-stats")]
            if !matches!(obj, Object::Array(_)) {
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
                // One walk of an array: the precise slots that name a young
                // object are noted, and only they are rewritten after the
                // copy.
                Object::Array(a) => {
                    let (_dirty, _cards, _words) = self.reach_array_slots(a, &mut gray, &mut fixups);
                    #[cfg(feature = "gc-stats")]
                    {
                        census.dirty_cards += _dirty;
                        census.cards += _cards;
                        census.remembered_words += if _words == usize::MAX { obj.slot_words(self) } else { _words };
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
                if let Some(obj) = self.find_object_by_addr(addr)
                    && !matches!(obj, Object::Array(_))
                {
                    #[cfg(feature = "gc-stats")]
                    {
                        let n = obj.slot_words(self);
                        census.rewrite_words += n;
                        census.rewrite_remembered_words += n;
                    }
                    obj.rewrite_precise_refs(self, &fwd);
                }
            }
            // A slot whose young object was pinned after it was noted
            // (an ambiguous word found it later) has nothing to forward.
            for &slot in &fixups {
                // SAFETY: a word of a remembered array's storage, which no
                // step of a minor grows, shrinks or moves.
                unsafe {
                    if let Some(to) = forward(&fwd, *slot) {
                        *slot = to;
                    }
                }
            }
            self.forward_side_lists(&fwd);
        }
        #[cfg(feature = "gc-stats")]
        {
            census.fixups = fixups.len();
        }
        MinorPlan {
            fwd,
            remembered,
            spans,
            #[cfg(feature = "gc-stats")]
            census,
        }
    }

    /// Reach the young objects a remembered array names, as
    /// [`Object::for_each_reference`] would find them, and note the address
    /// of every precise slot that names one in `fixups`: the minor then
    /// forwards those slots alone instead of walking the array again.
    ///
    /// A word array with a card table ([`super::CardTable`]) is scanned
    /// only on its dirty cards, which the scan clears: after the minor
    /// nothing young is left for a clean card to hide. Returns `(dirty
    /// cards, cards, words scanned)` of a card scan, and `usize::MAX`
    /// words for any other walk (the whole array).
    fn reach_array_slots(&self, gc: super::Gc<ObjArray>, gray: &mut Vec<Object>, fixups: &mut Vec<*mut Value>) -> (usize, usize, usize) {
        // SAFETY: a live array; nothing else borrows its payload during a
        // minor. Not `payload_mut`: a rewrite is no write the barrier logs.
        let a = unsafe { &mut (*gc.as_ptr().cast_mut()).data };
        if !a.may_hold_refs() {
            return (0, 0, usize::MAX);
        }
        let (k, mask, kind) = (a.lanes(), a.tag_mask, a.elem_kind());
        if k == 1 && kind == common::WORD_SCALAR {
            return (0, 0, usize::MAX);
        }
        let bounds = self.slab.bounds();
        if k == 1 {
            let precise = kind == common::WORD_POINTER;
            let Some((words, cards)) = a.words_and_cards_mut() else {
                return (0, 0, usize::MAX);
            };
            let n = words.len();
            let base = words.as_mut_ptr();
            let mut scan = |lo: usize, hi: usize| {
                for i in lo..hi {
                    // SAFETY: `i < hi <= n`.
                    self.reach_slot(unsafe { base.add(i) }, precise, bounds, gray, fixups);
                }
            };
            let t = match cards {
                Some(t) if !t.all => t,
                cards => {
                    scan(0, n);
                    if let Some(t) = cards {
                        t.all = false;
                        t.cards.fill(0);
                    }
                    return (0, 0, usize::MAX);
                }
            };
            let total = t.cards.len();
            let mut dirty = 0;
            let mut words = 0;
            let size = 1usize << super::CARD_SHIFT;
            // Eight cards at a time: most of a large array is clean.
            for (at, group) in t.cards.chunks_mut(8).enumerate() {
                if group.iter().all(|&c| c == 0) {
                    continue;
                }
                for (j, c) in group.iter_mut().enumerate() {
                    if *c == 0 {
                        continue;
                    }
                    *c = 0;
                    dirty += 1;
                    let lo = (at * 8 + j) * size;
                    if lo < n {
                        let hi = (lo + size).min(n);
                        words += hi - lo;
                        scan(lo, hi);
                    }
                }
            }
            return (dirty, total, words);
        }
        let Some(words) = a.gc_words_mut() else {
            return (0, 0, usize::MAX);
        };
        let n = words.len();
        let base = words.as_mut_ptr();
        {
            for i in (0..n).step_by(k) {
                // SAFETY: `i + 1 < n` for a whole element of `k >= 2` words.
                let tag = unsafe { *base.add(i + 1) }.bits();
                if tag < 64 && mask >> tag & 1 == 1 {
                    self.reach_slot(unsafe { base.add(i) }, true, bounds, gray, fixups);
                }
            }
        }
        (0, 0, usize::MAX)
    }

    /// [`Self::reach_array_slots`] for one word at `slot`.
    #[inline(always)]
    fn reach_slot(&self, slot: *mut Value, precise: bool, (lo, hi): (u64, u64), gray: &mut Vec<Object>, fixups: &mut Vec<*mut Value>) {
        // SAFETY: a word of the array's storage.
        let addr = unsafe { *slot }.heap_addr();
        // Most words of an unknown-kind array are ints: the slab's bounds
        // turn them down before a chunk lookup.
        if addr < lo || addr >= hi {
            return;
        }
        let Some(obj) = self.find_object_by_addr(addr) else {
            return;
        };
        let g = obj.generation();
        if g & (GEN_YOUNG | GEN_HELD) != GEN_YOUNG {
            return;
        }
        if precise {
            fixups.push(slot);
        } else if g & GEN_PINNED == 0 {
            obj.set_generation(g | GEN_PINNED);
        }
        obj.mark(gray);
    }

    /// The missed-barrier check ([`super::heap::heap_checks_on`]): at the
    /// top of a minor, an old or held object the remembered set does not
    /// name must not name a young object. Every write that could store
    /// one goes through the barrier, which records the object; one that
    /// did not is a store the minor would never see, so the young object
    /// would be freed or moved under it. Panics naming the holder.
    #[cold]
    #[inline(never)]
    fn check_barrier(&self, remembered: &[u64]) {
        for obj in self.live_objects() {
            let g = obj.generation();
            if !barriered(g) {
                continue;
            }
            if remembered.binary_search(&obj.addr()).is_ok() {
                if let Object::Array(a) = obj {
                    self.check_cards(a.as_ref(), obj.addr());
                }
                continue;
            }
            obj.for_each_reference(self, &mut |addr, precise| {
                if let Some(young) = self.young_object(addr) {
                    panic!(
                        "missed write barrier: {} at {:#x} (generation {g:#x}) is not remembered but names {} at {addr:#x}, which is young, through a{} word",
                        obj.kind_name(),
                        obj.addr(),
                        young.kind_name(),
                        if precise { " precise" } else { "n ambiguous" },
                    );
                }
            });
        }
    }

    /// A remembered array with a card table ([`super::CardTable`]) is
    /// scanned on its dirty cards only: a young object in a clean card is
    /// a write that did not mark its card. Panics naming the array and
    /// the element.
    fn check_cards(&self, a: &ObjArray, at: u64) {
        let (Some(t), Some(words)) = (a.cards(), a.words()) else {
            return;
        };
        if t.all || !a.may_hold_refs() {
            return;
        }
        for (i, w) in words.iter().enumerate() {
            let card = t.cards.get(i >> super::CARD_SHIFT).copied().unwrap_or(0);
            if card == 0
                && let Some(young) = self.young_object(w.heap_addr())
            {
                panic!(
                    "missed card mark: element {i} of {} at {at:#x} names {} at {:#x}, which is young, in a clean card",
                    Object::Array(unsafe { super::Gc::from_addr(at) }).kind_name(),
                    young.kind_name(),
                    young.addr(),
                );
            }
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
                // A large array this minor scanned whole gets a card table:
                // the next one scans only what was written meanwhile.
                if let Object::Array(gc) = obj {
                    // SAFETY: as in `reach_array_slots`.
                    let arr = unsafe { &mut (*gc.as_ptr().cast_mut()).data };
                    if arr.attach_cards() {
                        self.account_array(arr);
                    }
                }
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

    /// The copy-on-write pin is header state: a minor copies it with the
    /// slot, as it does the shared flag.
    #[test]
    fn a_minor_carries_the_cow_pin_and_flag() {
        let mut heap = nursery_heap();
        let (a, _) = heap.alloc(crate::memory::ObjInstance::with_slots(5, vec![Value::from(1i64)]), Object::Instance);
        let (b, _) = heap.alloc(crate::memory::ObjInstance::with_slots(5, vec![Value::from(2i64)]), Object::Instance);
        a.set_pinned(true);
        assert_eq!(heap.cow_share(Value::from(b.addr())), crate::memory::CowShare::Marked);
        let mut roots = [a.addr(), b.addr()];
        assert_eq!(minor(&mut heap, &[], &mut roots), 2);
        assert_ne!(roots[0], a.addr(), "moved");
        let (a2, b2) = (heap.find_object_by_addr(roots[0]).expect("a"), heap.find_object_by_addr(roots[1]).expect("b"));
        assert!(a2.is_pinned() && !a2.is_shared());
        assert!(b2.is_shared() && !b2.is_pinned());
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

    /// An old pointer array that survived a minor, with the barrier armed.
    fn old_pointer_array(heap: &mut Heap, len: usize) -> u64 {
        let (arr, _) = heap.alloc(ObjArray::new(vec![Value::from(0i64); len]), Object::Array);
        let Object::Array(mut gc) = arr else { unreachable!() };
        gc.as_mut().set_elem_kind(common::WORD_POINTER);
        let mut roots = [arr.addr()];
        minor(heap, &[], &mut roots);
        assert_eq!(heap.find_object_by_addr(roots[0]).expect("old array").generation(), GEN_UNLOGGED);
        roots[0]
    }

    /// A remembered array's precise slots are noted as the minor reaches
    /// them and forwarded alone after the copy: a young element moves and
    /// its slot follows, one an ambiguous root pins stays where it is, and
    /// a null slot is left alone.
    #[test]
    fn a_remembered_arrays_young_pinned_and_null_elements_after_a_minor() {
        let mut heap = nursery_heap();
        heap.heap_checks = true;
        let old = old_pointer_array(&mut heap, 4);
        let moved = cons_list(&mut heap, 10);
        let pinned = cons_list(&mut heap, 5);
        let other = cons_list(&mut heap, 3);
        let (moved_list, pinned_list, other_list) = (walk(&heap, moved), walk(&heap, pinned), walk(&heap, other));
        let Some(Object::Array(mut gc)) = heap.find_object_by_addr(old) else { unreachable!() };
        unsafe {
            gc.as_mut().set_unchecked(0, Value::from(moved));
            gc.as_mut().set_unchecked(1, Value::from(pinned));
            gc.as_mut().set_unchecked(3, Value::from(other));
        }
        let plan = heap.minor_plan(&[pinned], &[]);
        let copied = plan.fwd.len();
        heap.minor_finish(plan);
        assert_eq!(copied, 10 + 4 + 3, "all but the pinned head move");
        let a = gc.as_ref();
        let slot = |i: usize| a.get(i).expect("element").bits();
        assert_ne!(slot(0), moved, "a moved element's slot names the copy");
        assert_eq!(walk(&heap, slot(0)), moved_list);
        assert_eq!(slot(1), pinned, "a pinned element stays where it is");
        assert_eq!(walk(&heap, slot(1)), pinned_list);
        assert_eq!(slot(2), 0, "a null slot stays null");
        assert_ne!(slot(3), other);
        assert_eq!(walk(&heap, slot(3)), other_list);
    }

    /// An old pointer array of `len` nulls with a clean card table: a
    /// minor scanned it whole as remembered and attached one.
    fn tracked_array(heap: &mut Heap, len: usize) -> (u64, super::super::Gc<ObjArray>) {
        let old = old_pointer_array(heap, len);
        let Some(Object::Array(mut gc)) = heap.find_object_by_addr(old) else { unreachable!() };
        assert!(!gc.as_ref().has_cards(), "a new old array has no table");
        // A young object first, or the write is logged as quiet.
        let _ = cons_list(heap, 1);
        gc.as_mut().set(0, Value::from(0i64));
        minor(heap, &[], &mut []);
        assert!(gc.as_ref().has_cards(), "a remembered large array gets a table");
        assert!(gc.as_ref().cards().is_some_and(|t| !t.all && t.cards.iter().all(|&c| c == 0)));
        (old, gc)
    }

    /// The dirty cards of `gc`'s table.
    fn dirty(gc: super::super::Gc<ObjArray>) -> Vec<usize> {
        let t = gc.as_ref().cards().expect("table");
        if t.all {
            return vec![usize::MAX];
        }
        t.cards.iter().enumerate().filter(|(_, c)| **c != 0).map(|(j, _)| j).collect()
    }

    /// Run a minor with the checks on, and check that the young list the
    /// array holds at `at` came through it.
    fn survives(heap: &mut Heap, gc: super::super::Gc<ObjArray>, at: usize, list: &[i64]) {
        minor(heap, &[], &mut []);
        let head = gc.as_ref().get(at).expect("element").bits();
        assert_eq!(walk(heap, head), list);
        assert_eq!(dirty(gc), Vec::<usize>::new(), "the minor cleared the cards it scanned");
    }

    /// Card of element `i`.
    fn card(i: usize) -> usize {
        i >> super::super::CARD_SHIFT
    }

    /// Cards of elements `lo..hi`.
    fn cards_of(lo: usize, hi: usize) -> Vec<usize> {
        (card(lo)..=card(hi - 1)).collect()
    }

    /// `CARD_MIN_WORDS` elements: the smallest array a minor tracks.
    const TRACKED: usize = super::super::CARD_MIN_WORDS;

    #[test]
    fn a_small_remembered_array_gets_no_card_table() {
        let mut heap = nursery_heap();
        let old = old_pointer_array(&mut heap, TRACKED - 1);
        let Some(Object::Array(mut gc)) = heap.find_object_by_addr(old) else { unreachable!() };
        let _ = cons_list(&mut heap, 1);
        gc.as_mut().set(0, Value::from(0i64));
        minor(&mut heap, &[], &mut []);
        assert!(!gc.as_ref().has_cards());
    }

    #[test]
    fn set_and_store_indexed_mark_one_card() {
        let mut heap = nursery_heap();
        heap.heap_checks = true;
        let (_, mut gc) = tracked_array(&mut heap, TRACKED);
        let a = cons_list(&mut heap, 3);
        let b = cons_list(&mut heap, 4);
        let c = cons_list(&mut heap, 5);
        let (la, lb, lc) = (walk(&heap, a), walk(&heap, b), walk(&heap, c));
        gc.as_mut().set(40, Value::from(a));
        unsafe { gc.as_mut().set_unchecked(500, Value::from(b)) };
        assert!(gc.as_mut().store_indexed(1023, Value::from(c), false));
        assert_eq!(dirty(gc), vec![card(40), card(500), card(1023)]);
        minor(&mut heap, &[], &mut []);
        assert_eq!(walk(&heap, gc.as_ref().get(40).unwrap().bits()), la);
        assert_eq!(walk(&heap, gc.as_ref().get(500).unwrap().bits()), lb);
        assert_eq!(walk(&heap, gc.as_ref().get(1023).unwrap().bits()), lc);
    }

    #[test]
    fn a_push_marks_the_tail_card_and_grows_the_table() {
        let mut heap = nursery_heap();
        heap.heap_checks = true;
        let (_, mut gc) = tracked_array(&mut heap, TRACKED);
        let cards = gc.as_ref().cards().unwrap().cards.len();
        let mut lists = Vec::new();
        // Past the capacity the table covered.
        for _ in 0..TRACKED + 40 {
            let l = cons_list(&mut heap, 2);
            lists.push(walk(&heap, l));
            gc.as_mut().push(Value::from(l));
        }
        assert!(gc.as_ref().cards().unwrap().cards.len() > cards, "the table grew with the array");
        assert_eq!(dirty(gc).first(), Some(&card(TRACKED)), "the cards below the old length stay clean");
        minor(&mut heap, &[], &mut []);
        for (k, list) in lists.iter().enumerate() {
            assert_eq!(&walk(&heap, gc.as_ref().get(TRACKED + k).unwrap().bits()), list);
        }
    }

    #[test]
    fn extend_and_fill_mark_the_cards_they_write() {
        let mut heap = nursery_heap();
        heap.heap_checks = true;
        let (_, mut gc) = tracked_array(&mut heap, TRACKED);
        let l = cons_list(&mut heap, 6);
        let list = walk(&heap, l);
        let (src, _) = heap.alloc(ObjArray::new(vec![Value::from(l); 20]), Object::Array);
        let Object::Array(src) = src else { unreachable!() };
        gc.as_mut().extend_from(src.as_ref(), 0, 20);
        assert_eq!(dirty(gc), cards_of(TRACKED, TRACKED + 20));
        survives(&mut heap, gc, TRACKED + 19, &list);
        let l = cons_list(&mut heap, 7);
        let list = walk(&heap, l);
        gc.as_mut().fill_push(Value::from(l), 3);
        assert_eq!(dirty(gc), cards_of(TRACKED + 20, TRACKED + 23));
        survives(&mut heap, gc, TRACKED + 22, &list);
    }

    #[test]
    fn insert_remove_drain_and_rotate_mark_the_elements_they_move() {
        let mut heap = nursery_heap();
        heap.heap_checks = true;
        let (_, mut gc) = tracked_array(&mut heap, TRACKED);
        // Each shift below moves a young list stored at 1000 across a card
        // boundary.
        let fresh = |heap: &mut Heap, gc: &mut super::super::Gc<ObjArray>| {
            let y = cons_list(heap, 4);
            gc.as_mut().set(1000, Value::from(y));
            // Clear the card the set marked, as if a minor had run but
            // the young list stayed: only the shift's own marks remain.
            gc.as_mut().words_and_cards_mut().unwrap().1.unwrap().cards.fill(0);
            y
        };
        // remove(5): 1000 moves to 999.
        let y = fresh(&mut heap, &mut gc);
        let yl = walk(&heap, y);
        gc.as_mut().remove(5);
        assert!(dirty(gc).contains(&card(999)));
        survives(&mut heap, gc, 999, &yl);
        // insert(0): 1000 moves to 1001.
        let y = fresh(&mut heap, &mut gc);
        let yl = walk(&heap, y);
        gc.as_mut().insert(0, Value::from(0i64));
        assert!(dirty(gc).contains(&card(1001)));
        survives(&mut heap, gc, 1001, &yl);
        // drain_range(0, 10): 1000 moves to 990.
        let y = fresh(&mut heap, &mut gc);
        let yl = walk(&heap, y);
        gc.as_mut().drain_range(0, 10);
        assert!(dirty(gc).contains(&card(990)));
        survives(&mut heap, gc, 990, &yl);
        // rotate_last_to(1000): 1000 moves to 1001.
        let y = fresh(&mut heap, &mut gc);
        let yl = walk(&heap, y);
        gc.as_mut().rotate_last_to(1000);
        assert!(dirty(gc).contains(&card(1001)));
        survives(&mut heap, gc, 1001, &yl);
    }

    #[test]
    fn raw_word_access_dirties_the_whole_table() {
        let mut heap = nursery_heap();
        heap.heap_checks = true;
        let (_, mut gc) = tracked_array(&mut heap, TRACKED);
        let l = cons_list(&mut heap, 5);
        let list = walk(&heap, l);
        gc.as_mut().words_mut()[700] = Value::from(l);
        assert_eq!(dirty(gc), vec![usize::MAX], "all dirty");
        survives(&mut heap, gc, 700, &list);
        assert!(!gc.as_ref().cards().unwrap().all, "the minor cleared it");
    }

    #[test]
    fn pop_clear_and_reserve_mark_nothing_and_a_copy_has_no_table() {
        let mut heap = nursery_heap();
        let (_, mut gc) = tracked_array(&mut heap, TRACKED);
        gc.as_mut().pop();
        gc.as_mut().reserve(100);
        assert_eq!(dirty(gc), Vec::<usize>::new());
        assert!(!gc.as_ref().copy().has_cards(), "a copy is a new object");
        gc.as_mut().clear();
        assert_eq!(dirty(gc), Vec::<usize>::new());
    }

    #[test]
    fn a_lane_array_is_never_tracked() {
        let mut heap = nursery_heap();
        let words: Vec<Value> = (0..2 * TRACKED as i64).map(|i| Value::from(i & 1)).collect();
        let (arr, _) = heap.alloc(ObjArray::from_lanes(words, 2, 0b10), Object::Array);
        let mut roots = [arr.addr()];
        minor(&mut heap, &[], &mut roots);
        let Some(Object::Array(mut gc)) = heap.find_object_by_addr(roots[0]) else { unreachable!() };
        let _ = cons_list(&mut heap, 1);
        gc.as_mut().set_lanes(0, &[Value::from(0i64), Value::from(0i64)]);
        minor(&mut heap, &[], &mut []);
        assert!(!gc.as_ref().has_cards());
    }

    /// Native code's store view of a tracked array keeps loads in place
    /// and sends stores through the runtime, which marks the card.
    #[test]
    fn a_store_view_of_a_tracked_array_is_tracked() {
        let mut heap = nursery_heap();
        let (old, _) = tracked_array(&mut heap, TRACKED);
        let mut view = [0u64; 3];
        assert_eq!(unsafe { crate::native::coil_native_view(old, 1, view.as_mut_ptr()) }, super::super::VIEW_TRACKED);
        assert_eq!(unsafe { crate::native::coil_native_view(old, 0, view.as_mut_ptr()) }, super::super::VIEW_PLAIN);
        assert_eq!(view[1], TRACKED as u64);
        let small = old_pointer_array(&mut heap, 4);
        assert_eq!(unsafe { crate::native::coil_native_view(small, 1, view.as_mut_ptr()) }, super::super::VIEW_PLAIN);
    }

    /// A store into a tracked array that skips its card leaves a young
    /// object in a clean card: the check names the array and the element.
    #[test]
    #[should_panic(expected = "missed card mark: element 300 of a pointer-kind array")]
    fn the_barrier_check_finds_a_store_that_skipped_its_card() {
        let mut heap = nursery_heap();
        heap.heap_checks = true;
        let (_, mut gc) = tracked_array(&mut heap, TRACKED);
        let l = cons_list(&mut heap, 2);
        // Remembered through a marked store elsewhere...
        gc.as_mut().set(10, Value::from(0i64));
        // ...and a store in place, as a plain view would make it.
        let (ptr, _) = gc.as_ref().words_view_for_load().expect("word array");
        unsafe { ptr.add(300).write(Value::from(l)) };
        minor(&mut heap, &[], &mut []);
    }

    /// A store that skips the barrier leaves an old object naming a young
    /// one the remembered set does not know: the check at the top of the
    /// next minor names the holder.
    #[test]
    #[should_panic(expected = "missed write barrier: a pointer-kind array")]
    fn the_barrier_check_finds_a_store_that_skipped_the_barrier() {
        let mut heap = nursery_heap();
        heap.heap_checks = true;
        let old = old_pointer_array(&mut heap, 4);
        let head = cons_list(&mut heap, 5);
        let Some(Object::Array(gc)) = heap.find_object_by_addr(old) else { unreachable!() };
        // Native code's in-place store, through a view taken for loads.
        let (ptr, len) = gc.as_ref().words_view_for_load().expect("word array");
        assert_eq!(len, 4);
        unsafe { ptr.add(2).write(Value::from(head)) };
        minor(&mut heap, &[], &mut []);
    }

    /// #2687 risk 2 (#2702): native code takes a store view right after a minor,
    /// while no young object exists, so the barrier logs the array as quiet.
    /// The next allocation starts the nursery again and re-arms the array,
    /// but the view stays live (an allocation drops no views), so the view
    /// then stores a young object in place. The minor must still find it.
    #[test]
    fn a_store_view_taken_while_the_heap_is_quiet_survives_a_nursery_restart() {
        let mut heap = nursery_heap();
        heap.heap_checks = true;
        let old = old_pointer_array(&mut heap, 4);
        assert!(!heap.has_young_in_use(), "the minor left the nursery empty");
        let mut view = [0u64; 3];
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
