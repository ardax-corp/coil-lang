//! What a running program needs besides an interpreter: the heap and the
//! host natives today, the scheduler, reactor and string table as the
//! VM-free plan moves them (`docs/internals/native.md`). The VM's
//! [`crate::Machine`] owns one; native code reaches it through its
//! context, so host functions run without the interpreter.

/// The program's runtime state that native code and the VM share.
pub(crate) struct Runtime {
    pub(crate) heap: crate::memory::HeapSlot,
    pub(crate) natives: crate::ffi::Natives,
}

impl Runtime {
    pub(crate) fn new() -> Self {
        let mut heap = crate::memory::HeapSlot::default();
        heap.get_mut().set_nursery(crate::memory::nursery_enabled());
        Self { heap, natives: crate::ffi::Natives::new() }
    }
}
