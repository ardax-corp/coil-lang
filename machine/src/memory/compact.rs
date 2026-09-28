//! Mostly-copying evacuation of sparse slab chunks (`gc-compact`).
//!
//! Bartlett-style: every VM root **pins** its target — even precise-map
//! slots only say a word *may* hold a heap reference, so rewriting them could
//! corrupt an integer. Only objects reachable exclusively through precise
//! heap-interior references (`Member::Object` fields, enum / box payloads,
//! masked coroutine slots) move; those references are rewritten in place.
//! Anything an ambiguous word can reach (arrays that may hold references,
//! tuples, captures, `Member::Value`, unmasked coroutine words), `Root` /
//! `Weak` targets, immortal unit enums and every non-trivial kind (strings,
//! coroutines, closures, streams, …) stays put. See
//! `docs/internals/moving-gc.md`.
//!
//! Runs only between cycles (`GcPhase::Idle`) and outside a shared-heap
//! epoch; the VM additionally skips it while native code sits below the
//! running frame (a host frame could hold a raw handle).

use std::alloc::Layout;
use std::collections::{HashMap, HashSet};
use std::ptr::NonNull;

use super::super::AddrHashBuilder;

type AddrSet = HashSet<u64, AddrHashBuilder>;
type AddrMap = HashMap<u64, u64, AddrHashBuilder>;

use common::Value;

use super::{
    GcData, GcHeader, GcPhase, Heap, Member, ObjArray, ObjBoxed, ObjEnum, ObjInstance, ObjTuple,
    Object,
};

/// What one evacuation did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Evacuation {
    /// Objects copied to a new slot.
    pub moved: usize,
    /// Live objects that could not move (pinned or non-movable kind).
    pub pinned: usize,
    /// Chunks chosen for evacuation.
    pub chunks: usize,
    /// Bytes of emptied chunks given back to the OS.
    pub released: usize,
    /// More sparse chunks remain than one step moves.
    pub capped: bool,
}

/// `(chunk index, live objects, movable objects, slots)` for chunk choice.
type ChunkFill = (usize, usize, usize, usize);

/// Chunks one evacuation step may empty (4 MiB of slab).
const MAX_STEP_CHUNKS: usize = 64;

/// At least 1 MiB and a quarter of the slab.
fn worth_it(bytes: usize, slab_bytes: usize) -> bool {
    bytes >= (1 << 20) && bytes * 4 >= slab_bytes
}

impl Object {
    /// Slot layout of a kind evacuation may move; `None` for the rest.
    fn movable_layout(&self) -> Option<Layout> {
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

    /// Rewrite this object's precise references to moved objects.
    fn rewrite_precise_refs(&self, heap: &Heap, fwd: &AddrMap) {
        let moved = |m: &Member| -> Option<Member> {
            let Member::Object(o) = m else {
                return None;
            };
            let to = *fwd.get(&o.addr())?;
            heap.find_object_by_addr(to).map(Member::Object)
        };
        let fix = |slots: &mut [Member]| {
            for m in slots {
                if let Some(n) = moved(m) {
                    *m = n;
                }
            }
        };
        match self {
            Self::Instance(i) => {
                let inst = i.payload_mut();
                match &mut inst.storage {
                    super::InstanceStorage::Table(table) => {
                        let updates: Vec<_> = table
                            .iter()
                            .filter_map(|(k, v)| moved(&v).map(|n| (k, n)))
                            .collect();
                        for (k, n) in updates {
                            table.insert(k, n);
                        }
                    }
                    storage => fix(storage.as_mut_slice().unwrap_or_default()),
                }
            }
            Self::Enum(e) => fix(e.payload_mut().payload.as_mut_slice()),
            Self::Boxed(b) => {
                let b = b.payload_mut();
                if let Some(n) = moved(&b.payload) {
                    b.payload = n;
                }
            }
            Self::Root(r) => {
                if let Some(m) = &mut r.payload_mut().payload
                    && let Some(n) = moved(m)
                {
                    *m = n;
                }
            }
            Self::PolyFn(p) => {
                for m in p.payload_mut().captured_dicts.iter_mut().flatten() {
                    if let Some(n) = moved(m) {
                        *m = n;
                    }
                }
            }
            Self::Coroutine(c) => {
                let coro = c.payload_mut();
                let mask = coro.saved_live_mask;
                if mask == 0 {
                    return;
                }
                for (i, v) in coro.saved_stack.iter_mut().enumerate().take(64) {
                    if mask & (1u64 << i) == 0 {
                        continue;
                    }
                    let addr = v.heap_addr();
                    if let Some(&to) = fwd.get(&addr) {
                        // Keep the `Result` `Err` tag bit.
                        *v = Value::from(to | (v.raw() as u64 & 1));
                    }
                }
            }
            _ => {}
        }
    }
}

impl Heap {
    /// Evacuate sparse chunks (or, with `everything`, every chunk holding a
    /// movable object — the stress mode). `vm_pins` are the VM's roots; each
    /// pins its target. Returns what moved.
    pub fn evacuate(&mut self, vm_pins: &[u64], everything: bool) -> Evacuation {
        let mut out = Evacuation::default();
        if self.gc_phase != GcPhase::Idle || self.epoch_stw || self.head.is_none() {
            return out;
        }
        // Cheap gate before walking the heap: free slots must add up to a
        // real share of the slab.
        let slab_bytes = self.slab.mapped_bytes();
        if !everything && !worth_it(self.slab.free_slot_bytes(), slab_bytes) {
            return out;
        }
        let mut live = Vec::with_capacity(self.live_count);
        let mut current = self.head;
        while let Some(obj) = current {
            live.push(obj);
            current = obj.get_next();
        }

        let mut pinned: AddrSet = vm_pins.iter().map(|a| a & !1).collect();
        pinned.extend(self.immortal_enums.values().map(Object::addr));
        pinned.extend(self.gc_roots.iter().map(|a| a & !1));
        for obj in &live {
            if obj.movable_layout().is_none() {
                pinned.insert(obj.addr());
            }
            match obj {
                // FFI code may hold a rooted object's address; weak targets
                // are stored as raw words.
                Object::Root(r) => {
                    if let Some(Member::Object(o)) = &r.as_ref().payload {
                        pinned.insert(o.addr());
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

        // Per chunk (dense indices): live objects and how many could move.
        let index = self.slab.chunk_index();
        let nchunks = self.slab.chunk_count();
        let mut per_chunk: Vec<(usize, usize)> = vec![(0, 0); nchunks];
        for obj in &live {
            if let Some(i) = index.chunk_of(obj.addr()) {
                let e = &mut per_chunk[i];
                e.0 += 1;
                if obj.movable_layout().is_some() && !pinned.contains(&obj.addr()) {
                    e.1 += 1;
                }
            }
        }
        let mut capped = false;
        let chosen: Vec<usize> = if everything {
            (0..nchunks).filter(|&i| per_chunk[i].1 > 0).collect()
        } else {
            // Per size class, evacuate the sparsest chunks whose movable
            // objects fit in the free slots of the chunks that stay.
            let mut by_class: HashMap<(u32, u32), Vec<ChunkFill>> = HashMap::new();
            for (i, &(n, movable)) in per_chunk.iter().enumerate() {
                if n == 0 {
                    continue;
                }
                if let Some(class) = self.slab.chunk_class(i) {
                    by_class
                        .entry(class)
                        .or_default()
                        .push((i, n, movable, self.slab.chunk_slots(i)));
                }
            }
            let mut out = Vec::new();
            for chunks in by_class.values_mut() {
                // Only chunks every object of which can move become empty.
                chunks.sort_unstable_by_key(|&(_, n, _, _)| n);
                let mut free_left: usize = chunks.iter().map(|&(_, n, _, slots)| slots - n).sum();
                for &(i, n, movable, slots) in chunks.iter() {
                    // A chunk with a pinned object cannot empty: it stays a
                    // destination.
                    if movable != n {
                        continue;
                    }
                    free_left -= slots - n;
                    if movable > free_left {
                        break;
                    }
                    free_left -= movable;
                    out.push(i);
                }
            }
            // Bound one step (transient copies + forwarding map); the VM runs
            // the next step on the following cycle while `capped` is set.
            if out.len() > MAX_STEP_CHUNKS {
                out.sort_unstable_by_key(|&i| per_chunk[i].0);
                out.truncate(MAX_STEP_CHUNKS);
                capped = true;
            }
            out
        };
        let mut candidates = vec![false; nchunks];
        for &i in &chosen {
            candidates[i] = true;
        }
        // Worth it only when a real share of the slab would come back: moving
        // a few short-lived objects every cycle just refaults the chunks.
        let freeable = chosen.len() * super::super::slab::CHUNK_BYTES;
        if chosen.is_empty() || (!everything && !worth_it(freeable, slab_bytes)) {
            return out;
        }
        out.chunks = chosen.len();

        // New copies must land outside the chunks being emptied.
        let parked = self.slab.take_free_in(&candidates, &index);
        let mut fwd: AddrMap = AddrMap::default();
        for obj in &live {
            let addr = obj.addr();
            let Some(layout) = obj.movable_layout() else {
                out.pinned += 1;
                continue;
            };
            if pinned.contains(&addr) || !index.chunk_of(addr).is_some_and(|i| candidates[i]) {
                out.pinned += usize::from(pinned.contains(&addr));
                continue;
            }
            let to = self.slab.alloc(layout);
            // SAFETY: both are slots of the same size class; the old copy is
            // poisoned below and never dropped, so the payload moves bitwise.
            unsafe {
                std::ptr::copy_nonoverlapping(addr as *const u8, to.as_ptr(), layout.size());
            }
            fwd.insert(addr, to.as_ptr() as u64);
        }
        out.moved = fwd.len();

        // Poison the old headers before anything looks objects up again.
        for &old in fwd.keys() {
            unsafe { (*(old as *const GcHeader)).kind.set(0) };
        }
        let relocate = |o: Object| -> Object {
            fwd.get(&o.addr())
                .and_then(|&to| self.find_object_by_addr(to))
                .unwrap_or(o)
        };
        // Relink the intrusive list through the new copies.
        let head = self.head.map(relocate);
        let mut current = head;
        while let Some(obj) = current {
            let next = obj.get_next().map(relocate);
            obj.set_next(next);
            current = next;
        }
        self.head = head;
        // Rewrite precise interior references, then free the old slots.
        let mut current = self.head;
        while let Some(obj) = current {
            obj.rewrite_precise_refs(self, &fwd);
            current = obj.get_next();
        }
        for &old in fwd.keys() {
            self.slab.free(unsafe { NonNull::new_unchecked(old as *mut u8) });
        }
        for p in parked {
            self.slab.free(p);
        }
        // Candidates that kept no object are empty now: return their pages
        // instead of letting the next allocations refill them.
        let emptied: Vec<bool> = (0..nchunks)
            .map(|i| candidates[i] && per_chunk[i].0 == per_chunk[i].1)
            .collect();
        out.released = self.slab.release_chunks(&emptied, &index);
        out.capped = capped;
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{EnumPayload, ObjString};

    /// A cons list `Node(v, next)` of enums linked through `Member::Object`.
    fn cons_list(heap: &mut Heap, n: i64) -> Vec<u64> {
        let mut addrs = Vec::new();
        let mut next: Option<Object> = None;
        for v in 0..n {
            let payload = match next {
                Some(o) => EnumPayload::two(Member::Value(Value::from(v)), Member::Object(o)),
                None => EnumPayload::one(Member::Value(Value::from(v))),
            };
            let (obj, _) = heap.alloc(ObjEnum::new(0, payload), Object::Enum);
            addrs.push(obj.addr());
            next = Some(obj);
        }
        addrs
    }

    /// Walk the list from `head`, returning the payload values.
    fn walk(heap: &Heap, head: u64) -> Vec<i64> {
        let mut out = Vec::new();
        let mut cur = heap.find_object_by_addr(head);
        while let Some(Object::Enum(e)) = cur {
            let p = e.as_ref().payload.as_slice();
            let Member::Value(v) = p[0] else { panic!("value payload") };
            out.push(v.as_int());
            cur = match p.get(1) {
                Some(Member::Object(o)) => Some(*o),
                _ => None,
            };
        }
        out
    }

    #[test]
    fn evacuation_moves_a_linked_list_and_keeps_it_intact() {
        let mut heap = Heap::default();
        // Interleave garbage so the list's chunks end up sparse.
        let mut keep = Vec::new();
        for _ in 0..4 {
            keep.extend(cons_list(&mut heap, 500));
            for i in 0..2000 {
                let _ = heap.alloc(ObjString::from(format!("g{i}").as_str()), Object::String);
            }
        }
        let head = *keep.last().unwrap();
        // Root only the head: collect the garbage.
        heap.begin_mark(&[head]);
        while !heap.mark_quantum(usize::MAX) {}
        unsafe { heap.sweep() };
        let before = walk(&heap, head);
        assert_eq!(before.len(), 500, "only the last list is reachable from head");

        let ev = heap.evacuate(&[head], true);
        assert!(ev.moved > 0, "{ev:?}");
        assert_eq!(walk(&heap, head), before, "list survives the move");
        // Every moved list cell's old address reads as poisoned.
        let moved_old = keep[keep.len() - 500..keep.len() - 1]
            .iter()
            .filter(|&&a| heap.find_object_by_addr(a).is_none())
            .count();
        assert!(moved_old > 0);
        // And a full collection afterwards still sees the whole list.
        heap.begin_mark(&[head]);
        while !heap.mark_quantum(usize::MAX) {}
        unsafe { heap.sweep() };
        assert_eq!(walk(&heap, head), before);
    }

    /// An object an array may reference is pinned (array words are raw).
    #[test]
    fn array_referenced_object_stays_put() {
        let mut heap = Heap::default();
        let (s, _) = heap.alloc(ObjString::from("x"), Object::String);
        let (target, _) = heap.alloc(
            ObjEnum::new(0, EnumPayload::one(Member::Object(s))),
            Object::Enum,
        );
        let (arr, _) = heap.alloc(
            ObjArray::new(vec![Value::from(target.addr())]),
            Object::Array,
        );
        heap.begin_mark(&[arr.addr()]);
        while !heap.mark_quantum(usize::MAX) {}
        unsafe { heap.sweep() };
        let _ = heap.evacuate(&[arr.addr()], true);
        assert!(
            matches!(heap.find_object_by_addr(target.addr()), Some(Object::Enum(_))),
            "array-referenced enum must not move"
        );
    }
}
