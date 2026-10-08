//! High-level IR (HIR): a typed, desugared tree built after `check_program`.
//!
//! The port is phased (see the HIR plan doc). Phase 0 is [`layout`]: the one
//! query that decides how a value of a given type is represented, so the
//! AST codegen, MIR and the future HIR lowering cannot disagree. Phase 1 is
//! [`build`] (AST + checker facts to HIR) and [`print`] (`coil dissect
//! --hir`). Phase 2 is [`lower`]: by default (`--ast-codegen` / `COIL_HIR=0` opt out), codegen
//! lowers the bodies inside the core subset from HIR and keeps the AST walk
//! for every other body. Phase 3 widens that subset to enums, `Option` /
//! `Result` in every layout (boxed, niche, two-slot), `match`, `?`, `??` and
//! Result-mode returns.
//!
//! Each function body is an arena: nodes are [`HirExpr`]s in
//! [`HirBody::exprs`], children are [`HirId`] indices, and locals are
//! [`LocalId`] indices into [`HirBody::locals`]. Every node carries its
//! resolved type, its [`layout::Layout`], its source span and the sidecar
//! flags, so a consumer never asks the checker again.

pub mod build;
pub mod check;
pub mod layout;
pub mod inline;
pub mod lower;
pub mod match_tree;
pub mod print;

use crate::typechecking::def_id::DefId;
use crate::typechecking::id::NodeId;
use crate::typechecking::infer::ForInKind;
use crate::typechecking::ty::Ty;
use layout::Layout;

pub use build::build_module;

/// A node in one [`HirBody`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HirId(pub u32);

/// A local (parameter, `let`, pattern binding or capture) in one [`HirBody`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LocalId(pub u32);

/// Byte range in the module source.
pub type Span = (usize, usize);

/// Every body of one checked module.
#[derive(Debug, Default)]
pub struct HirModule {
    pub bodies: Vec<HirBody>,
}

/// What a [`HirBody`] was built from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyKind {
    Function,
    Method,
    Lambda,
    Test,
    /// Top-level statements and static initializers.
    TopLevel,
}

/// One function, method, lambda or test body.
#[derive(Debug, Clone)]
pub struct HirBody {
    pub name: String,
    pub kind: BodyKind,
    pub span: Span,
    pub params: Vec<LocalId>,
    pub ret: Option<Ty>,
    pub ret_layout: Layout,
    /// Bare `return v` was wrapped as `Return(Make Ok v)`.
    pub result_mode: bool,
    pub is_coro: bool,
    /// Generic over type parameters. Built once; a mono clone lowers a
    /// copy with the instance's types (`Compiler::hir_instance`).
    pub is_generic: bool,
    /// Lambda captures, outer local in the parent body to inner local.
    pub captures: Vec<(LocalId, LocalId)>,
    pub locals: Vec<HirLocal>,
    pub exprs: Vec<HirExpr>,
    pub root: Option<HirId>,
}

impl HirBody {
    pub fn expr(&self, id: HirId) -> &HirExpr {
        &self.exprs[id.0 as usize]
    }

    pub fn local(&self, id: LocalId) -> &HirLocal {
        &self.locals[id.0 as usize]
    }
}

#[derive(Debug, Clone)]
pub struct HirLocal {
    pub name: String,
    pub ty: Option<Ty>,
    pub kind: LocalKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalKind {
    Param,
    Let,
    Const,
    /// Bound by a `match` / `for` / destructuring pattern.
    Pattern,
    Capture,
    /// Introduced by a desugaring (`?`, `??`, `op=` on a place).
    Temp,
}

/// Sidecar facts copied onto a node.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HirFlags(u8);

impl HirFlags {
    pub const FRAME_LOCAL: Self = Self(1);
    pub const LAST_USE: Self = Self(2);
    pub const IN_BOUNDS: Self = Self(4);
    pub const NONNEG: Self = Self(8);
    /// An `Assign` built from `x++` / `--x`: its value is the old or new `x`.
    pub const ADJUST: Self = Self(16);
    /// With `ADJUST`: the prefix form, whose value is the new `x`.
    pub const PREFIX: Self = Self(32);
    /// An `Assign` from `x op= e` or `x++`: its place is also read, so
    /// the place's base and index are built twice.
    pub const COMPOUND: Self = Self(64);

    pub fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    pub fn insert(&mut self, other: Self) {
        self.0 |= other.0;
    }
}

#[derive(Debug, Clone)]
pub struct HirExpr {
    pub kind: HirKind,
    pub ty: Option<Ty>,
    pub layout: Layout,
    pub span: Span,
    /// The AST node this was built from; `None` for desugaring temps.
    pub node: Option<NodeId>,
    pub flags: HirFlags,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Lit {
    Int(i64),
    Float(f64),
    Str(String),
    Bool(bool),
    Unit,
}

/// A resolved binary operator. Arithmetic names its lane, so lowering
/// never re-derives `IntAdd` versus `StrConcat`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    IntAdd,
    IntSub,
    IntMul,
    IntDiv,
    IntRem,
    IntPow,
    FloatAdd,
    FloatSub,
    FloatMul,
    FloatDiv,
    FloatRem,
    FloatPow,
    Shl,
    Shr,
    BitAnd,
    BitOr,
    BitXor,
    StrConcat,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    /// An operator whose operand type is not a primitive lane (aggregate
    /// arithmetic, a trait operator, or an unresolved generic). The name is
    /// the source operator.
    Overloaded(&'static str),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    BitNot,
    Not,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexKind {
    Array,
    /// `s[i]` on a string: the byte at `i`.
    String,
    Dict,
    Tuple,
    Other,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Callee {
    /// A named function, static method or host native.
    Named { name: String, def: Option<DefId>, overload: Option<u32> },
    /// `recv.name(args)`: `recv` is the first entry of `args`.
    Method { name: String },
    /// A function value.
    Value(HirId),
}

#[derive(Debug, Clone, PartialEq)]
pub enum MakeKind {
    /// An enum variant; `fields` names record-variant fields in `args` order.
    Variant {
        enum_name: String,
        variant: String,
        tag: Option<u32>,
        fields: Option<Vec<String>>,
    },
    Class(String),
    Tuple,
    /// `[a, b]`.
    Array,
    /// List literal.
    List,
    Record(Vec<String>),
    Range { inclusive: bool },
}

/// A compile-time builtin form that keeps its own node until a later phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Builtin {
    Panic,
    TypeOf,
    Dload,
    Done,
    Declare,
    Invoke,
    Readonly,
    Default,
}

#[derive(Debug, Clone, PartialEq)]
pub enum HirPat {
    Wild,
    Bind(LocalId),
    Int(i64),
    Variant {
        enum_name: String,
        variant: String,
        tag: Option<u32>,
        fields: HirPatFields,
    },
    Tuple(Vec<HirPat>),
    Record(Vec<(String, HirPat)>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum HirPatFields {
    Unit,
    Tuple(Vec<HirPat>),
    Record(Vec<(String, HirPat)>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct HirArm {
    pub pat: HirPat,
    pub body: HirId,
}

#[derive(Debug, Clone, PartialEq)]
pub enum HirKind {
    Lit(Lit),
    Local(LocalId),
    /// A module-level name: function value, static, const, unit variant.
    Global { name: String, def: Option<DefId> },
    Field { base: HirId, name: String },
    Index { base: HirId, index: HirId, kind: IndexKind },
    Bin { op: BinOp, lhs: HirId, rhs: HirId },
    /// Short-circuit `&&` (`and = true`) or `||`.
    Logic { and: bool, lhs: HirId, rhs: HirId },
    Un { op: UnOp, operand: HirId },
    Cast { value: HirId },
    Call { callee: Callee, args: Vec<HirId> },
    /// `name: value` call argument.
    Named { name: String, value: HirId },
    /// `...value` call argument.
    Spread(HirId),
    Make { kind: MakeKind, args: Vec<HirId> },
    Block { stmts: Vec<HirId>, tail: Option<HirId> },
    Let { local: LocalId, init: Option<HirId> },
    /// Irrefutable destructuring `let (a, b) = init`.
    LetPat { pat: HirPat, init: HirId },
    Assign { place: HirId, value: HirId },
    /// `arr[] = v`.
    Append { base: HirId, value: HirId },
    If { cond: HirId, then: HirId, els: Option<HirId> },
    Loop { body: HirId },
    /// `for pat in iterable { body }` over the protocol the checker chose.
    ForIn {
        pat: HirPat,
        iterable: HirId,
        body: HirId,
        kind: Option<ForInKind>,
    },
    Break,
    Continue,
    Return(Option<HirId>),
    Match { scrutinee: HirId, arms: Vec<HirArm> },
    /// A lambda: the index of its body in [`HirModule::bodies`].
    Lambda { body: usize },
    Yield { value: HirId, from: bool },
    Resume { handle: HirId, value: Option<HirId> },
    /// `defer use (captures) { body }`: the body runs as a thunk whose
    /// frame holds the captures (`None`: a name that did not resolve).
    Defer { captures: Vec<Option<LocalId>>, body: HirId },
    Builtin { op: Builtin, args: Vec<HirId> },
    /// A construct this phase does not build yet.
    Unsupported(&'static str),
}

/// HIR lowering is on unless `COIL_HIR=0` (or `false` / `off` / `no`)
/// picks the AST codegen.
pub(crate) fn lowering_from_env() -> bool {
    !matches!(
        std::env::var("COIL_HIR").as_deref(),
        Ok("0" | "false" | "off" | "no")
    )
}

thread_local! {
    static HIR_CAPTURE: std::cell::RefCell<Option<HirCapture>> =
        const { std::cell::RefCell::new(None) };
}

/// HIR recorded by [`start_hir_capture`].
#[derive(Debug, Default)]
pub struct HirCapture {
    /// `(body name, printed HIR)` per body, in compile order.
    pub bodies: Vec<(String, String)>,
    /// [`check::problems`] of every captured module.
    pub problems: Vec<String>,
}

/// Start recording the HIR of each module codegen compiles (`coil dissect --hir`).
pub fn start_hir_capture() {
    HIR_CAPTURE.with(|c| *c.borrow_mut() = Some(HirCapture::default()));
}

/// Everything recorded since [`start_hir_capture`].
pub fn take_hir_capture() -> HirCapture {
    HIR_CAPTURE.with(|c| c.borrow_mut().take().unwrap_or_default())
}

pub(crate) fn hir_capture_active() -> bool {
    HIR_CAPTURE.with(|c| c.borrow().is_some())
}

/// Build `ast`'s HIR and record it, when capture is on.
pub(crate) fn capture_module(
    checker: &crate::typechecking::infer::Checker,
    sidecar: &crate::typechecking::infer::TypedSidecar,
    module_path: &str,
    ast: &parser::ast::Output<'_>,
) {
    if !hir_capture_active() {
        return;
    }
    let module = build_module(checker, sidecar, module_path, ast);
    let printed: Vec<(String, String)> = module
        .bodies
        .iter()
        .map(|b| (b.name.clone(), print::body_to_string(&module, b)))
        .collect();
    let problems = check::problems(&module, checker);
    HIR_CAPTURE.with(|c| {
        if let Some(out) = c.borrow_mut().as_mut() {
            out.bodies.extend(printed);
            out.problems.extend(problems);
        }
    });
}
