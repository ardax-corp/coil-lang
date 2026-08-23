//! Compile-time stack IL with symbolic labels.
//!
//! Codegen emits [`IlOp`]s (including [`IlOp::Label`] bind points and
//! label-targeted jumps). [`lower`] assigns PCs once, selecting fused
//! encodings along the way — no post-shrink jump relocation.

mod algebraic;
mod bounds;
mod canon;
mod cast_spill;
mod builder;
mod codebuf;
mod emit_buf;
mod func;
mod gvn;
mod gvn_ssa;
mod licm;
mod lower;
mod module;
mod op;
pub(crate) mod opt;
mod sp;
pub mod tell;
mod treeshake;

pub use bounds::{BoundsStats, last_bounds_stats};
pub use canon::{CanonStats, last_canon_stats};
pub use opt::OptLevel;

pub use builder::{IlBuilder, IlError};
pub use codebuf::CodeBuf;
pub use emit_buf::EmitBuf;
pub use func::IlFunc;
#[allow(unused_imports)]
pub use lower::{Lowered, lower, lower_module, lower_with_funcs};
pub use module::IlModule;
pub use op::{EntryKind, IlJumpKind, IlOp, Label};
pub use treeshake::{TreeshakeInput, prune_unused_functions};
