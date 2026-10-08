//! User-defined derives, attribute macros and function-style macros.
//!
//! A macro is ordinary coil that runs at compile time. `derive Name(TypeDecl t)`,
//! `attr name(FnDecl f, …)` / `attr name(TypeDecl t, …)` and
//! `macro name(Expr a, …)` are lowered to plain functions ([`lower`]); `quote`
//! templates become string building on the embedded `macro` module
//! ([`MACRO_SOURCE`]).
//!
//! Using one (`#[derive(Name)]`, `#[name(...)]`, `name!(…)`) records a
//! [`PendingMacro`] during attribute expansion. After discovery the pipeline
//! compiles each providing module into an expansion program, runs it through
//! a [`MacroHost`] (the VM, with no host access and a step budget), re-parses
//! the returned source and splices it next to (derive) or in place of
//! (attribute, call) the use. See `docs/internals/macros.md`.

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

/// Module path of the embedded task API (`use task::{scope, Scope, Task}`).
pub const TASK_MODULE: &str = "task";

/// Source of the embedded `task` module.
pub const TASK_SOURCE: &str = include_str!("../prelude/task.hy");

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

/// Pseudo path of the embedded task module.
pub fn task_module_path() -> PathBuf {
    PathBuf::from("<coil>/task.hy")
}

/// Embedded source for a pseudo path.
pub fn embedded_source(path: &Path) -> Option<&'static str> {
    if path == macro_module_path() {
        Some(MACRO_SOURCE)
    } else if path == derive_module_path() {
        Some(DERIVE_SOURCE)
    } else if path == task_module_path() {
        Some(TASK_SOURCE)
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
    } else if path == task_module_path() {
        Some(TASK_MODULE)
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
    if head == TASK_MODULE {
        return Some((task_module_path(), TASK_MODULE));
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

/// Function a `macro name` is lowered to.
pub fn macro_fn_name(name: &str) -> String {
    format!("__macro_{name}")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MacroKind {
    Derive,
    Attr,
    /// `macro name(…)`, used as `name!(…)`.
    Function,
}

impl MacroKind {
    pub fn describe(self) -> &'static str {
        match self {
            Self::Derive => "derive",
            Self::Attr => "attribute macro",
            Self::Function => "macro",
        }
    }
}

/// What a macro receives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MacroInput {
    TypeDecl,
    FnDecl,
    /// A function-style macro's arguments: `Expr` parameters, the last one
    /// possibly `Vec<Expr>` (the rest).
    Exprs,
}

/// Where a function-style macro call sits, which decides how its output
/// parses and where it goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallPosition {
    /// A derive or attribute on a declaration.
    Decl,
    /// `name!(…);` at the top level: the output is items.
    Item,
    /// `name!(…);` in a block: the output is statements.
    Stmt,
    /// Anywhere else: the output is one expression.
    Expr,
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
    /// Extra parameters of an attribute macro, or every parameter of a
    /// function-style macro: `(name, type as written)`.
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
    /// Span of the declaration it applies to, or of the `name!(…)` call
    /// (identifies it in the AST).
    pub target: parser::SimpleSpan,
    pub position: CallPosition,
    /// Owning class for an `impl` method, `None` for a top-level item.
    pub owner: Option<String>,
    /// Attribute arguments (attribute macros).
    pub args: Vec<MacroArg>,
    /// Where diagnostics point: the declaration header or the call.
    pub range: Range<usize>,
    /// Field / variant attributes on the type (derives): each must be a
    /// helper of one of the type's derives.
    pub member_attrs: Vec<String>,
    /// For a use in macro output: the module that produced it, whose own
    /// macros resolve without an import where the output lands.
    pub from_provider: Option<PathBuf>,
}

/// A compiled expansion program: everything a host needs to run its entries.
/// It depends only on the providing modules, so one is compiled per provider
/// set and reused for every macro call (see `pipeline_macros`).
pub struct CompiledExpansion {
    pub bytecode: std::sync::Arc<Vec<Byte>>,
    pub constants: std::sync::Arc<Vec<u64>>,
    pub strings: std::sync::Arc<Vec<String>>,
    pub static_slot_count: u32,
    pub operand_stack_slots: u32,
    pub program_debug: common::ProgramDebug,
}

/// Runs a compiled expansion program. Implemented by the `comptime` crate on
/// top of the VM so the compiler itself does not depend on `machine`.
pub trait MacroHost: Send + Sync {
    /// Call each `(entry, input)`: `entry` is the offset of a
    /// `fn(string input) -> string`. Returns each result, or the panic /
    /// error text.
    fn run(&self, program: &CompiledExpansion, calls: &[(u32, String)]) -> Vec<Result<String, String>>;
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
    ) || ["math_", "vec_", "packed_", "gc_", "string_"]
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
