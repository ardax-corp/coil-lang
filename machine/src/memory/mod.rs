//! The heap: allocation, collection, and the run-time switches for both.

mod addr_hash;
mod heap;
mod slab;
mod strbuf;

pub use addr_hash::*;
pub use heap::*;
pub use slab::{Bump, FREE_LINK};
pub use strbuf::StrData;
pub(crate) use strbuf::shared_layout;

use std::sync::atomic::{AtomicU8, Ordering};

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

static EARLY_FREE: AtomicU8 = AtomicU8::new(0);

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
