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
use crate::thread::{is_immediate_value, ThreadProgram};

/// GC frame maps are mandatory for shared-heap steal: S2b stack maps or
/// precise frame maps (archive minor 21+). A program with neither (pre-14,
/// or empty) spawns isolate.
pub fn program_has_real_maps(program: &ThreadProgram) -> bool {
    program.stack_maps.iter().any(|m| {
        !m.frame_slots.is_empty() || m.safepoints.iter().any(|s| !s.slots.is_empty())
    }) || program.precise_frames.iter().any(|m| m.any_pc.is_some() || !m.at_pc.is_empty())
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
            precise_frames: Arc::default(),
            class_word_kinds: Arc::default(),
            static_word_kinds: Arc::default(),
        };
        assert!(!program_has_real_maps(&p));
    }

    #[test]
    fn precise_frames_are_real_maps() {
        let p = ThreadProgram {
            code: std::sync::Arc::new(Vec::new()),
            constants: std::sync::Arc::new(Vec::new()),
            strings: std::sync::Arc::new(Vec::new()),
            static_slot_count: 0,
            debug: common::ProgramDebug::default(),
            operand_stack_slots: 8,
            stack_maps: Vec::new(),
            precise_frames: Arc::new(vec![common::PreciseFrameMap {
                entry_pc: 0,
                end_pc: 4,
                any_pc: None,
                at_pc: vec![common::SlotMap { pc: 2, slots: vec![0] }],
                frame_words: 0,
            }]),
            class_word_kinds: Arc::default(),
            static_word_kinds: Arc::default(),
        };
        assert!(program_has_real_maps(&p));
    }
}
