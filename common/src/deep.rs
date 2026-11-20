//! Stack for the passes that recurse once per level of an expression tree.
//!
//! A tree walk costs a native frame set per level, and a debug build's
//! frames are large: a chain of a few thousand binary operators (generated
//! code, a long `+` of strings) would overflow the thread's stack. A pass
//! wraps its per-node recursion in [`deep_stack`] at one place; past the
//! thread's stack the walk continues on a new heap segment instead of
//! aborting.

/// Below this much stack left, [`deep_stack`] switches to a new segment: the
/// most one level of a pass may use between two [`deep_stack`] calls.
const RED_ZONE: usize = 1024 * 1024;

/// Size of each new segment.
const SEGMENT: usize = 16 * 1024 * 1024;

/// Run `f`, on a new stack segment when less than [`RED_ZONE`] is left.
#[inline]
pub fn deep_stack<R>(f: impl FnOnce() -> R) -> R {
    stacker::maybe_grow(RED_ZONE, SEGMENT, f)
}
