//! Reference counts: each object's header count is the number of places
//! that own it: heap words the runtime and native code store, and frames'
//! words, which the compiler's insertion pass counts (`RcInc` / `RcDec`,
//! [`Heap::rc_retain`] / [`Heap::rc_drop`]). An object is freed when its
//! count reaches zero; cycles go to the cycle collector (`cycles.rs`).

use super::*;

thread_local! {
    /// Set while this thread frees objects born in the open steal epoch
    /// ([`Heap::rc_epoch_free`]): one of them whose count reaches zero
    /// goes on its worklist ([`Heap::rc_dec_obj`] says it was the last)
    /// instead of starting a release of its own.
    static EPOCH_FREEING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// That worklist and what it freed, kept between releases.
    static EPOCH_FREE_STACK: std::cell::Cell<(Vec<Object>, EpochFreed)> = std::cell::Cell::new(Default::default());
}

/// `COIL_RC_LEAKS=1`: a heap that goes away names on stderr the objects
/// still in it that are not immortal: what a static still held, or what
/// counts leaked (no collector frees it).
pub(crate) fn rc_leaks_shown() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("COIL_RC_LEAKS").is_some_and(|v| v == "1"))
}

impl GcHeader {
    #[inline]
    fn inc(&self) {
        let c = self.count.load(Ordering::Relaxed);
        if c != COUNT_IMMORTAL {
            // A count that would reach the immortal one stays one below it:
            // the object then lives as long as the program.
            self.count.store(c.saturating_add(1).min(COUNT_IMMORTAL - 1), Ordering::Relaxed);
        }
    }

    /// One owner fewer. Returns whether that was the last.
    #[inline]
    fn dec(&self) -> bool {
        let c = self.count.load(Ordering::Relaxed);
        if c == COUNT_IMMORTAL || c == 0 {
            return false;
        }
        self.count.store(c - 1, Ordering::Relaxed);
        c == 1
    }

    /// [`Self::inc`] by an epoch's mutators, which share the object.
    #[inline]
    fn inc_shared(&self) {
        let _ = self.count.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |c| {
            (c < COUNT_IMMORTAL - 1).then_some(c + 1)
        });
    }

    /// [`Self::dec`] by an epoch's mutators. Returns whether that was the
    /// last owner.
    #[inline]
    fn dec_shared(&self) -> bool {
        // Only a count kept outside an epoch becomes immortal, and an owner
        // that goes had a count: one subtraction.
        let c = self.count.load(Ordering::Relaxed);
        if c == COUNT_IMMORTAL || c == 0 {
            return false;
        }
        self.count.fetch_sub(1, Ordering::Release) == 1
    }
}

impl Object {
    /// The object's reference count ([`COUNT_IMMORTAL`] for one that lives
    /// as long as the program).
    #[inline]
    pub fn rc_count(&self) -> u32 {
        self.header().count.load(Ordering::Relaxed)
    }

    /// Make the object immortal: its count never changes again.
    #[inline]
    pub fn rc_set_immortal(&self) {
        self.header().count.store(COUNT_IMMORTAL, Ordering::Relaxed);
    }

    #[inline]
    pub fn rc_inc(&self) {
        self.header().inc();
    }

    /// Set the count (a steal epoch's logged changes, summed at its end).
    #[inline]
    pub(crate) fn rc_set_count(&self, n: u32) {
        self.header().count.store(n, Ordering::Relaxed);
    }

    /// One owner fewer; nothing is freed at zero yet (step 2). Returns
    /// whether that was the last.
    #[inline]
    pub fn rc_dec(&self) -> bool {
        self.header().dec()
    }
}

impl Heap {
    /// Counts are kept in this run: a check run (step 2 keeps them only
    /// there), outside a shared-heap steal epoch (whose workers never write
    /// the counts of objects from before it, plan step 8).
    #[inline]
    pub fn rc_counting(&self) -> bool {
        self.rc_on
    }

    /// Counts are kept and the headers hold them: no steal epoch is open
    /// (its changes wait in logs until it ends, [`Heap::rc_log`]).
    #[inline]
    pub fn rc_counts_live(&self) -> bool {
        self.rc_on && self.alloc_lock.is_null()
    }

    /// Born in the open steal epoch: its mutators count it in place
    /// (atomically), as no count of it predates the epoch. One from before
    /// keeps its header count until the epoch ends (plan step 8).
    #[inline]
    fn epoch_born(&self, o: Object) -> bool {
        o.header().epoch.get() == self.epoch_gen
    }

    /// `o` gains an owner (logged while a steal epoch is open, unless it was
    /// born in it).
    #[inline]
    pub(crate) fn rc_inc_obj(&self, o: Object) {
        if self.alloc_lock.is_null() {
            o.rc_inc();
        } else if self.epoch_born(o) {
            o.header().inc_shared();
        } else if o.rc_count() != COUNT_IMMORTAL {
            self.rc_log(o, false);
        }
    }

    /// `o` loses an owner; whether that was the last. While a steal epoch
    /// is open the change is logged and its end releases, unless `o` was
    /// born in it: then it is counted in place and freed on this thread
    /// when that was its last owner ([`Self::rc_epoch_free`]).
    #[inline(always)]
    pub(crate) fn rc_dec_obj(&self, o: Object) -> bool {
        if self.alloc_lock.is_null() {
            return o.rc_dec();
        }
        self.rc_dec_obj_epoch(o)
    }

    /// [`Self::rc_dec_obj`] while a steal epoch is open.
    #[inline(never)]
    fn rc_dec_obj_epoch(&self, o: Object) -> bool {
        if self.epoch_born(o) {
            if o.header().dec_shared() {
                if EPOCH_FREEING.get() {
                    return true;
                }
                self.rc_epoch_free(o);
            }
        } else if o.rc_count() != COUNT_IMMORTAL {
            self.rc_log(o, true);
        }
        false
    }

    /// Native code took the last owner of `w`, an object born in the open
    /// steal epoch, counting it in place: it is freed here
    /// ([`Self::rc_epoch_free`]).
    pub fn rc_epoch_zero(&self, w: Value) {
        if let Some(o) = self.rc_object(w) {
            self.rc_epoch_free(o);
        }
    }

    /// Free `obj`, born in the open steal epoch, whose last owner just
    /// went: on this thread, without the lock. Its words lose an owner;
    /// those born in the epoch that reach zero are freed in turn (a
    /// worklist), those from before are logged as any count change in an
    /// epoch is. Its slot is this thread's for its next allocation of the
    /// class ([`Self::epoch_dealloc`]).
    ///
    /// Sound because an object born in the epoch is counted in place by
    /// every mutator: a zero is its last owner, on any thread. A fork's
    /// arguments are no exception: its spawner owns them until the join.
    /// What owes more than its slot at its end (a `drop`, a close, a weak
    /// handle to clear, an interned string's table entry) is left to the
    /// epoch's end, as before ([`Self::rc_log_zero`]).
    #[inline(never)]
    pub(crate) fn rc_epoch_free(&self, obj: Object) {
        if !self.rc_frees() {
            self.rc_log_zero(obj);
            return;
        }
        EPOCH_FREEING.set(true);
        let (mut stack, mut freed) = EPOCH_FREE_STACK.take();
        stack.push(obj);
        while let Some(obj) = stack.pop() {
            if !self.epoch_freeable(obj) {
                self.rc_log_zero(obj);
                continue;
            }
            // Its last owner may have gone on another thread: see what it
            // wrote before (the decrement's release).
            std::sync::atomic::fence(Ordering::Acquire);
            match obj {
                Object::Enum(e) => self.rc_dec_payload(&e.as_ref().payload, &mut stack),
                Object::Instance(i) => self.rc_dec_fields(i.as_ref(), &mut stack),
                _ if self.rc_leaf(obj) => {}
                _ => self.rc_dec_children(obj, &mut stack),
            }
            // SAFETY: born in the epoch, its count is zero and its words
            // were released.
            unsafe { self.epoch_dealloc(obj, &mut freed) };
        }
        EPOCH_FREEING.set(false);
        self.epoch_keep_freed(&mut freed);
        EPOCH_FREE_STACK.set((stack, freed));
    }

    /// Whether [`Self::rc_epoch_free`] frees `obj` in place: a plain enum,
    /// instance, array, closure or string with nothing owed at its end.
    fn epoch_freeable(&self, obj: Object) -> bool {
        let kind = match obj {
            Object::Enum(e) => !e.as_ref().payload.is_empty(),
            Object::String(s) => !s.as_ref().interned(),
            Object::Instance(_) | Object::Array(_) | Object::Fn(_) => true,
            _ => false,
        };
        kind && obj.rc_count() == 0
            && !obj.is_weak_target()
            && !obj.is_pinned()
            && !self.pending_drop(obj)
            && self.side_list_of(obj).is_none()
    }

    /// A `FreeEarly` has nothing to do: counts free what dies, and with no
    /// type that has a `drop` (and none owed) no scope end runs one
    /// ([`Self::rc_doomed_drops`]).
    #[inline]
    pub fn free_early_is_noop(&self) -> bool {
        self.rc_on && self.finalizer_types.is_empty() && self.rc_due.is_empty()
    }

    /// Keep counts in this heap, or not (tests that count by hand).
    pub fn set_rc_counting(&mut self, on: bool) {
        self.rc_on = on;
        self.sync_free_gate();
    }

    /// The objects `obj`'s words name, as the collector reads its words,
    /// sorted by address (one entry per word).
    pub fn rc_words(&self, obj: Object) -> Vec<u64> {
        struct Words<'a>(&'a Heap, Vec<u64>);
        impl SlotVisitor for Words<'_> {
            fn word(&mut self, v: &Value, kind: u8) {
                if (kind == common::WORD_POINTER || kind == common::WORD_UNKNOWN)
                    && let Some(o) = self.0.rc_object(*v)
                {
                    self.1.push(o.addr());
                }
            }

            fn member(&mut self, m: &Member) {
                if let Member::Object(o) = m {
                    self.1.push(o.addr());
                }
            }
        }
        let mut w = Words(self, Vec::new());
        obj.visit_slots(self, &mut w);
        w.1.sort_unstable();
        w.1
    }

    /// Before a mutation of object `v` the counting cannot follow store by
    /// store (a host native's): the objects it names now.
    pub fn rc_before(&self, v: Value) -> Option<(Object, Vec<u64>)> {
        if !self.rc_counting() {
            return None;
        }
        let o = self.rc_object(v)?;
        let refs = match o {
            // An array of scalars or bytes holds no reference, whatever a
            // native does to it.
            Object::Array(a) => {
                let a = a.as_ref();
                a.lanes() > 1 || !matches!(a.elem_kind(), common::WORD_SCALAR | common::WORD_BYTE)
            }
            // A coroutine's saved words are its suspended frame's: frames
            // own nothing in step 2. An enum's payload and a closure's
            // captures are never written once made: no native changes them.
            Object::Instance(_) | Object::Root(_) => true,
            _ => false,
        };
        refs.then(|| (o, self.rc_words(o)))
    }

    /// Store `value` into slot `slot` of instance `inst` (in range), counting
    /// just that slot: the old word loses an owner and the new one gains
    /// one. A class row with a tagged slot (whose kind another slot's tag
    /// decides) falls back to [`Self::rc_before`] / [`Self::rc_after`].
    pub fn rc_set_instance_slot(&self, inst: &mut RefInstance, slot: usize, value: Value) {
        if !self.rc_counting() {
            inst.as_mut().set_slot(slot, value);
            return;
        }
        let type_id = inst.as_ref().type_id;
        if self.class_has_tagged(type_id) {
            self.rc_set_tagged_slot(inst, slot, value);
            return;
        }
        let kind = match self.class_field_kinds(type_id).get(slot).copied() {
            Some(common::WORD_UNKNOWN) | None => common::packed_word_kind(inst.as_ref().own_kinds(), slot),
            Some(k) => k,
        };
        let old = inst.as_ref().storage.as_slice()[slot];
        inst.as_mut().set_slot(slot, value);
        self.rc_inc_word(value, kind);
        self.rc_dec_word(old, kind);
    }

    /// [`Self::rc_set_instance_slot`] for a class row with a tagged slot: a
    /// store into a tag changes the kind of the slot it decides, so each
    /// slot's word and kind before and after are compared (on the stack for
    /// a small object, by snapshot otherwise).
    fn rc_set_tagged_slot(&self, inst: &mut RefInstance, slot: usize, value: Value) {
        const SMALL: usize = 16;
        let type_id = inst.as_ref().type_id;
        let n = inst.as_ref().storage.as_slice().len();
        if n > SMALL {
            let before = self.rc_before(Value::from(Object::Instance(*inst).addr()));
            inst.as_mut().set_slot(slot, value);
            self.rc_after(before);
            return;
        }
        let row = self.class_field_kinds(type_id);
        let mut before = [(Value::from(0i64), 0u8); SMALL];
        {
            let i = inst.as_ref();
            let slots = i.storage.as_slice();
            let mut kinds = common::ClassSlotKinds::new(row, n).with_own(i.own_kinds());
            for (b, &w) in before.iter_mut().zip(slots) {
                *b = (w, kinds.next_kind(slots));
            }
        }
        inst.as_mut().set_slot(slot, value);
        let i = inst.as_ref();
        let slots = i.storage.as_slice();
        let mut kinds = common::ClassSlotKinds::new(row, n).with_own(i.own_kinds());
        for (&(old, a), &w) in before.iter().zip(slots) {
            let b = kinds.next_kind(slots);
            if old != w || a != b {
                self.rc_inc_word(w, b);
                self.rc_dec_word(old, a);
            }
        }
    }

    /// Add own slot kinds `k` to instance `inst`, counting only the slots
    /// whose kind that changes (a `vec_stamp`): a word stamped a number no
    /// longer counts as an owner. Walks the old and new kinds side by side,
    /// as [`Object::visit_slots`] reads them.
    pub fn rc_add_instance_kinds(&self, inst: &mut RefInstance, k: common::PackedKinds) {
        let old_kinds = inst.as_ref().own_kinds();
        inst.as_mut().add_own_kinds(k);
        let new_kinds = inst.as_ref().own_kinds();
        if old_kinds == new_kinds || !self.rc_counting() {
            return;
        }
        let i = inst.as_ref();
        let row = self.class_field_kinds(i.type_id);
        let slots = i.storage.as_slice();
        let mut old = common::ClassSlotKinds::new(row, slots.len()).with_own(old_kinds);
        let mut new = common::ClassSlotKinds::new(row, slots.len()).with_own(new_kinds);
        for &w in slots {
            let (a, b) = (old.next_kind(slots), new.next_kind(slots));
            if a != b {
                self.rc_inc_word(w, b);
                self.rc_dec_word(w, a);
            }
        }
    }

    /// [`Self::rc_before`] of element `index` of a lane array only (a
    /// `LaneStore`, which would otherwise diff the whole array).
    pub fn rc_before_lane(&self, v: Value, index: usize) -> Option<(Object, Vec<u64>, usize)> {
        if !self.rc_counting() {
            return None;
        }
        let o @ Object::Array(a) = self.rc_object(v)? else { return None };
        (a.as_ref().lanes() > 1).then(|| (o, self.rc_lane_words(o, index), index))
    }

    /// [`Self::rc_after`] of [`Self::rc_before_lane`]'s element.
    pub fn rc_after_lane(&self, before: Option<(Object, Vec<u64>, usize)>) {
        let Some((o, old, index)) = before else { return };
        for a in old {
            if let Some(o) = unsafe { Object::from_header(a) } {
                self.rc_dec_pending(o);
            }
        }
        for a in self.rc_lane_words(o, index) {
            if let Some(o) = unsafe { Object::from_header(a) } {
                self.rc_inc_obj(o);
            }
        }
    }

    /// The objects element `index` of the lane array `o` names.
    fn rc_lane_words(&self, o: Object, index: usize) -> Vec<u64> {
        let Object::Array(a) = o else { return Vec::new() };
        let a = a.as_ref();
        let (k, mask) = (a.lanes(), a.tag_mask);
        let Some(words) = a.gc_words() else { return Vec::new() };
        (index * k..(index * k + k).min(words.len()))
            .filter(|&i| ObjArray::lane_pointer(words, k, mask, i))
            .filter_map(|i| self.rc_object(words[i]))
            .map(|o| o.addr())
            .collect()
    }

    /// After that mutation: each object it now names that it did not gains
    /// an owner, each it no longer names loses one.
    pub fn rc_after(&self, before: Option<(Object, Vec<u64>)>) {
        let Some((obj, old)) = before else { return };
        let new = self.rc_words(obj);
        let (mut i, mut j) = (0, 0);
        let by_addr = |a: u64| unsafe { Object::from_header(a) };
        while i < old.len() || j < new.len() {
            match (old.get(i), new.get(j)) {
                (Some(a), Some(b)) if a == b => {
                    i += 1;
                    j += 1;
                }
                (Some(&a), Some(&b)) if a < b => {
                    if let Some(o) = by_addr(a) {
                        self.rc_dec_pending(o);
                    }
                    i += 1;
                }
                (Some(&a), None) => {
                    if let Some(o) = by_addr(a) {
                        self.rc_dec_pending(o);
                    }
                    i += 1;
                }
                (_, Some(&b)) => {
                    if let Some(o) = by_addr(b) {
                        self.rc_inc_obj(o);
                    }
                    j += 1;
                }
                (None, None) => break,
            }
        }
    }

    /// [`Self::rc_before`] of each distinct object among `words` (a host
    /// native's arguments, a vector loop's frame).
    pub fn rc_before_all(&self, words: &[Value]) -> Vec<(Object, Vec<u64>)> {
        if !self.rc_counting() {
            return Vec::new();
        }
        let mut seen: Vec<(Object, Vec<u64>)> = Vec::new();
        // Null words (a frame's unused slots, unit) name nothing: no call.
        for &w in words.iter().filter(|w| w.heap_addr() & !1 != 0) {
            if let Some((o, refs)) = self.rc_before(w)
                && !seen.iter().any(|(s, _)| s.addr() == o.addr())
            {
                seen.push((o, refs));
            }
        }
        seen
    }

    /// [`Self::rc_after`] of each of [`Self::rc_before_all`]'s objects.
    #[inline]
    pub fn rc_after_all(&self, before: Vec<(Object, Vec<u64>)>) {
        if before.is_empty() {
            return;
        }
        for b in before {
            self.rc_after(Some(b));
        }
    }

    /// The object a pointer-kind heap word names (a `Result` niche word's
    /// `Err` bit cleared), if any.
    #[inline]
    pub(super) fn rc_object(&self, v: Value) -> Option<Object> {
        let addr = v.heap_addr() & !1;
        if addr == 0 {
            return None;
        }
        self.find_object_by_addr(addr)
    }

    /// The object a heap word of kind `kind` names, if any. A pointer word
    /// names a live object or nothing (`0`, or an object outside the slab,
    /// which is immortal): a range test and its header say which, as native
    /// code's inline counts do, without [`Self::find_object_by_addr`]'s slot
    /// search. A word of unknown kind takes that search; a number none.
    #[inline(always)]
    pub(super) fn rc_object_of_kind(&self, v: Value, kind: u8) -> Option<Object> {
        if kind == common::WORD_POINTER {
            let addr = v.heap_addr() & !1;
            if addr == 0 || !addr.is_multiple_of(8) || !self.slab.may_contain(addr) {
                return None;
            }
            // SAFETY: a pointer word in the slab's range is a live object's
            // slot start (the count invariant: its owner keeps it alive).
            return unsafe { Object::from_header(addr) };
        }
        if kind == common::WORD_UNKNOWN {
            return self.rc_object(v);
        }
        None
    }

    /// A heap word `v` of kind `kind` was stored: its object gains an owner.
    /// A word of unknown kind names an object only when the address is one
    /// (the census keeps those to the few the kinds cannot say).
    #[inline]
    pub fn rc_inc_word(&self, v: Value, kind: u8) {
        if let Some(o) = self.rc_object_of_kind(v, kind) {
            self.rc_inc_obj(o);
        }
    }

    /// A heap word `v` of kind `kind` was overwritten or its holder died:
    /// its object loses an owner.
    #[inline]
    pub fn rc_dec_word(&self, v: Value, kind: u8) {
        if let Some(o) = self.rc_object_of_kind(v, kind) {
            self.rc_dec_pending(o);
        }
    }

    /// `o` loses an owner on the host side; at zero, with frees on, it waits
    /// in [`Heap::rc_pending`] for the next `RcDec`.
    #[inline]
    fn rc_dec_pending(&self, o: Object) {
        if !self.alloc_lock.is_null() && self.epoch_born(o) {
            // Not freed in place ([`Self::rc_epoch_free`]): a host's zero
            // may come back, as a join hands its result over before the
            // joiner's code counts it. The epoch's end releases what stays
            // at zero.
            if o.header().dec_shared() {
                self.rc_log_zero(o);
            }
            return;
        }
        if self.rc_dec_obj(o) && self.rc_frees {
            self.rc_pending.borrow_mut().push(o.addr());
            // The next `RcDec` goes through the runtime, which releases it.
            self.free_gate.set(0);
        }
    }

    /// Release what [`Self::rc_dec_pending`] left at zero and still is.
    #[inline]
    pub(crate) fn rc_release_pending(&mut self) -> usize {
        if self.rc_pending.get_mut().is_empty() || !self.alloc_lock.is_null() {
            return 0;
        }
        self.rc_release_pending_slow()
    }

    #[inline(never)]
    fn rc_release_pending_slow(&mut self) -> usize {
        let mut n = 0;
        let mut pending = std::mem::take(self.rc_pending.get_mut());
        pending.sort_unstable();
        pending.dedup();
        for addr in pending {
            if let Some(o) = self.find_object_by_addr(addr)
                && o.rc_count() == 0
            {
                n += self.rc_release(o);
            }
        }
        self.sync_free_gate();
        n
    }

    /// Every pointer word of `obj` (as the collector reads them) gains an
    /// owner: a new object's initial words, or a copy's. A new coroutine's
    /// words are its generator's arguments, which its frame owns once it
    /// starts (a generator body's parameters are owned, plan step 4).
    pub fn rc_inc_slots(&self, obj: Object) {
        struct Inc<'a>(&'a Heap);
        impl SlotVisitor for Inc<'_> {
            fn word(&mut self, v: &Value, kind: u8) {
                self.0.rc_inc_word(*v, kind);
            }

            fn member(&mut self, m: &Member) {
                if let Member::Object(o) = m {
                    self.0.rc_inc_obj(*o);
                }
            }
        }
        obj.visit_slots(self, &mut Inc(self));
    }

    /// Whether a count that reaches zero frees its object
    /// ([`Self::set_rc_frees`]). Off until frames own their words (plan
    /// step 4): a frame may still name an object no heap word does.
    #[inline]
    pub fn rc_frees(&self) -> bool {
        self.rc_frees && self.rc_counting()
    }

    /// Free objects whose count reaches zero ([`Self::rc_drop`]).
    pub fn set_rc_frees(&mut self, on: bool) {
        self.rc_frees = on;
        self.sync_free_gate();
    }

    /// `RcInc` (plan step 3): `v`'s object gains an owner, when counts are
    /// kept and `v` names an object.
    #[inline]
    pub fn rc_retain(&self, v: Value) {
        if self.rc_counting()
            && let Some(o) = self.rc_object(v)
        {
            self.rc_inc_obj(o);
        }
    }

    /// `RcDec`: `v`'s object loses an owner, and is released when that was
    /// its last and [`Self::rc_frees`]. Returns how many objects it freed.
    #[inline]
    pub fn rc_drop(&mut self, v: Value) -> usize {
        if !self.rc_counting() {
            return 0;
        }
        self.rc_release_word(v) + self.rc_release_pending()
    }

    /// [`Self::rc_drop`] of a typed object word, a slot start when it is in
    /// the slab's span (native code's inline count read its header and
    /// found its last owner): no slot search.
    #[inline]
    pub fn rc_drop_typed(&mut self, v: Value) -> usize {
        if !self.rc_counting() {
            return 0;
        }
        let n = match self.rc_typed(v) {
            Some(o) if self.rc_dec_obj(o) && self.rc_frees => self.rc_release(o),
            _ => 0,
        };
        n + self.rc_release_pending()
    }

    /// [`Self::rc_drop`] in a heap that keeps counts.
    fn rc_release_word(&mut self, v: Value) -> usize {
        match self.rc_object(v) {
            Some(o) if self.rc_dec_obj(o) && self.rc_frees => self.rc_release(o),
            _ => 0,
        }
    }

    /// Free `obj`, whose count reached zero: each object its words name
    /// loses an owner, and those that reach zero are released in turn (a
    /// worklist, not recursion). An object the runtime cannot free here
    /// stays, at count zero, for the collector: a
    /// root handle, and every
    /// object while a collection or a steal epoch is open (one that owes a
    /// `drop` or a close waits for it in [`Self::rc_due`]). The last
    /// collection's root buffer is no test: it is stale once that ends, and
    /// frames count their words. Returns how many objects it freed.
    pub(super) fn rc_release(&mut self, obj: Object) -> usize {
        if self.epoch_stw || !self.alloc_lock.is_null() {
            return 0;
        }
        if self.rc_free_plain_string(obj) {
            return 1;
        }
        // A leaf names nothing: free it without the worklist.
        if self.rc_leaf(obj) && self.rc_releasable(obj) {
            // SAFETY: as below.
            unsafe { self.rc_free(obj) };
            return 1;
        }
        let mut stack = std::mem::take(&mut self.rc_stack);
        stack.push(obj);
        self.rc_release_worklist(stack)
    }

    /// [`Self::rc_release`]'s worklist: free each object on `stack` whose
    /// count is zero, pushing what its words name that reaches zero in
    /// turn. Keeps `stack` (emptied) as [`Self::rc_stack`].
    fn rc_release_worklist(&mut self, mut stack: Vec<Object>) -> usize {
        let mut n = 0;
        while let Some(obj) = stack.pop() {
            // The common case, an enum with a payload: its words released
            // straight from their packed kinds, without the slot visitor.
            if let Object::Enum(e) = obj
                && obj.rc_count() == 0
                && !e.as_ref().payload.is_empty()
                && !self.pending_drop(obj)
            {
                self.rc_dec_payload(&e.as_ref().payload, &mut stack);
                n += 1;
                // SAFETY: as below.
                unsafe { self.rc_free(obj) };
                continue;
            }
            // An instance likewise, from its class row's kinds.
            if let Object::Instance(i) = obj
                && obj.rc_count() == 0
                && !self.pending_drop(obj)
            {
                self.rc_dec_fields(i.as_ref(), &mut stack);
                n += 1;
                // SAFETY: as below.
                unsafe { self.rc_free(obj) };
                continue;
            }
            if !self.rc_releasable(obj) {
                // Its `drop` runs first, at the runtime's next safepoint.
                if obj.rc_count() == 0 && (self.pending_drop(obj) || Self::close_owed(obj)) && !self.rc_due.contains(&obj.addr()) {
                    self.rc_due.push(obj.addr());
                    continue;
                }
                continue;
            }
            if let Object::Array(a) = obj
                && a.as_ref().lanes() == 1
                && !a.as_ref().holds_no_refs()
            {
                n += self.rc_release_elements(obj, &mut stack);
            } else if !self.rc_leaf(obj) {
                self.rc_dec_children(obj, &mut stack);
            }
            n += 1;
            // SAFETY: its count is zero: no counted word names it, and
            // frames count theirs once [`Self::rc_frees`] is on.
            unsafe { self.rc_free(obj) };
        }
        self.rc_stack = stack;
        // A large release may leave whole chunks empty.
        if n >= RELEASE_CHECK_FREES {
            self.release_chunks_if_due();
        }
        n
    }

    /// Release the elements of array `obj`, which is being freed: an
    /// element that reaches zero and names nothing is freed on the spot,
    /// any other goes on `stack`. So a large array of leaves is released
    /// without a worklist entry per element. Returns how many it freed.
    fn rc_release_elements(&mut self, obj: Object, stack: &mut Vec<Object>) -> usize {
        let Object::Array(a) = obj else { return 0 };
        let kind = a.as_ref().gc_elem_kind();
        if kind != common::WORD_POINTER && kind != common::WORD_UNKNOWN {
            return 0;
        }
        let Some(words) = a.as_ref().gc_words() else { return 0 };
        let (ptr, len) = (words.as_ptr(), words.len());
        // Pointer words with no steal epoch open: a young inline string at
        // its last owner frees without the general tests.
        let strings = (!cfg!(feature = "rc-check") && kind == common::WORD_POINTER && self.alloc_lock.is_null()).then(string_free);
        let mut n = 0;
        for i in 0..len {
            // SAFETY: the array is live until this release frees it, and
            // freeing its elements does not touch its storage.
            let v = unsafe { *ptr.add(i) };
            if let Some(sf) = &strings
                && self.rc_free_young_string(v, sf)
            {
                n += 1;
                continue;
            }
            let Some(o) = self.rc_object_of_kind(v, kind) else { continue };
            if !self.rc_dec_obj(o) {
                continue;
            }
            if self.rc_free_plain_string(o) {
                n += 1;
                continue;
            }
            if self.rc_leaf(o) && self.rc_releasable(o) {
                n += 1;
                // SAFETY: its count reached zero.
                unsafe { self.rc_free(o) };
            } else {
                stack.push(o);
            }
        }
        n
    }

    /// Take the last owner of pointer word `v` and free its object when it
    /// is a young inline string at count one that no weak handle names, as
    /// native code's inline free does ([`StringFree`]): one test of its
    /// header word and one of its tag, then its slot goes on the string
    /// class's nursery free list. Whether it did; anything else is left
    /// as it was.
    #[inline(always)]
    fn rc_free_young_string(&mut self, v: Value, sf: &StringFree) -> bool {
        let addr = v.heap_addr() & !1;
        if addr == 0 || !addr.is_multiple_of(8) || !self.slab.may_contain(addr) {
            return false;
        }
        let one = 1u64 << (8 * HEADER_COUNT_OFFSET);
        let count_bits = u64::from(u32::MAX) << (8 * HEADER_COUNT_OFFSET);
        let p = addr as *mut u64;
        // SAFETY: a pointer word in the slab's range is a live object's
        // slot start, its header word first.
        let h = unsafe { p.read() };
        if h & (sf.header_mask | count_bits) != sf.header_want | one {
            return false;
        }
        // SAFETY: a string's slot: its `StrData` tag is at `tag_at`.
        if unsafe { *((addr + u64::from(sf.tag_at)) as *const u8) } != sf.inline_tag {
            return false;
        }
        self.counts.alloc_bytes -= sf.size as usize;
        debug_assert!(self.counts.live_count > 0);
        self.counts.live_count -= 1;
        self.freed_early = true;
        // SAFETY: its last owner went and an inline string holds nothing:
        // the header is poisoned as `Gc::recycle` leaves it.
        unsafe { p.write(h & sf.header_keep) };
        // SAFETY: a young slot of the string class, poisoned.
        unsafe { self.slab.push_young_free(sf.class as usize, NonNull::new_unchecked(p.cast())) };
        true
    }

    /// Free `obj` on the spot when it is a plain string at count zero
    /// (no weak handle names it, the intern table never took it): what
    /// [`Self::rc_leaf`], [`Self::rc_releasable`] and [`Self::rc_free`]
    /// decide for one, in one test, the commonest release of all (a text
    /// workload's words). Whether it freed it; anything else goes the
    /// general way.
    #[inline(always)]
    fn rc_free_plain_string(&mut self, obj: Object) -> bool {
        let Object::String(s) = obj else { return false };
        if obj.rc_count() != 0 || s.as_ref().interned() || obj.is_weak_target() {
            return false;
        }
        self.freed_early = true;
        // SAFETY: its count is zero and a string names nothing, as
        // [`Self::rc_release`]'s leaf path frees one.
        unsafe { self.dealloc_gc(s) };
        true
    }

    /// Free `obj`, whose owners are gone and whose words were released:
    /// its weaks clear, an interned string leaves the table, a weak handle
    /// the weak list.
    ///
    /// # Safety
    /// Nothing that will be read again names `obj`.
    pub(super) unsafe fn rc_free(&mut self, obj: Object) {
        if obj.is_weak_target() {
            self.clear_weaks_to(obj.addr());
        }
        // An interned string leaves the table first (a program string
        // is immortal and never gets here).
        if let Object::String(st) = obj
            && st.as_ref().interned()
            && self.strings.find(&st.as_ref().data, st.as_ref().hash_code()).is_some_and(|i| i.as_ptr() == st.as_ptr())
        {
            self.strings.remove(st);
        }
        // A weak handle leaves the weak list (its target is no owner).
        if let Object::Weak(_) = obj
            && let Some(i) = self.weaks.iter().position(|&w| w == obj.addr())
        {
            self.weaks.swap_remove(i);
        }
        self.freed_early = true;
        // SAFETY: the caller's.
        unsafe { self.dealloc(obj) };
    }

    /// True when `obj`'s words name no object for sure, so its release has
    /// no children to walk: a string, an instance of a class whose slots are
    /// all numbers, an enum whose payload kinds are.
    #[inline]
    fn rc_leaf(&self, obj: Object) -> bool {
        match obj {
            Object::String(_) => true,
            Object::Instance(i) => {
                let i = i.as_ref();
                self.class_leaf(i.type_id, i.storage.as_slice().len())
            }
            Object::Enum(e) => {
                let p = &e.as_ref().payload;
                let (n, kinds) = (p.as_slice().len(), p.kinds());
                n <= common::PACKED_KIND_WORDS
                    && (0..n).all(|i| matches!(common::packed_word_kind(kinds, i), common::WORD_SCALAR | common::WORD_BYTE))
            }
            Object::Array(a) => a.as_ref().holds_no_refs(),
            _ => false,
        }
    }

    /// A generator that started and is suspended: its close runs first, at
    /// the next safepoint, and releases its frame's words.
    fn close_owed(obj: Object) -> bool {
        matches!(obj, Object::Coroutine(c) if c.as_ref().state == CoroState::Suspended && c.as_ref().started)
    }

    /// `RcReuse`: `v`'s object loses an owner, as [`Self::rc_drop`]. When
    /// that was its last and it is a plain young enum with a payload (no
    /// `drop`, no weak handle, no pin), its words are released but its slot
    /// is kept for the `AllocAt` that follows: returns the slot's address,
    /// else `0`. The slot is no object: nothing may read it as one until
    /// that allocation fills it.
    pub fn rc_take_slot(&mut self, v: Value, moved: u32) -> u64 {
        if !self.rc_counting() {
            return 0;
        }
        let Some(o) = self.rc_object(v) else { return 0 };
        self.rc_take_object(o, v, moved)
    }

    /// [`Self::rc_take_slot`] of a typed object word (a slot start when it
    /// is in the slab's span), as native code's inline count reads one: no
    /// slot search.
    pub fn rc_take_last(&mut self, v: Value, moved: u32) -> u64 {
        if !self.rc_counting() {
            return 0;
        }
        let Some(o) = self.rc_typed(v) else { return 0 };
        self.rc_take_object(o, v, moved)
    }

    /// A typed object word's object: one in the slab's span, read by its
    /// header (a typed word names a slot start), without the slot search
    /// of [`Self::rc_object`].
    #[inline(always)]
    fn rc_typed(&self, v: Value) -> Option<Object> {
        let addr = v.heap_addr() & !1;
        if addr == 0 || !addr.is_multiple_of(8) || !self.slab.may_contain(addr) {
            return None;
        }
        // SAFETY: the slab's span stays mapped and the word names a slot start.
        unsafe { Object::from_header(addr) }
    }

    fn rc_take_object(&mut self, o: Object, v: Value, moved: u32) -> u64 {
        let (words, instance) = match &o {
            Object::Enum(e) => (&e.as_ref().payload, false),
            Object::Instance(i) => (&i.as_ref().storage, true),
            _ => {
                self.rc_drop(v);
                return 0;
            }
        };
        // An enum with no payload is a shared unit variant.
        let keep = (instance || !words.is_empty())
            && o.generation() & GEN_YOUNG != 0
            && self.rc_frees
            && !self.epoch_stw
            && self.alloc_lock.is_null()
            && o.rc_count() == 1
            && !o.is_weak_target()
            && !o.is_pinned()
            && !self.finalizer_object(o);
        let slots = words.as_slice();
        if !keep {
            // The moved words gain the owners the body did not count.
            if moved != 0 {
                let mut kinds = WordKinds::of(self, o);
                for (i, w) in slots.iter().enumerate() {
                    let kind = kinds.next(slots);
                    if i < 32 && moved & (1 << i) != 0 {
                        self.rc_inc_word(*w, kind);
                    }
                }
            }
            self.rc_drop(v);
            return 0;
        }
        o.rc_dec();
        // The moved words keep the owner the object held; the rest lose it.
        let mut stack = std::mem::take(&mut self.rc_stack);
        if !instance {
            // An enum's kinds are packed in its payload.
            let packed = words.kinds();
            for (i, w) in slots.iter().enumerate() {
                if i < 32 && moved & (1 << i) != 0 {
                    continue;
                }
                let c = match common::packed_word_kind(packed, i) {
                    common::WORD_POINTER => self.rc_typed(*w),
                    common::WORD_UNKNOWN => self.rc_object(*w),
                    _ => None,
                };
                if let Some(c) = c
                    && self.rc_dec_obj(c)
                {
                    stack.push(c);
                }
            }
        } else if !(moved == 0 && self.rc_leaf(o)) {
            let mut kinds = WordKinds::of(self, o);
            for (i, w) in slots.iter().enumerate() {
                let kind = kinds.next(slots);
                if i < 32 && moved & (1 << i) != 0 {
                    continue;
                }
                let c = match kind {
                    common::WORD_POINTER => self.rc_typed(*w),
                    common::WORD_UNKNOWN => self.rc_object(*w),
                    _ => None,
                };
                if let Some(c) = c
                    && self.rc_dec_obj(c)
                {
                    stack.push(c);
                }
            }
        }
        self.freed_early = true;
        // SAFETY: its count is zero and its words are released or moved.
        unsafe { self.hold_slot(o) };
        if stack.is_empty() {
            self.rc_stack = stack;
        } else {
            self.rc_release_worklist(stack);
        }
        if !self.rc_pending.get_mut().is_empty() {
            self.rc_release_pending();
        }
        o.addr()
    }

    /// Each object an enum payload's words name loses an owner; those that
    /// reach zero go on `stack`. The words [`Self::rc_dec_children`] reads
    /// for an enum.
    #[inline(always)]
    fn rc_dec_payload(&self, payload: &EnumPayload, stack: &mut Vec<Object>) {
        let kinds = payload.kinds();
        for (i, v) in payload.as_slice().iter().enumerate() {
            let kind = common::packed_word_kind(kinds, i);
            if let Some(o) = self.rc_object_of_kind(*v, kind)
                && self.rc_dec_obj(o)
            {
                stack.push(o);
            }
        }
    }

    /// Each object an instance's fields name loses an owner; those that
    /// reach zero go on `stack`. The words [`Self::rc_dec_children`] reads
    /// for an instance; a class of numbers only has none.
    #[inline(always)]
    fn rc_dec_fields(&self, inst: &ObjInstance, stack: &mut Vec<Object>) {
        let slots = inst.storage.as_slice();
        if self.class_leaf(inst.type_id, slots.len()) {
            return;
        }
        let mut kinds = common::ClassSlotKinds::new(self.class_field_kinds(inst.type_id), slots.len()).with_own(inst.storage.kinds());
        for v in slots {
            let kind = kinds.next_kind(slots);
            if let Some(o) = self.rc_object_of_kind(*v, kind)
                && self.rc_dec_obj(o)
            {
                stack.push(o);
            }
        }
    }

    /// Each object `obj`'s words name loses an owner; those that reach zero
    /// go on `stack` ([`Self::rc_release`]'s worklist). A started
    /// coroutine's saved words are its frame's, which counted them itself;
    /// one that never started owns its arguments (its allocation counted
    /// them, and its body takes them over when it starts).
    fn rc_dec_children(&self, obj: Object, stack: &mut Vec<Object>) {
        if matches!(obj, Object::Coroutine(c) if c.as_ref().started) {
            return;
        }
        struct Dec<'a>(&'a Heap, &'a mut Vec<Object>);
        impl SlotVisitor for Dec<'_> {
            #[inline(always)]
            fn word(&mut self, v: &Value, kind: u8) {
                if let Some(o) = self.0.rc_object_of_kind(*v, kind)
                    && self.0.rc_dec_obj(o)
                {
                    self.1.push(o);
                }
            }

            fn member(&mut self, m: &Member) {
                if let Member::Object(o) = m
                    && self.0.rc_dec_obj(*o)
                {
                    self.1.push(*o);
                }
            }
        }
        obj.visit_slots(self, &mut Dec(self, stack));
    }

    /// The `rc-check` build's heap check, at every cycle collection: no
    /// pointer word names a freed slot, and no object's count is below the
    /// heap words that own one of it (the words [`Self::rc_dec_children`]
    /// releases). Frames and runtime roots own counts too, so a count may
    /// be above that, never below. Panics naming the first object wrong.
    #[cfg(any(test, feature = "rc-check"))]
    pub(crate) fn rc_check(&self) {
        if !self.rc_counts_live() {
            return;
        }
        struct Own<'a>(&'a Heap, &'a mut std::collections::HashMap<u64, Vec<u64>>, u64);
        impl SlotVisitor for Own<'_> {
            fn word(&mut self, v: &Value, kind: u8) {
                if kind != common::WORD_POINTER && kind != common::WORD_UNKNOWN {
                    return;
                }
                let addr = v.heap_addr() & !1;
                if addr == 0 || !self.0.slab.contains_slot(addr) {
                    return;
                }
                // SAFETY: a slot origin in a mapped chunk.
                match unsafe { Object::from_header(addr) } {
                    Some(_) => self.1.entry(addr).or_default().push(self.2),
                    None => assert!(kind != common::WORD_POINTER, "rc-check: the object at {:#x} holds a freed object at {addr:#x}", self.2),
                }
            }

            fn member(&mut self, m: &Member) {
                if let Member::Object(o) = m {
                    self.1.entry(o.addr()).or_default().push(self.2);
                }
            }
        }
        let mut owners = std::collections::HashMap::new();
        for obj in self.objects() {
            if matches!(obj, Object::Coroutine(c) if c.as_ref().started) {
                continue;
            }
            obj.visit_slots(self, &mut Own(self, &mut owners, obj.addr()));
        }
        for (addr, holders) in owners {
            // SAFETY: counted above as a live object's address.
            let Some(o) = (unsafe { Object::from_header(addr) }) else { continue };
            let (c, n) = (o.rc_count(), holders.len() as u32);
            if c < COUNT_IMMORTAL - 1 && c < n {
                // SAFETY: each holder is a live object's address.
                let named: Vec<String> = holders.iter().filter_map(|&h| unsafe { Object::from_header(h) }).map(|h| format!("{} {:#x}", kind_name(h), h.addr())).collect();
                panic!("rc-check: the {} at {addr:#x} has count {c}, but {n} heap words own it (in {})", kind_name(o), named.join(", "));
            }
        }
    }

    /// [`rc_leaks_shown`]: tally the live objects that are not immortal by
    /// kind and print them (one line, nothing when there are none).
    pub(super) fn report_leaks(&self) {
        // What an immortal object holds (a static's value restored from the
        // compile-time snapshot, written since) lives as long as it does.
        let mut exempt = self.leak_exempt.clone();
        let immortal: Vec<Object> = self.objects().filter(|o| o.rc_count() == COUNT_IMMORTAL).collect();
        self.reach_into(immortal, &mut exempt);
        let mut by_kind: std::collections::BTreeMap<&str, (usize, usize)> = std::collections::BTreeMap::new();
        for obj in self.objects() {
            if obj.rc_count() == COUNT_IMMORTAL || exempt.contains(&obj.addr()) {
                continue;
            }
            let kind = kind_name(obj);
            let e = by_kind.entry(kind).or_default();
            e.0 += 1;
            e.1 += obj.size();
        }
        if by_kind.is_empty() {
            return;
        }
        let (n, bytes) = by_kind.values().fold((0, 0), |a, b| (a.0 + b.0, a.1 + b.1));
        let kinds: Vec<String> = by_kind.iter().map(|(k, (c, _))| format!("{k} {c}")).collect();
        eprintln!("coil rc-leaks: {n} objects, {bytes} bytes ({})", kinds.join(", "));
    }

    /// [`Self::report_leaks`] leaves out `roots` (the statics' words as the
    /// program ends) and what they reach: they live as long as the program.
    pub(crate) fn rc_leaks_exempt(&mut self, roots: &[Value]) {
        let mut seen = std::mem::take(&mut self.leak_exempt);
        let stack: Vec<Object> = roots.iter().filter_map(|v| self.rc_object(*v)).collect();
        self.reach_into(stack, &mut seen);
        self.leak_exempt = seen;
    }

    /// Add to `seen` the objects `stack` holds and every object they reach.
    fn reach_into(&self, mut stack: Vec<Object>, seen: &mut std::collections::HashSet<u64>) {
        struct Reach<'a>(&'a Heap, &'a mut Vec<Object>);
        // Any word that names an object counts here, whatever its kind
        // says: the report is the only reader, and erring keeps it quiet.
        impl SlotVisitor for Reach<'_> {
            fn word(&mut self, v: &Value, _kind: u8) {
                if let Some(o) = self.0.rc_object(*v) {
                    self.1.push(o);
                }
            }

            fn member(&mut self, m: &Member) {
                if let Member::Object(o) = m {
                    self.1.push(*o);
                }
            }
        }
        while let Some(o) = stack.pop() {
            if seen.insert(o.addr()) {
                o.visit_slots(self, &mut Reach(self, &mut stack));
            }
        }
    }

    /// A fresh enum a host call boxed for a call site that takes it as two
    /// words (a pair layout): its payload's owner moves to the caller, whose
    /// count ops count the word, and the box goes. A box someone else owns
    /// (its count above zero) stays.
    pub(crate) fn rc_unbox(&mut self, obj: Object) {
        if !self.rc_frees() || !self.rc_counts_live() || obj.rc_count() != 0 || !self.rc_releasable(obj) {
            return;
        }
        // A payload that reaches zero is the caller's to count, not freed.
        let mut moved = std::mem::take(&mut self.rc_stack);
        self.rc_dec_children(obj, &mut moved);
        moved.clear();
        self.rc_stack = moved;
        // SAFETY: nothing names a fresh box but the call's result word.
        unsafe { self.rc_free(obj) };
    }

    /// The objects whose `drop` is owed once `v` loses the owner a scope's
    /// end takes from it (a `FreeEarly` in a counting run, reference-count
    /// plan step 7): `v` when that is its last, and, breadth first (a parent's
    /// `drop` runs before its children's), each object only a doomed one
    /// owns. An object two doomed parents share counts two and is left to
    /// its count: its `drop` runs when that reaches zero.
    pub fn rc_doomed_drops(&self, v: Value) -> Vec<Object> {
        struct Kids<'a>(&'a Heap, &'a mut Vec<Object>);
        impl SlotVisitor for Kids<'_> {
            fn word(&mut self, v: &Value, kind: u8) {
                if (kind == common::WORD_POINTER || kind == common::WORD_UNKNOWN)
                    && let Some(o) = self.0.rc_object(*v)
                {
                    self.1.push(o);
                }
            }

            fn member(&mut self, m: &Member) {
                if let Member::Object(o) = m {
                    self.1.push(*o);
                }
            }
        }
        let mut drops = Vec::new();
        if !self.rc_counts_live() || self.finalizer_types.is_empty() {
            return drops;
        }
        let Some(root) = self.rc_object(v) else {
            return drops;
        };
        if root.rc_count() > 1 {
            return drops;
        }
        let (mut doomed, mut kids) = (vec![root], Vec::new());
        let mut i = 0;
        while i < doomed.len() {
            let obj = doomed[i];
            i += 1;
            if self.pending_drop(obj) {
                drops.push(obj);
            }
            if !matches!(obj, Object::Array(_) | Object::Enum(_) | Object::Instance(_)) {
                continue;
            }
            kids.clear();
            obj.visit_slots(self, &mut Kids(self, &mut kids));
            doomed.extend(kids.iter().copied().filter(|k| k.rc_count() == 1));
        }
        drops
    }

    /// The objects [`Self::rc_release`] left for their `drop`.
    pub fn take_rc_due(&mut self) -> Vec<u64> {
        std::mem::take(&mut self.rc_due)
    }

    /// Whether [`Self::rc_release`] may free `obj`.
    fn rc_releasable(&self, obj: Object) -> bool {
        // A closure owns its captured and filled words: its allocation
        // counted them ([`Self::rc_inc_slots`]).
        // A coroutine goes once its body is done (its close released its
        // frame's words). Freeing a stream closes its descriptor (a local
        // one is kept to the end of its scope, `hir/early_free.rs`). A
        // thread handle, channel end or lock drops its share of the
        // runtime's state, as a sweep did.
        (matches!(
            obj,
            Object::Fn(_) | Object::String(_) | Object::Weak(_) | Object::Stream(_) | Object::Thread(_) | Object::Sender(_) | Object::Receiver(_) | Object::Mutex(_) | Object::RwLock(_)
        )
            || matches!(obj, Object::Coroutine(c) if c.as_ref().state == CoroState::Done || !c.as_ref().started)
            // A root pins its value whoever names it (a host may hold the
            // handle as a number); once `unroot` emptied it, it is a plain
            // object.
            || matches!(obj, Object::Root(r) if r.as_ref().payload.is_none())
            || matches!(obj, Object::Array(_) | Object::Enum(_) | Object::Instance(_)) && self.early_freeable(obj))
            && !self.pending_drop(obj)
            && obj.rc_count() == 0
    }

}

/// A kind's name, as the leak report and the count check print it.
pub(super) fn kind_name(obj: Object) -> &'static str {
    match obj {
        Object::String(_) => "string",
        Object::Instance(_) => "instance",
        Object::Enum(_) => "enum",
        Object::Array(_) => "array",
        Object::Fn(_) => "fn",
        Object::Coroutine(_) => "coroutine",
        Object::Weak(_) => "weak",
        Object::Root(_) => "root",
        Object::Stream(_) => "stream",
        Object::Library(_) => "library",
        Object::Thread(_) => "thread",
        Object::Sender(_) => "sender",
        Object::Receiver(_) => "receiver",
        Object::Mutex(_) => "mutex",
        Object::RwLock(_) => "rwlock",
    }
}

/// The kinds of an enum's payload words or an instance's fields, in order,
/// as a release reads them.
enum WordKinds<'a> {
    Enum(common::PackedKinds, usize),
    Class(common::ClassSlotKinds<'a>),
}

impl<'a> WordKinds<'a> {
    #[inline(always)]
    fn of(heap: &'a Heap, o: Object) -> Self {
        match o {
            Object::Instance(i) => {
                let i = i.as_ref();
                Self::Class(common::ClassSlotKinds::new(heap.class_field_kinds(i.type_id), i.storage.as_slice().len()).with_own(i.storage.kinds()))
            }
            Object::Enum(e) => Self::Enum(e.as_ref().payload.kinds(), 0),
            _ => Self::Enum(0, 0),
        }
    }

    #[inline(always)]
    fn next(&mut self, slots: &[Value]) -> u8 {
        match self {
            Self::Enum(k, i) => {
                let kind = common::packed_word_kind(*k, *i);
                *i += 1;
                kind
            }
            Self::Class(c) => c.next_kind(slots),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mutation_moves_the_owners_its_words_gained_and_lost() {
        let mut heap = Heap::default();
        // Allocation counts nothing: the test counts by hand.
        heap.set_rc_counting(false);
        let (a, _) = heap.alloc(ObjString::from("a"), Object::String);
        let (b, _) = heap.alloc(ObjString::from("b"), Object::String);
        let (arr, mut gc) = heap.alloc(ObjArray::new(vec![Value::from(a.addr())]).of_kind(common::WORD_POINTER), Object::Array);
        a.rc_inc();
        let before = (arr, heap.rc_words(arr));
        gc.as_mut().store_indexed(0, Value::from(b.addr()), false);
        gc.as_mut().push(Value::from(b.addr()));
        heap.rc_after(Some(before));
        assert_eq!(a.rc_count(), 0);
        assert_eq!(b.rc_count(), 2);
    }

    /// The `rc-check` heap check passes a heap whose counts cover their
    /// heap owners and names an object whose count fell below them.
    #[test]
    #[should_panic(expected = "but 1 heap words own it")]
    fn the_heap_check_finds_a_count_below_its_owners() {
        let mut heap = Heap::default();
        let (a, _) = heap.alloc(ObjString::from("a"), Object::String);
        let _ = heap.alloc(ObjArray::new(vec![Value::from(a.addr())]).of_kind(common::WORD_POINTER), Object::Array);
        assert_eq!(a.rc_count(), 1, "the array's word owns one");
        heap.rc_check();
        while a.rc_count() > 0 {
            a.rc_dec();
        }
        heap.rc_check();
    }

    /// `--early-free check` accepts a free whose objects only the frame and
    /// each other own, and names one that a second holder still owns.
    #[test]
    fn the_early_free_check_names_an_object_owned_from_outside() {
        let mut heap = Heap::default();
        let (s, _) = heap.alloc(ObjString::from("s"), Object::String);
        let (arr, _) = heap.alloc(ObjArray::new(vec![Value::from(s.addr())]).of_kind(common::WORD_POINTER), Object::Array);
        arr.rc_inc();
        assert_eq!(heap.early_free_violation(&[arr, s]), None);
        let _ = heap.alloc(ObjArray::new(vec![Value::from(s.addr())]).of_kind(common::WORD_POINTER), Object::Array);
        let why = heap.early_free_violation(&[arr, s]).expect("a second holder owns the string");
        assert!(why.contains("count 2, 1 owners"), "{why}");
    }

    /// In a steal epoch an object born in it is freed on the thread that
    /// took its last owner: what only it named from the epoch goes too, an
    /// object from before loses the owner at the epoch's end.
    #[test]
    fn an_object_born_in_an_epoch_is_freed_there() {
        let lock = std::sync::Mutex::new(());
        let mut heap = Heap::default();
        heap.set_rc_counting(true);
        heap.set_rc_frees(true);
        let ptr = |o: Object| Value::from(o.addr());
        let (old, _) = heap.alloc(ObjString::from("old"), Object::String);
        old.rc_inc();
        let live = heap.live_object_count();
        heap.enter_epoch_stw(&lock);
        let (child, _) = heap.alloc(ObjString::from("child"), Object::String);
        let (arr, _) = heap.alloc(ObjArray::new(vec![ptr(child), ptr(old)]).of_kind(common::WORD_POINTER), Object::Array);
        heap.rc_inc_obj(arr);
        assert_eq!(child.rc_count(), 1, "the array owns it");
        let (arr_addr, child_addr) = (arr.addr(), child.addr());
        assert!(!heap.rc_dec_obj(arr));
        // SAFETY: the slots stay mapped; a freed one reads as no object.
        assert!(unsafe { Object::from_header(arr_addr) }.is_none(), "the array is freed");
        assert!(unsafe { Object::from_header(child_addr) }.is_none(), "so is what only it named");
        assert_eq!(old.rc_count(), 1, "an older header waits for the epoch's end");
        heap.flush_epoch_batch();
        heap.exit_epoch_stw();
        assert_eq!(old.rc_count(), 1, "the array's owner came and went");
        assert_eq!(heap.live_object_count(), live);
    }

    #[test]
    fn an_immortal_count_never_moves() {
        let mut heap = Heap::default();
        let (a, _) = heap.alloc(ObjString::from("a"), Object::String);
        a.rc_set_immortal();
        a.rc_inc();
        assert!(!a.rc_dec());
        assert_eq!(a.rc_count(), COUNT_IMMORTAL);
        let unit = heap.immortal_unit_enum(4);
        assert_eq!(unit.rc_count(), COUNT_IMMORTAL);
    }

    /// A list `a -> b -> c` with `b` also named by `d`: dropping `a`'s last
    /// owner frees `a` only, `b` keeps `d`'s owner, `c` stays with `b`.
    #[test]
    fn a_release_frees_what_only_the_object_kept() {
        let mut heap = Heap::default();
        heap.set_rc_counting(false);
        let ptr = |o: Object| Value::from(o.addr());
        let (c, _) = heap.alloc(ObjString::from("c"), Object::String);
        let (b, _) = heap.alloc(ObjArray::new(vec![ptr(c)]).of_kind(common::WORD_POINTER), Object::Array);
        let (a, _) = heap.alloc(ObjArray::new(vec![ptr(b)]).of_kind(common::WORD_POINTER), Object::Array);
        let (d, _) = heap.alloc(ObjArray::new(vec![ptr(b)]).of_kind(common::WORD_POINTER), Object::Array);
        // As a check run's allocation would count them.
        for o in [c, b, b] {
            o.rc_inc();
        }
        a.rc_inc();
        d.rc_inc();
        heap.rc_frees = true;
        let live = heap.counts.live_count;
        assert_eq!(heap.rc_release_word(ptr(a)), 1);
        assert_eq!(heap.counts.live_count, live - 1);
        assert_eq!(b.rc_count(), 1);
        assert_eq!(heap.rc_release_word(ptr(d)), 3, "d, then b, then c");
        assert_eq!(heap.counts.live_count, live - 4);
    }

    #[test]
    fn the_last_owner_reports_zero() {
        let mut heap = Heap::default();
        let (a, _) = heap.alloc(ObjString::from("a"), Object::String);
        a.rc_inc();
        a.rc_inc();
        assert!(!a.rc_dec());
        assert!(a.rc_dec());
        // A count at zero stays there.
        assert!(!a.rc_dec());
        assert_eq!(a.rc_count(), 0);
    }
}

