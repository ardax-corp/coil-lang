use std::{borrow::Borrow, fmt::Display};

use chumsky::span::SimpleSpan;
pub type Output<'parser> = (SimpleSpan, Box<Expression<'parser>>);

/// Kind of a type parameter.
///
/// - [`Kind::Type`] (`*`) — ordinary type parameter (`T`, `A`)
/// - [`Kind::Constraint`] — typeclass predicates
/// - [`Kind::Arrow`] — type constructor arrows such as `* -> * -> *`
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub enum Kind {
    /// `*` — a proper type.
    #[default]
    Type,
    /// `Constraint` — a typeclass predicate.
    Constraint,
    /// `domain -> codomain` — a type constructor kind.
    Arrow(Box<Kind>, Box<Kind>),
}

impl std::fmt::Display for Kind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Kind::Type => write!(f, "*"),
            Kind::Constraint => write!(f, "Constraint"),
            Kind::Arrow(domain, codomain) => {
                match domain.as_ref() {
                    Kind::Arrow(_, _) => write!(f, "({})", domain)?,
                    Kind::Type | Kind::Constraint => write!(f, "{}", domain)?,
                }
                write!(f, " -> {}", codomain)
            }
        }
    }
}

/// A generic type parameter with optional bounds and/or an explicit kind.
///
/// `T` → `TypeParam { name: "T", bounds: [], kind: Type }`
/// `T: Num + Eq` → `TypeParam { name: "T", bounds: ["Num", "Eq"], kind: Type }`
/// `F: * -> *` → `TypeParam { name: "F", bounds: [], kind: Arrow(Type, Type) }`
///
/// After `:`, a parameter takes class bounds (`Num + Eq`), a kind annotation
/// (`* -> *`), or a kind annotation followed by class bounds
/// (`F: * -> *, Container`).
#[derive(Clone, PartialEq, Debug)]
pub struct TypeParam<'expr> {
    pub name: &'expr str,
    /// Bound class names, e.g. `["Num", "Eq"]` for `T: Num + Eq`.
    pub bounds: Vec<&'expr str>,
    /// Explicit kind; defaults to [`Kind::Type`].
    pub kind: Kind,
}

/// A `where` clause constraint: `Convert<A, B>` or unary `Num<T>`.
#[derive(Clone, PartialEq, Debug)]
pub struct WhereConstraint<'expr> {
    pub class: &'expr str,
    pub args: Vec<Output<'expr>>,
}

impl<'a> Display for WhereConstraint<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.class)?;
        if !self.args.is_empty() {
            write!(f, "<")?;
            for (i, arg) in self.args.iter().enumerate() {
                if i > 0 {
                    write!(f, ", ")?;
                }
                write!(f, "{}", arg.1)?;
            }
            write!(f, ">")?;
        }
        Ok(())
    }
}

/// Compound assignment operator (`+=`, `-=`, …).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AssignOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Pow,
    Shl,
    Shr,
    BitAnd,
    BitOr,
    BitXor,
}

/// Prefix/postfix increment or decrement.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AdjustOp {
    Inc,
    Dec,
}

#[derive(Clone, PartialEq, Debug, Copy, Default)]
pub enum Visibility {
    #[default]
    Private,
    Public,
}

/// Class field modifier: instance (default), `const`, or `static`.
#[derive(Clone, PartialEq, Eq, Debug, Copy, Default)]
pub enum FieldModifier {
    #[default]
    Instance,
    Const,
    Static,
}

/// One `#[name]` / `#[name(args)]` attribute on a declaration.
#[derive(Clone, PartialEq, Debug)]
pub struct Attribute<'expr> {
    pub name: &'expr str,
    pub args: AttrArgs<'expr>,
}

/// Argument shapes for attributes.
#[derive(Clone, PartialEq, Debug)]
pub enum AttrArgs<'expr> {
    /// `#[test]` — no parentheses.
    Empty,
    /// `#[derive(Show, Eq)]` — comma-separated identifiers.
    Idents(Vec<&'expr str>),
    /// `#[ffi(lib = "c", name = "sym")]` — key/value pairs.
    KeyValues(Vec<(&'expr str, AttrLit<'expr>)>),
    /// `#[log("msg")]` / `#[retry(3, times = 2)]` — positional literals.
    Positional(Vec<AttrLit<'expr>>),
    /// `#[test("description")]` — single string literal.
    String(&'expr str),
}

/// Literal values in attribute key/value pairs.
#[derive(Clone, PartialEq, Debug)]
pub enum AttrLit<'expr> {
    String(&'expr str),
    Int(i64),
    Float(f64),
    Bool(bool),
}

#[derive(Clone, PartialEq, Debug)]
pub enum Expression<'expr> {
    Noop(Output<'expr>),
    Integer(i64),
    Float(f64),
    String(&'expr str),
    Bool(bool),
    Module(String, Output<'expr>),

    /// Function parameter `T name`, homogeneous rest `T... name`, or tuple
    /// rest `... name` (`ty` is `None` for bare tuple rest).
    Argument {
        /// Leading `///` documentation lines for this parameter.
        docs: Vec<&'expr str>,
        ty: Option<Output<'expr>>,
        name: &'expr str,
        is_rest: bool,
    },

    /// Call-site spread: `f(...expr)`.
    Spread(Output<'expr>),

    /// Function type in annotations: `fn(T x, ...args) -> R`.
    TypeFnSig {
        params: Output<'expr>,
        ret: Output<'expr>,
    },

    /// User-defined attribute declaration.
    AttrDecl {
        /// Leading `///` doc lines (without the `///` prefix).
        docs: Vec<&'expr str>,
        name: &'expr str,
        type_params: Vec<TypeParam<'expr>>,
        args: Output<'expr>,
        returns: Option<Output<'expr>>,
        where_constraints: Vec<WhereConstraint<'expr>>,
        body: Output<'expr>,
    },
    Identifier(&'expr str),
    Type(&'expr str),
    /// Generic type application in annotations: `Option<int>`, `Result<int, string>`.
    TypeApp {
        name: &'expr str,
        args: Vec<Output<'expr>>,
    },
    /// Associated-type projection in annotations: `Collect::Elem`, `C::Elem`.
    TypeProjection {
        owner: &'expr str,
        name: &'expr str,
        args: Vec<Output<'expr>>,
    },
    /// Function type annotation `A -> B`.
    TypeFun(Output<'expr>, Output<'expr>),
    /// Line comment `// …` (body without the `//` prefix).
    Comment(&'expr str),
    Return(Output<'expr>),
    ImplicitReturn(Output<'expr>),
    /// `raise expr` — early-return `Err(expr)` from a Result-mode function.
    Raise(Output<'expr>),
    /// `panic expr` — abort the program with a string message.
    Panic(Output<'expr>),
    Yield(Output<'expr>),
    YieldFrom(Output<'expr>),
    Resume(Output<'expr>, Option<Output<'expr>>),
    /// Postfix `expr?` — propagate `Err`/`None` from Result/Option.
    Try(Output<'expr>),
    /// `lhs ?? rhs` — coalesce: Some/Ok unwrap, None/Err → rhs.
    Coalesce(Output<'expr>, Output<'expr>),
    /// `expr as Ty` — primitive cast (`int` / `float` / `byte` / `bool`).
    Cast(Output<'expr>, Output<'expr>),
    /// `typeof expr` — compile-time fully-qualified type name as `string`.
    TypeOf(Output<'expr>),
    /// `expr?.field` — optional field access on `Option`.
    OptionalAccess(Output<'expr>, &'expr str),
    Negate(Output<'expr>),
    Not(Output<'expr>),
    LogicalNot(Output<'expr>),
    Positive(Output<'expr>),
    Default(&'expr str),
    /// `target += rhs` and related compound assignments.
    CompoundAssign(Output<'expr>, AssignOp, Output<'expr>),
    /// Prefix/postfix `++` / `--`.
    Adjust {
        op: AdjustOp,
        prefix: bool,
        target: Output<'expr>,
    },
    Add(Output<'expr>, Output<'expr>),
    Sub(Output<'expr>, Output<'expr>),
    Mul(Output<'expr>, Output<'expr>),
    Div(Output<'expr>, Output<'expr>),
    Mod(Output<'expr>, Output<'expr>),
    Pow(Output<'expr>, Output<'expr>),
    Shl(Output<'expr>, Output<'expr>),
    Shr(Output<'expr>, Output<'expr>),
    Xor(Output<'expr>, Output<'expr>),
    And(Output<'expr>, Output<'expr>),
    BitAnd(Output<'expr>, Output<'expr>),
    Or(Output<'expr>, Output<'expr>),
    BitOr(Output<'expr>, Output<'expr>),
    Eq(Output<'expr>, Output<'expr>),
    Neq(Output<'expr>, Output<'expr>),
    Leq(Output<'expr>, Output<'expr>),
    Geq(Output<'expr>, Output<'expr>),
    Le(Output<'expr>, Output<'expr>),
    Gt(Output<'expr>, Output<'expr>),

    /// Lazy `int`/`byte` range: `start..end` (half-open) or `start..=end` (closed).
    Range {
        start: Output<'expr>,
        end: Output<'expr>,
        inclusive: bool,
    },

    List(Vec<Output<'expr>>),
    Array(Vec<Output<'expr>>),
    Expr(Output<'expr>),
    Group(Output<'expr>),
    ExprStatement(Output<'expr>),
    Statement(Output<'expr>),
    Fragment(Vec<Output<'expr>>),
    Block(Vec<Output<'expr>>),
    Program(Vec<Output<'expr>>),
    /// `defer { … }` or `defer use (a, b) { … }` — runs on enclosing function exit.
    ///
    /// - `captures` — variable names from the optional `use (a, b)` list (same
    ///   explicit-capture rules as [`Self::Lambda`]).
    /// - `body` — the deferred block.
    Defer {
        captures: Vec<&'expr str>,
        body: Output<'expr>,
    },

    Assignment(Output<'expr>, Output<'expr>),

    /// Tuple literal or FFI arg-type / arg-value bundle.
    Tuple(Vec<Output<'expr>>),

    /// Anonymous record literal `{ name: expr, ... }`.
    Dict(Vec<RecordFieldValue<'expr>>),

    /// Element access `target[index]`; `index = None` means append LHS (`arr[] = v`).
    Index(Output<'expr>, Option<Output<'expr>>),

    /// `readonly expr` — sealed against external mutation.
    Readonly(Output<'expr>),

    /// `ClassName::member` — static field or method reference.
    QualifiedAccess {
        owner: &'expr str,
        member: &'expr str,
    },

    /// Top-level `static let` / `static const` singleton binding.
    StaticDecl {
        is_const: bool,
        name: &'expr str,
        ty: Option<Output<'expr>>,
        init: Output<'expr>,
    },

    /// Dynamic library load: `dload(path)`.
    Dload(Output<'expr>),

    /// Coroutine completion check: `done(handle)`.
    Done(Output<'expr>),

    /// Runtime FFI registration: `declare(lib, name, args_tuple, ret_type)`.
    Declare(Vec<Output<'expr>>),

    /// Runtime FFI call: `invoke(lib, fn_id, args_tuple)`.
    Invoke(Vec<Output<'expr>>),

    Use {
        path: Vec<String>,
        name: String,
        alias: Option<String>,
    },

    /// `extern "libname" { fn name(args) -> ret; ... }`.
    ExternBlock {
        library: String,
        declarations: Vec<ExternFunction<'expr>>,
    },

    Function {
        /// Leading `///` doc lines (without the `///` prefix).
        docs: Vec<&'expr str>,
        attrs: Vec<Attribute<'expr>>,
        name: &'expr str,
        is_coro: bool,
        /// `static fn` inside an `impl` block.
        is_static: bool,
        type_params: Vec<TypeParam<'expr>>,
        args: Output<'expr>,
        returns: Option<Output<'expr>>,
        /// Constraints from a trailing `where` clause (after returns).
        where_constraints: Vec<WhereConstraint<'expr>>,
        /// `None` = signature-only (`fn f(...) -> T;`); `Some` = block body.
        body: Option<Output<'expr>>,
    },

    Branch(Option<Output<'expr>>, Output<'expr>),

    If(Vec<Output<'expr>>),

    Call {
        name: Output<'expr>,
        args: Option<Vec<Output<'expr>>>,
    },

    /// Named call-site argument: `name: value` (Phase P2).
    ///
    /// Only valid inside [`Call`] argument lists (and other `params()`
    /// sites that reuse the same parser). Display renders as `name: value`.
    NamedArg(&'expr str, Output<'expr>),

    Break,
    Continue,

    Loop {
        identifier: Option<Output<'expr>>,
        iterable: Output<'expr>,
        body: Output<'expr>,
    },

    Variable(&'expr str, Option<Output<'expr>>),
    Constant(Output<'expr>, Option<Output<'expr>>),

    /// `let (a, b) = expr;` / `let { x, y } = expr;` — irrefutable
    /// destructuring (Phase P1). Distinct from match [`Pattern`] so
    /// enum constructors stay match-only.
    LetDestructure {
        pattern: LetPattern<'expr>,
        rhs: Output<'expr>,
    },

    Class {
        /// Leading `///` doc lines (without the `///` prefix).
        docs: Vec<&'expr str>,
        attrs: Vec<Attribute<'expr>>,
        name: &'expr str,
        type_params: Vec<TypeParam<'expr>>,
        fields: Vec<Output<'expr>>,
    },
    Implementation {
        /// Unused trait slot (`""` for inherent impls).
        what: &'expr str,
        owner: &'expr str,
        type_params: Vec<TypeParam<'expr>>,
        methods: Vec<Output<'expr>>,
    },
    Field {
        /// Leading `///` doc lines (without the `///` prefix).
        docs: Vec<&'expr str>,
        visibility: Visibility,
        modifier: FieldModifier,
        name: Output<'expr>,
        ty: Output<'expr>,
        /// Required initializer for `static` fields.
        init: Option<Output<'expr>>,
    },
    /// Method in an `impl` / trait body. Docs live on the inner [`Function`].
    Method(Visibility, Output<'expr>),
    Member(Output<'expr>),
    Access(Output<'expr>, &'expr str),

    Instantiate(Output<'expr>, Option<Vec<Output<'expr>>>),

    /// `type Name = T;` — type alias declaration.
    TypeAlias {
        /// Leading `///` doc lines (without the `///` prefix).
        docs: Vec<&'expr str>,
        name: &'expr str,
        type_params: Vec<TypeParam<'expr>>,
        ty: Box<Output<'expr>>,
    },

    /// Top-level `test("description") { … }` case for `coil test`.
    ///
    /// The name expression should be a string literal; the body is a block
    /// typechecked in Result mode (`Result<(), string>`).
    TestCase {
        name: Output<'expr>,
        body: Output<'expr>,
    },

    /// Top-level `enum` declaration.
    EnumDecl {
        /// Leading `///` doc lines (without the `///` prefix).
        docs: Vec<&'expr str>,
        attrs: Vec<Attribute<'expr>>,
        name: &'expr str,
        type_params: Vec<TypeParam<'expr>>,
        variants: Vec<Output<'expr>>,
    },

    /// `extern struct Name { field: type, ... };` — C-layout FFI struct.
    ExternStruct(ExternStructDecl<'expr>),

    /// One variant inside an `enum` body.
    EnumVariant {
        /// Leading `///` doc lines (without the `///` prefix).
        docs: Vec<&'expr str>,
        name: &'expr str,
        payload: EnumVariantPayload<'expr>,
        /// Optional `= lit` scalar discriminant (`Ok = 200`).
        discriminant: Option<Output<'expr>>,
    },
    /// Qualified constructor application `EnumName::Variant(...)`.
    Construct {
        enum_name: &'expr str,
        variant_name: &'expr str,
        fields: EnumConstructPayload<'expr>,
    },
    /// Pattern match expression.
    Match {
        scrutinee: Output<'expr>,
        arms: Vec<MatchArm<'expr>>,
    },

    /// `forall T. T` / `forall T: Num + Eq, U. (T, U)` in type annotations.
    Forall {
        params: Vec<TypeParam<'expr>>,
        ty: Box<Output<'expr>>,
    },

    /// `trait Name<T> { type Elem; fn ...; fn ... { default } }`
    TypeClass {
        /// Leading `///` doc lines (without the `///` prefix).
        docs: Vec<&'expr str>,
        name: &'expr str,
        type_params: Vec<TypeParam<'expr>>,
        /// Body items: `AssocTypeDecl`, and `Function` nodes (empty `Block` = sig-only).
        methods: Vec<Output<'expr>>,
    },

    /// `impl Num<int> { … }` or `impl Show for Point { … }` — trait instance.
    ///
    /// For the `impl Trait<A, B> for T` form, `args` is `[T, A, B]` (Self first).
    TypeClassImpl {
        class: &'expr str,
        /// Type annotations for the class type arguments, e.g. `[int]`.
        args: Vec<Output<'expr>>,
        /// Body items: `AssocTypeDef` and method `Function`/`Method` nodes.
        methods: Vec<Output<'expr>>,
    },

    /// Associated type declaration inside a trait: `type Elem;` or `type Ref<T>;`.
    AssocTypeDecl {
        name: &'expr str,
        type_params: Vec<TypeParam<'expr>>,
    },

    /// Associated type definition inside an impl: `type Elem = int;` or `type Ref<T> = T;`.
    AssocTypeDef {
        name: &'expr str,
        type_params: Vec<TypeParam<'expr>>,
        ty: Box<Output<'expr>>,
    },

    /// Anonymous function: `fn (T x) use (y) => expr` or `fn (T x) { block }`.
    ///
    /// - `args` — a `Fragment` of `Argument` nodes (same layout as `Function`).
    /// - `captures` — variable names from the optional `use (a, b)` capture list.
    /// - `body` — the function body: either a short expression (after `=>`) or
    ///   a `Block`.
    Lambda {
        args: Output<'expr>,
        captures: Vec<&'expr str>,
        body: Output<'expr>,
    },
}

/// Payload shape for an `enum` variant declaration.
#[derive(Clone, PartialEq, Debug)]
pub enum EnumVariantPayload<'expr> {
    /// No fields: `Foo`.
    Unit,
    /// Tuple of typed expressions (each `Output` is typically an
    /// `Expression::Type`): `Foo(T1, T2, ...)`.
    Tuple(Vec<Output<'expr>>),
    /// Record of named typed fields: `Foo { x: T, y: T }`.
    Record(Vec<RecordFieldDecl<'expr>>),
}

/// One typed field in an `enum` record variant declaration.
#[derive(Clone, PartialEq, Debug)]
pub struct RecordFieldDecl<'expr> {
    pub name: &'expr str,
    pub value: Output<'expr>,
}

/// One function declaration inside an `ExternBlock`.
#[derive(Clone, PartialEq, Debug)]
pub struct ExternFunction<'expr> {
    pub name: &'expr str,
    /// Optional C symbol when it differs from the Zero Script name (`#[ffi(name = "sym")]`).
    pub symbol: Option<&'expr str>,
    pub args: Output<'expr>,
    pub returns: Option<Output<'expr>>,
    /// C-style varargs (`fn printf(string fmt, ...)`) — bare `...`, not language `T... xs`.
    pub variadic: bool,
}

/// C-layout struct for FFI: `extern struct Name { field: type, ... }`.
#[derive(Clone, PartialEq, Debug)]
pub struct ExternStructDecl<'expr> {
    pub name: &'expr str,
    pub fields: Vec<(String, Output<'expr>)>,
}

/// One field in a record constructor.
#[derive(Clone, PartialEq, Debug)]
pub struct RecordFieldValue<'expr> {
    pub name: &'expr str,
    pub value: Output<'expr>,
}

/// One field in a record pattern. Shorthand `x` desugars to `x: x`.
#[derive(Clone, PartialEq, Debug)]
pub struct PatternField<'expr> {
    pub name: &'expr str,
    pub pattern: (SimpleSpan, Pattern<'expr>),
}

/// Payload shape for a qualified constructor application.
#[derive(Clone, PartialEq, Debug)]
pub enum EnumConstructPayload<'expr> {
    /// `Foo` (no fields — bare qualified enum value).
    Unit,
    /// `Foo(arg1, arg2, ...)`.
    Tuple(Vec<Output<'expr>>),
    /// `Foo { name: expr, ... }` — fields may appear in any order.
    Record(Vec<RecordFieldValue<'expr>>),
}

/// One arm inside a `match` expression.
#[derive(Clone, PartialEq, Debug)]
pub struct MatchArm<'expr> {
    pub pattern: (SimpleSpan, Pattern<'expr>),
    pub body: Output<'expr>,
}

/// Irrefutable pattern for `let` destructuring (`let (a, b) = …`,
/// `let { x, y } = …`). Nested tuples/records and `_` wildcards are
/// allowed; enum constructors are not (use `match`).
#[derive(Clone, PartialEq, Debug)]
pub enum LetPattern<'expr> {
    Wildcard,
    Binding { name: &'expr str },
    Tuple(Vec<LetPattern<'expr>>),
    Record(Vec<LetFieldPattern<'expr>>),
}

/// One field in a [`LetPattern::Record`]. Shorthand `x` desugars to
/// `x: Binding(x)`.
#[derive(Clone, PartialEq, Debug)]
pub struct LetFieldPattern<'expr> {
    pub name: &'expr str,
    pub pattern: LetPattern<'expr>,
}

/// Match pattern: wildcard, binding, or qualified constructor.
#[derive(Clone, PartialEq, Debug)]
pub enum Pattern<'expr> {
    Wildcard,
    /// `default =>` match-arm catch-all (same coverage as `_`).
    Default,
    Binding {
        name: &'expr str,
    },
    Constructor {
        enum_name: &'expr str,
        variant_name: &'expr str,
        payload: PatternPayload<'expr>,
    },
}

/// Payload shape for a constructor pattern.
#[derive(Clone, PartialEq, Debug)]
pub enum PatternPayload<'expr> {
    /// `Foo` (no sub-patterns — bare qualified enum pattern).
    Unit,
    /// `Foo(p1, p2, ...)`.
    Tuple(Vec<(SimpleSpan, Pattern<'expr>)>),
    /// `Foo { name (shorthand) or name: pattern, ... }`.
    /// Shorthand `x` desugars at parse time to
    /// `PatternField { name: "x", pattern: Binding("x") }`.
    Record(Vec<PatternField<'expr>>),
}

pub type PatternOutput<'expr> = (SimpleSpan, Pattern<'expr>);

/// Synthetic match patterns (attribute expansion, etc.) with a dummy span.
pub fn spanned_pattern<'expr>(pattern: Pattern<'expr>) -> PatternOutput<'expr> {
    (SimpleSpan::from(0..0), pattern)
}

fn fmt_attrs(attrs: &[Attribute<'_>]) -> String {
    if attrs.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    for attr in attrs {
        out.push_str(&format!("{}\n", attr));
    }
    out
}

fn fmt_attr_lit<'a>(lit: &AttrLit<'a>) -> String {
    match lit {
        AttrLit::String(s) => format!("\"{}\"", s),
        AttrLit::Int(i) => i.to_string(),
        AttrLit::Float(v) => v.to_string(),
        AttrLit::Bool(b) => b.to_string(),
    }
}

impl<'a> Display for Attribute<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "#[{}", self.name)?;
        match &self.args {
            AttrArgs::Empty => {}
            AttrArgs::Idents(idents) => {
                write!(f, "({})", idents.join(", "))?;
            }
            AttrArgs::KeyValues(kvs) => {
                let parts: Vec<String> = kvs
                    .iter()
                    .map(|(k, v)| match v {
                        AttrLit::String(s) => format!("{} = \"{}\"", k, s),
                        AttrLit::Int(i) => format!("{} = {}", k, i),
                        AttrLit::Float(v) => format!("{} = {}", k, v),
                        AttrLit::Bool(b) => format!("{} = {}", k, b),
                    })
                    .collect();
                write!(f, "({})", parts.join(", "))?;
            }
            AttrArgs::Positional(lits) => {
                let parts: Vec<String> = lits.iter().map(fmt_attr_lit).collect();
                write!(f, "({})", parts.join(", "))?;
            }
            AttrArgs::String(s) => write!(f, "(\"{}\")", s)?,
        }
        write!(f, "]")
    }
}

/// Description for `#[test]` / `#[test("desc")]` on a function declaration.
pub fn attr_test_desc<'expr>(attrs: &[Attribute<'expr>], fn_name: &str) -> Option<String> {
    let attr = attrs.iter().find(|a| a.name == "test")?;
    let desc = match &attr.args {
        AttrArgs::String(s) => (*s).to_string(),
        AttrArgs::Positional(lits) if lits.len() == 1 => match &lits[0] {
            AttrLit::String(s) => (*s).to_string(),
            AttrLit::Int(n) => n.to_string(),
            AttrLit::Float(f) => f.to_string(),
            AttrLit::Bool(b) => b.to_string(),
        },
        AttrArgs::Empty => fn_name.to_string(),
        _ => fn_name.to_string(),
    };
    Some(desc)
}

/// Attached `///` documentation lines for a declaration node, if any.
///
/// For [`Expression::Method`], docs are taken from the wrapped [`Expression::Function`].
pub fn item_docs<'expr>(expr: &'expr Expression<'expr>) -> Option<&'expr [&'expr str]> {
    let docs = match expr {
        Expression::Function { docs, .. }
        | Expression::Class { docs, .. }
        | Expression::Field { docs, .. }
        | Expression::TypeAlias { docs, .. }
        | Expression::EnumDecl { docs, .. }
        | Expression::EnumVariant { docs, .. }
        | Expression::TypeClass { docs, .. }
        | Expression::AttrDecl { docs, .. } => docs.as_slice(),
        Expression::Method(_, inner) => return item_docs(inner.1.as_ref()),
        _ => return None,
    };
    if docs.is_empty() {
        None
    } else {
        Some(docs)
    }
}

fn fmt_docs(docs: &[&str]) -> String {
    if docs.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    for line in docs {
        out.push_str("///");
        if !line.is_empty() {
            out.push(' ');
            out.push_str(line);
        }
        out.push('\n');
    }
    out
}

/// Format a `Vec<TypeParam>` as `<T, U: Num + Eq, F: * -> *>`.
fn fmt_type_params(params: &[TypeParam<'_>]) -> String {
    if params.is_empty() {
        return String::new();
    }
    let inner: Vec<String> = params.iter().map(|p| p.to_string()).collect();
    format!("<{}>", inner.join(", "))
}

impl<'a> Display for TypeParam<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.kind != Kind::Type {
            write!(f, "{}: {}", self.name, self.kind)?;
            if !self.bounds.is_empty() {
                write!(f, ", {}", self.bounds.join(" + "))?;
            }
            Ok(())
        } else if !self.bounds.is_empty() {
            write!(f, "{}: {}", self.name, self.bounds.join(" + "))
        } else {
            write!(f, "{}", self.name)
        }
    }
}

impl<'a> Display for LetPattern<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Wildcard => write!(f, "_"),
            Self::Binding { name } => write!(f, "{}", name),
            Self::Tuple(parts) => write!(
                f,
                "({})",
                parts
                    .iter()
                    .map(|p| p.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Self::Record(fields) => {
                let parts: Vec<String> = fields
                    .iter()
                    .map(|pf| match &pf.pattern {
                        LetPattern::Binding { name } if *name == pf.name => name.to_string(),
                        _ => format!("{}: {}", pf.name, pf.pattern),
                    })
                    .collect();
                write!(f, "{{ {} }}", parts.join(", "))
            }
        }
    }
}

impl<'a> Display for Pattern<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Wildcard => write!(f, "_"),
            Self::Default => write!(f, "default"),
            Self::Binding { name } => write!(f, "{}", name),
            Self::Constructor {
                enum_name,
                variant_name,
                payload,
            } => {
                write!(f, "{}::{}", enum_name, variant_name)?;
                match payload {
                    PatternPayload::Unit => Ok(()),
                    PatternPayload::Tuple(parts) => {
                        if !parts.is_empty() {
                            write!(
                                f,
                                "({})",
                                parts
                                    .iter()
                                    .map(|p| p.1.to_string())
                                    .collect::<Vec<String>>()
                                    .join(", ")
                            )?;
                        }
                        Ok(())
                    }
                    PatternPayload::Record(fields) => {
                        let parts: Vec<String> = fields
                            .iter()
                            .map(|pf| match &pf.pattern.1 {
                                // Shorthand `x`: render as just `x`.
                                Pattern::Binding { name } if *name == pf.name => name.to_string(),
                                _ => format!("{}: {}", pf.name, pf.pattern.1),
                            })
                            .collect();
                        write!(f, "{{ {} }}", parts.join(", "))
                    }
                }
            }
        }
    }
}

impl<'a> Display for Expression<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Integer(n) => write!(f, "{}", n),
            Self::Float(n) => write!(f, "{:.?}", n),
            Self::Bool(b) => write!(f, "{}", if *b { "true" } else { "false" }),
            Self::Identifier(id) => write!(f, "{}", id),
            Self::Not(n) => write!(f, "~{}", n.1),
            Self::LogicalNot(n) => write!(f, "!{}", n.1),
            Self::Sub(lhs, rhs) => write!(f, "{} - {}", lhs.borrow().1, rhs.borrow().1),
            Self::Add(lhs, rhs) => write!(f, "{} + {}", lhs.borrow().1, rhs.borrow().1),
            Self::Mul(lhs, rhs) => write!(f, "{} * {}", lhs.borrow().1, rhs.borrow().1),
            Self::Div(lhs, rhs) => write!(f, "{} / {}", lhs.borrow().1, rhs.borrow().1),
            Self::Mod(lhs, rhs) => write!(f, "{} % {}", lhs.borrow().1, rhs.borrow().1),
            Self::Shl(lhs, rhs) => write!(f, "{} << {}", lhs.borrow().1, rhs.borrow().1),
            Self::Shr(lhs, rhs) => write!(f, "{} >> {}", lhs.borrow().1, rhs.borrow().1),
            Self::BitOr(lhs, rhs) => write!(f, "{} | {}", lhs.borrow().1, rhs.borrow().1),
            Self::Or(lhs, rhs) => write!(f, "{} || {}", lhs.borrow().1, rhs.borrow().1),
            Self::And(lhs, rhs) => write!(f, "{} && {}", lhs.borrow().1, rhs.borrow().1),
            Self::BitAnd(lhs, rhs) => write!(f, "{} & {}", lhs.borrow().1, rhs.borrow().1),
            Self::Xor(lhs, rhs) => write!(f, "{} ^ {}", lhs.borrow().1, rhs.borrow().1),
            Self::Pow(lhs, rhs) => write!(f, "{} ** {}", lhs.borrow().1, rhs.borrow().1),
            Self::Gt(lhs, rhs) => write!(f, "{} > {}", lhs.borrow().1, rhs.borrow().1),
            Self::Le(lhs, rhs) => write!(f, "{} < {}", lhs.borrow().1, rhs.borrow().1),
            Self::Eq(lhs, rhs) => write!(f, "{} == {}", lhs.borrow().1, rhs.borrow().1),
            Self::Neq(lhs, rhs) => write!(f, "{} != {}", lhs.borrow().1, rhs.borrow().1),
            Self::Range {
                start,
                end,
                inclusive,
            } => {
                if *inclusive {
                    write!(f, "{}..={}", start.borrow().1, end.borrow().1)
                } else {
                    write!(f, "{}..{}", start.borrow().1, end.borrow().1)
                }
            }
            Self::CompoundAssign(lhs, op, rhs) => {
                let sym = match op {
                    AssignOp::Add => "+=",
                    AssignOp::Sub => "-=",
                    AssignOp::Mul => "*=",
                    AssignOp::Div => "/=",
                    AssignOp::Mod => "%=",
                    AssignOp::Pow => "**=",
                    AssignOp::Shl => "<<=",
                    AssignOp::Shr => ">>=",
                    AssignOp::BitAnd => "&=",
                    AssignOp::BitOr => "|=",
                    AssignOp::BitXor => "^=",
                };
                write!(f, "{} {} {}", lhs.borrow().1, sym, rhs.borrow().1)
            }
            Self::Adjust { op, prefix, target } => {
                let sym = match op {
                    AdjustOp::Inc => "++",
                    AdjustOp::Dec => "--",
                };
                if *prefix {
                    write!(f, "{}{}", sym, target.borrow().1)
                } else {
                    write!(f, "{}{}", target.borrow().1, sym)
                }
            }
            Self::Negate(n) => write!(f, "-{}", n.borrow().1),
            Self::Positive(n) => write!(f, "+{}", n.borrow().1),
            Self::Expr(e) => write!(f, "{}", e.1),
            Self::Return(e) => write!(f, "return {}", e.1),
            Self::ImplicitReturn(e) => write!(f, "{}", e.1),
            Self::ExprStatement(e) => write!(f, "{};", e.1),
            Self::LetDestructure { pattern, rhs } => {
                write!(f, "let {} = {}", pattern, rhs.1)
            }
            Self::Fragment(list) | Self::Block(list) => write!(
                f,
                "{}",
                list.iter()
                    .map(|e| e.1.to_string())
                    .collect::<Vec<String>>()
                    .join(" ")
            ),
            Self::Group(g) => write!(f, "({})", g.1),
            // `ExprStatement` already renders its trailing `;`; avoid `;;`.
            Self::Statement(s) => match s.1.as_ref() {
                Self::ExprStatement(_) => writeln!(f, "{}", s.1),
                _ => writeln!(f, "{};", s.1),
            },
            Self::String(s) => write!(f, "\"{}\"", s),
            Self::Dload(path) => write!(f, "dload({})", path.1),
            Self::Done(handle) => write!(f, "done({})", handle.1),
            Self::Tuple(items) => write!(
                f,
                "({})",
                items
                    .iter()
                    .map(|a| a.1.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Self::Array(items) => write!(
                f,
                "[{}]",
                items
                    .iter()
                    .map(|a| a.1.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Self::Index(target, index) => match index {
                Some(idx) => write!(f, "{}[{}]", target.1, idx.1),
                None => write!(f, "{}[]", target.1),
            },
            Self::Readonly(inner) => write!(f, "readonly {}", inner.1),
            Self::QualifiedAccess { owner, member } => write!(f, "{}::{}", owner, member),
            Self::StaticDecl {
                is_const,
                name,
                ty,
                init,
            } => {
                let ty_str = ty
                    .as_ref()
                    .map(|t| format!(": {}", t.1))
                    .unwrap_or_default();
                let kw = if *is_const {
                    "static const"
                } else {
                    "static let"
                };
                write!(f, "{}{} {} = {};", kw, ty_str, name, init.1)
            }
            Self::Declare(args) | Self::Invoke(args) => {
                let kw = if matches!(self, Self::Declare(_)) {
                    "declare"
                } else {
                    "invoke"
                };
                write!(
                    f,
                    "{}({})",
                    kw,
                    args.iter()
                        .map(|a| a.1.to_string())
                        .collect::<Vec<String>>()
                        .join(", ")
                )
            }
            Self::Function {
                docs,
                attrs,
                name,
                is_coro,
                is_static,
                type_params,
                args,
                returns,
                where_constraints,
                body,
            } => {
                let async_kw = if *is_coro { "async " } else { "" };
                let static_kw = if *is_static { "static " } else { "" };
                let tp = if type_params.is_empty() {
                    String::new()
                } else {
                    fmt_type_params(type_params)
                };
                let ret_str = returns
                    .as_ref()
                    .map(|ret| format!(" -> {}", ret.1))
                    .unwrap_or_default();
                let where_str = if where_constraints.is_empty() {
                    String::new()
                } else {
                    format!(
                        " where {}",
                        where_constraints
                            .iter()
                            .map(|c| c.to_string())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                };
                let attr_prefix = format!("{}{}", fmt_docs(docs), fmt_attrs(attrs));
                match body {
                    Some(b) => write!(
                        f,
                        "{}{}{}fn {}{}({}){}{} {{\n{}}}",
                        attr_prefix, async_kw, static_kw, name, tp, args.1, ret_str, where_str, b.1
                    ),
                    None => write!(
                        f,
                        "{}{}{}fn {}{}({}){}{};",
                        attr_prefix, async_kw, static_kw, name, tp, args.1, ret_str, where_str
                    ),
                }
            }
            Self::Defer { captures, body } => {
                write!(f, "defer")?;
                if !captures.is_empty() {
                    write!(f, " use ({})", captures.join(", "))?;
                }
                write!(f, " {}", body.1)
            }
            Self::TestCase { name, body } => {
                write!(f, "test({}) {{\n{}}}", name.1, body.1)
            }
            Self::Call { name, args } => {
                write!(
                    f,
                    "{}({})",
                    name.1,
                    args.clone().map_or(String::default(), |p| p
                        .iter()
                        .map(|p| p.1.to_string())
                        .collect::<Vec<String>>()
                        .join(", "))
                )
            }
            Self::NamedArg(name, value) => write!(f, "{}: {}", name, value.1),
            Self::Argument {
                docs,
                ty,
                name,
                is_rest,
            } => {
                write!(f, "{}", fmt_docs(docs))?;
                if *is_rest {
                    match ty {
                        None => write!(f, "... {}", name),
                        Some(t) => write!(f, "{}... {}", t.1, name),
                    }
                } else {
                    write!(f, "{} {}", ty.as_ref().expect("fixed param").1, name)
                }
            }
            Self::Spread(inner) => write!(f, "...{}", inner.1),
            Self::TypeFnSig { params, ret } => write!(f, "fn{} -> {}", params.1, ret.1),
            Self::AttrDecl {
                docs,
                name,
                type_params,
                args,
                returns,
                where_constraints,
                body,
            } => {
                write!(f, "{}attr {}", fmt_docs(docs), name)?;
                if !type_params.is_empty() {
                    write!(
                        f,
                        "<{}>",
                        type_params
                            .iter()
                            .map(|p| p.to_string())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )?;
                }
                write!(f, "{}", args.1)?;
                if let Some(ret) = returns {
                    write!(f, " -> {}", ret.1)?;
                }
                if !where_constraints.is_empty() {
                    write!(
                        f,
                        " where {}",
                        where_constraints
                            .iter()
                            .map(|c| c.to_string())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )?;
                }
                write!(f, " {}", body.1)
            }
            Self::Loop {
                identifier,
                iterable,
                body,
            } => match identifier {
                Some(ident) => write!(f, "for {} in {} {{\n{}}}", ident.1, iterable.1, body.1),
                None => write!(f, "while {} {{\n{}}}", iterable.1, body.1),
            },
            Self::Break => write!(f, "break"),
            Self::Continue => write!(f, "continue"),
            Self::Assignment(n, e) => {
                write!(f, "{} = {}", n.1, e.1)
            }
            Self::Noop(n) => write!(f, "@{{ {} }}@", n.1),
            Self::TypeAlias {
                docs,
                name,
                type_params,
                ty,
            } => {
                write!(f, "{}", fmt_docs(docs))?;
                if type_params.is_empty() {
                    write!(f, "type {} = {};", name, ty.1)
                } else {
                    write!(
                        f,
                        "type {}{} = {};",
                        name,
                        fmt_type_params(type_params),
                        ty.1
                    )
                }
            }
            Self::Dict(items) => {
                let parts: Vec<String> = items
                    .iter()
                    .map(|f| format!("{}: {}", f.name, f.value.1))
                    .collect();
                write!(f, "{{ {} }}", parts.join(", "))
            }
            Self::EnumDecl {
                docs,
                attrs,
                name,
                type_params,
                variants,
            } => {
                let tp = if type_params.is_empty() {
                    String::new()
                } else {
                    fmt_type_params(type_params)
                };
                let attr_prefix = format!("{}{}", fmt_docs(docs), fmt_attrs(attrs));
                let vs = variants
                    .iter()
                    .map(|v| match v.1.as_ref() {
                        Self::EnumVariant {
                            name,
                            payload,
                            discriminant,
                            ..
                        } => {
                            let mut s = match payload {
                                EnumVariantPayload::Unit => name.to_string(),
                                EnumVariantPayload::Tuple(parts) => {
                                    if parts.is_empty() {
                                        name.to_string()
                                    } else {
                                        format!(
                                            "{}({})",
                                            name,
                                            parts
                                                .iter()
                                                .map(|p| p.1.to_string())
                                                .collect::<Vec<String>>()
                                                .join(", ")
                                        )
                                    }
                                }
                                EnumVariantPayload::Record(fields) => {
                                    let parts: Vec<String> = fields
                                        .iter()
                                        .map(|rf| format!("{}: {}", rf.name, rf.value.1))
                                        .collect();
                                    format!("{} {{ {} }}", name, parts.join(", "))
                                }
                            };
                            if let Some(disc) = discriminant {
                                s.push_str(" = ");
                                s.push_str(&disc.1.to_string());
                            }
                            s
                        }
                        _ => String::from("?"),
                    })
                    .collect::<Vec<String>>()
                    .join(", ");
                write!(f, "{}enum {}{} {{ {} }}", attr_prefix, name, tp, vs)
            }
            Self::EnumVariant {
                docs,
                name,
                payload,
                discriminant,
            } => {
                write!(f, "{}", fmt_docs(docs))?;
                match payload {
                    EnumVariantPayload::Unit => write!(f, "{}", name)?,
                    EnumVariantPayload::Tuple(parts) => {
                        if parts.is_empty() {
                            write!(f, "{}", name)?;
                        } else {
                            write!(
                                f,
                                "{}({})",
                                name,
                                parts
                                    .iter()
                                    .map(|p| p.1.to_string())
                                    .collect::<Vec<String>>()
                                    .join(", ")
                            )?;
                        }
                    }
                    EnumVariantPayload::Record(fields) => {
                        let parts: Vec<String> = fields
                            .iter()
                            .map(|rf| format!("{}: {}", rf.name, rf.value.1))
                            .collect();
                        write!(f, "{} {{ {} }}", name, parts.join(", "))?;
                    }
                }
                if let Some(disc) = discriminant {
                    write!(f, " = {}", disc.1)?;
                }
                Ok(())
            }
            Self::Construct {
                enum_name,
                variant_name,
                fields,
            } => {
                write!(f, "{}::{}", enum_name, variant_name)?;
                match fields {
                    EnumConstructPayload::Unit => Ok(()),
                    EnumConstructPayload::Tuple(args) => {
                        write!(
                            f,
                            "({})",
                            args.iter()
                                .map(|a| a.1.to_string())
                                .collect::<Vec<String>>()
                                .join(", ")
                        )
                    }
                    EnumConstructPayload::Record(parts) => {
                        let strs: Vec<String> = parts
                            .iter()
                            .map(|rf| format!("{}: {}", rf.name, rf.value.1))
                            .collect();
                        write!(f, "{{ {} }}", strs.join(", "))
                    }
                }
            }
            Self::Match { scrutinee, arms } => {
                let as_str = arms
                    .iter()
                    .map(|a| {
                        let pat = a.pattern.1.to_string();
                        let body = a.body.1.to_string();
                        format!("{} => {}", pat, body)
                    })
                    .collect::<Vec<String>>()
                    .join(", ");
                write!(f, "match {} {{ {} }}", scrutinee.1, as_str)
            }
            Self::Access(receiver, field) => {
                write!(f, "{}.{}", receiver.1, field)
            }
            Self::OptionalAccess(receiver, field) => {
                write!(f, "{}?.{}", receiver.1, field)
            }
            Self::Try(inner) => write!(f, "{}?", inner.1),
            Self::Coalesce(lhs, rhs) => write!(f, "{} ?? {}", lhs.1, rhs.1),
            Self::Cast(expr, ty) => write!(f, "{} as {}", expr.1, ty.1),
            Self::TypeOf(inner) => write!(f, "typeof {}", inner.1),
            Self::Raise(inner) => write!(f, "raise {}", inner.1),
            Self::Panic(inner) => write!(f, "panic {}", inner.1),
            Self::Yield(inner) => write!(f, "yield {}", inner.1),
            Self::YieldFrom(inner) => write!(f, "yield from {}", inner.1),
            Self::Resume(target, None) => write!(f, "resume {}", target.1),
            Self::Resume(target, Some(arg)) => write!(f, "resume {} with {}", target.1, arg.1),
            Self::Type(n) => write!(f, "{}", n),
            Self::TypeApp { name, args } => {
                let args_s = args
                    .iter()
                    .map(|a| a.1.to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                write!(f, "{}<{}>", name, args_s)
            }
            Self::TypeProjection { owner, name, args } => {
                if args.is_empty() {
                    write!(f, "{}::{}", owner, name)
                } else {
                    let args_s = args
                        .iter()
                        .map(|a| a.1.to_string())
                        .collect::<Vec<_>>()
                        .join(", ");
                    write!(f, "{}::{}<{}>", owner, name, args_s)
                }
            }
            Self::TypeFun(arg, ret) => write!(f, "{} -> {}", arg.1, ret.1),
            Self::AssocTypeDecl { name, type_params } => {
                write!(f, "type {}{};", name, fmt_type_params(type_params))
            }
            Self::AssocTypeDef {
                name,
                type_params,
                ty,
            } => write!(
                f,
                "type {}{} = {};",
                name,
                fmt_type_params(type_params),
                ty.1
            ),
            Self::Class {
                docs,
                attrs,
                name,
                type_params,
                fields,
            } => {
                let tp = if type_params.is_empty() {
                    String::new()
                } else {
                    fmt_type_params(type_params)
                };
                let attr_prefix = format!("{}{}", fmt_docs(docs), fmt_attrs(attrs));
                let fs: Vec<String> = fields.iter().map(|f| f.1.to_string()).collect();
                write!(
                    f,
                    "{}class {}{} {{ {} }}",
                    attr_prefix,
                    name,
                    tp,
                    fs.join(", ")
                )
            }
            Self::Implementation {
                what,
                owner,
                type_params,
                methods,
            } => {
                let tp = if type_params.is_empty() {
                    String::new()
                } else {
                    fmt_type_params(type_params)
                };
                let ms: Vec<String> = methods.iter().map(|m| m.1.to_string()).collect();
                if what.is_empty() {
                    write!(f, "impl {}{} {{ {} }}", owner, tp, ms.join(" "))
                } else {
                    write!(
                        f,
                        "impl {} for {}{} {{ {} }}",
                        what,
                        owner,
                        tp,
                        ms.join(" ")
                    )
                }
            }
            Self::Forall { params, ty } => {
                write!(
                    f,
                    "forall {}. {}",
                    params
                        .iter()
                        .map(|p| p.to_string())
                        .collect::<Vec<_>>()
                        .join(", "),
                    ty.1
                )
            }
            Self::TypeClass {
                docs,
                name,
                type_params,
                methods,
            } => {
                write!(f, "{}", fmt_docs(docs))?;
                let tp = if type_params.is_empty() {
                    String::new()
                } else {
                    fmt_type_params(type_params)
                };
                let ms: Vec<String> = methods.iter().map(|m| m.1.to_string()).collect();
                write!(f, "trait {}{} {{ {} }}", name, tp, ms.join(" "))
            }
            Self::TypeClassImpl {
                class,
                args,
                methods,
            } => {
                // Prefer `impl Trait for T` / `impl Trait<A, B> for T` when
                // there is at least one type argument (Self-first convention).
                let ms: Vec<String> = methods.iter().map(|m| m.1.to_string()).collect();
                if let Some((for_ty, rest)) = args.split_first() {
                    let for_s = for_ty.1.to_string();
                    if rest.is_empty() {
                        write!(f, "impl {} for {} {{ {} }}", class, for_s, ms.join(" "))
                    } else {
                        let rest_s: Vec<String> = rest.iter().map(|a| a.1.to_string()).collect();
                        write!(
                            f,
                            "impl {}<{}> for {} {{ {} }}",
                            class,
                            rest_s.join(", "),
                            for_s,
                            ms.join(" ")
                        )
                    }
                } else {
                    write!(f, "impl {} {{ {} }}", class, ms.join(" "))
                }
            }
            Self::Lambda {
                args,
                captures,
                body,
            } => {
                // Prefer comma-separated params for round-trip with `arg_list`.
                let params = match args.1.as_ref() {
                    Self::Fragment(items) => items
                        .iter()
                        .map(|a| a.1.to_string())
                        .collect::<Vec<_>>()
                        .join(", "),
                    other => other.to_string(),
                };
                write!(f, "fn ({})", params)?;
                if !captures.is_empty() {
                    write!(f, " use ({})", captures.join(", "))?;
                }
                match body.1.as_ref() {
                    Self::Block(_) => write!(f, " {{\n{}}}", body.1),
                    _ => write!(f, " => {}", body.1),
                }
            }
            Self::Use { path, name, alias } => {
                write!(f, "use ")?;
                for (i, seg) in path.iter().enumerate() {
                    if i > 0 {
                        write!(f, "::")?;
                    }
                    write!(f, "{}", seg)?;
                }
                if !path.is_empty() {
                    write!(f, "::")?;
                }
                write!(f, "{}", name)?;
                if let Some(a) = alias {
                    write!(f, " as {}", a)?;
                }
                write!(f, ";")
            }
            Self::Comment(text) => {
                write!(f, "//")?;
                if !text.is_empty() {
                    write!(f, " {}", text)?;
                }
                Ok(())
            }
            Self::Field {
                docs,
                visibility,
                modifier,
                name,
                ty,
                init,
            } => {
                write!(f, "{}", fmt_docs(docs))?;
                if matches!(visibility, Visibility::Public) {
                    write!(f, "pub ")?;
                }
                match modifier {
                    FieldModifier::Static => write!(f, "static ")?,
                    FieldModifier::Const => write!(f, "const ")?,
                    FieldModifier::Instance => {}
                }
                write!(f, "{}: {}", name.1, ty.1)?;
                if let Some(init) = init {
                    write!(f, " = {}", init.1)?;
                }
                Ok(())
            }
            Self::Program(items) => {
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        writeln!(f)?;
                        writeln!(f)?;
                    }
                    write!(f, "{}", item.1)?;
                }
                Ok(())
            }
            e => write!(f, "<unhandled: {:?}>", e),
        }
    }
}
