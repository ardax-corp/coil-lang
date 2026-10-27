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

/// Module path of the embedded property-test values (`use arbitrary::{Arbitrary, Gen}`).
pub const ARBITRARY_MODULE: &str = "arbitrary";

/// Source of the embedded `arbitrary` module.
pub const ARBITRARY_SOURCE: &str = include_str!("../prelude/arbitrary.hy");

/// Module path of the always-included core prelude (`Option`, `Result`
/// and their methods). Every program depends on it.
pub const CORE_MODULE: &str = "core";

/// Source of the embedded `core` module.
pub const CORE_SOURCE: &str = include_str!("../prelude/core.hy");

/// An enum the core prelude marks with `#[lang(..)]`.
#[derive(Debug)]
pub struct LangEnum {
    /// The lang item (`option`, `result`).
    pub lang: String,
    pub name: String,
    pub type_params: Vec<String>,
    /// Variants in tag order, each with its payload's type parameter names.
    pub variants: Vec<(String, Vec<String>)>,
}

/// The `#[lang]` enums of [`CORE_SOURCE`], parsed once. The checker builds
/// `Option` and `Result` from these declarations.
pub fn core_lang_enums() -> &'static [LangEnum] {
    use parser::ast::EnumVariantPayload;
    use parser::ast::Expression;
    static ENUMS: std::sync::OnceLock<Vec<LangEnum>> = std::sync::OnceLock::new();
    ENUMS.get_or_init(|| {
        let ast = parser::Pratt::default()
            .parse(CORE_SOURCE)
            .expect("core prelude parses");
        let Expression::Program(items) = ast.1.as_ref() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for item in items {
            let Expression::EnumDecl { attrs, name, type_params, variants, .. } = item.1.as_ref()
            else {
                continue;
            };
            let Some(lang) = crate::attrs::lang_item(attrs) else { continue };
            let variants = variants
                .iter()
                .filter_map(|v| match v.1.as_ref() {
                    Expression::EnumVariant { name, payload, .. } => {
                        let params = match payload {
                            EnumVariantPayload::Unit => Vec::new(),
                            EnumVariantPayload::Tuple(parts) => parts
                                .iter()
                                .map(|p| match p.1.as_ref() {
                                    Expression::Type(t) | Expression::Identifier(t) => t.to_string(),
                                    _ => panic!("core prelude: `{name}` payloads must be type parameters"),
                                })
                                .collect(),
                            EnumVariantPayload::Record(_) => {
                                panic!("core prelude: `{name}` must be a tuple variant")
                            }
                        };
                        Some((name.to_string(), params))
                    }
                    _ => None,
                })
                .collect();
            out.push(LangEnum {
                lang: lang.to_string(),
                name: name.to_string(),
                type_params: type_params.iter().map(|p| p.name.to_string()).collect(),
                variants,
            });
        }
        out
    })
}

/// Derives [`DERIVE_MODULE`] declares: usable without a `use`.
pub const PRELUDE_DERIVES: &[&str] = &[
    "Show",
    "Eq",
    "Ord",
    "Default",
    "Hash",
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

/// Pseudo path of the embedded `arbitrary` module.
pub fn arbitrary_module_path() -> PathBuf {
    PathBuf::from("<coil>/arbitrary.hy")
}

/// Pseudo path of the core prelude module.
pub fn core_module_path() -> PathBuf {
    PathBuf::from("<coil>/core.hy")
}

/// Embedded source for a pseudo path.
pub fn embedded_source(path: &Path) -> Option<&'static str> {
    if path == macro_module_path() {
        Some(MACRO_SOURCE)
    } else if path == derive_module_path() {
        Some(DERIVE_SOURCE)
    } else if path == task_module_path() {
        Some(TASK_SOURCE)
    } else if path == arbitrary_module_path() {
        Some(ARBITRARY_SOURCE)
    } else if path == core_module_path() {
        Some(CORE_SOURCE)
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
    } else if path == arbitrary_module_path() {
        Some(ARBITRARY_MODULE)
    } else if path == core_module_path() {
        Some(CORE_MODULE)
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
    if head == ARBITRARY_MODULE {
        return Some((arbitrary_module_path(), ARBITRARY_MODULE));
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

/// Function the `k`-th overload of `macro name` is lowered to: the first
/// keeps [`macro_fn_name`], later ones get a suffix.
pub fn macro_overload_fn_name(name: &str, k: usize) -> String {
    match k {
        0 => macro_fn_name(name),
        k => format!("__macro_{name}__{k}"),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MacroKind {
    Derive,
    Attr,
    /// `macro name(…)`, used as `name!(…)`.
    Function,
    /// A top-level `comptime { … }` block: its own expansion program.
    Comptime,
}

impl MacroKind {
    pub fn describe(self) -> &'static str {
        match self {
            Self::Derive => "derive",
            Self::Attr => "attribute macro",
            Self::Function => "macro",
            Self::Comptime => "`comptime` block",
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

impl MacroDecl {
    /// A function-style macro's arity: the fixed `Expr` parameters, and
    /// whether a last `Vec<Expr>` takes the rest.
    pub fn arity(&self) -> (usize, bool) {
        let rest = self.params.last().is_some_and(|(_, t)| t.starts_with("Vec<"));
        (self.params.len() - usize::from(rest), rest)
    }

    /// Whether a call with `argc` arguments fits this macro's arity.
    pub fn accepts(&self, argc: usize) -> bool {
        match self.arity() {
            (fixed, true) => argc >= fixed,
            (fixed, false) => argc == fixed,
        }
    }

    /// The arity as an error message words it: `2`, `at least 1`.
    pub fn describe_arity(&self) -> String {
        match self.arity() {
            (fixed, true) => format!("at least {fixed}"),
            (fixed, false) => fixed.to_string(),
        }
    }
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
    /// Per-class field word kinds, so captured instances know which fields
    /// are scalars.
    pub class_word_kinds: Vec<common::ClassWordKinds>,
    /// Word kind of each static slot (`common::WORD_*`).
    pub static_word_kinds: std::sync::Arc<Vec<u8>>,
}

/// Runs a compiled expansion program. Implemented by the `comptime` crate on
/// top of the VM so the compiler itself does not depend on `machine`.
pub trait MacroHost: Send + Sync {
    /// Id of the compiler and VM this host was built with, which keys the
    /// on-disk cache of compiled expansion programs; empty keeps none.
    fn build_id(&self) -> &'static str {
        ""
    }

    /// Call each `(entry, input)`: `entry` is the offset of a
    /// `fn(string input) -> string`, each call within `limits` and with the
    /// IO `io` grants. Returns each result, or the panic / error text, with
    /// the IO the call did.
    fn run(
        &self,
        program: &CompiledExpansion,
        calls: &[(u32, String)],
        limits: &crate::ComptimeLimits,
        io: &ComptimeIo,
    ) -> Vec<MacroRun>;

    /// Compute `consts` in order on a compile-time VM of `program`: call
    /// each initializer, store its value to its static slot (later
    /// initializers may read earlier ones), then capture every value as one
    /// snapshot, so values shared between them stay shared. An explicit
    /// item runs within `limits`; a silent one within the silent tier
    /// ([`crate::ComptimeLimits::SILENT_CALL_STEPS`] each,
    /// [`crate::ComptimeLimits::SILENT_MODULE_STEPS`] for all of them) and,
    /// when it fails, is left out instead of failing the rest.
    ///
    /// Then, on the same VM (so a call can read the consts), each of
    /// `folds` runs within the silent tier; one that fails is `None`.
    ///
    /// An explicit item may do the IO `io[item.io]` grants; nothing else
    /// may do any.
    fn eval_consts(
        &self,
        program: &CompiledExpansion,
        consts: &[ConstEval],
        folds: &[FoldCall],
        limits: &crate::ComptimeLimits,
        io: &[ComptimeIo],
    ) -> Result<ConstEvalOutcome, ConstEvalError>;
}

/// A word or a string: an argument or result of a call folded at compile
/// time.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum FoldValue {
    Word(u64),
    Text(String),
}

/// A pure call with constant arguments to compute at compile time: its
/// callee's entry, the arguments, and whether it returns a `string`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FoldCall {
    pub entry: u32,
    pub args: Vec<FoldValue>,
    pub text: bool,
}

/// A value to compute at compile time: the entry of its initializer
/// (`__static_init$NAME`), the static slot its value is stored to and that
/// slot's word kind (`common::WORD_*`). `silent` for a `static let`, whose
/// failure keeps its startup initializer; a `const`'s fails the build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConstEval {
    pub entry: u32,
    pub slot: u32,
    pub kind: u8,
    pub silent: bool,
    /// A value with no snapshot form (a function value, a handle) keeps
    /// its startup initializer instead of failing the build: a `const`'s,
    /// not a `comptime` expression's.
    pub startup_ok: bool,
    /// Index of its grants in the request's `io` (unused when `silent`:
    /// silent work never does IO).
    pub io: u32,
    /// Its value from the result cache (its static slot already set):
    /// restored instead of calling the initializer.
    pub cached: Option<std::sync::Arc<common::HeapSnapshot>>,
    /// Record the IO it does, and when it did some, snapshot its value
    /// alone, for the result cache ([`ConstEvalOutcome::records`]).
    pub record: bool,
}

/// A `record` item that did IO: its index, the IO, and its value alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConstRecord {
    pub index: usize,
    pub accesses: Vec<ComptimeAccess>,
    pub snapshot: common::HeapSnapshot,
}

/// The computed values, the silent items left out (indices into the
/// request), which keep their startup initializers, and each fold's
/// result.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConstEvalOutcome {
    pub snapshot: common::HeapSnapshot,
    pub skipped: Vec<usize>,
    pub folded: Vec<Option<FoldValue>>,
    /// The folds that ran out of the silent tier's steps, with the
    /// microseconds each ran before it stopped: `comptime` would compute
    /// them (coil-lang#1918).
    pub over_budget: Vec<(usize, u64)>,
    pub records: Vec<ConstRecord>,
}

/// Why [`MacroHost::eval_consts`] gave no snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConstEvalError {
    /// Initializer `index` panicked or ran out of budget; the reason.
    Failed { index: usize, reason: String },
    /// A value reached from a const has no compile-time form. `root` is
    /// the index of the const in the request.
    Unsnapshottable(common::SnapshotError),
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

/// The IO one compile-time evaluation may do, resolved for the module its
/// code is in ([`crate::ComptimeGrants::for_module`]): files under `read` /
/// `write` (absolute), environment variables `env`, TCP connections to `net`
/// (`host` or `host:port`). The host checks each native call against it. `offline`
/// (`--comptime-offline`) refuses the environment and the network: an
/// evaluation runs then only when no cached result was found. `replay` is
/// the language server's offline mode: it also refuses writes, and its
/// refusals say to build the project.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct ComptimeIo {
    pub read: Vec<PathBuf>,
    pub write: Vec<PathBuf>,
    pub env: Vec<String>,
    pub net: Vec<String>,
    pub offline: bool,
    pub replay: bool,
}

/// One granted IO call of a compile-time evaluation, as the result cache
/// records it ([`crate::comptime_cache`]).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ComptimeAccess {
    /// A file or directory read (absolute), with [`crate::comptime_cache::path_digest`]
    /// of it when first read.
    Read { path: PathBuf, digest: [u8; 32] },
    Write(PathBuf),
    Env(String),
    /// A TCP connection, `host:port`.
    Net(String),
}

/// One macro call's output (or failure) and the IO it did.
pub type MacroRun = (Result<String, String>, Vec<ComptimeAccess>);

impl ComptimeIo {
    /// The effect bits (`common::EffectFlags`) code with these grants may
    /// have while compiling.
    pub fn effects(&self) -> u16 {
        use common::EffectFlags as F;
        [
            (!self.read.is_empty(), F::READ),
            (!self.write.is_empty(), F::WRITE),
            (!self.env.is_empty(), F::ENV),
            (!self.net.is_empty(), F::NET),
        ]
        .into_iter()
        .filter(|(on, _)| *on)
        .fold(0, |acc, (_, bits)| acc | bits)
    }

    pub fn check_read(&self, path: &str) -> Result<(), String> {
        if under_any(&absolute_path(Path::new(path)), &self.read) {
            return Ok(());
        }
        Err(format!("reading `{path}` while compiling needs `--comptime-read {}`", grant_hint(path)))
    }

    pub fn check_write(&self, path: &str) -> Result<(), String> {
        if under_any(&absolute_path(Path::new(path)), &self.write) {
            if self.replay {
                return Err(format!(
                    "the editor does not run compile-time code that writes files (`{path}`); build the project to run it"
                ));
            }
            return Ok(());
        }
        Err(format!("writing `{path}` while compiling needs `--comptime-write {}`", grant_hint(path)))
    }

    pub fn check_env(&self, name: &str) -> Result<(), String> {
        if self.env.iter().any(|n| n == name) {
            return self.check_online(&format!("reads environment variable `{name}`"));
        }
        Err(format!("reading environment variable `{name}` while compiling needs `--comptime-env {name}`"))
    }

    /// A TCP connection to `host:port`: granted by `host` or `host:port`.
    pub fn check_net(&self, host: &str, port: i64) -> Result<(), String> {
        let at = format!("{host}:{port}");
        if self.net.iter().any(|g| g.eq_ignore_ascii_case(host) || g.eq_ignore_ascii_case(&at)) {
            return self.check_online(&format!("connects to `{at}`"));
        }
        Err(format!("connecting to `{at}` while compiling needs `--comptime-net {at}`"))
    }

    fn check_online(&self, what: &str) -> Result<(), String> {
        if !self.offline {
            return Ok(());
        }
        if self.replay {
            return Err(format!(
                "the editor uses cached compile-time results only, and this one is not cached or is out of date: it {what} while compiling; build the project to record it"
            ));
        }
        Err(format!(
            "with `--comptime-offline` there is no cached result for this, and it {what} while compiling; build once without `--comptime-offline` to record it"
        ))
    }
}

/// The directory to grant for `path`: itself when it is one, else its
/// parent (`.` for a bare file name).
fn grant_hint(path: &str) -> String {
    let p = Path::new(path);
    if p.is_dir() {
        return path.to_string();
    }
    match p.parent().map(|d| d.to_string_lossy().into_owned()) {
        Some(d) if !d.is_empty() => format!("{d}/"),
        _ => ".".to_string(),
    }
}

fn under_any(path: &Path, roots: &[PathBuf]) -> bool {
    // A `..` left in a path that does not exist yet could climb out through
    // a symlink, so such a path is under nothing.
    !path.components().any(|c| c == std::path::Component::ParentDir) && roots.iter().any(|root| path.starts_with(root))
}

/// `path` made absolute against the working directory, with symlinks
/// resolved as far as it exists, so a link cannot lead out of a granted
/// directory. A part that does not exist yet is kept as written.
pub fn absolute_path(path: &Path) -> PathBuf {
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(path)
    };
    if let Ok(real) = joined.canonicalize() {
        return real;
    }
    // The longest prefix that exists, resolved, then the rest.
    let mut rest = Vec::new();
    let mut base = joined.as_path();
    loop {
        match (base.parent(), base.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name.to_os_string());
                base = parent;
                if let Ok(mut real) = base.canonicalize() {
                    real.extend(rest.iter().rev());
                    return real;
                }
            }
            // Ends in `..` or reached the root: leave it as written.
            _ => return joined,
        }
    }
}

/// Host natives a macro may call: pure computation only (no IO, files,
/// network, environment, clocks, threads or process control).
pub fn native_allowed_at_compile_time(name: &str) -> bool {
    matches!(
        name,
        "from_bytes" | "to_bytes" | "ord" | "char" | "hash_string" | "result_unit_probe" | "simd_axpy_reduce" | "black_box"
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

/// Drop what only the compile-time expansion program needs from a provider
/// module compiled into a program: the lowered macro functions in `decls`,
/// and its `use macro::…` items once nothing left in the module names what
/// they import. The program never calls a macro, and compiling the `macro`
/// module into it would clash with user types of the same names (`Code`,
/// `Reader`).
pub fn strip_compile_time_items(ast: &mut parser::ast::Output<'_>, source: &str, decls: &[MacroDecl]) {
    use parser::ast::Expression;
    let Expression::Program(children) = ast.1.as_mut() else {
        return;
    };
    children.retain(|c| match c.1.as_ref() {
        Expression::Function { name, .. } => !decls.iter().any(|d| d.fn_name == *name),
        _ => true,
    });
    fn macro_use(node: &parser::ast::Output<'_>) -> bool {
        matches!(node.1.as_ref(), Expression::Use { path, .. } if path.first().map(String::as_str) == Some(MACRO_MODULE))
    }
    fn imported(node: &parser::ast::Output<'_>, out: &mut Vec<String>) {
        match node.1.as_ref() {
            Expression::Use { name, alias, .. } if macro_use(node) => {
                out.push(alias.clone().unwrap_or_else(|| name.clone()));
            }
            Expression::Fragment(items) => items.iter().for_each(|i| imported(i, out)),
            _ => {}
        }
    }
    let mut names = Vec::new();
    children.iter().for_each(|c| imported(c, &mut names));
    if names.is_empty() || names.iter().any(|n| n == "*") {
        return;
    }
    let mentions = |text: &str, name: &str| {
        text.match_indices(name).any(|(at, _)| {
            let before = text[..at].chars().next_back();
            let after = text[at + name.len()..].chars().next();
            !before.is_some_and(|c| c.is_alphanumeric() || c == '_')
                && !after.is_some_and(|c| c.is_alphanumeric() || c == '_')
        })
    };
    let still_used = children.iter().any(|c| {
        let is_use = matches!(c.1.as_ref(), Expression::Use { .. } | Expression::Fragment(_));
        !is_use
            && source
                .get(c.0.into_range())
                .is_some_and(|text| names.iter().any(|n| mentions(text, n)))
    });
    if still_used {
        return;
    }
    fn drop_macro_uses(items: &mut Vec<parser::ast::Output<'_>>) {
        items.retain(|i| !macro_use(i));
        for i in items.iter_mut() {
            if let Expression::Fragment(inner) = i.1.as_mut() {
                drop_macro_uses(inner);
            }
        }
    }
    drop_macro_uses(children);
}
