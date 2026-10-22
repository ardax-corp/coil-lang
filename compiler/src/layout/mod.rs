//! Value representation, decided in one place from the static type.
//!
//! [`rep`] says how a value of a type is held (a word, a pointer niche, a
//! two-slot pair) and how the GC reads it; [`abi`] says how it crosses a
//! call edge. Every consumer (HIR lowering, MIR emit, `HostInvoke`
//! packing, per-type clones, frame maps) asks here.

pub mod abi;
pub mod rep;

pub use abi::*;
pub use rep::*;
