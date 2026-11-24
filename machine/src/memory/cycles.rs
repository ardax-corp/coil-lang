//! Cycle collection under reference counts (reference-count plan step 9).
//!
//! Counts free every object whose owners go, except those that own each
//! other: a cycle, which only an object of a class the compiler marks can
//! close (an identity class reaching itself, or one holding a function or a
//! coroutine: `Heap::register_cyclic_type`), since a write to a shared
//! value copies it. Every cycle holds one, so the walks start there. Trial deletion finds
//! them (Bacon and Rajan, synchronous): each object's count less the owners
//! other heap objects account for is what owns it from outside the heap
//! (frames, statics, the runtime). An object with such an owner, a runtime
//! root or a kind the counts cannot free is live, and so is everything it
//! reaches; the rest owns only itself and is garbage.
//!
//! The trial counts live in a side table, so the headers are never touched
//! and an interrupted pass changes nothing.

use super::*;
use std::collections::HashSet;

/// What a pass found an object to be.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Color {
    /// Seen: its trial count is its count less the heap words naming it.
    Gray,
    /// Reached from outside the heap: live.
    Black,
}

impl Heap {
    /// A cycle pass ran on allocation pressure with no tracing collector
    /// ([`super::rc::rc_trace_off`]): the next one waits until the live bytes
    /// grow by the growth factor.
    pub(crate) fn cycles_ran(&mut self) {
        self.gc_next_threshold = self.counts.alloc_bytes.saturating_mul(self.gc_growth_factor.max(2)).max(GC_NEXT_THRESHOLD).min(self.byte_cap);
    }

    /// Objects of class `type_id` can close a reference cycle (the
    /// compiler's finalizer registry, `CYCLIC_TYPE_FLAG`).
    pub fn register_cyclic_type(&mut self, type_id: u32) {
        let word = type_id as usize / 64;
        if self.cyclic_types.len() <= word {
            self.cyclic_types.resize(word + 1, 0);
        }
        self.cyclic_types[word] |= 1 << (type_id % 64);
    }

    /// Whether `obj` may be in a cycle that only its own objects own: an
    /// instance of a class [`Self::register_cyclic_type`] named.
    fn cycle_start(&self, obj: Object) -> bool {
        match obj {
            Object::Instance(gc) => {
                let t = gc.as_ref().type_id as usize;
                self.cyclic_types.get(t / 64).is_some_and(|w| w & (1 << (t % 64)) != 0)
            }
            _ => false,
        }
    }

    /// The objects `obj`'s counted words name, as a release decrements them.
    fn rc_children(&self, obj: Object, out: &mut Vec<Object>) {
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
        obj.visit_slots(self, &mut Kids(self, out));
    }

    /// Whether a cycle pass may free `obj` once nothing outside its cycle
    /// owns it. A coroutine, a thread, a stream or a library is kept (with
    /// all it reaches), as is a pinned object.
    fn cycle_freeable(obj: Object) -> bool {
        matches!(obj, Object::Instance(_) | Object::Enum(_) | Object::Array(_) | Object::Fn(_) | Object::String(_) | Object::Root(_) | Object::Weak(_))
            && !obj.is_pinned()
    }

    /// The garbage cycles of this heap: the objects only other garbage owns.
    /// `roots` are the addresses the runtime holds without a count; they
    /// and what they reach are live. Empty unless counts are kept and live
    /// and no collection is under way.
    pub(crate) fn cycle_garbage(&self, roots: &[u64]) -> Vec<Object> {
        if !self.rc_counts_live() || self.cyclic_types.is_empty() {
            return Vec::new();
        }
        let mut seen: HashMap<u64, (Color, i64)> = HashMap::new();
        let (mut work, mut kids) = (Vec::new(), Vec::new());
        // Trial deletion: each object of a cyclic class starts a walk of
        // what it reaches, and each word walked takes one from its
        // target's trial count. An object something outside the walk owns
        // keeps a count above zero.
        let holders: Vec<Object> = self.objects().filter(|&o| self.cycle_start(o)).collect();
        for &h in &holders {
            let c = h.rc_count();
            if c == 0 || c == COUNT_IMMORTAL || seen.contains_key(&h.addr()) {
                continue;
            }
            seen.insert(h.addr(), (Color::Gray, i64::from(c)));
            work.push(h);
            while let Some(o) = work.pop() {
                kids.clear();
                self.rc_children(o, &mut kids);
                for &k in &kids {
                    let c = k.rc_count();
                    if c == COUNT_IMMORTAL {
                        continue;
                    }
                    let e = seen.entry(k.addr()).or_insert_with(|| {
                        work.push(k);
                        (Color::Gray, i64::from(c))
                    });
                    e.1 -= 1;
                }
            }
        }
        // Live: an outside owner, a runtime root, a kind kept whole; and
        // everything they reach.
        let mut live: Vec<Object> = Vec::new();
        for (&addr, &(_, trial)) in &seen {
            // SAFETY: `seen` holds only live objects' addresses.
            let Some(o) = (unsafe { Object::from_header(addr) }) else { continue };
            if trial > 0 || !Self::cycle_freeable(o) {
                live.push(o);
            }
        }
        live.extend(roots.iter().filter_map(|&a| self.find_object_by_addr(a)));
        while let Some(o) = live.pop() {
            match seen.get_mut(&o.addr()) {
                Some((Color::Black, _)) => continue,
                Some(e) => e.0 = Color::Black,
                // A root no walk saw holds nothing seen: nothing to keep.
                None => {
                    seen.insert(o.addr(), (Color::Black, 0));
                }
            }
            kids.clear();
            self.rc_children(o, &mut kids);
            live.extend(kids.iter().copied().filter(|k| seen.get(&k.addr()).is_some_and(|e| e.0 != Color::Black)));
        }
        let mut garbage: Vec<Object> = seen
            .iter()
            .filter(|(_, e)| e.0 == Color::Gray)
            // SAFETY: as above.
            .filter_map(|(&a, _)| unsafe { Object::from_header(a) })
            .collect();
        garbage.sort_unstable_by_key(|o| o.addr());
        garbage
    }

    /// Free `garbage` ([`Self::cycle_garbage`]): the words its objects hold
    /// on objects outside it are released, then they go. Returns how many
    /// objects that freed, cascades included.
    pub(crate) fn free_cycles(&mut self, garbage: &[Object]) -> usize {
        if garbage.is_empty() || !self.rc_frees() {
            return 0;
        }
        let inside: HashSet<u64> = garbage.iter().map(|o| o.addr()).collect();
        let (mut kids, mut zero) = (Vec::new(), Vec::new());
        for &o in garbage {
            kids.clear();
            self.rc_children(o, &mut kids);
            for &k in &kids {
                if !inside.contains(&k.addr()) && self.rc_dec_obj(k) {
                    zero.push(k);
                }
            }
        }
        for &o in garbage {
            o.rc_set_count(0);
            // SAFETY: only other garbage named it, and that goes too.
            unsafe { self.rc_free(o) };
        }
        let mut n = garbage.len();
        for k in zero {
            n += self.rc_release(k);
        }
        n
    }
}
