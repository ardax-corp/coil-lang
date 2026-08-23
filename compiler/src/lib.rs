mod attrs;
mod block_builder;
mod const_fold;
#[cfg(any(test, feature = "dissect"))]
mod dissect;
// `il::tell` is exercised by `tests/cursor_model.rs`, which diffs bytecode
// against the VM cursor and symbolic-IL tell against bytecode; the rest of
// the IL stays crate-private.
pub(crate) mod il;
pub use il::tell;
pub use il::{BoundsStats, CanonStats, OptLevel, last_bounds_stats, last_canon_stats};
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
pub use manifest::{DependencySpec, Manifest, ManifestError, PackageInfo};
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
