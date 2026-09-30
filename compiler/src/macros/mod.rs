//! User-defined derive and attribute macros.
//!
//! A macro is ordinary coil that runs at compile time. `derive Name(TypeDecl t)`
//! and `attr name(FnDecl f, …)` / `attr name(TypeDecl t, …)` are lowered to
//! plain functions ([`lower`]); `quote` templates become string building on
//! the embedded `macro` module ([`MACRO_SOURCE`]).
//!
//! Using one (`#[derive(Name)]`, `#[name(...)]`) records a [`PendingMacro`]
//! during attribute expansion. After discovery the pipeline compiles the
//! providing modules into one expansion program, runs it through a
//! [`MacroHost`] (the VM, with no host access and a step budget), re-parses
//! the returned source and splices it next to (derive) or in place of
//! (attribute) the declaration. See `docs/internals/macros.md`.

pub mod encode;
pub mod lower;

use std::ops::Range;
use std::path::{Path, PathBuf};

use common::Byte;

/// Module path of the embedded declaration model (`use macro::{TypeDecl, Code}`).
pub const MACRO_MODULE: &str = "macro";

/// Source of the embedded `macro` module.
pub const MACRO_SOURCE: &str = include_str!("../prelude/macro.hy");

/// Module path of the built-in derives (`Show`, `Eq`, …), always in scope.
/// Not under `prelude::`, which is the compiler's virtual module.
pub const DERIVE_MODULE: &str = "derive";

/// Source of the built-in derives.
pub const DERIVE_SOURCE: &str = include_str!("../prelude/derive.hy");

/// Derives [`DERIVE_MODULE`] declares: usable without a `use`.
pub const PRELUDE_DERIVES: &[&str] = &[
    "Show",
    "Eq",
    "Ord",
    "Default",
    "Hash",
    "String",
    "Send",
    "Sensitive",
];

/// Pseudo path the embedded `macro` module is compiled from.
pub fn macro_module_path() -> PathBuf {
    PathBuf::from("<coil>/macro.hy")
}

/// Pseudo path of the built-in derive module.
pub fn derive_module_path() -> PathBuf {
    PathBuf::from("<coil>/derive.hy")
}

/// Embedded source for a pseudo path.
pub fn embedded_source(path: &Path) -> Option<&'static str> {
    if path == macro_module_path() {
        Some(MACRO_SOURCE)
    } else if path == derive_module_path() {
        Some(DERIVE_SOURCE)
    } else {
        None
    }
}

/// Module path of an embedded pseudo path.
pub fn embedded_module(path: &Path) -> Option<&'static str> {
    if path == macro_module_path() {
        Some(MACRO_MODULE)
    } else if path == derive_module_path() {
        Some(DERIVE_MODULE)
    } else {
        None
    }
}

/// Embedded module a `use path::name` refers to, if any.
pub fn embedded_use(path: &[String], name: &str) -> Option<(PathBuf, &'static str)> {
    let head = path.first().map(String::as_str).unwrap_or(name);
    if head == MACRO_MODULE {
        return Some((macro_module_path(), MACRO_MODULE));
    }
    (head == DERIVE_MODULE).then(|| (derive_module_path(), DERIVE_MODULE))
}

/// Function a `derive Name` is lowered to.
pub fn derive_fn_name(name: &str) -> String {
    format!("__derive_{name}")
}

/// Function a macro `attr name` is lowered to.
pub fn attr_fn_name(name: &str) -> String {
    format!("__attr_{name}")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MacroKind {
    Derive,
    Attr,
}

impl MacroKind {
    pub fn describe(self) -> &'static str {
        match self {
            Self::Derive => "derive",
            Self::Attr => "attribute macro",
        }
    }
}

/// What an attribute macro receives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MacroInput {
    TypeDecl,
    FnDecl,
}

/// A `derive` or macro `attr` declared by a module.
#[derive(Clone, Debug)]
pub struct MacroDecl {
    pub kind: MacroKind,
    /// Name as used at the call site (`ToJson`, `log`).
    pub name: String,
    /// Lowered function name ([`derive_fn_name`] / [`attr_fn_name`]).
    pub fn_name: String,
    /// Field / variant attributes a derive owns (`attrs(json)`).
    pub helpers: Vec<String>,
    pub input: MacroInput,
    /// Extra parameters of an attribute macro: `(name, type as written)`.
    pub params: Vec<(String, String)>,
}

/// An attribute argument as written: `key = value`, or positional (`key` empty).
#[derive(Clone, Debug, PartialEq)]
pub struct MacroArg {
    pub key: String,
    /// Literal value; strings without their quotes.
    pub value: String,
    /// `string`, `int`, `float`, `bool` or `ident`.
    pub kind: &'static str,
}

/// A use of a user macro found during attribute expansion.
#[derive(Clone, Debug)]
pub struct PendingMacro {
    pub kind: MacroKind,
    pub name: String,
    /// Span of the declaration it applies to (identifies it in the AST).
    pub target: parser::SimpleSpan,
    /// Owning class for an `impl` method, `None` for a top-level item.
    pub owner: Option<String>,
    /// Attribute arguments (attribute macros).
    pub args: Vec<MacroArg>,
    /// Where diagnostics point: the declaration header.
    pub range: Range<usize>,
    /// Field / variant attributes on the type (derives): each must be a
    /// helper of one of the type's derives.
    pub member_attrs: Vec<String>,
}

/// Runs a compiled expansion program. Implemented by the `comptime` crate on
/// top of the VM so the compiler itself does not depend on `machine`.
pub trait MacroHost: Send + Sync {
    /// Call each entry (a zero-argument function returning `string`) and
    /// return its result, or the panic / error text.
    fn run(
        &self,
        program: &crate::Pipeline,
        bytecode: &[Byte],
        constants: &[u64],
        entries: &[u32],
    ) -> Vec<Result<String, String>>;
}

static DEFAULT_HOST: std::sync::OnceLock<std::sync::Arc<dyn MacroHost>> = std::sync::OnceLock::new();

/// Install the host new pipelines use to run macros (binaries call this once
/// at startup; see the `comptime` crate). Later calls are ignored.
pub fn install_default_host(host: std::sync::Arc<dyn MacroHost>) {
    let _ = DEFAULT_HOST.set(host);
}

/// The host installed with [`install_default_host`], if any.
pub fn default_host() -> Option<std::sync::Arc<dyn MacroHost>> {
    DEFAULT_HOST.get().cloned()
}

/// Step budget for one macro call (loop back-edges + calls).
pub const MACRO_STEP_BUDGET: u64 = 20_000_000;

/// Host natives a macro may call: pure computation only (no IO, files,
/// network, environment, clocks, threads or process control).
pub fn native_allowed_at_compile_time(name: &str) -> bool {
    matches!(
        name,
        "from_bytes" | "to_bytes" | "ord" | "char" | "hash_string" | "result_unit_probe" | "simd_axpy_reduce"
    ) || ["math_", "vec_", "packed_", "gc_"]
        .iter()
        .any(|p| name.starts_with(p))
}

/// Escape `s` as the body of a coil string literal.
pub fn escape_string_lit(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out
}

/// `"…"` coil string literal for `s`.
pub fn string_lit(s: &str) -> String {
    format!("\"{}\"", escape_string_lit(s))
}
