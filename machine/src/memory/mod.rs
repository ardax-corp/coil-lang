//! Operand stack, call frames, and mark-and-sweep heap.

mod addr_hash;
mod frame;
mod heap;
mod slab;
mod stack;

pub use addr_hash::*;
pub use frame::*;
pub use heap::*;
pub use stack::*;
