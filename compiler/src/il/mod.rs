//! Compile-time stack IL with symbolic labels.
//!
//! Instruction lowering + label resolution + fuse — not a semantic IR.
//! Names, types, and call meaning live in DefIds / the typed sidecar.
//! Numeric SSA (COI-267) lives in [`crate::mir`], not here.
//! Codegen emits [`IlOp`]s (including [`IlOp::Label`] bind points and
//! label-targeted jumps). [`lower`] assigns PCs once, selecting fused
//! encodings along the way — no post-shrink jump relocation.

mod builder;
mod codebuf;
mod func;
mod op;
pub(crate) mod opt;
pub(crate) mod pure_call;
// Stack-height deltas, for `coil dissect`'s depth column.
#[cfg(any(test, feature = "dissect"))]
mod sp;

pub use opt::{EarlyFree, OptLevel};
#[cfg(any(test, feature = "dissect"))]
#[cfg(any(test, feature = "dissect"))]

pub use builder::IlBuilder;
pub use codebuf::CodeBuf;
pub use func::IlFunc;
pub use op::{EntryKind, IlJumpKind, IlOp, Label};
