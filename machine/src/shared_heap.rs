//! C1/C2 shared-heap steal epoch (Layer A STW).
//!
//! Counted-loop chunks and expression IPA arms run as stolen jobs on one
//! [`Heap`] and several stacks. No collect during the epoch; a job that would
//! GC aborts to isolate / sequential fallback. See
//! `docs/internals/shared-heap-sendability.md`.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use common::Value;

use crate::memory::{Heap, Object};
use crate::thread::{is_immediate_value, ThreadProgram};

/// `COIL_SHARED_HEAP=0` / `false` / `off` / `no` forces the isolate spawn path.
pub fn runtime_enabled() -> bool {
    match std::env::var("COIL_SHARED_HEAP") {
        Ok(v) if matches!(v.as_str(), "0" | "false" | "off" | "no") => false,
        _ => true,
    }
}

/// S2b maps are mandatory for shared-heap steal (pre-14 / empty → isolate).
pub fn program_has_real_maps(program: &ThreadProgram) -> bool {
    program.stack_maps.iter().any(|m| {
        !m.frame_slots.is_empty() || m.safepoints.iter().any(|s| !s.slots.is_empty())
    })
}

/// C1 share whitelist: immediates, immortal unit enums, Arc handles already
/// on this Heap. Arrays / strings / Fn / cycles stay isolate.
pub fn is_c1_shareable(heap: &Heap, v: Value) -> bool {
    if is_immediate_value(heap, v) {
        return true;
    }
    let Some(obj) = heap.find_object_by_addr(v.raw() as u64) else {
        return true;
    };
    match obj {
        Object::Enum(gc) if gc.as_ref().payload.is_empty() => true,
        Object::Sender(_) | Object::Receiver(_) | Object::Mutex(_) | Object::RwLock(_) => true,
        _ => false,
    }
}

/// One steal epoch: several stacks, one Heap, no in-epoch collect.
pub struct SharedHeapEpoch {
    heap: *mut Heap,
    alloc_lock: Mutex<()>,
    /// Inflight shared jobs (spawn +1, join −1). Epoch ends at 0.
    pub(crate) jobs: AtomicUsize,
    aborted: AtomicBool,
}

unsafe impl Send for SharedHeapEpoch {}
unsafe impl Sync for SharedHeapEpoch {}

impl SharedHeapEpoch {
    pub fn new(heap: *mut Heap) -> Arc<Self> {
        Arc::new(Self {
            heap,
            alloc_lock: Mutex::new(()),
            jobs: AtomicUsize::new(0),
            aborted: AtomicBool::new(false),
        })
    }

    pub fn heap_ptr(&self) -> *mut Heap {
        self.heap
    }

    pub fn alloc_lock(&self) -> &Mutex<()> {
        &self.alloc_lock
    }

    pub fn abort(&self) {
        self.aborted.store(true, Ordering::SeqCst);
    }

    pub fn is_aborted(&self) -> bool {
        self.aborted.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::Heap;

    #[test]
    fn immediates_are_shareable() {
        let heap = Heap::default();
        assert!(is_c1_shareable(&heap, Value::from(42_i64)));
        assert!(is_c1_shareable(&heap, Value::from(0_i64)));
    }

    #[test]
    fn empty_maps_are_not_real() {
        let p = ThreadProgram {
            code: std::sync::Arc::new(Vec::new()),
            constants: std::sync::Arc::new(Vec::new()),
            strings: std::sync::Arc::new(Vec::new()),
            static_slot_count: 0,
            debug: common::ProgramDebug::default(),
            operand_stack_slots: 8,
            stack_maps: Vec::new(),
        };
        assert!(!program_has_real_maps(&p));
    }
}
