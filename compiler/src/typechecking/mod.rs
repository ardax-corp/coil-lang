//! Type inference for coil (explicit generics, monomorphic `let`).
//!
//! Runs after parsing and before code emission. Exposes [`Checker`]
//! for inference, native registration, and span-indexed type lookup.

pub mod aggregate_arith;
pub mod bare_cases;
pub mod const_eval;
pub mod control_flow;
pub mod def_id;
pub mod env;
pub mod fn_value_escape;
pub mod generics;
pub mod id;
pub mod index_facts;
pub mod infer;
pub mod kind;
pub mod loop_par;
pub mod par_hint;
pub mod par_profit;
pub mod pretty;
pub mod purity;
pub mod resolve;
pub mod subst;
pub mod ty;
pub mod unify;
pub mod virtual_modules;

pub use aggregate_arith::{
    AggregateArithInfo, AggregateMode, AggregateOp, AggregateShape, LinearAlgebraKind,
    MatrixCellOp, ScalarSide,
};
#[allow(unused_imports)] // public API for Matrix helpers
pub use aggregate_arith::{is_matrix_ty, unwrap_matrix_ty, wrap_matrix_ty};
pub use def_id::{DefId, DefInterner, DefKind, ModuleId};
#[allow(unused_imports)] // public API re-export
pub use infer::{TestTry, TryInto, 
    CStructDef, CallbackSigDef, Checker, ForInCounted, ForInInfo, ForInKind, SelectedOverload,
    TypedSidecar,
};
#[allow(unused_imports)] // public API for kind-aware callers / tests
pub use kind::Kind;
#[allow(unused_imports)] // public API re-export
pub use loop_par::{LoopParSite, LoopParSites, LoopReduceOp, analyze_loop_par_sites};
#[allow(unused_imports)] // public API re-export
pub use par_hint::{EscapeKind, NamedEscape, ParEscapeHint, analyze_par_escape_hints};
pub use par_profit::{
    ArgForm, PAR_SPEC_HOPS, ParArm, ParBinOp, ParCombine, ParForkSite, analyze_par_fork_sites,
    args_worth_parallel, arm_callee, collect_par_worker_fns, guards_hold,
    DEFAULT_LOOP_GRAIN, par_worker_name, unary_dynamic_cutoff,
};
#[allow(unused_imports)] // public API re-export
pub use purity::{analyze_fn_effects, analyze_recursive_fns};
#[allow(unused_imports)] // public API re-export
pub use ty::{ScalarBacking, Ty};
#[allow(unused_imports)] // public API for Vec helpers / codegen
pub use ty::{vec_app_ty, vec_element_ty};
pub use virtual_modules::{
    BuiltinExport, FfiBuiltin, PreludeFn, StringBuiltin, VirtualModules,
};
// Re-export for callers / tests that match on GC virtual exports.
#[allow(unused_imports)]
pub use virtual_modules::GcBuiltin;
