//! Operand stack, call frames, and mark-and-sweep heap.

mod addr_hash;
mod frame;
mod heap;
mod slab;
mod stack;
mod strbuf;

pub use addr_hash::*;
pub use frame::*;
pub use heap::*;
pub use stack::*;
pub use strbuf::StrData;

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

/// What a run does at a compiled early free (`--early-free`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EarlyFreeMode {
    /// Free the object (the default).
    Free,
    /// Free nothing.
    Off,
    /// Free nothing, and abort when the next mark still reaches an object
    /// an early free gave up.
    Check,
}

static NURSERY: AtomicBool = AtomicBool::new(true);
static EARLY_FREE: AtomicU8 = AtomicU8::new(0);

/// Allocate young objects in a copying nursery (`--nursery on|off`, on by
/// default). Read when a VM starts, so set it before running.
pub fn set_nursery(on: bool) {
    NURSERY.store(on, Ordering::Relaxed);
}

pub fn nursery_enabled() -> bool {
    NURSERY.load(Ordering::Relaxed)
}

/// What compiled early frees do in this process (`--early-free`).
pub fn set_early_free_mode(mode: EarlyFreeMode) {
    EARLY_FREE.store(mode as u8, Ordering::Relaxed);
}

pub fn early_free_mode() -> EarlyFreeMode {
    match EARLY_FREE.load(Ordering::Relaxed) {
        1 => EarlyFreeMode::Off,
        2 => EarlyFreeMode::Check,
        _ => EarlyFreeMode::Free,
    }
}
