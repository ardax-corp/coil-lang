//! Static contract checking for `coil-verify`: each function's HIR is run
//! symbolically ([`encode`]) and every contract check it can reach becomes
//! an SMT-LIB query that is unsatisfiable when the check can never fail.
//!
//! Ints are 64-bit bit-vectors. Int `+ - *` and negation trap on overflow,
//! so a path that goes on past one did not overflow.
//! A call to a function of the same module assumes the callee's `ensures`
//! and must establish its `requires`; any other call, a loop and a value
//! the encoder does not model (records, enums, floats) is abstracted by a
//! fresh value, which keeps a proof sound but can make a counterexample
//! spurious ([`Query::exact`]).

pub mod encode;

pub use crate::hir::Span;

/// One function's goals.
#[derive(Debug, Clone)]
pub struct FnCheck {
    pub name: String,
    pub span: Span,
    /// The parameters a counterexample shows, in declaration order.
    pub params: Vec<Param>,
    pub goals: Vec<Goal>,
}

/// A parameter as the solver sees it.
#[derive(Debug, Clone)]
pub struct Param {
    pub name: String,
    pub shape: ParamShape,
}

#[derive(Debug, Clone)]
pub enum ParamShape {
    /// An int, byte or bool: the constant itself.
    Scalar(String),
    /// A `Vec` or string: the constant holding its length.
    Seq { len: String },
    /// Not modelled.
    Opaque,
}

impl Param {
    /// The SMT constants a model is asked for.
    pub fn consts(&self) -> Option<&str> {
        match &self.shape {
            ParamShape::Scalar(c) | ParamShape::Seq { len: c } => Some(c),
            ParamShape::Opaque => None,
        }
    }
}

/// One contract clause, checked wherever the function can reach it.
#[derive(Debug, Clone)]
pub struct Goal {
    /// `requires`, `ensures`, `invariant` or `decreases`.
    pub keyword: String,
    /// The clause as written, with its message: `ensures result >= 0`.
    pub clause: String,
    /// The callee whose `requires` a call must establish.
    pub callee: Option<String>,
    pub span: Span,
    /// The clause holds when every query is unsatisfiable.
    pub queries: Vec<Query>,
}

#[derive(Debug, Clone)]
pub struct Query {
    /// A complete SMT-LIB script: `sat` means the check can fail.
    pub smt: String,
    /// No abstraction on the way to the check: a model is a real input that
    /// breaks the clause.
    pub exact: bool,
}

/// `(module path, goals)` captured so far.
type Capture = Vec<(String, Vec<FnCheck>)>;

thread_local! {
    static VERIFY_CAPTURE: std::cell::RefCell<Option<Capture>> =
        const { std::cell::RefCell::new(None) };
}

/// Start encoding the goals of each module codegen compiles.
pub fn start_verify_capture() {
    VERIFY_CAPTURE.with(|c| *c.borrow_mut() = Some(Vec::new()));
}

/// `(module path, goals)` of the entry module compiled since [`start_verify_capture`].
pub fn take_verify_capture() -> Vec<(String, Vec<FnCheck>)> {
    VERIFY_CAPTURE.with(|c| c.borrow_mut().take().unwrap_or_default())
}

pub(crate) fn capture_module(
    checker: &crate::typechecking::infer::Checker,
    sidecar: &crate::typechecking::infer::TypedSidecar,
    module_path: &str,
    ast: &parser::ast::Output<'_>,
) {
    if VERIFY_CAPTURE.with(|c| c.borrow().is_none()) {
        return;
    }
    // Only the entry file, which compiles as the unnamed module.
    if !module_path.is_empty() {
        return;
    }
    let module = crate::hir::build_module(checker, sidecar, module_path, ast);
    let checks = encode::verify_module(&module);
    VERIFY_CAPTURE.with(|c| {
        if let Some(out) = c.borrow_mut().as_mut() {
            out.push((module_path.to_string(), checks));
        }
    });
}
