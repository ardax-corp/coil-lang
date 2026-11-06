//! C1/C2 shared-heap steal epoch (Layer A STW).
//!
//! Counted-loop chunks and expression IPA arms run as stolen jobs on one
//! [`Heap`] and several stacks. No collect during the epoch; a job that would
//! GC aborts to isolate / sequential fallback. Shared-heap spawn is always
//! on; debugger-attached runs and programs without stack maps still use
//! isolate `PortableValue` spawn. See
//! `docs/internals/shared-heap-sendability.md`.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use common::Value;

use crate::memory::{Heap, Object};
use crate::thread::is_immediate_value;

/// C1 share whitelist: immediates, immortal unit enums, Arc handles already
/// on this Heap. Arrays / strings / Fn / cycles stay isolate.
pub fn is_c1_shareable(heap: &Heap, v: Value) -> bool {
    if is_immediate_value(heap, v) {
        return true;
    }
    let Some(obj) = heap.find_object_by_addr(v.bits()) else {
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
}
