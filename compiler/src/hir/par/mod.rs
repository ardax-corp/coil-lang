//! Automatic parallelization: where a body may fork
//! (`docs/internals/auto-par.md`).

pub mod loop_par;

pub use loop_par::{LoopParSite, LoopParSites, LoopReduceOp};
