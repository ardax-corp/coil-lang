mod attrs;
mod block_builder;
mod const_fold;
#[cfg(any(test, feature = "dissect"))]
mod dissect;
// `il::tell` is exercised by `tests/cursor_model.rs`, which diffs bytecode
// against the VM cursor and symbolic-IL tell against bytecode; the rest of
// the IL stays crate-private.
pub(crate) mod il;
pub(crate) mod profile;
pub use il::tell;
pub use il::{BoundsStats, CanonStats, OptLevel, last_bounds_stats, last_canon_stats};
pub use il::opt::{OptStats, last_opt_stats};
pub use profile::{
    InstrumentMap, LoadError, ProfileData, PROFILE_VERSION, current_profile, instrument_for_pgo,
    instrument_for_pgo_mut, last_instrument_map, optimize_with_profile, profile_from_runtime,
    set_current_profile, set_pgo_instrument,
};
mod manifest;
mod monomorphize;
mod pipeline;
mod project_index;
mod strip_tests;
pub mod symbols;
mod typechecking;
#[macro_use]
mod codegen;

#[cfg(any(test, feature = "dissect"))]
pub use dissect::{
    DissectArtifacts, FnSym, IlSnapshot, filter_symbols, format_bytecode, format_bytecode_section,
    format_il, format_symbol_index, matches_fn_pat,
};
pub use manifest::{DependencySpec, Manifest, ManifestError, PackageInfo, Scripts};
pub use pipeline::*;
pub use project_index::ProjectIndex;
pub use reporting::{ErrorCode, Label, Message, MessageKind};
pub use typechecking::env::{Env, Frame};
pub use typechecking::pretty::format_ty_for_diag;
pub use typechecking::{
    BuiltinExport, CStructDef, CallbackSigDef, Checker, FfiBuiltin, ForInInfo, ForInKind, Ty,
    VirtualModules,
};

pub use codegen::{Compiler, PROLOGUE_BYTECODE_LEN, unescape_coil_string};
pub use symbols::{RefSite, SymbolDef, SymbolIndex, SymbolKind};
