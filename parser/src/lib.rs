//! Pratt parser for coil source.
//!
//! Builds a span-annotated `Expression` AST for the compiler pipeline.

use ast::{
    AdjustOp, AssignOp, AttrArgs, AttrLit, Attribute, EnumConstructPayload, EnumVariantPayload,
    Expression, FieldModifier, LetFieldPattern, LetPattern, MatchArm, Output, Pattern,
    PatternField, PatternOutput, PatternPayload, RecordFieldDecl, RecordFieldValue, TypeParam,
    Visibility,
};
use std::{
    collections::HashSet,
    marker::PhantomData,
    num::{ParseFloatError, ParseIntError},
};

pub use chumsky::span::SimpleSpan;
use chumsky::{
    IterParser, Parser,
    error::{Rich, RichReason},
    extra,
    pratt::{infix, left, none, postfix, prefix, right},
    prelude::{any, choice, empty, just, none_of, recursive},
    text,
};
use reporting::{ErrorCode, Label, Message};

#[repr(u16)]
enum Precedence {
    Assign,
    /// `??` null-coalesce (between Or and Assign).
    Coalesce,
    Or,
    Xor,
    And,
    Equal,
    /// `..` / `..=` — below comparisons, non-associative (Phase P3).
    Range,
    Compare,
    Binary,
    Term,
    Factor,
    /// `as` — below unary `-` so `-1 as byte` is `(-1) as byte` (Rust-like).
    Cast,
    Negate,
    Unary,
    Call,
    Primary,
}

macro_rules! op {
    ($operator: literal) => {
        just($operator).padded()
    };
}

macro_rules! keyword {
    ($word: literal) => {
        text::keyword($word).padded()
    };
}

macro_rules! output {
    ($kind: tt) => {
        |v, e| (e.span(), Box::new(Expression::$kind(v)))
    };
    ($kind: tt) => {
        |(lhs, rhs), e| (e.span(), Box::new(Expression::$kind(lhs, rhs)))
    };
}

pub mod ast;
pub mod fmt;

pub use ast::item_docs;
pub use fmt::{format_program, format_range, format_source};

#[derive(Default)]
pub struct Pratt<'pratt> {
    _data: PhantomData<&'pratt ()>,
}

fn first_duplicate_name<'a, I>(names: I) -> Option<&'a str>
where
    I: IntoIterator<Item = &'a str>,
{
    let mut seen = HashSet::new();
    for name in names {
        if !seen.insert(name) {
            return Some(name);
        }
    }
    None
}

fn duplicate_field_error<'src>(
    names: impl IntoIterator<Item = &'src str>,
    span: SimpleSpan,
) -> Option<Rich<'src, char>> {
    first_duplicate_name(names)
        .map(|name| Rich::custom(span, format!("Duplicate field `{name}`")))
}

fn is_duplicate_field_parse_error(err: &Rich<'_, char>) -> bool {
    matches!(
        err.reason(),
        RichReason::Custom(msg) if msg.starts_with("Duplicate field `")
    )
}

impl<'pratt> Pratt<'pratt> {
    fn int(
        &self,
    ) -> impl Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>> + Clone + 'pratt
    {
        text::int(10)
            .to_slice()
            .from_str()
            .validate(|v: Result<i64, ParseIntError>, e, emitter| match v {
                Ok(value) => value,
                Err(msg) => {
                    emitter.emit(Rich::custom(e.span(), msg.to_string()));

                    0_i64
                }
            })
            .labelled("integer")
            .map_with(output!(Integer))
    }

    fn float(
        &self,
    ) -> impl Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>> + Clone + 'pratt
    {
        text::int(10)
            .then(just(".").then(text::int(10)))
            .to_slice()
            .from_str()
            .validate(|v: Result<f64, ParseFloatError>, e, emitter| match v {
                Ok(value) => value,
                Err(msg) => {
                    emitter.emit(Rich::custom(e.span(), msg.to_string()));

                    0_f64
                }
            })
            .labelled("float")
            .map_with(output!(Float))
    }

    /// Body of a `"..."` literal (between the quotes).
    ///
    /// A backslash escapes the next character, so `\"` is content and does not
    /// end the literal. The returned slice is the raw source (escapes intact);
    /// [`crate::codegen::unescape_coil_string`] expands them later.
    fn string_lit_body(
        &self,
    ) -> impl Parser<'pratt, &'pratt str, &'pratt str, extra::Err<Rich<'pratt, char>>> + Clone + 'pratt
    {
        choice((
            just('\\').then(any()).ignored(),
            none_of('"').ignored(),
        ))
        .repeated()
        .to_slice()
    }

    fn string(
        &self,
    ) -> impl Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>> + Clone + 'pratt
    {
        just('"')
            .ignore_then(self.string_lit_body())
            .then_ignore(just('"'))
            .map_with(output!(String))
            .labelled("string")
    }
    fn ident(
        &self,
    ) -> impl Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>> + Clone + 'pratt
    {
        text::ident().padded().map_with(output!(Identifier))
    }

    /// Type atoms and function types without nested `fn(...)` signatures in
    /// parameter positions (used by `arg_list` to avoid parser recursion).
    fn type_annotation_no_fn(
        &self,
    ) -> impl Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>> + Clone + 'pratt
    {
        use chumsky::Parser;
        recursive(|type_ann| {
            self.type_annotation_atoms(type_ann.clone())
                .then(op!("->").ignore_then(type_ann.clone()).or_not())
                .map_with(|(lhs, rhs), e| match rhs {
                    Some(rhs) => (e.span(), Box::new(Expression::TypeFun(lhs, rhs))),
                    None => lhs,
                })
        })
    }

    /// Shared type-atom parser: arrays, tuples, `forall`, projections, names.
    fn type_annotation_atoms<T>(
        &self,
        type_ann: T,
    ) -> impl Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>> + Clone + 'pratt
    where
        T: Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>>
            + Clone
            + 'pratt,
    {
        use chumsky::Parser;
        let array_type = type_ann
            .clone()
            .then(
                op!(";")
                    .ignore_then(
                        choice((
                            text::int(10)
                                .to_slice()
                                .from_str::<i64>()
                                .validate(|v: Result<i64, _>, _, _| v.unwrap_or(0))
                                .map_with(|n, e| (e.span(), Box::new(Expression::Integer(n)))),
                            text::ident().padded().map_with(output!(Type)),
                        )),
                    )
                    .or_not(),
            )
            .delimited_by(op!('['), op!(']'))
            .map_with(|(elem, n_opt), e| match n_opt {
                Some(n) => (
                    e.span(),
                    Box::new(Expression::Array(vec![elem, n])),
                ),
                None => (e.span(), Box::new(Expression::Array(vec![elem]))),
            });
        let tuple_type = self.tuple_atom(type_ann.clone());
        let named_type = text::ident()
            .padded()
            .then(
                type_ann
                    .clone()
                    .separated_by(op!(','))
                    .allow_trailing()
                    .collect::<Vec<_>>()
                    .delimited_by(op!('<'), op!('>'))
                    .or_not(),
            )
            .map_with(|(name, args_opt), e| match args_opt {
                Some(args) => (e.span(), Box::new(Expression::TypeApp { name, args })),
                None => (e.span(), Box::new(Expression::Type(name))),
            });
        let projection_type = text::ident()
            .padded()
            .then_ignore(op!("::"))
            .then(text::ident().padded())
            .then(
                type_ann
                    .clone()
                    .separated_by(op!(','))
                    .allow_trailing()
                    .collect::<Vec<_>>()
                    .delimited_by(op!('<'), op!('>'))
                    .or_not(),
            )
            .map_with(|((owner, name), args_opt), e| {
                (
                    e.span(),
                    Box::new(Expression::TypeProjection {
                        owner,
                        name,
                        args: args_opt.unwrap_or_default(),
                    }),
                )
            });
        let forall_type = keyword!("forall")
            .ignore_then(
                self.single_type_param()
                    .separated_by(op!(","))
                    .at_least(1)
                    .collect::<Vec<_>>(),
            )
            .then_ignore(op!("."))
            .then(type_ann.clone())
            .map_with(|(params, ty), e| {
                (
                    e.span(),
                    Box::new(Expression::Forall {
                        params,
                        ty: Box::new(ty),
                    }),
                )
            });
        choice((
            array_type,
            tuple_type,
            forall_type,
            projection_type,
            named_type,
        ))
    }

    /// Type annotation: bare identifiers, `[T]`, `[T; N]`, `(T1, T2, ...)`, or
    /// `fn(T x, ...args) -> R`.
    fn type_annotation(
        &self,
    ) -> impl Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>> + Clone + 'pratt
    {
        use chumsky::Parser;
        recursive(|type_ann| {
            let base_atom = self.type_annotation_atoms(type_ann.clone());
            let fn_sig_type = keyword!("fn")
                .ignore_then(self.arg_list_typed(base_atom.clone()))
                .then(op!("->").ignore_then(type_ann.clone()))
                .map_with(|(params, ret), e| {
                    (e.span(), Box::new(Expression::TypeFnSig { params, ret }))
                });
            choice((fn_sig_type, base_atom))
                .then(op!("->").ignore_then(type_ann.clone()).or_not())
                .map_with(|(lhs, rhs), e| match rhs {
                    Some(rhs) => (e.span(), Box::new(Expression::TypeFun(lhs, rhs))),
                    None => lhs,
                })
        })
    }

    /// One type parameter: `T`, `T: Num + Eq`, `F: * -> *`, or
    /// `c: * -> Constraint`.
    ///
    /// After `:`, either class bounds or a kind annotation. A kind annotation
    /// may be followed by class bounds separated with a comma.
    fn single_type_param(
        &self,
    ) -> impl Parser<'pratt, &'pratt str, TypeParam<'pratt>, extra::Err<Rich<'pratt, char>>>
    + Clone
    + 'pratt {
        use crate::ast::Kind;

        let kind_ann = recursive(|kind| {
            let atom = just('*')
                .padded()
                .to(Kind::Type)
                .or(keyword!("Constraint").to(Kind::Constraint))
                .or(kind.clone().delimited_by(op!("("), op!(")")));

            atom.then(op!("->").ignore_then(kind).or_not())
                .map(|(domain, codomain)| match codomain {
                    Some(codomain) => Kind::Arrow(Box::new(domain), Box::new(codomain)),
                    None => domain,
                })
        });

        let class_bound = text::ident().padded().then_ignore(op!(":").not());
        let class_bounds = class_bound
            .separated_by(op!("+"))
            .at_least(1)
            .collect::<Vec<_>>();

        // After `:`, try kind first (leading `*` or `(`), else class bounds.
        let after_colon = kind_ann
            .then(op!(",").ignore_then(class_bounds.clone()).or_not())
            .map(|(kind, bounds)| (bounds.unwrap_or_default(), kind))
            .or(class_bounds.map(|bounds| (bounds, Kind::Type)));

        text::ident()
            .padded()
            .then(op!(":").ignore_then(after_colon).or_not())
            .map(|(name, ann)| {
                let (bounds, kind) = ann.unwrap_or_else(|| (Vec::new(), Kind::Type));
                TypeParam { name, bounds, kind }
            })
    }

    /// `<T, U: Num + Eq, F: * -> *, ...>` — optional generic type parameter list.
    ///
    /// Returns an empty `Vec` when no `<` is found.
    fn type_param_list(
        &self,
    ) -> impl Parser<'pratt, &'pratt str, Vec<TypeParam<'pratt>>, extra::Err<Rich<'pratt, char>>>
    + Clone
    + 'pratt {
        self.single_type_param()
            .separated_by(op!(","))
            .allow_trailing()
            .collect::<Vec<_>>()
            .delimited_by(op!("<"), op!(">"))
            .or_not()
            .map(|opt| opt.unwrap_or_default())
    }

    fn expr(
        &self,
    ) -> impl Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>> + Clone + 'pratt
    {
        recursive(|expr| {
            let stmt = self.statement_with_expr(expr.clone());
            let atom = choice((
                // `match` is a keyword atom — registered before
                // `self.ident()` so the identifier parser refuses
                // to match it.
                self.match_expr(expr.clone(), stmt.clone()),
                // `done` stays a keyword builtin. `dload` / `declare` /
                // `invoke` are ordinary calls resolved via `use ffi::{…}`.
                self.done_(expr.clone()),
                self.resume_(expr.clone()),
                self.yield_expr_(expr.clone()),
                // `raise expr` as an expression atom (also a statement).
                keyword!("raise")
                    .ignore_then(expr.clone())
                    .map_with(|inner, e| (e.span(), Box::new(Expression::Raise(inner)))),
                // `panic expr` as an expression atom (also a statement).
                keyword!("panic")
                    .ignore_then(expr.clone())
                    .map_with(|inner, e| (e.span(), Box::new(Expression::Panic(inner)))),
                // `typeof expr` — compile-time type name (string).
                keyword!("typeof")
                    .ignore_then(expr.clone())
                    .map_with(|inner, e| (e.span(), Box::new(Expression::TypeOf(inner)))),
                // `(a, b, c)` — tuple atom. MUST come before
                // `self.call(...)` (which expects a leading
                // ident) AND before `self.ident()`.
                self.tuple_atom(expr.clone()),
                // `[a, b, c]` — array atom (optionally `readonly […]`).
                self.readonly_array_atom(expr.clone()),
                self.dict_atom(expr.clone()),
                // `EnumName::Variant(args)` — qualified constructor
                // application. MUST be tried before `qualified_access`
                // so multi-segment paths (`ffi::types::Int`) and enum
                // unit/tuple/record shapes win over static field access.
                self.construct(expr.clone()),
                self.qualified_access(),
                self.readonly_instantiate(expr.clone()),
                self.instantiate(expr.clone()),
                // float comes before int so that `1.0` is parsed as a
                // float, not an `int` `1` followed by a stray `.0`.
                self.float(),
                self.int(),
                self.string(),
                // Keyword atoms come before self.ident() so they're
                // registered in chumsky's KEYWORDS set before the
                // identifier parser is built (which then refuses to
                // match them).
                keyword!("true")
                    .map_with(|state, e| (e.span(), Box::new(Expression::Bool(state == "true"))))
                    .labelled("boolean"),
                keyword!("false")
                    .map_with(|state, e| (e.span(), Box::new(Expression::Bool(state == "true"))))
                    .labelled("boolean"),
                keyword!("new")
                    .ignore_then(text::ident())
                    .map_with(|class, e| {
                        let class_output = (e.span(), Box::new(Expression::Identifier(class)));
                        (
                            e.span(),
                            Box::new(Expression::Instantiate(class_output, None)),
                        )
                    })
                    .labelled("new"),
                // Anonymous `fn (…)` before `ident` so `fn` stays a keyword.
                self.lambda_atom(expr.clone(), stmt),
                self.ident(),
            ));

            let pratt_expr = choice((atom, self.group(expr.clone()))).pratt((
                // No postfix `!` here — it would conflict with `!=`
                // (which should be parsed as a single infix operator).
                // Prefix `!` is logical NOT; prefix `~` is bitwise NOT on integers.
                infix(
                    right(Precedence::Binary as u16),
                    op!("<<"),
                    |lhs, _, rhs, e| (e.span(), Box::new(Expression::Shl(lhs, rhs))),
                ),
                infix(
                    right(Precedence::Binary as u16),
                    op!(">>"),
                    |lhs, _, rhs, e| (e.span(), Box::new(Expression::Shr(lhs, rhs))),
                ),
                infix(
                    right(Precedence::Binary as u16),
                    op!('&'),
                    |lhs, _, rhs, e| (e.span(), Box::new(Expression::BitAnd(lhs, rhs))),
                ),
                infix(
                    right(Precedence::And as u16),
                    op!("&&"),
                    |lhs, _, rhs, e| (e.span(), Box::new(Expression::And(lhs, rhs))),
                ),
                infix(
                    right(Precedence::Binary as u16),
                    op!('|'),
                    |lhs, _, rhs, e| (e.span(), Box::new(Expression::BitOr(lhs, rhs))),
                ),
                infix(right(Precedence::Or as u16), op!("||"), |lhs, _, rhs, e| {
                    (e.span(), Box::new(Expression::Or(lhs, rhs)))
                }),
                infix(
                    right(Precedence::Factor as u16),
                    choice((op!("**"), op!("*"), op!("/"), op!("%"))),
                    |lhs, op, rhs, e| {
                        (
                            e.span(),
                            Box::new(match op {
                                "**" => Expression::Pow(lhs, rhs),
                                "*" => Expression::Mul(lhs, rhs),
                                "/" => Expression::Div(lhs, rhs),
                                "%" => Expression::Mod(lhs, rhs),
                                _ => unreachable!("No other operators"),
                            }),
                        )
                    },
                ),
                infix(
                    right(Precedence::Compare as u16),
                    choice((op!(">="), op!("<="), op!(">"), op!("<"))),
                    |lhs, op, rhs, e| {
                        (
                            e.span(),
                            Box::new(match op {
                                ">" => Expression::Gt(lhs, rhs),
                                ">=" => Expression::Geq(lhs, rhs),
                                "<=" => Expression::Leq(lhs, rhs),
                                "<" => Expression::Le(lhs, rhs),
                                _ => unreachable!("No more comparison operators"),
                            }),
                        )
                    },
                ),
                // `..=` before `..` so the digraph wins. Non-associative
                // (reject `a..b..c`). Float `1.0` stays an atom; postfix
                // `.field` requires an ident after `.`, so `0..10` is fine.
                infix(
                    none(Precedence::Range as u16),
                    choice((op!("..="), op!(".."))),
                    |lhs, op, rhs, e| {
                        (
                            e.span(),
                            Box::new(Expression::Range {
                                start: lhs,
                                end: rhs,
                                inclusive: op == "..=",
                            }),
                        )
                    },
                ),
                infix(
                    right(Precedence::Equal as u16),
                    choice((op!("=="), op!("!="))),
                    |lhs, op, rhs, e| {
                        (
                            e.span(),
                            Box::new(match op {
                                "==" => Expression::Eq(lhs, rhs),
                                "!=" => Expression::Neq(lhs, rhs),
                                _ => unreachable!("No more equality operators"),
                            }),
                        )
                    },
                ),
                infix(right(Precedence::Xor as u16), op!('^'), |lhs, _, rhs, e| {
                    (e.span(), Box::new(Expression::Xor(lhs, rhs)))
                }),
                infix(
                    right(Precedence::Assign as u16),
                    choice((
                        op!("**="),
                        op!("<<="),
                        op!(">>="),
                        op!("+="),
                        op!("-="),
                        op!("*="),
                        op!("/="),
                        op!("%="),
                        op!("&="),
                        op!("|="),
                        op!("^="),
                    )),
                    |lhs, op, rhs, e| {
                        let assign_op = match op {
                            "+=" => AssignOp::Add,
                            "-=" => AssignOp::Sub,
                            "*=" => AssignOp::Mul,
                            "/=" => AssignOp::Div,
                            "%=" => AssignOp::Mod,
                            "**=" => AssignOp::Pow,
                            "<<=" => AssignOp::Shl,
                            ">>=" => AssignOp::Shr,
                            "&=" => AssignOp::BitAnd,
                            "|=" => AssignOp::BitOr,
                            "^=" => AssignOp::BitXor,
                            _ => unreachable!("No other compound assignment operators"),
                        };
                        (
                            e.span(),
                            Box::new(Expression::CompoundAssign(lhs, assign_op, rhs)),
                        )
                    },
                ),
                infix(
                    right(Precedence::Assign as u16),
                    op!("="),
                    |lhs, _, rhs, e| (e.span(), Box::new(Expression::Assignment(lhs, rhs))),
                ),
                prefix(
                    Precedence::Unary as u16,
                    choice((op!("++"), op!("--"))),
                    |op, rhs, e| {
                        (
                            e.span(),
                            Box::new(Expression::Adjust {
                                op: if op == "++" {
                                    AdjustOp::Inc
                                } else {
                                    AdjustOp::Dec
                                },
                                prefix: true,
                                target: rhs,
                            }),
                        )
                    },
                ),
                prefix(
                    Precedence::Negate as u16,
                    choice((op!('-'), op!('~'), op!('+'), op!('!'))),
                    |c, rhs, e| {
                        (
                            e.span(),
                            Box::new(match c {
                                '-' => Expression::Negate(rhs),
                                '+' => Expression::Positive(rhs),
                                '~' => Expression::Not(rhs),
                                '!' => Expression::LogicalNot(rhs),
                                _ => unreachable!("No other prefix operators"),
                            }),
                        )
                    },
                ),
                infix(left(Precedence::Term as u16), op!('-'), |lhs, _, rhs, e| {
                    (e.span(), Box::new(Expression::Sub(lhs, rhs)))
                }),
                infix(left(Precedence::Term as u16), op!('+'), |lhs, _, rhs, e| {
                    (e.span(), Box::new(Expression::Add(lhs, rhs)))
                }),
                // `??` between Or and Assign (right-associative).
                infix(
                    right(Precedence::Coalesce as u16),
                    op!("??"),
                    |lhs, _, rhs, e| (e.span(), Box::new(Expression::Coalesce(lhs, rhs))),
                ),
                postfix(
                    Precedence::Primary as u16,
                    choice((op!("++"), op!("--"))),
                    |lhs, op, e| {
                        (
                            e.span(),
                            Box::new(Expression::Adjust {
                                op: if op == "++" {
                                    AdjustOp::Inc
                                } else {
                                    AdjustOp::Dec
                                },
                                prefix: false,
                                target: lhs,
                            }),
                        )
                    },
                ),
                // `?.field` before bare `.` / `?` so the digraph wins.
                postfix(
                    Precedence::Primary as u16,
                    just("?.").ignore_then(text::ident()),
                    |lhs, field, e| (e.span(), Box::new(Expression::OptionalAccess(lhs, field))),
                ),
                postfix(
                    Precedence::Primary as u16,
                    just('.').ignore_then(text::ident()),
                    |lhs, field, e| (e.span(), Box::new(Expression::Access(lhs, field))),
                ),
                // Postfix `?` must not steal the first `?` of `??`.
                postfix(
                    Precedence::Primary as u16,
                    just('?').then_ignore(just('?').not()),
                    |lhs, _, e| (e.span(), Box::new(Expression::Try(lhs))),
                ),
                postfix(
                    Precedence::Primary as u16,
                    choice((
                        expr.clone().map(Some).delimited_by(op!('['), op!(']')),
                        op!('[').ignore_then(op!(']')).to(None),
                    )),
                    |lhs, index, e| (e.span(), Box::new(Expression::Index(lhs, index))),
                ),
                postfix(
                    Precedence::Call as u16,
                    self.params(expr.clone()),
                    |lhs, args, e| (e.span(), Box::new(Expression::Call { name: lhs, args })),
                ),
                // Below unary `-` so `-1 as byte` is `(-1) as byte`; still above
                // `*`/`+`/assignment so `c = m as byte` stays `c = (m as byte)`.
                postfix(
                    Precedence::Cast as u16,
                    op!("as").ignore_then(self.type_annotation()),
                    |lhs, ty, e| (e.span(), Box::new(Expression::Cast(lhs, ty))),
                ),
            ));
            pratt_expr
        })
        .map_with(output!(Expr))
    }

    fn group<
        T: Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>>
            + Clone
            + 'pratt,
    >(
        &self,
        expr: T,
    ) -> impl Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>> + Clone + 'pratt
    {
        expr.repeated()
            .at_least(0)
            .collect()
            .map_with(output!(Fragment))
            .delimited_by(op!('('), op!(')'))
            .map_with(output!(Group))
    }

    fn block<
        T: Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>>
            + Clone
            + 'pratt,
    >(
        &self,
        stmt: T,
    ) -> impl Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>> + Clone + 'pratt
    {
        stmt.repeated()
            .at_least(0)
            .collect()
            .map_with(output!(Block))
            .delimited_by(op!('{'), op!('}'))
    }

    /// Expression-valued brace body: statements followed by an optional bare
    /// trailing expression. Used by match arms and long-form lambdas.
    fn brace_body<
        S: Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>>
            + Clone
            + 'pratt,
        E: Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>>
            + Clone
            + 'pratt,
    >(
        &self,
        stmt: S,
        expr: E,
    ) -> impl Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>> + Clone + 'pratt
    {
        stmt.repeated()
            .collect::<Vec<_>>()
            .then(expr.or_not())
            .delimited_by(op!("{"), op!("}"))
            .map_with(|(mut statements, trailing), e| {
                statements.extend(trailing);
                (e.span(), Box::new(Expression::Block(statements)))
            })
    }

    fn arg_list_typed<T>(
        &self,
        ty_parser: T,
    ) -> impl Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>> + Clone + 'pratt
    where
        T: Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>>
            + Clone
            + 'pratt,
    {
        // `... name` (tuple rest), `T... name` (homogeneous rest), or `T name` (fixed).
        let tuple_rest_arg = self
            .docs_prefix()
            .then(op!("...").ignore_then(text::ident().padded()))
            .map_with(|(docs, name), e| {
                (
                    e.span(),
                    Box::new(Expression::Argument {
                        docs,
                        ty: None,
                        name,
                        is_rest: true,
                    }),
                )
            });
        let rest_arg = self
            .docs_prefix()
            .then(ty_parser
            .clone()
            .then_ignore(just("...").padded())
            .then(text::ident().padded()))
            .map_with(|(docs, (ty, name)), e| {
                (
                    e.span(),
                    Box::new(Expression::Argument {
                        docs,
                        ty: Some(ty),
                        name,
                        is_rest: true,
                    }),
                )
            });
        let fixed_arg = self
            .docs_prefix()
            .then(ty_parser
            .clone()
            .then(text::ident().padded()))
            .map_with(|(docs, (ty, name)), e| {
                (
                    e.span(),
                    Box::new(Expression::Argument {
                        docs,
                        ty: Some(ty),
                        name,
                        is_rest: false,
                    }),
                )
            })
            .labelled("typed parameter (`Type name`)");
        // Common mistake: Rust-style `name: Type` instead of coil `Type name`.
        let rust_style_param = self
            .docs_prefix()
            .then(text::ident().padded())
            .then_ignore(op!(":"))
            .then(ty_parser.clone())
            .try_map(|((docs, name), _ty), span| {
                let _ = docs;
                Err(Rich::custom(
                    span,
                    format!(
                        "parameter `{name}` uses `name: Type` syntax; write `Type {name}` instead (for example `int {name}`)"
                    ),
                ))
            });
        // Bare `name)` / `name,` — consume the trailing delimiter so the
        // custom error span is at least as far as the typed-param failure
        // (which otherwise wins on `found ')' expected identifier`).
        let untyped_param = self
            .docs_prefix()
            .then(text::ident().padded())
            .then(choice((op!(','), op!(')'))))
            .try_map(|((docs, name), _), span| {
                let _ = docs;
                Err(Rich::custom(
                    span,
                    format!(
                        "parameter `{name}` is missing a type; write `Type {name}` (for example `int {name}`)"
                    ),
                ))
            });
        let arg = tuple_rest_arg
            .or(rest_arg)
            .or(rust_style_param)
            .or(fixed_arg)
            .or(untyped_param);

        arg.separated_by(op!(','))
            .allow_trailing()
            .collect::<Vec<_>>()
            .map_with(output!(Fragment))
            .delimited_by(op!("("), op!(")"))
            .labelled("parameter list")
    }

    fn arg_list(
        &self,
    ) -> impl Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>> + Clone + 'pratt
    {
        self.arg_list_typed(self.type_annotation_no_fn())
    }

    /// Anonymous lambda: `fn (T x) use (y) => expr` or
    /// `fn (T x) { statement; … trailing_expr }`.
    ///
    /// Distinct from named `fn name(…)` declarations (`func`): this form has
    /// no name between `fn` and `(`. Optional `use (id, …)` after the param
    /// list lists explicit captures (same `use` keyword as module imports;
    /// disambiguated by position after `fn (…)`).
    ///
    /// Long-form bodies use the statement and expression handles from the
    /// surrounding recursive expression parser, avoiding construction-time
    /// re-entry through `statement()` → `expr()`.
    fn lambda_atom<
        E: Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>>
            + Clone
            + 'pratt,
        S: Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>>
            + Clone
            + 'pratt,
    >(
        &self,
        expr: E,
        stmt: S,
    ) -> impl Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>> + Clone + 'pratt
    {
        let captures = keyword!("use")
            .ignore_then(
                text::ident()
                    .padded()
                    .separated_by(op!(','))
                    .allow_trailing()
                    .collect::<Vec<_>>()
                    .delimited_by(op!("("), op!(")")),
            )
            .or_not()
            .map(|opt| opt.unwrap_or_default());

        let short_body = op!("=>").ignore_then(expr.clone());
        // Use only handles built inside the surrounding `recursive(|expr| …)`.
        let long_body = self.brace_body(stmt, expr);

        keyword!("fn")
            .ignore_then(self.arg_list())
            .then(captures)
            .then(choice((short_body, long_body)))
            .map_with(|((args, captures), body), e| {
                (
                    e.span(),
                    Box::new(Expression::Lambda {
                        args,
                        captures,
                        body,
                    }),
                )
            })
            .labelled("lambda")
    }

    /// Parse one `where` constraint: `Convert<A, B>` or unary `Num<T>`.
    fn where_constraint(
        &self,
    ) -> impl Parser<
        'pratt,
        &'pratt str,
        ast::WhereConstraint<'pratt>,
        extra::Err<Rich<'pratt, char>>,
    > + Clone
    + 'pratt {
        text::ident()
            .padded()
            .then(
                self.type_annotation()
                    .separated_by(op!(','))
                    .allow_trailing()
                    .collect::<Vec<_>>()
                    .delimited_by(op!("<"), op!(">")),
            )
            .map(|(class, args)| ast::WhereConstraint { class, args })
    }

    /// Optional `where Class<T1, T2>, …` clause after a function's return type.
    fn where_clause(
        &self,
    ) -> impl Parser<
        'pratt,
        &'pratt str,
        Vec<ast::WhereConstraint<'pratt>>,
        extra::Err<Rich<'pratt, char>>,
    > + Clone
    + 'pratt {
        keyword!("where")
            .ignore_then(
                self.where_constraint()
                    .separated_by(op!(','))
                    .allow_trailing()
                    .at_least(1)
                    .collect(),
            )
            .or_not()
            .map(|opt| opt.unwrap_or_default())
    }

    /// Parses the function *signature* (`async? static? fn Name<T>(args) -> ret where …`)
    /// without consuming the body block.
    fn func_sig(
        &self,
    ) -> impl Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>> + Clone + 'pratt
    {
        self.docs_prefix()
            .then(keyword!("async").or_not())
            .then(keyword!("static").or_not())
            .then(keyword!("fn"))
            .then(text::ident().padded())
            .then(self.type_param_list())
            .then(self.arg_list())
            .then(op!("->").ignore_then(self.type_annotation()).or_not())
            .then(self.where_clause())
            .map_with(
                |(
                    (((((((docs, is_coro), is_static), _), name), type_params), args), returns),
                    where_constraints,
                ),
                 e| {
                    let empty_block = (e.span(), Box::new(Expression::Block(vec![])));
                    (
                        e.span(),
                        Box::new(Expression::Function {
                            docs,
                            attrs: vec![],
                            name,
                            is_coro: is_coro.is_some(),
                            is_static: is_static.is_some(),
                            type_params,
                            args,
                            returns,
                            where_constraints,
                            body: Some(empty_block),
                        }),
                    )
                },
            )
    }

    /// `attr Name<T>(target, extras..., ...args) -> R { body }`
    fn attr_decl(
        &self,
    ) -> impl Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>> + Clone + 'pratt
    {
        self.docs_prefix()
            .then(
                keyword!("attr")
                    .ignore_then(text::ident().padded())
                    .then(self.type_param_list())
                    .then(self.arg_list_typed(self.type_annotation()))
                    .then(op!("->").ignore_then(self.type_annotation()).or_not())
                    .then(self.where_clause())
                    .then(self.block(self.statement())),
            )
            .map_with(
                |(docs, (((((name, type_params), args), returns), where_constraints), body)), e| {
                    (
                        e.span(),
                        Box::new(Expression::AttrDecl {
                            docs,
                            name,
                            type_params,
                            args,
                            returns,
                            where_constraints,
                            body,
                        }),
                    )
                },
            )
    }

    fn func<
        T: Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>>
            + Clone
            + 'pratt,
    >(
        &self,
        stmt: T,
    ) -> impl Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>> + Clone + 'pratt
    {
        self.docs_prefix()
            .then(self.func_after_docs(stmt))
            .map_with(|(docs, mut func), e| {
                if let Expression::Function { docs: d, .. } = func.1.as_mut() {
                    *d = docs;
                }
                (e.span(), func.1)
            })
    }

    /// `#[…] async? static? fn …` without a leading `///` prefix (docs applied by callers).
    fn func_after_docs<
        T: Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>>
            + Clone
            + 'pratt,
    >(
        &self,
        stmt: T,
    ) -> impl Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>> + Clone + 'pratt
    {
        self.attr_list()
            .then(keyword!("async").or_not())
            .then(keyword!("static").or_not())
            .then(keyword!("fn"))
            .then(text::ident().padded())
            .then(self.type_param_list())
            .then(self.arg_list())
            .then(op!("->").ignore_then(self.type_annotation()).or_not())
            .then(self.where_clause())
            .then(choice((self.block(stmt).map(Some), op!(";").to(None))))
            .map_with(|full, e| {
                let (
                    (
                        (
                            ((((((attrs, is_coro), is_static), _), name), type_params), args),
                            returns,
                        ),
                        where_constraints,
                    ),
                    body,
                ) = full;
                (
                    e.span(),
                    Box::new(Expression::Function {
                        docs: Vec::new(),
                        attrs,
                        name,
                        is_coro: is_coro.is_some(),
                        is_static: is_static.is_some(),
                        type_params,
                        args,
                        returns,
                        where_constraints,
                        body,
                    }),
                )
            })
    }

    fn yield_expr_<
        T: Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>>
            + Clone
            + 'pratt,
    >(
        &self,
        expr: T,
    ) -> impl Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>> + Clone + 'pratt
    {
        keyword!("yield").ignore_then(choice((
            keyword!("from")
                .ignore_then(expr.clone())
                .map_with(output!(YieldFrom)),
            expr.map_with(output!(Yield)),
        )))
    }

    fn resume_<
        T: Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>>
            + Clone
            + 'pratt,
    >(
        &self,
        expr: T,
    ) -> impl Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>> + Clone + 'pratt
    {
        keyword!("resume")
            .ignore_then(expr.clone())
            .then(keyword!("with").ignore_then(expr).or_not())
            .map_with(|(target, arg), e| (e.span(), Box::new(Expression::Resume(target, arg))))
    }

    /// `defer { … }` or `defer use (a, b) { … }`.
    ///
    /// Optional `use (id, …)` after `defer` lists explicit captures from the
    /// enclosing function (same keyword and list shape as lambda captures).
    fn defer<
        T: Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>>
            + Clone
            + 'pratt,
    >(
        &self,
        stmt: T,
    ) -> impl Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>> + Clone + 'pratt
    {
        let captures = keyword!("use")
            .ignore_then(
                text::ident()
                    .padded()
                    .separated_by(op!(','))
                    .allow_trailing()
                    .collect::<Vec<_>>()
                    .delimited_by(op!("("), op!(")")),
            )
            .or_not()
            .map(|opt| opt.unwrap_or_default());

        keyword!("defer")
            .ignore_then(captures)
            .then(self.block(stmt))
            .map_with(|(captures, body), e| {
                (e.span(), Box::new(Expression::Defer { captures, body }))
            })
    }

    fn while_<
        S: Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>>
            + Clone
            + 'pratt,
        E: Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>>
            + Clone
            + 'pratt,
    >(
        &self,
        stmt: S,
        expr: E,
    ) -> impl Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>> + Clone + 'pratt
    {
        keyword!("while")
            .ignore_then(expr)
            .then(self.block(stmt))
            .map_with(|(iterable, body), e| {
                (
                    e.span(),
                    Box::new(Expression::Loop {
                        identifier: None,
                        iterable,
                        body,
                    }),
                )
            })
    }

    fn for_<
        S: Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>>
            + Clone
            + 'pratt,
        E: Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>>
            + Clone
            + 'pratt,
    >(
        &self,
        stmt: S,
        expr: E,
    ) -> impl Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>> + Clone + 'pratt
    {
        // C-style: `for (init?; cond; step?) { body }`
        let init = choice((self.variable(expr.clone()), expr.clone())).or_not();
        let step = expr.clone().or_not();
        let c_style = init
            .then_ignore(op!(";"))
            .then(expr.clone())
            .then_ignore(op!(";"))
            .then(step)
            .delimited_by(op!("("), op!(")"))
            .then(self.block(stmt.clone()))
            .map_with(|(((init, cond), step), body), e| {
                (
                    e.span(),
                    Box::new(Expression::For {
                        init,
                        cond,
                        step,
                        body,
                    }),
                )
            });

        // For-in: `for x in expr { body }` → Loop { identifier: Some(x), … }
        let for_in = text::ident()
            .padded()
            .map_with(output!(Identifier))
            .then_ignore(keyword!("in"))
            .then(expr)
            .then(self.block(stmt))
            .map_with(|((identifier, iterable), body), e| {
                (
                    e.span(),
                    Box::new(Expression::Loop {
                        identifier: Some(identifier),
                        iterable,
                        body,
                    }),
                )
            });

        // Prefer the paren form so `for (…)` never misparses as for-in.
        keyword!("for").ignore_then(choice((c_style, for_in)))
    }

    fn if_<
        S: Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>>
            + Clone
            + 'pratt,
        E: Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>>
            + Clone
            + 'pratt,
    >(
        &self,
        stmt: S,
        expr: E,
    ) -> impl Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>> + Clone + 'pratt
    {
        // `recursive` enables `else if` to call back into this parser.
        recursive(|if_parser| {
            keyword!("if")
                .ignore_then(expr.clone().labelled("condition"))
                .then(self.block(stmt.clone()).labelled("block `{ ... }`"))
                .then(
                    keyword!("else")
                        .ignore_then(choice((
                            // `else { body }` — a Block.
                            self.block(stmt.clone()).labelled("block `{ ... }`"),
                            // `else if ...` — recurse into the if-parser.
                            if_parser,
                        )))
                        .or_not(),
                )
                .map_with(|((cond, body), else_clause), e| {
                    let then_branch: Output =
                        (e.span(), Box::new(Expression::Branch(Some(cond), body)));
                    let mut branches: Vec<Output> = vec![then_branch];
                    if let Some(else_output) = else_clause {
                        match else_output.1.as_ref() {
                            // `else if c2 {b2} [else {b3} ...]` — the
                            // inner `if_parser` returned a fully-
                            // formed If whose branches we flatten
                            // into ours.
                            Expression::If(more_branches) => {
                                branches.extend(more_branches.iter().cloned());
                            }
                            // `else { body }` — a Block. Wrap as the
                            // terminal Branch(None, body).
                            _ => {
                                branches.push((
                                    e.span(),
                                    Box::new(Expression::Branch(None, else_output)),
                                ));
                            }
                        }
                    }
                    (e.span(), Box::new(Expression::If(branches)))
                })
        })
    }

    /// `done(handle)` — true when a coroutine has completed.
    fn done_<
        T: Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>>
            + Clone
            + 'pratt,
    >(
        &self,
        expr: T,
    ) -> impl Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>> + Clone + 'pratt
    {
        text::keyword("done")
            .labelled("done builtin")
            .ignore_then(
                expr.clone()
                    .separated_by(op!(','))
                    .allow_trailing()
                    .at_least(1)
                    .at_most(1)
                    .collect::<Vec<_>>()
                    .delimited_by(op!('('), op!(')')),
            )
            .map_with(|args, e| {
                (
                    e.span(),
                    Box::new(Expression::Done(args.into_iter().next().unwrap())),
                )
            })
    }

    fn return_<
        T: Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>>
            + Clone
            + 'pratt,
    >(
        &self,
        expr: T,
    ) -> impl Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>> + Clone + 'pratt
    {
        keyword!("return")
            .labelled("return")
            .ignore_then(expr.or_not())
            .map_with(|opt, e| {
                let span = e.span();
                let result = opt.unwrap_or_else(|| (span, Box::new(Expression::Tuple(Vec::new()))));
                (span, Box::new(Expression::Return(result)))
            })
    }

    fn raise_<
        T: Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>>
            + Clone
            + 'pratt,
    >(
        &self,
        expr: T,
    ) -> impl Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>> + Clone + 'pratt
    {
        keyword!("raise")
            .labelled("raise")
            .ignore_then(expr)
            .map_with(|result, e| (e.span(), Box::new(Expression::Raise(result))))
    }

    fn panic_<
        T: Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>>
            + Clone
            + 'pratt,
    >(
        &self,
        expr: T,
    ) -> impl Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>> + Clone + 'pratt
    {
        keyword!("panic")
            .labelled("panic")
            .ignore_then(expr)
            .map_with(|result, e| (e.span(), Box::new(Expression::Panic(result))))
    }

    fn comment(
        &self,
    ) -> impl Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>> + Clone + 'pratt
    {
        // `//` that is not `///` — doc comments are handled by `docs_prefix`.
        just("//")
            .then_ignore(just('/').not())
            .ignore_then(none_of('\n').repeated().to_slice())
            .then_ignore(just('\n').or_not())
            .map_with(|text: &str, e| {
                let text = text.strip_prefix(' ').unwrap_or(text);
                (e.span(), Box::new(Expression::Comment(text)))
            })
            .padded()
    }

    /// One `///` doc line; returns the body without the `///` prefix.
    fn doc_comment_line(
        &self,
    ) -> impl Parser<'pratt, &'pratt str, &'pratt str, extra::Err<Rich<'pratt, char>>> + Clone + 'pratt
    {
        just("///")
            .ignore_then(none_of('\n').repeated().to_slice())
            .then_ignore(just('\n').or_not())
            .map(|text: &str| text.strip_prefix(' ').unwrap_or(text))
            .padded()
    }

    /// Zero or more leading `///` lines before a declaration.
    fn docs_prefix(
        &self,
    ) -> impl Parser<'pratt, &'pratt str, Vec<&'pratt str>, extra::Err<Rich<'pratt, char>>> + Clone + 'pratt
    {
        self.doc_comment_line().repeated().collect()
    }

    /// Bare `///` not followed by a documentable item — hard error.
    fn orphan_doc_comment(
        &self,
    ) -> impl Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>> + Clone + 'pratt
    {
        self.doc_comment_line()
            .repeated()
            .at_least(1)
            .collect::<Vec<_>>()
            .try_map(|_docs, span| {
                Err(Rich::custom(
                    span,
                    "doc comment (`///`) must immediately precede a declaration",
                ))
            })
    }

    fn expr_statement<
        T: Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>>
            + Clone
            + 'pratt,
    >(
        &self,
        expr: T,
    ) -> impl Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>> + Clone + 'pratt
    {
        expr.then_ignore(op!(';'))
            .map_with(output!(ExprStatement))
    }

    fn break_(
        &self,
    ) -> impl Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>> + Clone + 'pratt
    {
        keyword!("break")
            .then_ignore(op!(";"))
            .map_with(|_, e| (e.span(), Box::new(Expression::Break)))
    }

    fn continue_(
        &self,
    ) -> impl Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>> + Clone + 'pratt
    {
        keyword!("continue")
            .then_ignore(op!(";"))
            .map_with(|_, e| (e.span(), Box::new(Expression::Continue)))
    }

    fn statement(
        &self,
    ) -> impl Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>> + Clone + 'pratt
    {
        self.statement_with_expr(self.expr())
    }

    fn statement_with_expr<
        T: Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>>
            + Clone
            + 'pratt,
    >(
        &self,
        expr: T,
    ) -> impl Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>> + Clone + 'pratt
    {
        recursive(|stmt| {
            choice((
                self.break_(),
                self.continue_(),
                self.for_(stmt.clone(), expr.clone()),
                self.while_(stmt.clone(), expr.clone()),
                self.if_(stmt.clone(), expr.clone()),
                self.block(stmt.clone()),
                self.type_alias(),
                self.variable(expr.clone()).then_ignore(op!(';')),
                self.constant(expr.clone()).then_ignore(op!(';')),
                // Statement keywords before `expr_statement`: otherwise
                // `return -1;` parses as `Sub(Identifier("return"), 1)`.
                self.return_(expr.clone()).then_ignore(op!(';')),
                self.raise_(expr.clone()).then_ignore(op!(';')),
                self.panic_(expr.clone()).then_ignore(op!(';')),
                self.yield_expr_(expr.clone()).then_ignore(op!(';')),
                // `defer { … }` before `expr_statement` so `defer` is not
                // parsed as a bare identifier call / expression.
                self.defer(stmt.clone()),
                self.expr_statement(expr.clone()),
                self.comment(),
                self.orphan_doc_comment(),
            ))
        })
        .map_with(output!(Statement))
    }

    fn declaration(
        &self,
    ) -> impl Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>> + Clone + 'pratt
    {
        let stmt = self.statement();

        // `enum_decl` is registered before `variable()` (which lives
        // inside `stmt`) so a leading `enum` keyword is not
        // mis-parsed as `let`.
        //
        // Ordering notes:
        //  - `typeclass_decl` before `type_alias` so `trait` keyword
        //    is not confused with a user identifier.
        //  - `trait_impl_for_block` (`impl Trait for T` / `impl Trait<A,B> for T`)
        //    is tried before other `impl` forms so `for` is unambiguous.
        //  - `impl_block` handles inherent impls and simple trait impls
        //    (`impl Num<int>`, `impl Show<Point>`) via the type-arg heuristic.
        //  - `typeclass_impl_block` is the fallback for complex type-annotation
        //    args (e.g. `impl Foo<Option<int>>`) that `type_param_list()` inside
        //    `impl_block` cannot parse.  Chumsky 0.12 backtracks on failure, so
        //    `impl_block` failing (after consuming `impl Name`) causes `choice` to
        //    retry with `typeclass_impl_block`.
        choice((
            self.static_decl(),
            self.class(),
            self.typeclass_decl(stmt.clone()),
            self.trait_impl_for_block(stmt.clone()),
            self.impl_block(stmt.clone()),
            self.typeclass_impl_block(stmt.clone()),
            self.test_case(stmt.clone()),
            self.attr_decl(),
            self.func(stmt.clone()),
            self.type_alias(),
            self.use_(),
            self.mod_(),
            self.enum_decl(),
            self.defer(stmt.clone()),
            self.extern_struct(),
            self.extern_block(),
            self.orphan_doc_comment(),
            stmt.clone(),
        ))
    }

    /// `test("description") { … }` — harness test case declaration.
    fn test_case<
        T: Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>>
            + Clone
            + 'pratt,
    >(
        &self,
        stmt: T,
    ) -> impl Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>> + Clone + 'pratt
    {
        keyword!("test")
            .ignore_then(self.expr().delimited_by(op!("("), op!(")")))
            .then(self.block(stmt))
            .map_with(|(name, body), e| (e.span(), Box::new(Expression::TestCase { name, body })))
            .labelled("test case")
    }

    /// `type Name = T;` — type alias declaration.
    fn type_alias(
        &self,
    ) -> impl Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>> + Clone + 'pratt
    {
        self.docs_prefix()
            .then(
                keyword!("type")
                    .ignore_then(text::ident().padded())
                    .then(self.type_param_list())
                    .then_ignore(op!("="))
                    .then(self.type_annotation())
                    .then_ignore(op!(";")),
            )
            .map_with(|(docs, ((name, type_params), ty)), e| {
                (
                    e.span(),
                    Box::new(Expression::TypeAlias {
                        docs,
                        name,
                        type_params,
                        ty: Box::new(ty),
                    }),
                )
            })
            .labelled("type alias")
    }

    /// `use path::item;`, `use path::item as alias;`, `use path::*;`,
    /// or `use path::{a, b as c};` (brace-group import).
    fn use_(
        &self,
    ) -> impl Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>> + Clone + 'pratt
    {
        let segment = text::ident().padded();

        // One item inside `{ … }`: `name`, `path::name`, or either with `as`.
        let brace_item = text::ident()
            .padded()
            .then(
                op!("::")
                    .ignore_then(text::ident().padded().map(|s: &str| s.to_string()))
                    .repeated()
                    .collect::<Vec<String>>(),
            )
            .then(
                keyword!("as")
                    .ignore_then(text::ident().padded())
                    .map(|s: &str| s.to_string())
                    .or_not(),
            )
            .map(
                |((first, mut rest), alias): ((&str, Vec<String>), Option<String>)| {
                    rest.insert(0, first.to_string());
                    (rest, alias)
                },
            );

        // Empty `{ }` is a parse error (silent no-op would hide typos).
        let brace_group = just('{')
            .padded()
            .ignore_then(
                brace_item
                    .separated_by(op!(","))
                    .allow_trailing()
                    .at_least(1)
                    .collect::<Vec<_>>(),
            )
            .then_ignore(just('}').padded());

        // After the first ident: zero or more `::ident`, then either
        // `::{…}`, `::*`, or end-of-path (+ optional `as`).
        let path_middle = op!("::")
            .ignore_then(text::ident().padded().map(|s: &str| s.to_string()))
            .repeated()
            .collect::<Vec<String>>();

        #[derive(Clone)]
        enum EndKind {
            Brace(Vec<(Vec<String>, Option<String>)>),
            Glob,
            Concrete(Option<String>),
        }

        let path_end = choice((
            // `::{a, b as c}`
            op!("::").ignore_then(brace_group).map(EndKind::Brace),
            // `::*`
            op!("::").ignore_then(just('*').padded()).to(EndKind::Glob),
            // bare end — optional `as alias`
            keyword!("as")
                .ignore_then(text::ident().padded())
                .map(|s: &str| s.to_string())
                .or_not()
                .map(EndKind::Concrete),
        ));

        keyword!("use")
            .ignore_then(segment.then(path_middle).then(path_end))
            .then_ignore(op!(";"))
            .map_with(
                |((first, middle), end): ((&str, Vec<String>), EndKind), e| {
                    let span = e.span();
                    match end {
                        EndKind::Brace(items) => {
                            // `use foo::bar::{a, b as c};` → path = [foo, bar]
                            let mut path = Vec::with_capacity(1 + middle.len());
                            path.push(first.to_string());
                            path.extend(middle);
                            let children: Vec<Output<'pratt>> = items
                                .into_iter()
                                .map(|(mut item_path, alias)| {
                                    let name = item_path.pop().expect("brace item has a name");
                                    let mut child_path = path.clone();
                                    child_path.extend(item_path);
                                    (
                                        span,
                                        Box::new(Expression::Use {
                                            path: child_path,
                                            name,
                                            alias,
                                        }),
                                    )
                                })
                                .collect();
                            if children.len() == 1 {
                                children.into_iter().next().unwrap()
                            } else {
                                (span, Box::new(Expression::Fragment(children)))
                            }
                        }
                        EndKind::Glob => {
                            let mut path = Vec::with_capacity(1 + middle.len());
                            path.push(first.to_string());
                            path.extend(middle);
                            (
                                span,
                                Box::new(Expression::Use {
                                    path,
                                    name: "*".to_string(),
                                    alias: None,
                                }),
                            )
                        }
                        EndKind::Concrete(alias) => {
                            // `use foo;` / `use foo::bar;` / `use foo::bar as x;`
                            let mut segs = Vec::with_capacity(1 + middle.len());
                            segs.push(first.to_string());
                            segs.extend(middle);
                            let name = segs.pop().expect("at least the leading ident");
                            (
                                span,
                                Box::new(Expression::Use {
                                    path: segs,
                                    name,
                                    alias,
                                }),
                            )
                        }
                    }
                },
            )
            .labelled("use statement")
    }

    /// `mod name;` — forward module declaration (loads the file; does not import items).
    fn mod_(
        &self,
    ) -> impl Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>> + Clone + 'pratt
    {
        keyword!("mod")
            .ignore_then(text::ident().padded().map(|s: &str| s))
            .then_ignore(op!(";"))
            .map_with(|name: &'pratt str, e| {
                let noop_span = e.span();
                // Noop wraps an Output, which wraps a Box<Expression>.
                // We use a leaf Integer(0) as the inner expression.
                // The pipeline doesn't traverse the body; the
                // name is all that matters.
                let inner: Output = (noop_span, Box::new(Expression::Integer(0)));
                let body: Output = (noop_span, Box::new(Expression::Noop(inner)));
                (
                    e.span(),
                    Box::new(Expression::Module(name.to_string(), body)),
                )
            })
            .labelled("mod declaration")
    }

    /// `extern struct Name { field: type, ... };` — C-layout FFI struct.
    fn extern_struct(
        &self,
    ) -> impl Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>> + Clone + 'pratt
    {
        use crate::ast::ExternStructDecl;
        let field = text::ident()
            .padded()
            .then_ignore(op!(":"))
            .then(self.type_annotation())
            .map_with(|(name, ty), _e| (name.to_string(), ty));

        keyword!("extern")
            .ignore_then(keyword!("struct"))
            .ignore_then(text::ident().padded())
            .then(
                field
                    .separated_by(op!(","))
                    .allow_trailing()
                    .collect::<Vec<_>>()
                    .delimited_by(op!("{"), op!("}")),
            )
            .then_ignore(op!(";"))
            .map_with(|(name, fields), e| {
                (
                    e.span(),
                    Box::new(Expression::ExternStruct(ExternStructDecl { name, fields })),
                )
            })
            .labelled("extern struct declaration")
    }

    /// Extern-only parameter list: fixed `T name` args plus optional trailing bare `...`.
    /// Language rest (`T... name`) is rejected with a clear diagnostic.
    fn extern_arg_list(
        &self,
    ) -> impl Parser<'pratt, &'pratt str, (Output<'pratt>, bool), extra::Err<Rich<'pratt, char>>>
    + Clone
    + 'pratt {
        #[derive(Clone)]
        enum ExternArg<'a> {
            Fixed(Output<'a>),
            /// Matched `T... name` — rejected after the list is collected.
            IllegalRest,
        }

        let illegal_rest = self
            .type_annotation()
            .then_ignore(just("...").padded())
            .then(text::ident().padded())
            .to(ExternArg::IllegalRest);
        let fixed_arg = self
            .type_annotation()
            .then(text::ident().padded())
            .map_with(|(ty, name), e| {
                ExternArg::Fixed((
                    e.span(),
                    Box::new(Expression::Argument {
                        docs: Vec::new(),
                        ty: Some(ty),
                        name,
                        is_rest: false,
                    }),
                ))
            });
        // Prefer illegal-rest so `int... xs` is recognized (then rejected).
        let arg = illegal_rest.or(fixed_arg);

        let bare_only = just("...")
            .padded()
            .map_with(|_, e| ((e.span(), Box::new(Expression::Fragment(Vec::new()))), true));

        let fixed_then_ellipsis = arg
            .separated_by(op!(','))
            .at_least(1)
            .collect::<Vec<_>>()
            .then(
                op!(',')
                    .ignore_then(just("...").padded())
                    .or_not()
                    .map(|o| o.is_some()),
            )
            .try_map(|(args, variadic), span| {
                if args.iter().any(|a| matches!(a, ExternArg::IllegalRest)) {
                    return Err(Rich::custom(
                        span,
                        "use bare `...` for C varargs; `T... name` is only for language rest parameters",
                    ));
                }
                let fixed: Vec<Output<'_>> = args
                    .into_iter()
                    .filter_map(|a| match a {
                        ExternArg::Fixed(o) => Some(o),
                        ExternArg::IllegalRest => None,
                    })
                    .collect();
                Ok((
                    (span, Box::new(Expression::Fragment(fixed))),
                    variadic,
                ))
            });

        let empty = empty().map_with(|_, e| {
            (
                (e.span(), Box::new(Expression::Fragment(Vec::new()))),
                false,
            )
        });

        choice((bare_only, fixed_then_ellipsis, empty)).delimited_by(op!("("), op!(")"))
    }

    /// `extern "libname" { fn name(args) -> ret; ... }` — declare
    /// external (FFI) functions from a shared library.
    ///
    /// The block contains a list of zero-or-more function
    /// declarations with a trailing semicolon (no body). Each
    /// `fn name(args) -> ret;` inside the `{ ... }` produces an
    /// `ExternFunction` (a separate struct, not an `Expression`
    /// variant — extern functions are metadata, not runtime
    /// expressions). The whole block produces an
    /// `Expression::ExternBlock` carrying the library name and
    /// the list of declared functions.
    fn extern_block(
        &self,
    ) -> impl Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>> + Clone + 'pratt
    {
        use crate::ast::ExternFunction;
        // The `fn name(args) -> ret;` sub-parser produces
        // `ExternFunction` directly (not an `Output`). The
        // declaration chain accepts parsers of any output type
        // as long as the final `map_with` produces an `Output`.
        let extern_function_decl = keyword!("fn")
            .then(text::ident().padded())
            .then(self.extern_arg_list())
            .then(op!("->").ignore_then(self.type_annotation()).or_not())
            // The trailing `;` is required (no body).
            .then_ignore(op!(";"))
            .map_with(
                |(((_, name), (args, variadic)), returns), _e| ExternFunction {
                    name,
                    symbol: None,
                    args,
                    returns,
                    variadic,
                },
            );

        // Inline string-literal parser for the library name.
        // We don't use `self.string()` because it returns an
        // `Output` (wrapping the value in an `Expression`),
        // but we just need the raw `String` for the library
        // name (it's metadata, not a runtime expression).
        let library_name = just('"')
            .ignore_then(self.string_lit_body())
            .then_ignore(just('"'))
            .map(|s: &'pratt str| s.to_string());

        keyword!("extern")
            .ignore_then(library_name)
            .then(
                extern_function_decl
                    .repeated()
                    .collect::<Vec<_>>()
                    .delimited_by(op!("{"), op!("}")),
            )
            .map_with(|(library, declarations), e| {
                (
                    e.span(),
                    Box::new(Expression::ExternBlock {
                        library,
                        declarations,
                    }),
                )
            })
    }

    /// Zero or more `#[attr]` / `#[attr(args)]` prefixes on a declaration.
    fn attr_list(
        &self,
    ) -> impl Parser<'pratt, &'pratt str, Vec<Attribute<'pratt>>, extra::Err<Rich<'pratt, char>>>
    + Clone
    + 'pratt {
        let attr_float = text::int(10)
            .then(just('.').then(text::int(10)))
            .to_slice()
            .from_str::<f64>()
            .validate(|v: Result<f64, _>, _, _| v.unwrap_or(0.0))
            .map(AttrLit::Float);

        let attr_lit = choice((
            just('"')
                .ignore_then(self.string_lit_body())
                .then_ignore(just('"'))
                .map(AttrLit::String),
            text::int(10)
                .to_slice()
                .from_str::<i64>()
                .validate(|v: Result<i64, _>, _, _| v.unwrap_or(0))
                .map(AttrLit::Int),
            attr_float,
            keyword!("true").to(AttrLit::Bool(true)),
            keyword!("false").to(AttrLit::Bool(false)),
        ));

        let attr_kv = text::ident().then_ignore(op!("=")).then(attr_lit.clone());

        let attr_args = choice((
            attr_kv
                .padded()
                .separated_by(op!(','))
                .at_least(1)
                .collect::<Vec<_>>()
                .map(AttrArgs::KeyValues),
            just('"')
                .ignore_then(self.string_lit_body())
                .then_ignore(just('"'))
                .map(AttrArgs::String),
            attr_lit
                .padded()
                .separated_by(op!(','))
                .at_least(1)
                .collect::<Vec<_>>()
                .map(AttrArgs::Positional),
            text::ident()
                .padded()
                .separated_by(op!(','))
                .at_least(1)
                .collect::<Vec<_>>()
                .map(AttrArgs::Idents),
        ))
        .delimited_by(op!("("), op!(")"));

        let attribute = text::ident()
            .then(attr_args.or_not())
            .map(|(name, args)| Attribute {
                name,
                args: args.unwrap_or(AttrArgs::Empty),
            });

        op!("#")
            .ignore_then(attribute.delimited_by(op!("["), op!("]")))
            .repeated()
            .collect::<Vec<_>>()
    }

    /// `class Name { [pub] field: Type, ... }`
    ///
    /// Fields are private by default; `pub` makes them public.
    fn class(
        &self,
    ) -> impl Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>> + Clone + 'pratt
    {
        self.docs_prefix()
            .then(self.attr_list())
            .then(keyword!("class"))
            .then(text::ident().padded())
            .then(self.type_param_list())
            .then(
                self.field_decl()
                    .separated_by(op!(','))
                    .allow_trailing()
                    .collect::<Vec<_>>()
                    .delimited_by(op!("{"), op!("}")),
            )
            .map_with(|(((((docs, attrs), _), name), type_params), fields), e| {
                (
                    e.span(),
                    Box::new(Expression::Class {
                        docs,
                        attrs,
                        name,
                        type_params,
                        fields,
                    }),
                )
            })
    }

    /// `[pub] [static|const] name: Type [= expr]` — class field declaration.
    fn field_decl(
        &self,
    ) -> impl Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>> + Clone + 'pratt
    {
        self.docs_prefix()
            .then(keyword!("pub").or_not())
            .then(
                choice((
                    just("static").padded().to(FieldModifier::Static),
                    keyword!("const").to(FieldModifier::Const),
                ))
                .or_not(),
            )
            .then(text::ident())
            .then_ignore(op!(":").labelled("':' before field type"))
            .then(self.type_annotation().labelled("field type"))
            .then(op!("=").ignore_then(self.expr()).or_not())
            .map_with(|(((((docs, vis), modifier), name), ty), init), e| {
                let visibility = if vis.is_some() {
                    Visibility::Public
                } else {
                    Visibility::Private
                };
                let modifier = modifier.unwrap_or(FieldModifier::Instance);
                let name_output: Output = (e.span(), Box::new(Expression::Identifier(name)));
                (
                    e.span(),
                    Box::new(Expression::Field {
                        docs,
                        visibility,
                        modifier,
                        name: name_output,
                        ty,
                        init,
                    }),
                )
            })
            .labelled("class field (`name: Type`)")
    }

    /// Top-level `static let` / `static const` singleton binding.
    fn static_decl(
        &self,
    ) -> impl Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>> + Clone + 'pratt
    {
        just("static")
            .padded()
            .ignore_then(choice((
                keyword!("const").to(true),
                just("let").padded().to(false),
            )))
            .then(text::ident().padded())
            .then(op!(":").ignore_then(self.type_annotation()).or_not())
            .then_ignore(op!("="))
            .then(self.expr())
            .then_ignore(op!(";"))
            .map_with(|(((is_const, name), ty), init), e| {
                (
                    e.span(),
                    Box::new(Expression::StaticDecl {
                        is_const,
                        name,
                        ty,
                        init,
                    }),
                )
            })
            .labelled("static declaration")
    }

    /// `ClassName::member` — static field access (not enum constructor).
    fn qualified_access(
        &self,
    ) -> impl Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>> + Clone + 'pratt
    {
        text::ident()
            .then_ignore(just("::").padded())
            .then(text::ident())
            .then_ignore(choice((op!("("), op!("{"))).not())
            .map_with(|(owner, member), e| {
                (
                    e.span(),
                    Box::new(Expression::QualifiedAccess { owner, member }),
                )
            })
            .labelled("qualified access")
    }

    /// `readonly [a, b, …]` array literal.
    fn readonly_array_atom<
        T: Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>>
            + Clone
            + 'pratt,
    >(
        &self,
        expr: T,
    ) -> impl Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>> + Clone + 'pratt
    {
        choice((
            keyword!("readonly")
                .ignore_then(self.array_atom(expr.clone()))
                .map_with(|inner, e| (e.span(), Box::new(Expression::Readonly(inner)))),
            self.array_atom(expr),
        ))
    }

    /// `new readonly Class(args)` or `readonly new Class(args)`.
    fn readonly_instantiate<
        T: Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>>
            + Clone
            + 'pratt,
    >(
        &self,
        expr: T,
    ) -> impl Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>> + Clone + 'pratt
    {
        let new_then_class = keyword!("new")
            .ignore_then(text::ident())
            .then(self.params(expr.clone()));
        let readonly_new = keyword!("readonly")
            .ignore_then(new_then_class.clone())
            .map_with(|(class, args), e| {
                let class_output = (e.span(), Box::new(Expression::Identifier(class)));
                (
                    e.span(),
                    Box::new(Expression::Readonly((
                        e.span(),
                        Box::new(Expression::Instantiate(class_output, args)),
                    ))),
                )
            });
        let new_readonly = keyword!("new")
            .ignore_then(keyword!("readonly"))
            .ignore_then(text::ident())
            .then(self.params(expr))
            .map_with(|(class, args), e| {
                let class_output = (e.span(), Box::new(Expression::Identifier(class)));
                (
                    e.span(),
                    Box::new(Expression::Readonly((
                        e.span(),
                        Box::new(Expression::Instantiate(class_output, args)),
                    ))),
                )
            });
        choice((readonly_new, new_readonly))
    }

    /// `trait Name<T, U: Bound> { type Elem; fn sig(…) -> ret; fn default(…) { body } }`
    ///
    /// Each body item is either:
    /// - Associated type declaration: `type Elem;`
    /// - Signature-only method: `fn name(args) -> ret;`  (represented as a
    ///   `Function` with an empty `Block` body).
    /// - Default implementation: `fn name(args) -> ret { body }`.
    fn typeclass_decl<
        T: Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>>
            + Clone
            + 'pratt,
    >(
        &self,
        stmt: T,
    ) -> impl Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>> + Clone + 'pratt
    {
        // A method that ends in `;` is signature-only: emit an empty Block.
        let sig_only = self.func_sig().then_ignore(op!(";"));

        // A method with a full block body (the default implementation).
        let default_method = self.func(stmt);

        // Associated type declaration: `type Elem;` / `type Ref<T>;`
        let assoc_decl = keyword!("type")
            .ignore_then(text::ident().padded())
            .then(self.type_param_list())
            .then_ignore(op!(";"))
            .map_with(|(name, type_params), e| {
                (
                    e.span(),
                    Box::new(Expression::AssocTypeDecl { name, type_params }),
                )
            });

        self.docs_prefix()
            .then(
                keyword!("trait")
                    .ignore_then(text::ident().padded())
                    .then(self.type_param_list())
                    .then(
                        choice((assoc_decl, sig_only, default_method))
                            .padded()
                            .repeated()
                            .collect::<Vec<_>>()
                            .delimited_by(op!("{"), op!("}")),
                    ),
            )
            .map_with(|(docs, ((name, type_params), methods)), e| {
                (
                    e.span(),
                    Box::new(Expression::TypeClass {
                        docs,
                        name,
                        type_params,
                        methods,
                    }),
                )
            })
    }

    /// Preferred trait-instance form: `impl Trait for Type { … }` or
    /// `impl Trait<A, B> for Type { … }`.
    ///
    /// The type after `for` is prepended as the first type argument (Self
    /// slot), so `impl Show for Foo` ≡ `impl Show<Foo>` and
    /// `impl Thing<string, int> for Message` ≡ `impl Thing<Message, string, int>`.
    fn trait_impl_for_block<
        T: Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>>
            + Clone
            + 'pratt,
    >(
        &self,
        stmt: T,
    ) -> impl Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>> + Clone + 'pratt
    {
        let assoc_def = keyword!("type")
            .ignore_then(text::ident().padded())
            .then(self.type_param_list())
            .then_ignore(op!("="))
            .then(self.type_annotation())
            .then_ignore(op!(";"))
            .map_with(|((name, type_params), ty), e| {
                (
                    e.span(),
                    Box::new(Expression::AssocTypeDef {
                        name,
                        type_params,
                        ty: Box::new(ty),
                    }),
                )
            });

        let opt_bracket_args = self
            .type_annotation()
            .padded()
            .separated_by(op!(","))
            .allow_trailing()
            .at_least(1)
            .collect::<Vec<_>>()
            .delimited_by(op!("<"), op!(">"))
            .or_not()
            .map(|opt| opt.unwrap_or_default());

        keyword!("impl")
            .ignore_then(text::ident())
            .then(opt_bracket_args)
            .then_ignore(keyword!("for"))
            .then(self.type_annotation().padded())
            .then(
                choice((assoc_def, self.method_decl(stmt)))
                    .repeated()
                    .collect::<Vec<_>>()
                    .delimited_by(op!("{"), op!("}")),
            )
            .map_with(|(((class, bracket_args), for_ty), methods), e| {
                let mut args = Vec::with_capacity(bracket_args.len() + 1);
                args.push(for_ty);
                args.extend(bracket_args);
                (
                    e.span(),
                    Box::new(Expression::TypeClassImpl {
                        class,
                        args,
                        methods,
                    }),
                )
            })
    }

    /// Inherent `impl` block OR typeclass instance.
    ///
    /// After `impl Name`, an optional `<…>` section is parsed via
    /// `type_param_list()`.  The result is classified at map time:
    ///
    /// - No `<…>` → `Implementation` (inherent, no type params).
    /// - `<T>`, `<T: Num>` (type-parameter shape) → `Implementation`.
    /// - `<int>`, `<string>`, `<Point>`, etc. (concrete type args) →
    ///   `TypeClassImpl`.
    ///
    /// A bare angle-bracket name is treated as a type parameter when it has
    /// bounds (`T: Num`) or is a single uppercase letter (`T`, `U`). Multi-
    /// character names without bounds (`Point`, `int`) are concrete instance
    /// heads — including user enums for `impl Show<Point>`.
    ///
    /// For complex type-annotation args (e.g. `impl Foo<Option<int>>`),
    /// `typeclass_impl_block` is the fallback when `impl_block` fails to parse.
    fn impl_block<
        T: Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>>
            + Clone
            + 'pratt,
    >(
        &self,
        stmt: T,
    ) -> impl Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>> + Clone + 'pratt
    {
        // Known lowercase primitive type names — these can never be TypeParam
        // names; if they appear inside `<>`, the block is a typeclass impl.
        const PRIMITIVES: &[&str] = &["int", "float", "string", "bool", "void", "unit"];

        // Associated type definition: `type Elem = int;` / `type Ref<T> = T;`
        let assoc_def = keyword!("type")
            .ignore_then(text::ident().padded())
            .then(self.type_param_list())
            .then_ignore(op!("="))
            .then(self.type_annotation())
            .then_ignore(op!(";"))
            .map_with(|((name, type_params), ty), e| {
                (
                    e.span(),
                    Box::new(Expression::AssocTypeDef {
                        name,
                        type_params,
                        ty: Box::new(ty),
                    }),
                )
            });

        keyword!("impl")
            .ignore_then(text::ident())
            .then(self.type_param_list())
            .then(
                // Methods (and assoc type defs for typeclass impls) are
                // separated by juxtaposition (newlines / whitespace), not commas.
                choice((assoc_def, self.method_decl(stmt)))
                    .repeated()
                    .collect::<Vec<_>>()
                    .delimited_by(op!("{"), op!("}")),
            )
            .map_with(|((name, type_params), methods), e| {
                // Classify by inspecting parsed param names.
                let looks_like_type_param = |p: &TypeParam<'_>| -> bool {
                    if PRIMITIVES.contains(&p.name) {
                        return false;
                    }
                    // Bounded names are always type parameters (`T: Num`).
                    if !p.bounds.is_empty() {
                        return true;
                    }
                    // Single uppercase letter (`T`, `U`) — type-parameter shape.
                    let mut chars = p.name.chars();
                    matches!(chars.next(), Some(c) if c.is_uppercase()) && chars.next().is_none()
                };
                let is_typeclass_impl = !type_params.is_empty()
                    && type_params.iter().any(|p| !looks_like_type_param(p));
                if is_typeclass_impl {
                    // e.g. `impl Num<int>` / `impl Show<Point>` → typeclass instance.
                    // Re-wrap each param name as a bare Type annotation.
                    let args = type_params
                        .into_iter()
                        .map(|p| (e.span(), Box::new(Expression::Type(p.name))))
                        .collect();
                    (
                        e.span(),
                        Box::new(Expression::TypeClassImpl {
                            class: name,
                            args,
                            methods,
                        }),
                    )
                } else {
                    // e.g. `impl Cell {}` or `impl Cell<T>` or `impl Cell<T: Num>`.
                    (
                        e.span(),
                        Box::new(Expression::Implementation {
                            what: "",
                            owner: name,
                            type_params,
                            methods,
                        }),
                    )
                }
            })
    }

    /// Typeclass-impl block for complex type-annotation arguments, e.g.
    /// `impl Foo<Option<int>> { … }`.
    ///
    /// Each angle-bracket item is parsed as a full `type_annotation`, so
    /// this parser handles any well-formed type, including generics.  It is
    /// registered BEFORE `impl_block` in `declaration()` so that it wins for
    /// cases that `type_param_list` cannot represent.
    ///
    /// Bare uppercase idents (which look like type params) are accepted here
    /// too — if the user writes `impl Foo<T>` and `T` is ambiguous this
    /// parser will win only when it appears before `impl_block` in the
    /// `choice`; that ordering is intentional (inherent impls prefer
    /// `impl_block`).
    fn typeclass_impl_block<
        T: Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>>
            + Clone
            + 'pratt,
    >(
        &self,
        stmt: T,
    ) -> impl Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>> + Clone + 'pratt
    {
        // Associated type definition: `type Elem = int;` / `type Ref<T> = T;`
        let assoc_def = keyword!("type")
            .ignore_then(text::ident().padded())
            .then(self.type_param_list())
            .then_ignore(op!("="))
            .then(self.type_annotation())
            .then_ignore(op!(";"))
            .map_with(|((name, type_params), ty), e| {
                (
                    e.span(),
                    Box::new(Expression::AssocTypeDef {
                        name,
                        type_params,
                        ty: Box::new(ty),
                    }),
                )
            });

        keyword!("impl")
            .ignore_then(text::ident())
            .then(
                // Require a non-empty `<` type_annotation+ `>` — without
                // angle brackets this parser doesn't match and falls through
                // to `impl_block`.
                self.type_annotation()
                    .padded()
                    .separated_by(op!(","))
                    .allow_trailing()
                    .at_least(1)
                    .collect::<Vec<_>>()
                    .delimited_by(op!("<"), op!(">")),
            )
            .then(
                choice((assoc_def, self.method_decl(stmt)))
                    .repeated()
                    .collect::<Vec<_>>()
                    .delimited_by(op!("{"), op!("}")),
            )
            .map_with(|((class, args), methods), e| {
                (
                    e.span(),
                    Box::new(Expression::TypeClassImpl {
                        class,
                        args,
                        methods,
                    }),
                )
            })
    }

    /// `[#[attr]]* [pub] [#[attr]]* fn name(...) -> ret { body }` — a method
    /// declaration inside an `impl` block. `pub` may sit before or after attributes.
    fn method_decl<
        T: Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>>
            + Clone
            + 'pratt,
    >(
        &self,
        stmt: T,
    ) -> impl Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>> + Clone + 'pratt
    {
        self.docs_prefix()
            .then(self.attr_list())
            .then(keyword!("pub").or_not())
            .then(self.func_after_docs(stmt))
            .map_with(|(((docs, attrs_before), vis), mut func), e| {
                if let Expression::Function {
                    docs: d, attrs, ..
                } = func.1.as_mut()
                {
                    *d = docs;
                    if !attrs_before.is_empty() {
                        attrs.splice(0..0, attrs_before);
                    }
                }
                let visibility = if vis.is_some() {
                    Visibility::Public
                } else {
                    Visibility::Private
                };
                (e.span(), Box::new(Expression::Method(visibility, func)))
            })
    }

    /// `new ClassName(args)` — instantiation.
    ///
    /// Constructed as an atom-style parser so it can be embedded in
    /// expressions. Returns `(ClassName, args)` via `Instantiate`.
    fn instantiate<
        T: Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>>
            + Clone
            + 'pratt,
    >(
        &self,
        expr: T,
    ) -> impl Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>> + Clone + 'pratt
    {
        keyword!("new")
            .ignore_then(text::ident())
            .then(self.params(expr))
            .map_with(|(class, args), e| {
                let class_output = (e.span(), Box::new(Expression::Identifier(class)));
                (
                    e.span(),
                    Box::new(Expression::Instantiate(class_output, args)),
                )
            })
    }

    fn variable<
        T: Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>>
            + Clone
            + 'pratt,
    >(
        &self,
        expr: T,
    ) -> impl Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>> + Clone + 'pratt
    {
        let simple = keyword!("let")
            .ignore_then(text::ident())
            .then(op!(":").ignore_then(self.type_annotation()).or_not())
            .then(op!("=").ignore_then(expr.clone()).or_not())
            .map_with(|((name, ty), val), e| {
                let mut result = vec![(e.span(), Box::new(Expression::Variable(name, ty)))];
                if let Some(v) = val {
                    result.push(v);
                }
                (e.span(), Box::new(Expression::Fragment(result)))
            });

        // `let (a, b) = expr;` / `let { x, y } = expr;` — tried before
        // the simple `let name` form so `(` / `{` are not misread as
        // identifiers. Top-level LHS is tuple/record only (not a bare
        // binding — that stays on the simple path).
        let destructure = keyword!("let")
            .ignore_then(self.let_destructure_lhs())
            .then_ignore(op!("="))
            .then(expr)
            .map_with(|(pattern, rhs), e| {
                (
                    e.span(),
                    Box::new(Expression::LetDestructure { pattern, rhs }),
                )
            });

        choice((destructure, simple))
    }

    /// Top-level `let` destructure LHS: `(p, …)` or `{ field, … }` only.
    fn let_destructure_lhs(
        &self,
    ) -> impl Parser<'pratt, &'pratt str, LetPattern<'pratt>, extra::Err<Rich<'pratt, char>>>
    + Clone
    + 'pratt {
        choice((self.let_tuple_pattern(), self.let_record_pattern()))
    }

    fn let_tuple_pattern(
        &self,
    ) -> impl Parser<'pratt, &'pratt str, LetPattern<'pratt>, extra::Err<Rich<'pratt, char>>>
    + Clone
    + 'pratt {
        let inner = self.let_pattern();
        // Require a comma (or trailing comma) so `(a)` is not a
        // 1-tuple — same rule as tuple literals.
        let tuple_multi = inner
            .clone()
            .separated_by(op!(','))
            .at_least(2)
            .allow_trailing()
            .collect::<Vec<_>>()
            .delimited_by(op!('('), op!(')'));
        let tuple_trailing = inner
            .then_ignore(op!(','))
            .map(|p| vec![p])
            .delimited_by(op!('('), op!(')'));
        choice((tuple_multi, tuple_trailing)).map(LetPattern::Tuple)
    }

    fn let_record_pattern(
        &self,
    ) -> impl Parser<'pratt, &'pratt str, LetPattern<'pratt>, extra::Err<Rich<'pratt, char>>>
    + Clone
    + 'pratt {
        let field = text::ident()
            .padded()
            .then(op!(":").ignore_then(self.let_pattern()).or_not())
            .map(|(name, sub)| {
                let pattern = sub.unwrap_or(LetPattern::Binding { name });
                LetFieldPattern { name, pattern }
            });
        field
            .separated_by(op!(','))
            .allow_trailing()
            .collect::<Vec<_>>()
            .delimited_by(op!("{"), op!("}"))
            .validate(|fields, e, emitter| {
                if let Some(err) = duplicate_field_error(fields.iter().map(|f| f.name), e.span()) {
                    emitter.emit(err);
                }
                LetPattern::Record(fields)
            })
    }

    /// Nested irrefutable `let` pattern: `_`, binding, `(p, …)`, `{ field, … }`.
    fn let_pattern(
        &self,
    ) -> impl Parser<'pratt, &'pratt str, LetPattern<'pratt>, extra::Err<Rich<'pratt, char>>>
    + Clone
    + 'pratt {
        recursive(|pattern_parser| {
            let record_field = text::ident()
                .padded()
                .then(op!(":").ignore_then(pattern_parser.clone()).or_not())
                .map(|(name, sub)| {
                    let pattern = sub.unwrap_or(LetPattern::Binding { name });
                    LetFieldPattern { name, pattern }
                });

            let record = record_field
                .separated_by(op!(','))
                .allow_trailing()
                .collect::<Vec<_>>()
                .delimited_by(op!("{"), op!("}"))
                .validate(|fields, e, emitter| {
                    if let Some(err) =
                        duplicate_field_error(fields.iter().map(|f| f.name), e.span())
                    {
                        emitter.emit(err);
                    }
                    LetPattern::Record(fields)
                });

            let tuple_multi = pattern_parser
                .clone()
                .separated_by(op!(','))
                .at_least(2)
                .allow_trailing()
                .collect::<Vec<_>>()
                .delimited_by(op!('('), op!(')'));
            let tuple_trailing = pattern_parser
                .clone()
                .then_ignore(op!(','))
                .map(|p| vec![p])
                .delimited_by(op!('('), op!(')'));
            let tuple = choice((tuple_multi, tuple_trailing)).map(LetPattern::Tuple);

            choice((
                just("_").padded().to(LetPattern::Wildcard),
                tuple,
                record,
                text::ident()
                    .padded()
                    .map(|name| LetPattern::Binding { name }),
            ))
        })
    }

    fn constant<
        T: Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>>
            + Clone
            + 'pratt,
    >(
        &self,
        expr: T,
    ) -> impl Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>> + Clone + 'pratt
    {
        keyword!("const")
            .ignore_then(text::ident().map_with(output!(Identifier)))
            .then(op!(":").ignore_then(self.type_annotation()).or_not())
            .then_ignore(op!("="))
            .then(expr)
            .map_with(|((name, ty), val), e| {
                let result = vec![(e.span(), Box::new(Expression::Constant(name, ty))), val];
                (e.span(), Box::new(Expression::Fragment(result)))
            })
    }

    fn params<
        T: Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>>
            + Clone
            + 'pratt,
    >(
        &self,
        expr: T,
    ) -> impl Parser<
        'pratt,
        &'pratt str,
        Option<Vec<Output<'pratt>>>,
        extra::Err<Rich<'pratt, char>>,
    > + Clone
    + 'pratt {
        // Named call-site arg: `ident : expr` → `NamedArg`. Tried before
        // bare `expr` so `f(a: 1)` does not parse as a labelled type /
        // weird binary form. Positional `expr` still wins when there is
        // no colon after the identifier.
        let named = text::ident()
            .padded()
            .then_ignore(op!(":"))
            .then(expr.clone())
            .map_with(|(name, value), e| (e.span(), Box::new(Expression::NamedArg(name, value))))
            .labelled("named argument");
        let spread = op!("...")
            .ignore_then(expr.clone())
            .map_with(|inner, e| (e.span(), Box::new(Expression::Spread(inner))));
        let arg = spread.or(named).or(expr.clone());
        arg.separated_by(op!(','))
            .allow_trailing()
            .collect::<Vec<_>>()
            .or_not()
            .delimited_by(op!('('), op!(')'))
    }

    /// Tuple literal. Requires a comma inside the parens — `(1)` is a group, `(1,)` is a 1-tuple.
    fn tuple_atom<
        T: Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>>
            + Clone
            + 'pratt,
    >(
        &self,
        expr: T,
    ) -> impl Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>> + Clone + 'pratt
    {
        // A tuple is:
        //   - `()` (empty — used by zero-arity FFI declare/invoke),
        //   - `at_least(2)` items separated by commas, or
        //   - exactly 1 item with a trailing comma.
        // Non-empty forms still require a comma so `(1)` stays a group.
        use chumsky::Parser;
        let empty = op!('(')
            .ignore_then(op!(')'))
            .to(Vec::new())
            .labelled("empty tuple");
        let two_or_more = expr
            .clone()
            .separated_by(op!(','))
            .allow_trailing()
            .at_least(2)
            .collect::<Vec<_>>()
            .delimited_by(op!('('), op!(')'));
        let one_with_trailing = expr
            .clone()
            .then_ignore(op!(','))
            .map(|e| vec![e])
            .delimited_by(op!('('), op!(')'))
            .labelled("single-element tuple");
        choice((empty, two_or_more, one_with_trailing))
            .map_with(|items, e| (e.span(), Box::new(Expression::Tuple(items))))
            .labelled("tuple")
    }

    /// Anonymous record literal `{ name: expr, ... }`.
    fn dict_atom<
        T: Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>>
            + Clone
            + 'pratt,
    >(
        &self,
        expr: T,
    ) -> impl Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>> + Clone + 'pratt
    {
        use crate::ast::RecordFieldValue;
        use chumsky::Parser;
        // Each field: `name : expr`.
        let field = text::ident()
            .padded()
            .then_ignore(op!(":"))
            .then(expr)
            .map_with(|(name, value), e| (e.span(), RecordFieldValue { name, value }))
            .labelled("dict field");
        field
            .separated_by(op!(','))
            .allow_trailing()
            .collect::<Vec<_>>()
            .delimited_by(op!("{"), op!("}"))
            .validate(|fields, e, emitter| {
                let fs: Vec<RecordFieldValue<'pratt>> =
                    fields.into_iter().map(|(_, f)| f).collect();
                if let Some(err) = duplicate_field_error(fs.iter().map(|f| f.name), e.span()) {
                    emitter.emit(err);
                }
                (e.span(), Box::new(Expression::Dict(fs)))
            })
            .labelled("dict")
    }

    /// Array literal `[a, b, ...]`. Empty `[]` is allowed.
    fn array_atom<
        T: Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>>
            + Clone
            + 'pratt,
    >(
        &self,
        expr: T,
    ) -> impl Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>> + Clone + 'pratt
    {
        use chumsky::Parser;
        let inner = expr
            .clone()
            .separated_by(op!(','))
            .allow_trailing()
            .collect::<Vec<_>>();
        inner
            .delimited_by(op!('['), op!(']'))
            .map_with(|items, e| (e.span(), Box::new(Expression::Array(items))))
            .labelled("array")
    }

    /// `target[index]` postfix indexing helper. The actual
    /// wiring lives at the `pratt` call site below; this
    /// method is unused and reserved for future expansion
    /// (e.g., for explicit `slice(i, j)` syntax).
    #[allow(dead_code)]
    fn index_postfix_disabled<
        T: Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>>
            + Clone
            + 'pratt,
    >(
        &self,
        _expr: T,
    ) {
    }

    /// Qualified constructor `EnumName::Variant(...)`. Must appear before `call` in the atom choice.
    fn construct<
        T: Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>>
            + Clone
            + 'pratt,
    >(
        &self,
        expr: T,
    ) -> impl Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>> + Clone + 'pratt
    {
        // Record field: `name : expr`. Duplicate names emit a chumsky
        // error so fmt/parser tooling never round-trips illegal records.
        let record_field = text::ident()
            .padded()
            .then_ignore(op!(":"))
            .then(expr.clone())
            .map_with(|(name, value), e| (e.span(), RecordFieldValue { name, value }))
            .labelled("record field");

        let record_payload = record_field
            .separated_by(op!(','))
            .allow_trailing()
            .collect::<Vec<_>>()
            .delimited_by(op!("{"), op!("}"))
            .validate(|fields, e, emitter| {
                let fs: Vec<RecordFieldValue<'pratt>> =
                    fields.into_iter().map(|(_, f)| f).collect();
                if let Some(err) = duplicate_field_error(fs.iter().map(|f| f.name), e.span()) {
                    emitter.emit(err);
                }
                EnumConstructPayload::Record(fs)
            })
            .labelled("record payload");

        // Tuple payload: `(arg1, arg2, ...)` — `None` means Unit.
        // Empty parens `()` are also treated as Unit (so users can
        // write `Option::None()` instead of `Option::None`).
        let tuple_payload = self.params(expr.clone()).map(|opt| match opt {
            Some(args) if args.is_empty() => EnumConstructPayload::Unit,
            Some(args) => EnumConstructPayload::Tuple(args),
            None => EnumConstructPayload::Unit,
        });

        // Shape selector: tuple or record. Both are optional
        // (Unit is the default when nothing matches).
        let shape = choice((tuple_payload, record_payload)).or_not();

        // `Enum::Variant` or `ffi::types::Int` (multi-segment path;
        // last segment is the variant, the rest is the enum/module path).
        text::ident()
            .padded()
            .separated_by(just("::").padded())
            .at_least(2)
            .collect::<Vec<_>>()
            .then(shape)
            .map_with(|(segments, fields), e| {
                let mut segments = segments;
                let variant_name = segments.pop().unwrap();
                let enum_name = if segments.len() == 1 {
                    segments.pop().unwrap()
                } else {
                    // Leak into arena-less `'pratt` by joining into a
                    // single owned string stored via Box::leak for the
                    // AST lifetime — the parser AST borrows from the
                    // source, so multi-segment paths need a stable
                    // string. Join with `::` into a Cow isn't available
                    // here; use the source-backed approach: reconstruct
                    // from the collected idents.
                    //
                    // `segments` are `&str` slices into the source, but
                    // joining them requires an owned String. Store via
                    // the expression's span by using a concatenated
                    // owned string leaked for the duration of the parse
                    // (same pattern as other temporary AST strings is
                    // not used elsewhere — instead keep two-segment
                    // form when possible).
                    let joined = segments.join("::");
                    Box::leak(joined.into_boxed_str()) as &str
                };
                // `module::fn(...)` when both sides look like module/fn paths
                // (`string::format`). PascalCase owners stay Construct
                // (`Point::new`); PascalCase members stay Construct
                // (`ffi::types::Int`, `Option::Some`).
                if enum_name
                    .chars()
                    .next()
                    .is_some_and(|ch| ch.is_ascii_lowercase())
                    && variant_name
                        .chars()
                        .next()
                        .is_some_and(|ch| ch.is_ascii_lowercase())
                {
                    let name = (
                        e.span(),
                        Box::new(Expression::QualifiedAccess {
                            owner: enum_name,
                            member: variant_name,
                        }),
                    );
                    return match fields {
                        Some(EnumConstructPayload::Tuple(args)) => (
                            e.span(),
                            Box::new(Expression::Call {
                                name,
                                args: Some(args),
                            }),
                        ),
                        Some(EnumConstructPayload::Unit) | None => name,
                        Some(EnumConstructPayload::Record(_)) => (
                            e.span(),
                            Box::new(Expression::QualifiedAccess {
                                owner: enum_name,
                                member: variant_name,
                            }),
                        ),
                    };
                }
                (
                    e.span(),
                    Box::new(Expression::Construct {
                        enum_name,
                        variant_name,
                        fields: fields.unwrap_or(EnumConstructPayload::Unit),
                    }),
                )
            })
    }

    /// `match scrutinee { pat => body, ... }` — a pattern-match expression.
    /// Brace bodies accept statements plus an optional trailing expression;
    /// unbraced bodies are expressions. Patterns are parsed by
    /// [`Self::pattern`].
    ///
    /// Takes the recursive `expr` parser as a parameter (rather than
    /// calling `self.expr()`) so nested match expressions share the
    /// outer `recursive` group instead of spawning a fresh one on
    /// every call — which would overflow the stack at construction
    /// time.
    fn match_expr<
        E: Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>>
            + Clone
            + 'pratt,
        S: Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>>
            + Clone
            + 'pratt,
    >(
        &self,
        expr: E,
        stmt: S,
    ) -> impl Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>> + Clone + 'pratt
    {
        keyword!("match")
            .ignore_then(expr.clone())
            .then(
                self.arm(expr.clone(), stmt)
                    .separated_by(op!(','))
                    .allow_trailing()
                    .collect::<Vec<_>>()
                    .delimited_by(op!('{'), op!('}')),
            )
            .map_with(|(scrutinee, arms), e| {
                (e.span(), Box::new(Expression::Match { scrutinee, arms }))
            })
    }

    /// `pattern => expr` — one arm inside a `match` block.
    ///
    /// Arm bodies may be a brace block `{ statement; … trailing_expr }` or any
    /// other `expr`. The brace form is tried **before** the general `expr` so
    /// that `{ self.foo(); x }` is a block rather than a dict literal (dicts
    /// require `name: value` fields and would otherwise report `found '.'
    /// expected ':'` on `self.method()`).
    ///
    /// Returns a [`MatchArm`] directly (not an `Output`) because
    /// patterns are not expressions and don't carry a span.
    fn arm<
        E: Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>>
            + Clone
            + 'pratt,
        S: Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>>
            + Clone
            + 'pratt,
    >(
        &self,
        expr: E,
        stmt: S,
    ) -> impl Parser<'pratt, &'pratt str, MatchArm<'pratt>, extra::Err<Rich<'pratt, char>>>
    + Clone
    + 'pratt {
        // Use only handles built inside the surrounding `recursive(|expr| …)`.
        let brace_body = self.brace_body(stmt, expr.clone());

        self.pattern()
            .then_ignore(op!("=>"))
            .then(choice((brace_body, expr)))
            .map_with(|(pattern, body), _| MatchArm { pattern, body })
    }

    /// A match-arm pattern: wildcard, binding, or qualified constructor (tuple or record payload).
    fn pattern(
        &self,
    ) -> impl Parser<'pratt, &'pratt str, PatternOutput<'pratt>, extra::Err<Rich<'pratt, char>>> + Clone + 'pratt
    {
        recursive(|pattern_parser| {
            let record_pattern_field = text::ident()
                .padded()
                .then(op!(":").ignore_then(pattern_parser.clone()).or_not())
                .map_with(|(name, sub_pat), e| {
                    let pattern = match sub_pat {
                        Some(p) => p,
                        None => (e.span(), Pattern::Binding { name }),
                    };
                    PatternField { name, pattern }
                })
                .labelled("record pattern field");

            let record_pattern_payload = record_pattern_field
                .separated_by(op!(','))
                .allow_trailing()
                .collect::<Vec<_>>()
                .delimited_by(op!("{"), op!("}"))
                .validate(|fields, e, emitter| {
                    if let Some(err) =
                        duplicate_field_error(fields.iter().map(|f| f.name), e.span())
                    {
                        emitter.emit(err);
                    }
                    PatternPayload::Record(fields)
                })
                .labelled("record pattern payload");

            // `Enum::Variant(p1, p2, ...)` — the first ident must be
            // followed by `::`, otherwise this alternative fails and
            // the choice falls through to the binding alternative.
            //
            // Shape selector: nothing (Unit), tuple `(p1, p2)`,
            // record `{ name, name: pat, ... }`. Empty parens `()`
            // are treated as Unit (so `Option::None()` is
            // equivalent to `Option::None`).
            let tuple_payload = pattern_parser
                .clone()
                .separated_by(op!(','))
                .allow_trailing()
                .collect::<Vec<_>>()
                .delimited_by(op!('('), op!(')'))
                .map(|parts| {
                    if parts.is_empty() {
                        PatternPayload::Unit
                    } else {
                        PatternPayload::Tuple(parts)
                    }
                });

            let payload_choice = tuple_payload
                .or(record_pattern_payload)
                .or_not()
                .map(|opt| opt.unwrap_or(PatternPayload::Unit));

            let constructor = text::ident()
                .padded()
                .then_ignore(just("::").padded())
                .then(text::ident().padded())
                .then(payload_choice)
                .map_with(
                    |((enum_name, variant_name), payload), e| {
                        (
                            e.span(),
                            Pattern::Constructor {
                                enum_name,
                                variant_name,
                                payload,
                            },
                        )
                    },
                );

            choice((
                just("_")
                    .padded()
                    .map_with(|_, e| (e.span(), Pattern::Wildcard)),
                keyword!("default").map_with(|_, e| (e.span(), Pattern::Wildcard)),
                constructor,
                text::ident()
                    .padded()
                    .map_with(|name, e| (e.span(), Pattern::Binding { name })),
            ))
        })
    }

    /// `enum Name { Variant1, Variant2(T1, T2), ... }` — a top-level
    /// sum-type declaration. Registered in [`Self::declaration`]
    /// before `variable()` so a leading `enum` keyword isn't
    /// mis-parsed as `let`.
    ///
    /// Optional derive attribute: `#[derive(Show, Eq)] enum Name { … }`.
    fn enum_decl(
        &self,
    ) -> impl Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>> + Clone + 'pratt
    {
        self.docs_prefix()
            .then(self.attr_list())
            .then(keyword!("enum"))
            .then(text::ident().padded())
            .then(self.type_param_list())
            .then(
                self.enum_variant()
                    .separated_by(op!(','))
                    .allow_trailing()
                    .collect::<Vec<_>>()
                    .delimited_by(op!('{'), op!('}')),
            )
            .map_with(|(((((docs, attrs), _), name), type_params), variants), e| {
                (
                    e.span(),
                    Box::new(Expression::EnumDecl {
                        docs,
                        attrs,
                        name,
                        type_params,
                        variants,
                    }),
                )
            })
    }

    /// One variant inside an `enum` body (`Variant`, `Variant(T, ...)`, or `Variant { x: T, ... }`).
    fn enum_variant(
        &self,
    ) -> impl Parser<'pratt, &'pratt str, Output<'pratt>, extra::Err<Rich<'pratt, char>>> + Clone + 'pratt
    {
        // Record field: `name : Type` — the type is parsed via
        // `type_annotation()` so it can be generic (`Inner<T>`), an array,
        // or a tuple. Duplicate names are rejected at parse time.
        let record_field_decl = text::ident()
            .padded()
            .then_ignore(op!(":"))
            .then(self.type_annotation().padded())
            .map_with(|(name, value), _| RecordFieldDecl { name, value })
            .labelled("record field declaration");

        let record_payload_decl = record_field_decl
            .separated_by(op!(','))
            .allow_trailing()
            .collect::<Vec<_>>()
            .delimited_by(op!("{"), op!("}"))
            .validate(|fields, e, emitter| {
                if let Some(err) = duplicate_field_error(fields.iter().map(|f| f.name), e.span()) {
                    emitter.emit(err);
                }
                EnumVariantPayload::Record(fields)
            })
            .labelled("record variant payload");

        // Tuple payload: each element is a full type annotation so that
        // generic payloads like `Node(Tree<T>, Tree<T>)` are accepted.
        let tuple_payload_decl = self
            .type_annotation()
            .padded()
            .separated_by(op!(','))
            .allow_trailing()
            .collect::<Vec<_>>()
            .delimited_by(op!('('), op!(')'))
            .map(EnumVariantPayload::Tuple);

        let payload_choice = tuple_payload_decl
            .or(record_payload_decl)
            .or_not()
            .map(|opt| opt.unwrap_or(EnumVariantPayload::Unit));

        self.docs_prefix()
            .then(text::ident().padded())
            .then(payload_choice)
            .map_with(|((docs, name), payload), e| {
                (
                    e.span(),
                    Box::new(Expression::EnumVariant {
                        docs,
                        name,
                        payload,
                    }),
                )
            })
    }

    pub fn parse(&self, input: &'pratt str) -> Result<Output<'pratt>, Message> {
        match self
            .declaration()
            .repeated()
            .collect()
            .map_with(output!(Program))
            .or(self.comment())
            .parse(input)
            .into_result()
        {
            Err(errs) => {
                let primary_err = errs
                    .iter()
                    .find(|err| is_duplicate_field_parse_error(err))
                    .or(errs.first());
                let primary = primary_err
                    .map(|err| err.span().into_range())
                    .unwrap_or_default();
                let title = primary_err
                    .map(|err| format_parse_error_title(input, err))
                    .unwrap_or_else(|| "Parse error".to_string());
                let code = if errs.iter().any(is_duplicate_field_parse_error) {
                    ErrorCode::DuplicateField
                } else {
                    ErrorCode::ParseError
                };
                let mut message = Message::error(code, title, primary);

                errs.iter().for_each(|err| {
                    message.push(Label::new(
                        format_parse_error_label(input, err),
                        err.span().into_range(),
                    ));
                });

                if let Some(help) = primary_err.and_then(|err| parse_error_help(input, err)) {
                    message.with_help(help);
                }

                Err(message)
            }
            Ok(ast) => Ok(ast),
        }
    }
}

/// Prefer custom chumsky messages as the diagnostic title; fall back to a short summary.
fn format_parse_error_title(input: &str, err: &Rich<'_, char>) -> String {
    if let Some(msg) = missing_param_type_message(input, err) {
        return msg;
    }
    match err.reason() {
        RichReason::Custom(msg) => msg.to_string(),
        RichReason::ExpectedFound { expected, found } => {
            let expected_label = expected
                .iter()
                .find_map(|pat| {
                    let s = pat.to_string();
                    // Prefer labelled productions over raw token dumps.
                    if s.contains(' ') || s.contains('`') || s.starts_with('"') {
                        Some(s)
                    } else {
                        None
                    }
                })
                .or_else(|| expected.first().map(|p| p.to_string()));
            match (found.as_ref().map(|c| c.to_string()), expected_label) {
                (Some(found), Some(exp)) => format!("unexpected `{found}`, expected {exp}"),
                (None, Some(exp)) => format!("unexpected end of input, expected {exp}"),
                (Some(found), None) => format!("unexpected `{found}`"),
                (None, None) => "Parse error".to_string(),
            }
        }
    }
}

fn format_parse_error_label(input: &str, err: &Rich<'_, char>) -> String {
    if let Some(msg) = missing_param_type_message(input, err) {
        return msg;
    }
    match err.reason() {
        RichReason::Custom(msg) => msg.to_string(),
        _ => err.to_string(),
    }
}

fn parse_error_help(input: &str, err: &Rich<'_, char>) -> Option<String> {
    if missing_param_type_message(input, err).is_some() {
        return Some(
            "function parameters are written `Type name`, not `name` or `name: Type`".to_string(),
        );
    }
    match err.reason() {
        RichReason::Custom(msg) if msg.starts_with("Duplicate field `") => {
            Some("record fields must have unique names".to_string())
        }
        RichReason::Custom(msg)
            if msg.contains("missing a type") || msg.contains("name: Type") =>
        {
            Some(
                "function parameters are written `Type name`, not `name` or `name: Type`"
                    .to_string(),
            )
        }
        RichReason::ExpectedFound { expected, .. }
            if expected.iter().any(|p| {
                let s = p.to_string();
                s.contains("block") || s.contains("`{ ... }`")
            }) =>
        {
            Some("control-flow bodies use braces, e.g. `if cond { ... }`".to_string())
        }
        RichReason::ExpectedFound { expected, found }
            if matches!(found.as_ref().map(|c| **c), Some('}'))
                && expected.iter().any(|p| p.to_string().contains(':')) =>
        {
            Some("class fields are written `name: Type`".to_string())
        }
        _ => None,
    }
}

/// Detect missing / Rust-style parameter types from the chumsky failure and nearby source.
fn missing_param_type_message(input: &str, err: &Rich<'_, char>) -> Option<String> {
    match err.reason() {
        RichReason::Custom(msg)
            if msg.contains("missing a type") || msg.contains("name: Type") =>
        {
            return Some(msg.to_string());
        }
        RichReason::ExpectedFound { expected, found }
            if matches!(found.as_ref().map(|c| **c), Some(')') | Some(','))
                && expected.iter().any(|p| p.to_string() == "identifier")
                && expected.iter().any(|p| {
                    let s = p.to_string();
                    s.contains(':')
                        || s.contains('<')
                        || s.contains('.')
                        || s.contains("something else")
                }) =>
        {
            return Some(
                "function parameter is missing a type; write `Type name` (for example `int n`)"
                    .to_string(),
            );
        }
        _ => {}
    }

    // `fn f(n: int)` often fails after the colon with a confusing token error.
    // Recover the parameter name from the source prefix when it looks like `name:`.
    let start = err.span().start;
    let head = input.get(..start.min(input.len())).unwrap_or("");
    let trimmed = head.trim_end();
    if let Some(before_colon) = trimmed.strip_suffix(':') {
        let named = before_colon.trim_end();
        let name = named
            .rsplit(|c: char| !c.is_ascii_alphanumeric() && c != '_')
            .next()
            .unwrap_or("")
            .trim();
        let prefix = named
            .trim_end_matches(|c: char| c.is_ascii_alphanumeric() || c == '_')
            .trim_end();
        if !name.is_empty()
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            && (prefix.ends_with('(') || prefix.ends_with(','))
        {
            return Some(format!(
                "parameter `{name}` uses `name: Type` syntax; write `Type {name}` instead (for example `int {name}`)"
            ));
        }
    }
    None
}


#[cfg(test)]
#[path = "tests/tests.rs"]
mod tests;
#[cfg(test)]
#[path = "tests/tests_error_handling.rs"]
mod tests_error_handling;
#[cfg(test)]
#[path = "tests/tests_diagnostics.rs"]
mod tests_diagnostics;
#[cfg(test)]
#[path = "tests/tests_classes.rs"]
mod tests_classes;
#[cfg(test)]
#[path = "tests/tests_generics.rs"]
mod tests_generics;
#[cfg(test)]
#[path = "tests/tests_lambdas.rs"]
mod tests_lambdas;
