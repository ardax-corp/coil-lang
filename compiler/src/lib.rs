mod ast_cache;
mod attrs;
mod block_builder;
pub mod builtin_docs;
pub(crate) mod escape;
pub mod comptime_cache;
mod comptime_expr;
mod const_fold;
pub mod debug_vars;
#[cfg(any(test, feature = "dissect"))]
mod dissect;
// `il::tell` is exercised by `tests/cursor_model.rs`, which diffs bytecode
// against the VM cursor and symbolic-IL tell against bytecode; the rest of
// the IL stays crate-private.
pub(crate) mod il;
pub(crate) mod mir;
pub use il::opt::{BodyTier, OptStats, last_opt_stats};
pub use il::tell;
pub use il::OptLevel;
pub use common::DebugLevel;
pub use hir::effects::{
    EffectsCapture, describe_fns, effect_declaration_errors, start_effects_capture, take_effects_capture,
};
pub use hir::{ContractLevel, start_hir_capture, take_hir_capture};
pub use mir::{start_mir_capture, take_mir_capture};
pub(crate) mod hir;
mod layout;
mod host_grants;
mod local_scopes;
pub mod macros;
mod module_roots;
mod monomorphize;
mod pipeline;
mod project_index;
mod shadow_traits;
mod static_writes;
mod strip_tests;
mod variadic;
pub mod symbols;
mod typechecking;
pub mod verify;
#[macro_use]
mod codegen;

#[cfg(any(test, feature = "dissect"))]
pub use dissect::{
    DissectArtifacts, FnSym, IlSnapshot, filter_symbols, format_bytecode, format_bytecode_annotated,
    format_bytecode_section,
    format_il, format_symbol_index, matches_fn_pat,
};
pub use host_grants::{ComptimeGrants, ComptimeLimits, HostGrants, Scoped};
pub use module_roots::{default_module_roots, resolve_use_in_roots};
pub use pipeline::*;
pub use project_index::ProjectIndex;

/// COI-407: trailing `if` end-label stays bound after the next body is replaced.
#[doc(hidden)]
pub fn prove_coi407_trailing_if_end() {
    crate::il::module::prove_trailing_if_end_after_next_body_replace();
}
pub use reporting::{ErrorCode, Label, Message, MessageKind};
pub use typechecking::env::{Env, Frame};
pub use typechecking::pretty::format_ty_for_diag;
pub use typechecking::{
    BuiltinExport, CStructDef, CallbackSigDef, Checker, DefId, DefInterner, DefKind, FfiBuiltin,
    ForInCounted, ForInInfo, ForInKind, ModuleId, Ty, VirtualModules,
};

pub use codegen::{Compiler, PROLOGUE_BYTECODE_LEN, unescape_coil_string};
pub use local_scopes::{binding_at, local_bindings, local_bindings_in, LocalBinding};
pub use typechecking::bare_cases::BareCases;
pub use symbols::{PathPart, RefSite, SymbolDef, SymbolIndex, SymbolKind};
