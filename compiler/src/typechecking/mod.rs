//! Type inference for coil (explicit generics, monomorphic `let`).
//!
//! Runs after parsing and before bytecode emission. Exposes [`Checker`]
//! for inference, native registration, and span-indexed type lookup.

pub mod aggregate_arith;
pub mod const_eval;
pub mod control_flow;
pub mod def_id;
pub mod env;
pub mod generics;
pub mod id;
pub mod infer;
pub mod resolve;
pub mod kind;
pub mod loop_par;
pub mod local_escape;
pub mod index_facts;
pub mod pretty;
pub mod par_profit;
pub mod purity;
pub mod stack_bound;
pub mod subst;
pub mod ty;
pub mod unify;
pub mod virtual_modules;

pub use aggregate_arith::{
    AggregateArithInfo, AggregateArithKind, AggregateOp, LinearAlgebraKind, ScalarSide,
};
#[allow(unused_imports)] // public API for Matrix helpers
pub use aggregate_arith::{is_matrix_ty, unwrap_matrix_ty, wrap_matrix_ty};
pub use def_id::{DefId, DefInterner, DefKind, ModuleId};
#[allow(unused_imports)] // public API re-export
pub use infer::{
    CStructDef, CallbackSigDef, Checker, ForInInfo, ForInKind, SelectedOverload,
    TypedSidecar,
};
#[allow(unused_imports)] // public API for kind-aware callers / tests
pub use kind::Kind;
#[allow(unused_imports)] // public API re-export
pub use loop_par::{LoopParSite, LoopParSites, LoopReduceOp, analyze_loop_par_sites};
#[allow(unused_imports)] // public API re-export
pub use par_profit::{
    ArgForm, ParArm, ParBinOp, ParCombine, ParForkSite, analyze_par_fork_sites, args_worth_parallel,
    arm_callee, collect_par_specialization_args, eval_arm_args, par_cost_threshold,
    par_specialization_name,
};
#[cfg(test)]
pub use par_profit::par_work_units;
#[allow(unused_imports)] // public API re-export
pub use purity::{
    EffectFlags, RecursivePureSet, analyze_fn_effects, analyze_pure_fns, analyze_recursive_fns,
    analyze_recursive_pure,
};
#[allow(unused_imports)] // public API re-export
pub use stack_bound::{
    BoundSource, DEFAULT_OPERAND_STACK_SLOTS, FnStackBound, MAX_OPERAND_STACK_SLOTS, StackBoundReport,
    analyze_stack_bounds, operand_slots_for_frames,
};
#[allow(unused_imports)] // public API re-export
pub use ty::{ScalarBacking, Ty};
#[allow(unused_imports)] // public API for Vec helpers / codegen
pub use ty::{vec_app_ty, vec_element_ty};
pub use virtual_modules::{
    BuiltinExport, FfiBuiltin, IoBuiltin, PreludeFn, StringBuiltin, ThreadBuiltin, VirtualModules,
};
// Re-export for callers / tests that match on GC virtual exports.
#[allow(unused_imports)]
pub use virtual_modules::GcBuiltin;
