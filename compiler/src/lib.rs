mod ast_cache;
mod attrs;
pub mod builtin_docs;
pub(crate) mod effects;
pub(crate) mod escape;
pub mod comptime_cache;
mod comptime_expr;
mod const_fold;
pub mod debug_vars;
#[cfg(any(test, feature = "dissect"))]
mod dissect;
pub(crate) mod bodies;
pub(crate) mod opt;
pub(crate) mod pure_call;
pub(crate) mod mir;
pub use opt::{OptStats, last_opt_stats};
pub use opt::{EarlyFree, OptLevel};
pub use common::DebugLevel;
pub use hir::effects::{
    EffectsCapture, describe_fns, effect_declaration_errors, start_effects_capture, take_effects_capture,
};
pub use hir::{ContractLevel, start_hir_capture, take_hir_capture};
pub use mir::{BoxKind, BoxSite, start_box_capture, take_box_capture};
/// MIR for a native backend (`coil-native`): the capture and the types it reads.
pub mod native_mir {
    pub use crate::bodies::Label;
    pub use crate::mir::archive;
    pub use crate::mir::coverage;
    pub use crate::mir::{
        BlockId, LaneExpr, LaneVal, MirAllocKind, MirBlock, MirBinOp, MirCastKind, MirCmpOp, MirConst, MirFfiOp, MirFunc, MirGcKind, MirInst, MirLaneShape, MirTy,
        MirUnaryOp, NativeBody, Terminator, ValueId, VectorOp, start_native_capture, start_raw_capture, vector_op,
        VecExpr, VecLoopKind, liveness, spin_headers,
        take_native_capture, ffi_use,
        alloc_call, AllocCall, allocates, barrier_pairs, heap_word, held_words, is_generator, numeric, opaque_words, pointer_words, root_handles, scalar, ty_of, used_params, word_kind, is_tail_call_inst, pair_words, in_range_steps,
    };
}
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
pub use dissect::{DissectArtifacts, FnSym, format_symbol_index, matches_fn_pat};
pub use host_grants::{ComptimeGrants, ComptimeLimits, HostGrants, Scoped};
pub use module_roots::{default_module_roots, resolve_use_in_roots};
pub use pipeline::*;
pub use project_index::ProjectIndex;

pub use reporting::{ErrorCode, Label, Message, MessageKind};
pub use typechecking::env::{Env, Frame};
pub use typechecking::pretty::format_ty_for_diag;
pub use typechecking::{
    BuiltinExport, CStructDef, CallbackSigDef, Checker, DefId, DefInterner, DefKind, FfiBuiltin,
    ForInCounted, ForInInfo, ForInKind, ModuleId, Ty, VirtualModules,
};

pub use codegen::{Compiler, unescape_coil_string};
pub use local_scopes::{binding_at, local_bindings, local_bindings_in, LocalBinding};
pub use typechecking::bare_cases::BareCases;
pub use symbols::{PathPart, RefSite, SymbolDef, SymbolIndex, SymbolKind};
