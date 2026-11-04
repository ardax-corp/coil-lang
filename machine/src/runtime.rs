//! What a running program needs besides an interpreter: the heap and the
//! host natives today, the scheduler, reactor and string table as the
//! VM-free plan moves them (`docs/internals/native.md`). The VM's
//! [`crate::Machine`] owns one; native code reaches it through its
//! context, so host functions run without the interpreter.

use std::sync::Arc;

use common::Value;

use crate::AddrHashBuilder;

/// The program's runtime state that native code and the VM share.
pub(crate) struct Runtime {
    pub(crate) heap: crate::memory::HeapSlot,
    pub(crate) natives: crate::ffi::Natives,
    /// Task scheduler, created by the first `task::scope` (see `task.rs`).
    pub(crate) sched: Option<Box<crate::task::Scheduler>>,
    /// Work-stealing pool sized by [`Self::worker_cap`].
    pub(crate) reactor: std::sync::Arc<crate::reactor::Reactor>,
    /// IO readiness reactor (sync adapters + async waiters).
    pub(crate) io_reactor: std::sync::Arc<crate::io_reactor::IoReactor>,
    /// Undetached spawns owned by this VM (joined at end of `run_with_pool`).
    pub(crate) live_threads: crate::thread::LiveThreadRegistry,
    /// Shared concurrent OS-worker budget for this root VM (and its workers).
    pub(crate) worker_cap: std::sync::Arc<crate::thread::WorkerCap>,
    /// Global static slots (`LoadStatic` / `StoreStatic`).
    pub(crate) statics: Vec<Value>,
    pub(crate) program_strings: Arc<Vec<String>>,
    /// Interned handle per `program_strings` index. Not a GC root: sweep
    /// zeros the table so unmarked literals can die; STRING still stacks
    /// the handle before `maybe_gc`.
    pub(crate) program_string_cache: Vec<Value>,
    /// Native calls in a VM call, innermost last: the heap words their
    /// frames hold are pinned roots ([`crate::native::NativeCtx::for_each_root`]).
    pub(crate) native_roots: Vec<(*mut crate::native::NativeCtx, usize)>,
    /// Arguments of the native FFI calls in flight (nested when a callback
    /// re-enters the VM and calls out again). `FfiInvoke` pops them off the
    /// operand stack, so they are rooted here until the call returns:
    /// `ConstBytes` lends a string's bytes to C for the whole call.
    pub(crate) ffi_arg_roots: Vec<Value>,
    /// `type_id` → drop method entry PC (empty = no user finalizers).
    pub(crate) finalizer_by_type: std::collections::HashMap<u32, u32, AddrHashBuilder>,
    /// Drop entry PCs (for explicit `obj.drop()` once-bit intercept).
    pub(crate) finalizer_pcs: std::collections::HashSet<u32, AddrHashBuilder>,
}

impl Runtime {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn with(sched: Option<Box<crate::task::Scheduler>>, reactor: std::sync::Arc<crate::reactor::Reactor>, io_reactor: std::sync::Arc<crate::io_reactor::IoReactor>, live_threads: crate::thread::LiveThreadRegistry, worker_cap: std::sync::Arc<crate::thread::WorkerCap>, statics: Vec<Value>, program_strings: Arc<Vec<String>>, program_string_cache: Vec<Value>, native_roots: Vec<(*mut crate::native::NativeCtx, usize)>, ffi_arg_roots: Vec<Value>, finalizer_by_type: std::collections::HashMap<u32, u32, AddrHashBuilder>, finalizer_pcs: std::collections::HashSet<u32, AddrHashBuilder>) -> Self {
        let mut heap = crate::memory::HeapSlot::default();
        heap.get_mut().set_nursery(crate::memory::nursery_enabled());
        Self { heap, natives: crate::ffi::Natives::new(), sched, reactor, io_reactor, live_threads, worker_cap, statics, program_strings, program_string_cache, native_roots, ffi_arg_roots, finalizer_by_type, finalizer_pcs }
    }
}
