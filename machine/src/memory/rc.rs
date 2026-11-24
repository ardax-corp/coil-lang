//! Reference counts: each object's header count is the number of places
//! that own it: heap words the runtime and native code store, and frames'
//! words, which the compiler's insertion pass counts (`RcInc` / `RcDec`,
//! [`Heap::rc_retain`] / [`Heap::rc_drop`]). An object is freed when its
//! count reaches zero; cycles go to the cycle collector (`cycles.rs`).

use super::*;

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
        let c = self.count.get();
        if c != COUNT_IMMORTAL {
            // A count that would reach the immortal one stays one below it:
            // the object then lives as long as the program.
            self.count.set(c.saturating_add(1).min(COUNT_IMMORTAL - 1));
        }
    }

    /// One owner fewer. Returns whether that was the last.
    #[inline]
    fn dec(&self) -> bool {
        let c = self.count.get();
        if c == COUNT_IMMORTAL || c == 0 {
            return false;
        }
        self.count.set(c - 1);
        c == 1
    }
}

impl Object {
    /// The object's reference count ([`COUNT_IMMORTAL`] for one that lives
    /// as long as the program).
    #[inline]
    pub fn rc_count(&self) -> u32 {
        self.header().count.get()
    }

    /// Make the object immortal: its count never changes again.
    #[inline]
    pub fn rc_set_immortal(&self) {
        self.header().count.set(COUNT_IMMORTAL);
    }

    #[inline]
    pub fn rc_inc(&self) {
        self.header().inc();
    }

    /// Set the count (a steal epoch's logged changes, summed at its end).
    #[inline]
    pub(crate) fn rc_set_count(&self, n: u32) {
        self.header().count.set(n);
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

    /// `o` gains an owner (logged while a steal epoch is open).
    #[inline]
    pub(crate) fn rc_inc_obj(&self, o: Object) {
        if self.alloc_lock.is_null() {
            o.rc_inc();
        } else if o.rc_count() != COUNT_IMMORTAL {
            self.rc_log(o, false);
        }
    }

    /// `o` loses an owner; whether that was the last. Never while a steal
    /// epoch is open: the change is logged and its end releases.
    #[inline]
    pub(crate) fn rc_dec_obj(&self, o: Object) -> bool {
        if self.alloc_lock.is_null() {
            return o.rc_dec();
        }
        if o.rc_count() != COUNT_IMMORTAL {
            self.rc_log(o, true);
        }
        false
    }

    /// Keep counts in this heap, or not (tests that count by hand).
    pub fn set_rc_counting(&mut self, on: bool) {
        self.rc_on = on;
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
        for &w in words {
            if let Some((o, refs)) = self.rc_before(w)
                && !seen.iter().any(|(s, _)| s.addr() == o.addr())
            {
                seen.push((o, refs));
            }
        }
        seen
    }

    /// [`Self::rc_after`] of each of [`Self::rc_before_all`]'s objects.
    pub fn rc_after_all(&self, before: Vec<(Object, Vec<u64>)>) {
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

    /// A heap word `v` of kind `kind` was stored: its object gains an owner.
    /// A word of unknown kind names an object only when the address is one
    /// (the census keeps those to the few the kinds cannot say).
    #[inline]
    pub fn rc_inc_word(&self, v: Value, kind: u8) {
        if (kind == common::WORD_POINTER || kind == common::WORD_UNKNOWN)
            && let Some(o) = self.rc_object(v)
        {
            self.rc_inc_obj(o);
        }
    }

    /// A heap word `v` of kind `kind` was overwritten or its holder died:
    /// its object loses an owner.
    #[inline]
    pub fn rc_dec_word(&self, v: Value, kind: u8) {
        if (kind == common::WORD_POINTER || kind == common::WORD_UNKNOWN)
            && let Some(o) = self.rc_object(v)
        {
            self.rc_dec_pending(o);
        }
    }

    /// `o` loses an owner on the host side; at zero, with frees on, it waits
    /// in [`Heap::rc_pending`] for the next `RcDec`.
    #[inline]
    fn rc_dec_pending(&self, o: Object) {
        if self.rc_dec_obj(o) && self.rc_frees {
            self.rc_pending.borrow_mut().push(o.addr());
        }
    }

    /// Release what [`Self::rc_dec_pending`] left at zero and still is.
    pub(crate) fn rc_release_pending(&mut self) -> usize {
        if self.rc_pending.get_mut().is_empty() || !self.alloc_lock.is_null() || self.gc_phase != GcPhase::Idle {
            return 0;
        }
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
        if self.gc_phase != GcPhase::Idle || self.epoch_stw || !self.alloc_lock.is_null() {
            return 0;
        }
        // A leaf names nothing: free it without the worklist.
        if self.rc_leaf(obj) && self.rc_releasable(obj) {
            // SAFETY: as below.
            unsafe { self.rc_free(obj) };
            return 1;
        }
        let mut stack = std::mem::take(&mut self.rc_stack);
        stack.push(obj);
        let mut n = 0;
        while let Some(obj) = stack.pop() {
            if !self.rc_releasable(obj) {
                // Its `drop` runs first, at the runtime's next safepoint.
                if obj.rc_count() == 0 && (self.pending_drop(obj) || Self::close_owed(obj)) && !self.rc_due.contains(&obj.addr()) {
                    self.rc_due.push(obj.addr());
                    continue;
                }
                continue;
            }
            if !self.rc_leaf(obj) {
                self.rc_dec_children(obj, &mut stack);
            }
            n += 1;
            // SAFETY: its count is zero: no counted word names it, and
            // frames count theirs once [`Self::rc_frees`] is on.
            unsafe { self.rc_free(obj) };
        }
        self.rc_stack = stack;
        n
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
            && st.as_ref().hash_set()
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
            _ => false,
        }
    }

    /// A generator that started and is suspended: its close runs first, at
    /// the next safepoint, and releases its frame's words.
    fn close_owed(obj: Object) -> bool {
        matches!(obj, Object::Coroutine(c) if c.as_ref().state == CoroState::Suspended && c.as_ref().started)
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
            fn word(&mut self, v: &Value, kind: u8) {
                if (kind == common::WORD_POINTER || kind == common::WORD_UNKNOWN)
                    && let Some(o) = self.0.rc_object(*v)
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

    /// [`rc_leaks_shown`]: tally the live objects that are not immortal by
    /// kind and print them (one line, nothing when there are none).
    pub(super) fn report_leaks(&self) {
        let mut by_kind: std::collections::BTreeMap<&str, (usize, usize)> = std::collections::BTreeMap::new();
        for obj in self.objects() {
            if obj.rc_count() == COUNT_IMMORTAL {
                continue;
            }
            let kind = match obj {
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
            };
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
            || matches!(obj, Object::Array(_) | Object::Enum(_) | Object::Instance(_)) && self.early_freeable(obj))
            && !self.pending_drop(obj)
            && obj.rc_count() == 0
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
