//! Interpreter and allocation counters for tests and `vm_profile` builds:
//! dispatches (per PC too), heap allocations, collections, fast `Make*`
//! paths and string interning. Without the feature each reads 0.

#[cfg(any(test, feature = "vm_profile"))]
use std::sync::atomic::{AtomicU64, Ordering};

// Thread-local dispatch counter (tests / `vm_profile` only).
#[cfg(any(test, feature = "vm_profile"))]
thread_local! {
    pub(crate) static VM_DISPATCH_COUNT: AtomicU64 = const { AtomicU64::new(0) };
}

/// Reset the VM dispatch counter.
#[cfg(any(test, feature = "vm_profile"))]
pub fn reset_dispatch_count() {
    VM_DISPATCH_COUNT.with(|c| c.store(0, Ordering::Relaxed));
}

/// Read the VM dispatch counter.
#[cfg(any(test, feature = "vm_profile"))]
#[must_use]
pub fn dispatch_count() -> u64 {
    VM_DISPATCH_COUNT.with(|c| c.load(Ordering::Relaxed))
}

#[cfg(not(any(test, feature = "vm_profile")))]
#[must_use]
pub fn dispatch_count() -> u64 {
    0
}

#[cfg(not(any(test, feature = "vm_profile")))]
pub fn reset_dispatch_count() {}

// Per-PC dispatch histogram (tests / `vm_profile` only). Off until
// [`begin_pc_profile`] so ordinary test runs do not pay for it.
#[cfg(any(test, feature = "vm_profile"))]
thread_local! {
    pub(crate) static VM_PC_PROFILE: std::cell::RefCell<Option<Vec<u64>>> =
        const { std::cell::RefCell::new(None) };
}

/// Start counting dispatches per PC on this thread.
#[cfg(any(test, feature = "vm_profile"))]
pub fn begin_pc_profile() {
    VM_PC_PROFILE.with(|p| *p.borrow_mut() = Some(Vec::new()));
}

/// Stop profiling and return dispatch counts indexed by PC.
#[cfg(any(test, feature = "vm_profile"))]
pub fn take_pc_profile() -> Vec<u64> {
    VM_PC_PROFILE.with(|p| p.borrow_mut().take().unwrap_or_default())
}

// Allocation / GC counters (`vm_profile` + tests). Useful for binary_trees-style
// heap traffic without needing external alloc tracers.
#[cfg(any(test, feature = "vm_profile"))]
thread_local! {
    pub(crate) static VM_ALLOC_COUNT: AtomicU64 = const { AtomicU64::new(0) };
    pub(crate) static VM_GC_COUNT: AtomicU64 = const { AtomicU64::new(0) };
    pub(crate) static VM_MINOR_COUNT: AtomicU64 = const { AtomicU64::new(0) };
    pub(crate) static VM_MAKE_FAST_COUNT: AtomicU64 = const { AtomicU64::new(0) };
    pub(crate) static VM_INTERN_STR_COUNT: AtomicU64 = const { AtomicU64::new(0) };
}

/// Record one managed heap object allocation.
#[cfg(any(test, feature = "vm_profile"))]
#[inline]
pub(crate) fn note_heap_alloc() {
    VM_ALLOC_COUNT.with(|c| {
        c.fetch_add(1, Ordering::Relaxed);
    });
}

#[cfg(not(any(test, feature = "vm_profile")))]
#[inline]
pub(crate) fn note_heap_alloc() {}

/// Reset allocation / GC / Make* fast-path counters.
#[cfg(any(test, feature = "vm_profile"))]
pub fn reset_alloc_profile() {
    VM_ALLOC_COUNT.with(|c| c.store(0, Ordering::Relaxed));
    VM_GC_COUNT.with(|c| c.store(0, Ordering::Relaxed));
    VM_MINOR_COUNT.with(|c| c.store(0, Ordering::Relaxed));
    VM_MAKE_FAST_COUNT.with(|c| c.store(0, Ordering::Relaxed));
    VM_INTERN_STR_COUNT.with(|c| c.store(0, Ordering::Relaxed));
}

#[cfg(not(any(test, feature = "vm_profile")))]
pub fn reset_alloc_profile() {}

/// Number of managed objects allocated since the last reset.
#[cfg(any(test, feature = "vm_profile"))]
#[must_use]
pub fn alloc_count() -> u64 {
    VM_ALLOC_COUNT.with(|c| c.load(Ordering::Relaxed))
}

#[cfg(not(any(test, feature = "vm_profile")))]
#[must_use]
pub fn alloc_count() -> u64 {
    0
}

/// Number of nursery minor collections since the last reset.
#[cfg(any(test, feature = "vm_profile"))]
#[must_use]
pub fn minor_gc_count() -> u64 {
    VM_MINOR_COUNT.with(|c| c.load(Ordering::Relaxed))
}

#[cfg(not(any(test, feature = "vm_profile")))]
#[must_use]
pub fn minor_gc_count() -> u64 {
    0
}

/// Number of mark-and-sweep collections since the last reset.
#[cfg(any(test, feature = "vm_profile"))]
#[must_use]
pub fn gc_count() -> u64 {
    VM_GC_COUNT.with(|c| c.load(Ordering::Relaxed))
}

#[cfg(not(any(test, feature = "vm_profile")))]
#[must_use]
pub fn gc_count() -> u64 {
    0
}

/// Number of MakeTuple / MakeArray / MakeEnum fixed-arity fast paths taken.
#[cfg(any(test, feature = "vm_profile"))]
#[must_use]
pub fn make_fast_count() -> u64 {
    VM_MAKE_FAST_COUNT.with(|c| c.load(Ordering::Relaxed))
}

#[cfg(not(any(test, feature = "vm_profile")))]
#[must_use]
pub fn make_fast_count() -> u64 {
    0
}

#[cfg(any(test, feature = "vm_profile"))]
#[inline]
pub(crate) fn note_make_fast() {
    VM_MAKE_FAST_COUNT.with(|c| {
        c.fetch_add(1, Ordering::Relaxed);
    });
}

#[cfg(not(any(test, feature = "vm_profile")))]
#[inline]
pub(crate) fn note_make_fast() {}

/// Record one `Heap::intern_str` (hash + intern table probe).
#[cfg(any(test, feature = "vm_profile"))]
#[inline]
pub(crate) fn note_intern_str() {
    VM_INTERN_STR_COUNT.with(|c| {
        c.fetch_add(1, Ordering::Relaxed);
    });
}

#[cfg(not(any(test, feature = "vm_profile")))]
#[inline]
pub(crate) fn note_intern_str() {}

/// Number of `intern_str` calls since the last reset.
#[cfg(any(test, feature = "vm_profile"))]
#[must_use]
pub fn intern_str_count() -> u64 {
    VM_INTERN_STR_COUNT.with(|c| c.load(Ordering::Relaxed))
}

#[cfg(not(any(test, feature = "vm_profile")))]
#[must_use]
pub fn intern_str_count() -> u64 {
    0
}
