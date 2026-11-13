//! High-level IR (HIR): a typed, desugared tree built after `check_program`.
//!
//! The port is phased (see the HIR plan doc). Phase 0 is [`layout`]: the one
//! query that decides how a value of a given type is represented, so HIR
//! planning and MIR cannot disagree. Phase 1 is
//! [`build`] (AST + checker facts to HIR) and [`print`] (`coil dissect
//! --hir`). Phase 2 is [`refusal`]: codegen lowers every function body from
//! HIR (the AST walk is gone since phase 8). Phase 3 widens that subset to enums, `Option` /
//! `Result` in every layout (boxed, niche, two-slot), `match`, `?`, `??` and
//! Result-mode returns.
//!
//! Each function body is an arena: nodes are [`HirExpr`]s in
//! [`HirBody::exprs`], children are [`HirId`] indices, and locals are
//! [`LocalId`] indices into [`HirBody::locals`]. Every node carries its
//! resolved type, its [`layout::Layout`], its source span and the sidecar
//! flags, so a consumer never asks the checker again.

pub mod build;
pub mod call_graph;
pub mod cold;
pub mod check;
pub mod effects;
pub mod enum_sroa;
pub mod inline;
pub mod outline;
pub mod par;
pub mod classify;
pub mod query;
pub mod refusal;
pub mod match_tree;
pub mod print;
pub mod stage;
pub mod synth;
pub mod fold;
pub mod bounds;
pub mod unroll;
pub mod sink_return;
pub mod tuple_sroa;
pub mod readonly_sroa;
pub mod tables;
pub(crate) mod stream_lint;
pub(crate) mod view_lint;

use crate::typechecking::def_id::DefId;
use crate::typechecking::id::NodeId;
use crate::typechecking::infer::ForInKind;
use crate::typechecking::ty::Ty;
use crate::layout::Layout;

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
    /// Effects declared on trait methods (`trait T { pure fn m(); }`): the
    /// bound every impl of `T::m` is checked against, and what a call that
    /// dispatches through `T` costs.
    pub trait_effects: Vec<TraitEffects>,
    /// A trait instance's method bodies by the key its calls name
    /// (`Class__Ty__method`, the instance's `method_fqns`).
    pub instance_fns: std::collections::HashMap<String, usize>,
}

/// A trait method and its declared effects (`None`: undeclared, so a call
/// through the trait has unknown effects).
#[derive(Debug, Clone)]
pub struct TraitEffects {
    pub trait_name: String,
    pub method: String,
    pub declared: Option<DeclaredEffects>,
}

/// Effects a function promises with `pure fn` or `uses {…}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclaredEffects {
    /// The `uses {…}` names (empty for `pure`).
    pub names: Vec<String>,
    /// As written: `pure` or `uses {read, write}`.
    pub text: String,
    pub span: Span,
}

/// What a [`HirBody`] was built from.
/// Optimizer hints a function states (`docs/internals/opt-hints.md`). They
/// change speed only: `#[inline]` raises the function's inline limit (best
/// effort), `#[inline(always)]` lifts it, `#[inline(never)]` keeps its
/// calls, `#[cold]` marks calls to it rare, `#[hot]` raises its limit
/// further and specializes it past the clone budget. Checked in `attrs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct OptHints {
    pub inline: bool,
    pub always: bool,
    pub never: bool,
    pub cold: bool,
    pub hot: bool,
}

/// Escape facts a function states: `#[noescape(a, b)]` names parameters
/// the call never keeps or hands back, `#[fresh]` says its result is fresh.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StatedEscape {
    pub noescape: Vec<String>,
    pub fresh: bool,
    pub span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyKind {
    Function,
    Method,
    Lambda,
    Test,
    /// Top-level statements and static initializers.
    TopLevel,
    /// One static's initializer, returning its value.
    Static,
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
    /// A parameter or result declared as a bare type parameter was typed
    /// ground: the checker unified the parameter with a ground type in the
    /// body (coil-lang#801), so its types do not hold for every instance.
    pub pinned_param: bool,
    /// Lambda captures, outer local in the parent body to inner local.
    pub captures: Vec<(LocalId, LocalId)>,
    /// `pure fn` / `uses {…}` on the function.
    pub declared: Option<DeclaredEffects>,
    /// Escape facts the function states (`#[noescape(..)]`, `#[fresh]`),
    /// checked against the inferred ones.
    pub stated_escape: Option<StatedEscape>,
    /// `#[inline]` / `#[cold]` / `#[hot]` on the function.
    pub hints: OptHints,
    /// The function's `requires` / `ensures` clauses: what runs inside one
    /// must have no visible effect.
    pub contract_spans: Vec<Span>,
    pub locals: Vec<HirLocal>,
    pub exprs: Vec<HirExpr>,
    pub root: Option<HirId>,
    /// `Global` reads of a generic function whose `let` local is only ever
    /// called: those calls were rewritten to call the function by name
    /// (`let g = wrap; g(2.5)`, #916), so the value itself is never used.
    pub aliased_fn_values: Vec<HirId>,
    /// `symbol::fn_key()` calls in a generic body (or a lambda in one), with
    /// the body's key name: the shared body panics there, and a mono clone
    /// (`Compiler::hir_instance`) writes the key of its instance instead.
    pub fn_key_sites: Vec<(HirId, String)>,
    /// `mut e` that borrows nothing (E0150): outside a `let` / `for`
    /// binding (`None`), or binding a scalar, which is copied (its type).
    pub stray_muts: Vec<(Span, Option<String>)>,
    /// The callees inlining spliced in ([`HirExpr::scope`]).
    pub inline_scopes: Vec<HirInlineScope>,
}

impl HirBody {
    pub fn expr(&self, id: HirId) -> &HirExpr {
        &self.exprs[id.0 as usize]
    }

    /// The operands of the string concatenation chain `id` heads, left to
    /// right (`a + b + c` is `[a, b, c]`): the chain is one `format`, so
    /// its partial strings are never built.
    pub fn concat_leaves(&self, id: HirId) -> Vec<HirId> {
        let mut out = Vec::new();
        let mut work = vec![id];
        while let Some(at) = work.pop() {
            match self.expr(at).kind {
                HirKind::Bin { op: BinOp::StrConcat, lhs, rhs } => {
                    work.push(rhs);
                    work.push(lhs);
                }
                _ => out.push(at),
            }
        }
        out
    }

    pub fn local(&self, id: LocalId) -> &HirLocal {
        &self.locals[id.0 as usize]
    }

    /// The direct subexpressions of `id`, in evaluation order. A lambda's
    /// body is another body, so it is not among them.
    pub fn children(&self, id: HirId) -> Children<'_> {
        let mut c = Children { head: [None; 2], rest: &[], tail: None, arms: &[] };
        match &self.expr(id).kind {
            HirKind::Field { base: a, .. }
            | HirKind::Un { operand: a, .. }
            | HirKind::Cast { value: a }
            | HirKind::Named { value: a, .. }
            | HirKind::Spread(a)
            | HirKind::LetPat { init: a, .. }
            | HirKind::Loop { body: a }
            | HirKind::Defer { body: a, .. } => c.head = [Some(*a), None],
            HirKind::Yield { value: a, zero, .. } => {
                c.head = [Some(*a), None];
                c.tail = *zero;
            }
            HirKind::Index { base: a, index: b, .. }
            | HirKind::Bin { lhs: a, rhs: b, .. }
            | HirKind::Logic { lhs: a, rhs: b, .. }
            | HirKind::Assign { place: a, value: b }
            | HirKind::Append { base: a, value: b }
            | HirKind::ForIn { iterable: a, body: b, .. } => c.head = [Some(*a), Some(*b)],
            HirKind::Call { callee, args } => {
                if let Callee::Value(v) = callee {
                    c.head = [Some(*v), None];
                }
                c.rest = args;
            }
            HirKind::Make { args, .. } | HirKind::Builtin { args, .. } => c.rest = args,
            HirKind::Block { stmts, tail } => {
                c.rest = stmts;
                c.tail = *tail;
            }
            HirKind::Let { init, .. } => c.head = [*init, None],
            HirKind::If { cond, then, els } => {
                c.head = [Some(*cond), Some(*then)];
                c.tail = *els;
            }
            HirKind::Return(v) => c.head = [*v, None],
            HirKind::Match { scrutinee, arms } => {
                c.head = [Some(*scrutinee), None];
                c.arms = arms;
            }
            HirKind::Resume { handle, value, zero } => {
                c.head = [Some(*handle), *value];
                c.tail = *zero;
            }
            HirKind::Lit(_)
            | HirKind::Local(_)
            | HirKind::Global { .. }
            | HirKind::Break
            | HirKind::Continue
            | HirKind::Lambda { .. }
            | HirKind::Clear(_)
            | HirKind::Unsupported(_) => {}
        }
        c
    }

    /// Every node of `id`'s subtree, `id` first.
    pub fn visit(&self, id: HirId, f: &mut impl FnMut(&HirExpr)) {
        self.visit_ids(id, &mut |k| f(self.expr(k)));
    }

    /// Every id of `id`'s subtree in pre-order, `id` first. A loop, not a
    /// recursion: a chain of thousands of operators is that deep.
    pub fn visit_ids(&self, id: HirId, f: &mut impl FnMut(HirId)) {
        let _ = self.any_visiting(id, &|_| false, f);
    }

    /// Whether some node of `id`'s subtree, `id` included, satisfies
    /// `pred`, in pre-order; stops at the first that does.
    pub fn any(&self, id: HirId, pred: &impl Fn(HirId) -> bool) -> bool {
        self.any_visiting(id, pred, &mut |_| {})
    }

    /// [`Self::any`], calling `seen` on each node before `pred`.
    fn any_visiting(&self, id: HirId, pred: &impl Fn(HirId) -> bool, seen: &mut impl FnMut(HirId)) -> bool {
        let mut work = vec![id];
        while let Some(at) = work.pop() {
            seen(at);
            if pred(at) {
                return true;
            }
            let first = work.len();
            work.extend(self.children(at));
            work[first..].reverse();
        }
        false
    }

    /// Each node's parent, by id (`None` for a root and for an unreached
    /// node).
    pub fn parents(&self) -> Vec<Option<HirId>> {
        let mut parent = vec![None; self.exprs.len()];
        for i in 0..self.exprs.len() {
            let id = HirId(i as u32);
            for c in self.children(id) {
                parent[c.0 as usize] = Some(id);
            }
        }
        parent
    }

    /// Append `e`; its id.
    pub fn push(&mut self, e: HirExpr) -> HirId {
        let id = HirId(self.exprs.len() as u32);
        self.exprs.push(e);
        id
    }

    /// Append a node the compiler made: one word, no AST node, no flags.
    pub fn push_new(&mut self, kind: HirKind, ty: Option<Ty>, span: Span) -> HirId {
        self.push(HirExpr { kind, ty, layout: Layout::Word, span, node: None, flags: HirFlags::default(), scope: 0, src: None })
    }

    /// A fresh compiler temp named `name`.
    pub fn temp_named(&mut self, name: String, ty: Option<Ty>) -> LocalId {
        let id = LocalId(self.locals.len() as u32);
        self.locals.push(HirLocal { name, ty, kind: LocalKind::Temp, captured: false, borrow: false });
        id
    }

    /// A fresh compiler temp named `prefix` and its id (`$free3`).
    pub fn temp(&mut self, prefix: &str, ty: Option<Ty>) -> LocalId {
        self.temp_named(format!("{prefix}{}", self.locals.len()), ty)
    }
}

/// [`HirBody::children`]: up to two leading operands, then a list, an
/// optional tail and `match` arm bodies, without allocating.
pub struct Children<'a> {
    head: [Option<HirId>; 2],
    rest: &'a [HirId],
    tail: Option<HirId>,
    arms: &'a [HirArm],
}

impl Iterator for Children<'_> {
    type Item = HirId;

    fn next(&mut self) -> Option<HirId> {
        if let Some(id) = self.head[0].take().or_else(|| self.head[1].take()) {
            return Some(id);
        }
        if let Some((&id, rest)) = self.rest.split_first() {
            self.rest = rest;
            return Some(id);
        }
        if let Some(id) = self.tail.take() {
            return Some(id);
        }
        let (arm, arms) = self.arms.split_first()?;
        self.arms = arms;
        Some(arm.body)
    }
}

impl DoubleEndedIterator for Children<'_> {
    fn next_back(&mut self) -> Option<HirId> {
        if let Some((arm, arms)) = self.arms.split_last() {
            self.arms = arms;
            return Some(arm.body);
        }
        if let Some(id) = self.tail.take() {
            return Some(id);
        }
        if let Some((&id, rest)) = self.rest.split_last() {
            self.rest = rest;
            return Some(id);
        }
        self.head[1].take().or_else(|| self.head[0].take())
    }
}

impl HirKind {
    /// A short name for the construct, as refusal reasons give it.
    pub fn name(&self) -> &'static str {
        match self {
            HirKind::Lit(_) => "literal",
            HirKind::Local(_) => "local",
            HirKind::Global { .. } => "global",
            HirKind::Field { .. } => "field",
            HirKind::Index { .. } => "index",
            HirKind::Bin { .. } => "operator",
            HirKind::Logic { .. } => "logic",
            HirKind::Un { .. } => "unary",
            HirKind::Cast { .. } => "cast",
            HirKind::Call { .. } => "call",
            HirKind::Named { .. } => "named-argument",
            HirKind::Spread(_) => "spread",
            HirKind::Make { kind, .. } => match kind {
                MakeKind::Range { .. } => "make-range",
                MakeKind::Record(_) => "make-record",
                MakeKind::Tuple => "make-unit",
                _ => "make",
            },
            HirKind::Block { .. } => "block-value",
            HirKind::Let { .. } => "let",
            HirKind::LetPat { .. } => "let-pattern",
            HirKind::Assign { .. } => "assign",
            HirKind::Append { .. } => "append",
            HirKind::If { .. } => "if-value",
            HirKind::Loop { .. } => "loop",
            HirKind::ForIn { .. } => "for-in",
            HirKind::Break | HirKind::Continue => "jump",
            HirKind::Return(_) => "return",
            HirKind::Match { .. } => "match",
            HirKind::Lambda { .. } => "lambda",
            HirKind::Yield { .. } => "yield",
            HirKind::Resume { .. } => "resume",
            HirKind::Defer { .. } => "defer",
            HirKind::Builtin { .. } => "builtin",
            HirKind::Clear(_) => "clear",
            HirKind::Unsupported(_) => "unsupported",
        }
    }
}

impl HirPat {
    /// Each local the pattern binds, left to right.
    pub fn for_each_bind(&self, f: &mut impl FnMut(LocalId)) {
        match self {
            HirPat::Bind(l) => f(*l),
            HirPat::Variant { fields: HirPatFields::Tuple(ps), .. } | HirPat::Tuple(ps) => ps.iter().for_each(|p| p.for_each_bind(f)),
            HirPat::Variant { fields: HirPatFields::Record(fs), .. } | HirPat::Record(fs) => fs.iter().for_each(|(_, p)| p.for_each_bind(f)),
            HirPat::Variant { fields: HirPatFields::Unit, .. } | HirPat::Wild | HirPat::Int(_) => {}
        }
    }

    /// Whether the pattern binds `local`.
    pub fn binds(&self, local: LocalId) -> bool {
        let mut found = false;
        self.for_each_bind(&mut |l| found |= l == local);
        found
    }
}

#[derive(Debug, Clone)]
pub struct HirLocal {
    pub name: String,
    pub ty: Option<Ty>,
    pub kind: LocalKind,
    /// A lambda or `defer` in this body captures it, so it stays one plain
    /// word in its own slot.
    pub captured: bool,
    /// Written `let x = mut place`, `for mut x in …` or `use (mut x)`: a
    /// borrow of the object, which copy-on-write writes in place.
    pub borrow: bool,
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
pub struct HirFlags(u16);

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
    /// A `from_bytes` call whose buffer nothing reads again: the string
    /// takes the buffer's bytes (`from_bytes_take`) instead of a copy.
    pub const TAKE: Self = Self(128);

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
    /// `0`: code of the body itself. `k`: inlining spliced it in from the
    /// callee [`HirBody::inline_scopes`]`[k - 1]` names (#2578).
    pub scope: u32,
    /// Where the node is in the source when `span` is not that place: a
    /// node of another module's callee spliced in takes the call's span,
    /// which the planner's facts are keyed by, and keeps its own here,
    /// in its scope's file ([`HirInlineScope::file`], #2666).
    pub src: Option<Span>,
}

/// A callee inlining spliced into a body ([`HirExpr::scope`]): its name,
/// the call it replaced and the scope that call was in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HirInlineScope {
    pub callee: String,
    /// The call's span, in the body's file.
    pub call: Span,
    /// The call's own [`HirExpr::scope`].
    pub call_scope: u32,
    /// Where the callee's result is computed: its `return` statement (the
    /// spliced result is lowered inside the caller's statement, with no
    /// statement of its own).
    pub ret: Span,
    /// The spliced nodes have the call's span (another module's callee).
    pub at_call: bool,
    /// The file of the callee's code when it is not the caller's: another
    /// module's (its nodes' [`HirExpr::src`] are in it).
    pub file: Option<u32>,
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

impl BinOp {
    /// Int arithmetic, which traps on overflow or a zero divisor.
    pub fn int_may_trap(self) -> bool {
        matches!(self, BinOp::IntAdd | BinOp::IntSub | BinOp::IntMul | BinOp::IntDiv | BinOp::IntRem | BinOp::IntPow)
    }
}

/// Whether the operator at `id` itself (not its operands) can trap: int
/// arithmetic, negating an int (`-MIN` overflows), or a shift whose count
/// is not a literal in `0..=63` (#1076).
pub fn op_may_trap(body: &HirBody, id: HirId) -> bool {
    let e = body.expr(id);
    match &e.kind {
        HirKind::Bin { op: BinOp::Shl | BinOp::Shr, rhs, .. } => {
            !matches!(body.expr(*rhs).kind, HirKind::Lit(Lit::Int(k)) if (0..64).contains(&k))
        }
        HirKind::Bin { op, .. } => op.int_may_trap(),
        HirKind::Un { op: UnOp::Neg, .. } => !e.ty.as_ref().is_some_and(|t| {
            matches!(crate::typechecking::ty::strip_readonly(t), Ty::Con(n) if matches!(n.as_str(), "float" | "f32" | "f64"))
        }),
        _ => false,
    }
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
    /// A call another module's context already planned (cross-module
    /// inlining): codegen takes its key and ABI verbatim, with no sidecar
    /// lookups, overloads or dictionaries. Analyses know nothing of the
    /// callee and treat it as an unknown function.
    Resolved(Box<ResolvedCall>),
}

/// A [`Callee::Resolved`] call.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedCall {
    /// The function table key the call goes to (for printing and
    /// diagnostics).
    pub key: String,
    /// Its plan, by index in codegen's table of resolved call plans: the
    /// ABI, receiver boxing and instance dictionaries, all of it fixed
    /// by types, none of it read from a frame or sidecar.
    pub plan: u32,
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
    Default,
    /// `PRINT` of a string (compiler-made bodies only: the test-runner
    /// `main`).
    Print,
    /// The argument's bits read as the node's type (compiler-made bodies
    /// only: a dictionary entry's argument or result). Values cross
    /// generic edges in their own representation, so this is a retype:
    /// a `float` or `bool` goes as its `i64` bits and comes back from them.
    Retype,
    /// `STRINGIFY` of a word (compiler-made bodies only: a primitive's
    /// `Show::show`). `tag` is the static scalar type (`ValueTag`) of an
    /// immediate, formatted as it is; `None` inspects the word (a box by
    /// its tag, a heap string as itself).
    Stringify { tag: Option<u32> },
    /// The same `Option` / `Result` value in its node type's layout, from
    /// its operand type's (compiler-made bodies only: a thunk between a
    /// dictionary's boxed enum and a method's niche word). A niche word
    /// boxes to its enum object; an enum object becomes its niche word.
    Relayout,
    /// Free the object a local holds, which nothing else reaches
    /// (the `FreeEarly` instruction; `deep`: and everything it reaches). Added after
    /// the HIR optimizer by the early-free rewrite only.
    FreeEarly { deep: bool },
    /// Copy-on-write: the `Cow` instruction on its operand's object, which
    /// it returns. `unique: false` marks it shared (a second place now
    /// names it); `unique: true` makes it safe to write through, copying it
    /// when shared. Added by the copy-on-write rewrite only.
    Cow { unique: bool },
    /// Copy-on-write pin of its operand's object (the `CowPin`
    /// instruction): a `bool`, whether it was pinned. While pinned, a share
    /// of it gives a copy. Added by the copy-on-write rewrite only (#2842).
    CowPin,
    /// Clear the pin of operand 0's object unless operand 1, the `bool` its
    /// `CowPin` gave, is `true` (the `CowUnpin` instruction).
    CowUnpin,
    /// Enter (`true`) or leave a `region { … }` block (escape step 5): the
    /// `gc_region_enter` / `gc_region_exit` HostInvoke. No arguments.
    Region { enter: bool },
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
    /// `zero`: the zero of the value a binding `yield` receives, made
    /// where it is resumed without `with` and the type's zero is an object
    /// ([`crate::layout::Zero::Built`], #2717).
    Yield { value: HirId, from: bool, zero: Option<HirId> },
    /// `zero`: the zero of the value, made where the `resume` completes a
    /// body that ended without `return e` and the type's zero is an object.
    Resume { handle: HirId, value: Option<HirId>, zero: Option<HirId> },
    /// `defer use (captures) { body }`: the body runs as a thunk whose
    /// frame holds the captures (`None`: a name that did not resolve).
    Defer { captures: Vec<Option<LocalId>>, body: HirId },
    Builtin { op: Builtin, args: Vec<HirId> },
    /// Zero every frame word of these locals (spliced callee locals past
    /// their last use), so the caller's frame no longer keeps their heap
    /// values alive. Reads nothing.
    Clear(Vec<LocalId>),
    /// A construct this phase does not build yet.
    Unsupported(&'static str),
}

/// Which contract clauses compile to runtime checks (`--contracts`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ContractLevel {
    /// `requires` and `ensures`.
    #[default]
    All,
    /// Only `requires`: cheap entry checks that protect a library from its
    /// callers.
    Requires,
    Off,
}

impl ContractLevel {
    pub fn parse(s: &str) -> Result<Self, String> {
        match s {
            "all" => Ok(Self::All),
            "requires" => Ok(Self::Requires),
            "off" => Ok(Self::Off),
            _ => Err(format!("invalid --contracts `{s}` (expected all|requires|off)")),
        }
    }

    pub fn checks_requires(self) -> bool {
        self != Self::Off
    }

    pub fn checks_ensures(self) -> bool {
        self == Self::All
    }
}

thread_local! {
    static CONTRACT_LEVEL: std::cell::Cell<ContractLevel> =
        const { std::cell::Cell::new(ContractLevel::All) };
}

/// The level HIR builds checks at on this thread (set per compile).
pub(crate) fn set_contract_level(level: ContractLevel) {
    CONTRACT_LEVEL.with(|c| c.set(level));
}

pub(crate) fn contract_level() -> ContractLevel {
    CONTRACT_LEVEL.with(|c| c.get())
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

/// Record `problem` with the captured HIR's, when capture is on: something
/// a later stage found wrong with a body.
pub(crate) fn capture_problem(problem: impl FnOnce() -> String) {
    HIR_CAPTURE.with(|c| {
        if let Some(out) = c.borrow_mut().as_mut() {
            out.problems.push(problem());
        }
    });
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
