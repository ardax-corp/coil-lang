//! Reference counts (reference-count plan step 2): each object's header
//! count is the number of places that own it. Step 2 counts the heap words
//! the runtime and native code store; frames own nothing yet, so nothing is
//! freed at zero until the insertion pass (step 4) counts them too. The MIR
//! ops `RcInc` / `RcDec` (step 3) are [`Heap::rc_retain`] / [`Heap::rc_drop`];
//! [`Heap::set_rc_frees`] turns on the release at zero.
//!
//! The oracle (`COIL_RC_CHECK=1`, or `abort`) runs at every major mark, once
//! marking is done: it counts the pointer words of every reachable object
//! (a coroutine's saved words are its suspended frame's, which own nothing
//! yet) and reports each reachable object whose count is below the number of
//! words that name it (a word stored without its increment) and those above
//! it (a decrement missed, or a referrer that died uncounted).

use super::*;

/// What [`Heap::rc_check`] does at a major mark (`COIL_RC_CHECK`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RcCheckMode {
    /// No check (the default).
    Off,
    /// Count and print a summary line to stderr.
    Report,
    /// [`Self::Report`], and panic when a count is below its references.
    Abort,
    /// Keep counts with no oracle (`COIL_RC_CHECK=count`): what a run
    /// costs with counts (reference-count plan step 6).
    Count,
}

/// The process's [`RcCheckMode`], read once from `COIL_RC_CHECK`.
pub fn rc_check_mode() -> RcCheckMode {
    static MODE: std::sync::OnceLock<RcCheckMode> = std::sync::OnceLock::new();
    *MODE.get_or_init(|| match std::env::var("COIL_RC_CHECK").as_deref() {
        Ok("abort") => RcCheckMode::Abort,
        Ok("count") => RcCheckMode::Count,
        Ok("0") | Ok("") | Err(_) => RcCheckMode::Off,
        Ok(_) => RcCheckMode::Report,
    })
}

/// `COIL_RC_FREE=1` in a check run: a count that reaches zero frees its
/// object ([`Heap::set_rc_frees`]). The code must count its frames' words
/// (compiled in a check run, plan step 4). `COIL_RC_FREE=check` releases
/// without freeing: the objects are recorded instead, and the next major
/// mark aborts naming who still holds one it reaches, as
/// `--early-free check` does.
pub(crate) fn rc_frees_asked() -> bool {
    rc_check_mode() != RcCheckMode::Off && std::env::var("COIL_RC_FREE").is_ok_and(|v| v == "1" || v == "check")
}

/// `COIL_RC_FREE=check` ([`rc_frees_asked`]).
fn rc_frees_checked() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| rc_frees_asked() && std::env::var("COIL_RC_FREE").is_ok_and(|v| v == "check"))
}

/// `COIL_RC_SWEPT=1` in a check run: the sweep names each object it frees
/// (what the counts leaked, or a cycle's) on stderr.
pub(crate) fn rc_swept_shown() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| rc_check_mode() != RcCheckMode::Off && std::env::var_os("COIL_RC_SWEPT").is_some_and(|v| v == "1"))
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
        self.rc_on && self.alloc_lock.is_null()
    }

    /// Keep counts in this heap whatever `COIL_RC_CHECK` says (tests).
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
            // own nothing in step 2.
            Object::Instance(_) | Object::Enum(_) | Object::Fn(_) | Object::Root(_) => true,
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
        let row = self.class_field_kinds(inst.as_ref().type_id);
        if row.contains(&common::WORD_TAGGED) {
            let before = self.rc_before(Value::from(Object::Instance(*inst).addr()));
            inst.as_mut().set_slot(slot, value);
            self.rc_after(before);
            return;
        }
        let kind = match row.get(slot).copied() {
            Some(common::WORD_UNKNOWN) | None => common::packed_word_kind(inst.as_ref().own_kinds(), slot),
            Some(k) => k,
        };
        let old = inst.as_ref().storage.as_slice()[slot];
        inst.as_mut().set_slot(slot, value);
        self.rc_inc_word(value, kind);
        self.rc_dec_word(old, kind);
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
                o.rc_inc();
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
                        o.rc_inc();
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
    fn rc_object(&self, v: Value) -> Option<Object> {
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
            o.rc_inc();
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
        if o.rc_dec() && self.rc_frees {
            self.rc_pending.borrow_mut().push(o.addr());
        }
    }

    /// Release what [`Self::rc_dec_pending`] left at zero and still is.
    pub(crate) fn rc_release_pending(&mut self) -> usize {
        if self.rc_pending.get_mut().is_empty() || self.gc_phase != GcPhase::Idle {
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
                    o.rc_inc();
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
            o.rc_inc();
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
            Some(o) if o.rc_dec() && self.rc_frees => self.rc_release(o),
            _ => 0,
        }
    }

    /// Free `obj`, whose count reached zero: each object its words name
    /// loses an owner, and those that reach zero are released in turn (a
    /// worklist, not recursion). An object the runtime cannot free here
    /// stays, at count zero, for the collector: a
    /// root handle, a coroutine (its words are frame-shaped), an
    /// object whose `drop` has not run (step 7), and every
    /// object while a collection or a steal epoch is open. The last
    /// collection's root buffer is no test: it is stale once that ends, and
    /// frames count their words. Returns how many objects it freed.
    fn rc_release(&mut self, obj: Object) -> usize {
        if self.gc_phase != GcPhase::Idle || self.epoch_stw || !self.alloc_lock.is_null() {
            if rc_swept_shown() {
                eprintln!("coil rc-swept: release of {} at {:#x} skipped (phase {:?}, epoch {}, lock {})", obj.kind_name(), obj.addr(), self.gc_phase, self.epoch_stw, !self.alloc_lock.is_null());
            }
            return 0;
        }
        let mut stack = std::mem::take(&mut self.rc_stack);
        stack.push(obj);
        let mut n = 0;
        while let Some(obj) = stack.pop() {
            if !self.rc_releasable(obj) {
                // Its `drop` runs first, at the runtime's next safepoint.
                if obj.rc_count() == 0 && self.pending_drop(obj) && !self.rc_due.contains(&obj.addr()) {
                    self.rc_due.push(obj.addr());
                    continue;
                }
                if rc_swept_shown() && obj.rc_count() == 0 {
                    eprintln!("coil rc-swept: {} at {:#x} not releasable", obj.kind_name(), obj.addr());
                }
                continue;
            }
            self.rc_dec_children(obj, &mut stack);
            n += 1;
            if obj.is_weak_target() {
                self.clear_weaks_to(obj.addr());
            }
            if rc_frees_checked() {
                self.doomed.push(obj.addr());
                continue;
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
            // SAFETY: its count is zero: no counted word names it, and
            // frames count theirs once [`Self::rc_frees`] is on.
            unsafe { self.dealloc(obj) };
        }
        self.rc_stack = stack;
        n
    }

    /// Each object `obj`'s words name loses an owner; those that reach zero
    /// go on `stack` ([`Self::rc_release`]'s worklist).
    fn rc_dec_children(&self, obj: Object, stack: &mut Vec<Object>) {
        struct Dec<'a>(&'a Heap, &'a mut Vec<Object>);
        impl SlotVisitor for Dec<'_> {
            fn word(&mut self, v: &Value, kind: u8) {
                if (kind == common::WORD_POINTER || kind == common::WORD_UNKNOWN)
                    && let Some(o) = self.0.rc_object(*v)
                    && o.rc_dec()
                {
                    self.1.push(o);
                }
            }

            fn member(&mut self, m: &Member) {
                if let Member::Object(o) = m
                    && o.rc_dec()
                {
                    self.1.push(*o);
                }
            }
        }
        obj.visit_slots(self, &mut Dec(self, stack));
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
        if !self.rc_counting() || self.finalizer_types.is_empty() {
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
        (matches!(obj, Object::Fn(_) | Object::String(_) | Object::Weak(_)) || matches!(obj, Object::Array(_) | Object::Enum(_) | Object::Instance(_)) && self.early_freeable(obj))
            && !self.pending_drop(obj)
            && obj.rc_count() == 0
    }

    /// The oracle ([`rc_check_mode`]), between a major mark and its sweep.
    pub(crate) fn rc_check(&self) {
        let mode = rc_check_mode();
        if matches!(mode, RcCheckMode::Off | RcCheckMode::Count) {
            return;
        }
        struct Tally<'a> {
            heap: &'a Heap,
            refs: HashMap<u64, u32>,
            unknown: u64,
        }
        impl SlotVisitor for Tally<'_> {
            fn word(&mut self, v: &Value, kind: u8) {
                match kind {
                    common::WORD_POINTER => {}
                    common::WORD_UNKNOWN => {
                        self.unknown += 1;
                    }
                    _ => return,
                }
                if let Some(o) = self.heap.rc_object(*v) {
                    *self.refs.entry(o.addr()).or_default() += 1;
                }
            }

            fn member(&mut self, m: &Member) {
                if let Member::Object(o) = m {
                    *self.refs.entry(o.addr()).or_default() += 1;
                }
            }
        }
        let mut t = Tally { heap: self, refs: HashMap::new(), unknown: 0 };
        let mut live = Vec::new();
        for obj in self {
            if obj.is_marked() {
                // A coroutine's saved words are its frame's (not counted).
                if !matches!(obj, Object::Coroutine(_)) {
                    obj.visit_slots(self, &mut t);
                }
                live.push(obj);
            }
        }
        let (mut below, mut equal, mut above, mut immortal) = (0u64, 0u64, 0u64, 0u64);
        let mut by_kind: std::collections::BTreeMap<&'static str, u64> = Default::default();
        let mut short: Vec<(u64, &'static str, u32, u32)> = Vec::new();
        for &obj in &live {
            let count = obj.rc_count();
            let refs = t.refs.get(&obj.addr()).copied().unwrap_or(0);
            if count == COUNT_IMMORTAL {
                immortal += 1;
            } else if count < refs {
                below += 1;
                *by_kind.entry(obj.kind_name()).or_default() += 1;
                if short.len() < 4 {
                    short.push((obj.addr(), obj.kind_name(), count, refs));
                }
            } else if count == refs {
                equal += 1;
            } else {
                above += 1;
            }
        }
        let kinds: Vec<String> = by_kind.iter().map(|(k, n)| format!("{k}={n}")).collect();
        eprintln!(
            "coil rc-check: below={below} equal={equal} above={above} immortal={immortal} unknown-words={} swept={} [{}]",
            t.unknown,
            self.rc_swept,
            kinds.join(" ")
        );
        // Who names the first few undercounted objects.
        if !short.is_empty() {
            struct Who<'a> {
                heap: &'a Heap,
                want: Vec<u64>,
                at: usize,
                hits: Vec<(u64, usize)>,
            }
            impl SlotVisitor for Who<'_> {
                fn word(&mut self, v: &Value, kind: u8) {
                    if (kind == common::WORD_POINTER || kind == common::WORD_UNKNOWN)
                        && let Some(o) = self.heap.rc_object(*v)
                        && self.want.contains(&o.addr())
                    {
                        self.hits.push((o.addr(), self.at));
                    }
                    self.at += 1;
                }

                fn member(&mut self, m: &Member) {
                    if let Member::Object(o) = m
                        && self.want.contains(&o.addr())
                    {
                        self.hits.push((o.addr(), self.at));
                    }
                    self.at += 1;
                }
            }
            for (addr, kind, count, refs) in &short {
                let mut named = Vec::new();
                for &obj in &live {
                    if matches!(obj, Object::Coroutine(_)) {
                        continue;
                    }
                    let mut w = Who { heap: self, want: vec![*addr], at: 0, hits: Vec::new() };
                    obj.visit_slots(self, &mut w);
                    for (_, at) in w.hits {
                        let what = match obj {
                            Object::Instance(i) => format!("{} type {} slot {at}", obj.kind_name(), i.as_ref().type_id),
                            Object::Enum(e) => format!("{} tag {} word {at}", obj.kind_name(), e.as_ref().tag),
                            _ => format!("{} word {at}", obj.kind_name()),
                        };
                        named.push(what);
                    }
                }
                eprintln!("coil rc-check:   {kind} count {count} < {refs}: named by {}", named.join(", "));
            }
        }
        if mode == RcCheckMode::Abort && below > 0 {
            panic!("coil rc-check: {below} reachable objects have a count below their heap references");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_mutation_moves_the_owners_its_words_gained_and_lost() {
        let mut heap = Heap::default();
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
