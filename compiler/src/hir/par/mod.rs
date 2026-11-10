//! Automatic parallelization: where a body may fork
//! (`docs/internals/auto-par.md`).

pub mod loop_par;
pub mod par_hint;
pub mod par_profit;
pub mod sign;

pub use loop_par::{LoopParSite, LoopParSites, LoopReduceOp};
pub use par_profit::{
    ArgForm, DEFAULT_LOOP_GRAIN, PAR_SPEC_HOPS, ParArm, ParBinOp, ParCombine, ParForkSite, args_worth_parallel, arm_callee, guards_hold,
    par_worker_name, unary_dynamic_cutoff,
};
