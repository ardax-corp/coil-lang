//! Attribute expansion (`#[derive(...)]`, user `attr`, etc.).
//!
//! Runs before the ID pre-walk and typechecking: expands `#[derive]` into
//! synthetic `TypeClassImpl` siblings. Compile-time FFI is `extern "lib" { fn …; }`
//! only — `#[ffi]` is rejected.

use std::collections::{HashMap, HashSet};

use parser::{
    SimpleSpan,
    ast::{
        AttrArgs, AttrLit, Attribute, EnumConstructPayload, EnumVariantPayload, Expression,
        ExternFunction, ExternStructDecl, LetPattern, MatchArm, Output, Pattern, PatternField,
        PatternPayload, RecordFieldDecl, RecordFieldValue, Visibility,
    },
};
use reporting::{ErrorCode, Message};

type PatternOut<'a> = (SimpleSpan, Pattern<'a>);

fn span_pat<'a>(span: SimpleSpan, pattern: Pattern<'a>) -> PatternOut<'a> {
    (span, pattern)
}

fn wildcard_tuple<'a>(span: SimpleSpan, arity: usize) -> PatternPayload<'a> {
    PatternPayload::Tuple(vec![(span, Pattern::Wildcard); arity])
}

fn record_wildcard_fields<'a>(span: SimpleSpan, fields: &[&'a str]) -> PatternPayload<'a> {
    PatternPayload::Record(
        fields
            .iter()
            .map(|fname| PatternField {
                name: fname,
                pattern: span_pat(span, Pattern::Wildcard),
            })
            .collect(),
    )
}

/// Builtin traits the compiler knows how to synthesize.
const DERIVABLE: &[&str] = &[
    "Show",
    "Eq",
    "Ord",
    "Default",
    "Hash",
    "Serialize",
    "Deserialize",
    "Send",
    "String",
    "Sensitive",
];

const KNOWN_ATTRS: &[&str] = &["derive", "ffi", "test", "max_depth", "repr"];

/// Result of attribute expansion before typechecking.
#[derive(Default, Clone)]
pub struct ExpandResult {
    pub messages: Vec<Message>,
    /// Class name → decorated constructor function name.
    pub decorated_class_ctors: HashMap<String, String>,
}

/// Expand every supported attribute on a program AST.
pub fn expand_program(ast: &mut Output<'_>) -> ExpandResult {
    let Expression::Program(children) = ast.1.as_mut() else {
        return ExpandResult::default();
    };
    let mut user_attrs = HashSet::new();
    let mut attr_extra_names: HashMap<String, Vec<String>> = HashMap::new();
    let mut messages = Vec::new();
    collect_and_desugar_attr_decls(
        children,
        &mut user_attrs,
        &mut attr_extra_names,
        &mut messages,
    );
    let attr_bodies = collect_attr_function_bodies(children);
    let mut decorated_class_ctors = HashMap::new();
    messages.extend(expand_decls(
        children,
        &user_attrs,
        &attr_extra_names,
        &attr_bodies,
        &mut decorated_class_ctors,
    ));
    ExpandResult {
        messages,
        decorated_class_ctors,
    }
}

fn derive_traits_from_attrs<'a>(attrs: &[Attribute<'a>]) -> Vec<&'a str> {
    let mut out = Vec::new();
    for attr in attrs {
        if attr.name == "derive" {
            if let AttrArgs::Idents(idents) = &attr.args {
                out.extend(idents.iter().copied());
            }
        }
    }
    out
}

fn strip_processed_attrs(attrs: &mut Vec<Attribute<'_>>) {
    attrs.retain(|a| a.name != "derive" && a.name != "ffi");
}

fn is_known_attr(name: &str, user_attrs: &HashSet<String>) -> bool {
    KNOWN_ATTRS.contains(&name) || user_attrs.contains(name)
}

fn validate_attrs(
    attrs: &[Attribute<'_>],
    target: &str,
    user_attrs: &HashSet<String>,
    messages: &mut Vec<Message>,
    span: SimpleSpan,
    is_ffi: bool,
) {
    for attr in attrs {
        if user_attrs.contains(attr.name) && is_ffi {
            messages.push(Message::error(
                ErrorCode::GenericTypeError,
                format!(
                    "User-defined attribute `{}` cannot be applied to FFI functions",
                    attr.name
                ),
                span.into_range(),
            ));
        }
        if attr.name == "test" {
            messages.push(Message::error(
                ErrorCode::GenericTypeError,
                "`#[test]` is not supported; use `test(\"desc\") { … }`".to_string(),
                span.into_range(),
            ));
        }
        if attr.name == "max_depth" && target != "function" {
            messages.push(Message::error(
                ErrorCode::GenericTypeError,
                format!("Attribute `max_depth` is not valid on {}", target),
                span.into_range(),
            ));
        }
        if attr.name == "repr" && target != "enum" {
            messages.push(Message::error(
                ErrorCode::GenericTypeError,
                format!("Attribute `repr` is not valid on {}", target),
                span.into_range(),
            ));
        }
        if !is_known_attr(attr.name, user_attrs) {
            messages.push(Message::error(
                ErrorCode::GenericTypeError,
                format!("Unknown attribute `{}`", attr.name),
                span.into_range(),
            ));
        }
    }
}

fn collect_and_desugar_attr_decls(
    decls: &mut Vec<Output<'_>>,
    user_attrs: &mut HashSet<String>,
    attr_extra_names: &mut HashMap<String, Vec<String>>,
    messages: &mut Vec<Message>,
) {
    let mut i = 0;
    while i < decls.len() {
        if let Expression::AttrDecl {
            docs,
            name,
            type_params,
            args,
            returns,
            where_constraints,
            body,
        } = decls[i].1.as_ref()
        {
            let span = decls[i].0;
            if let Err(msg) = validate_attr_protocol(args, body, span) {
                messages.push(msg);
            }
            let params = fn_param_nodes(args);
            if params.len() >= 2 {
                attr_extra_names.insert(
                    (*name).to_string(),
                    params[1..params.len() - 1]
                        .iter()
                        .map(|(_, n, _)| (*n).to_string())
                        .collect(),
                );
            }
            user_attrs.insert((*name).to_string());
            decls[i] = at(
                span,
                Expression::Function {
                    docs: docs.clone(),
                    attrs: vec![],
                    name,
                    is_coro: false,
                    is_static: false,
                    type_params: type_params.clone(),
                    args: args.clone(),
                    returns: returns.clone(),
                    where_constraints: where_constraints.clone(),
                    body: Some(body.clone()),
                },
            );
        }
        i += 1;
    }
}

fn validate_attr_protocol(args: &Output, body: &Output, span: SimpleSpan) -> Result<(), Message> {
    let _ = body;
    validate_attr_protocol_shape(args, span)
}

fn validate_attr_protocol_shape(args: &Output, span: SimpleSpan) -> Result<(), Message> {
    let params = fn_param_nodes(args);
    if params.len() < 2 {
        return Err(Message::error(
            ErrorCode::GenericTypeError,
            "Attribute declaration requires at least `target` and trailing `...args` parameters"
                .to_string(),
            span.into_range(),
        ));
    }
    let last_rest = params.last().map(|(_, _, r)| *r).unwrap_or(false);
    let last_name = params.last().map(|(_, n, _)| *n).unwrap_or("");
    if !last_rest || last_name != "args" {
        return Err(Message::error(
            ErrorCode::GenericTypeError,
            "Attribute declaration must end with a bare `...args` tuple-rest parameter".to_string(),
            span.into_range(),
        ));
    }
    Ok(())
}

/// True when the attr body contains `yield` outside a `target(...args)` call site.
fn attr_body_crosses_yield(body: &Output<'_>) -> bool {
    fn walk(expr: &Output<'_>, in_target: bool) -> bool {
        match expr.1.as_ref() {
            Expression::Yield(_) | Expression::YieldFrom(_) if !in_target => return true,
            Expression::Call { name, args }
                if matches!(name.1.as_ref(), Expression::Identifier("target"))
                    && is_target_args_spread(args) =>
            {
                return false;
            }
            _ => {}
        }
        match expr.1.as_ref() {
            Expression::Program(items)
            | Expression::Block(items)
            | Expression::Fragment(items)
            | Expression::Declare(items)
            | Expression::Invoke(items)
            | Expression::List(items)
            | Expression::Array(items)
            | Expression::Tuple(items) => items.iter().any(|c| walk(c, false)),
            Expression::Return(inner)
            | Expression::ImplicitReturn(inner)
            | Expression::Raise(inner)
            | Expression::Panic(inner)
            | Expression::Yield(inner)
            | Expression::YieldFrom(inner)
            | Expression::Try(inner)
            | Expression::Negate(inner)
            | Expression::Not(inner)
            | Expression::LogicalNot(inner)
            | Expression::Positive(inner)
            | Expression::Dload(inner)
            | Expression::Done(inner)
            | Expression::Expr(inner)
            | Expression::Group(inner)
            | Expression::Member(inner)
            | Expression::Spread(inner)
            | Expression::Noop(inner)
            | Expression::TypeOf(inner) => walk(inner, false),
            Expression::Defer { body, .. } => walk(body, false),
            Expression::Resume(handle, send) => {
                walk(handle, false) || send.as_ref().is_some_and(|s| walk(s, false))
            }
            Expression::If(branches) => branches.iter().any(|b| walk(b, false)),
            Expression::Match { scrutinee, arms } => {
                walk(scrutinee, false)
                    || arms.iter().any(|arm| walk(&arm.body, false))
            }
            Expression::Loop { iterable, body, .. } => walk(iterable, false) || walk(body, false),
            Expression::Add(l, r)
            | Expression::Sub(l, r)
            | Expression::Mul(l, r)
            | Expression::Div(l, r)
            | Expression::Mod(l, r)
            | Expression::Pow(l, r)
            | Expression::Shl(l, r)
            | Expression::Shr(l, r)
            | Expression::Xor(l, r)
            | Expression::And(l, r)
            | Expression::BitAnd(l, r)
            | Expression::Or(l, r)
            | Expression::BitOr(l, r)
            | Expression::Eq(l, r)
            | Expression::Neq(l, r)
            | Expression::Leq(l, r)
            | Expression::Geq(l, r)
            | Expression::Le(l, r)
            | Expression::Gt(l, r)
            | Expression::Coalesce(l, r) => walk(l, false) || walk(r, false),
            Expression::Assignment(lhs, rhs) | Expression::CompoundAssign(lhs, _, rhs) => {
                walk(lhs, false) || walk(rhs, false)
            }
            Expression::Access(receiver, _) | Expression::OptionalAccess(receiver, _) => {
                walk(receiver, false)
            }
            Expression::Index(receiver, index) => {
                walk(receiver, false) || index.as_ref().is_some_and(|i| walk(i, false))
            }
            Expression::Cast(inner, ty) => walk(inner, false) || walk(ty, false),
            Expression::NamedArg(_, inner) => walk(inner, false),
            Expression::Branch(cond, body) => {
                cond.as_ref().is_some_and(|c| walk(c, false)) || walk(body, false)
            }
            Expression::Call { name, args } => {
                walk(name, false)
                    || args
                        .as_ref()
                        .is_some_and(|items| items.iter().any(|a| walk(a, false)))
            }
            Expression::Lambda { args, body, .. } => walk(args, false) || walk(body, false),
            Expression::Range { start, end, .. } => walk(start, false) || walk(end, false),
            Expression::Adjust { target, .. } => walk(target, false),
            _ => false,
        }
    }
    walk(body, false)
}

fn fn_param_nodes<'a>(args: &'a Output<'a>) -> Vec<(Option<Output<'a>>, &'static str, bool)> {
    let mut out = Vec::new();
    if let Expression::Fragment(children) = args.1.as_ref() {
        for child in children {
            if let Expression::Argument {
                ty,
                name,
                is_rest,
                ..
            } = child.1.as_ref()
            {
                out.push((ty.clone(), leak((*name).to_string()), *is_rest));
            }
        }
    }
    out
}

fn is_user_attr(attr: &Attribute<'_>, user_attrs: &HashSet<String>) -> bool {
    user_attrs.contains(attr.name) && !KNOWN_ATTRS.contains(&attr.name)
}

fn user_attrs_on<'a>(
    attrs: &'a [Attribute<'a>],
    user_attrs: &HashSet<String>,
) -> Vec<&'a Attribute<'a>> {
    attrs
        .iter()
        .filter(|a| is_user_attr(a, user_attrs))
        .collect()
}

fn strip_user_attrs(attrs: &mut Vec<Attribute<'_>>, user_attrs: &HashSet<String>) {
    attrs.retain(|a| !is_user_attr(a, user_attrs));
}

fn attr_literal_expr<'a>(span: SimpleSpan, lit: &AttrLit<'a>) -> Output<'a> {
    match lit {
        AttrLit::String(s) => str_lit(span, s),
        AttrLit::Int(i) => at(span, Expression::Integer(*i)),
        AttrLit::Float(f) => at(span, Expression::Float(*f)),
        AttrLit::Bool(b) => at(span, Expression::Bool(*b)),
    }
}

fn resolve_attr_extras<'a>(
    attr: &Attribute<'a>,
    extra_params: &[(&'static str, Option<Output>)],
    span: SimpleSpan,
    messages: &mut Vec<Message>,
) -> Option<Vec<Output<'a>>> {
    let mut values: Vec<Option<Output<'a>>> = vec![None; extra_params.len()];
    match &attr.args {
        AttrArgs::KeyValues(kvs) => {
            for (key, lit) in kvs {
                match extra_params.iter().position(|(n, _)| *n == *key) {
                    Some(idx) => values[idx] = Some(attr_literal_expr(span, lit)),
                    None => messages.push(Message::error(
                        ErrorCode::GenericTypeError,
                        format!("Unknown key `{}` in `#[{}(...)]`", key, attr.name),
                        span.into_range(),
                    )),
                }
            }
        }
        AttrArgs::Positional(lits) => {
            if lits.len() > extra_params.len() {
                messages.push(Message::error(
                    ErrorCode::GenericTypeError,
                    format!(
                        "Too many positional arguments for `#[{}(...)]` (expected {}, got {})",
                        attr.name,
                        extra_params.len(),
                        lits.len()
                    ),
                    span.into_range(),
                ));
                return None;
            }
            for (i, lit) in lits.iter().enumerate() {
                values[i] = Some(attr_literal_expr(span, lit));
            }
        }
        AttrArgs::String(s) => {
            if !extra_params.is_empty() {
                values[0] = Some(str_lit(span, s));
            }
        }
        AttrArgs::Idents(idents) => {
            for (i, id) in idents.iter().enumerate() {
                if i < extra_params.len() {
                    values[i] = Some(ident(span, id));
                }
            }
        }
        AttrArgs::Empty => {}
    }
    let mut out = Vec::new();
    let mut missing = false;
    for (i, (name, _)) in extra_params.iter().enumerate() {
        match values[i].take() {
            Some(v) => out.push(v),
            None => {
                missing = true;
                messages.push(Message::error(
                    ErrorCode::GenericTypeError,
                    format!("Missing argument `{}` for `#[{}(...)]`", name, attr.name),
                    span.into_range(),
                ));
            }
        }
    }
    if missing { None } else { Some(out) }
}

fn collect_attr_function_bodies<'a>(decls: &[Output<'a>]) -> HashMap<String, Output<'a>> {
    let mut out = HashMap::new();
    for node in decls {
        if let Expression::Function {
            docs: _,
            name,
            body: Some(body),
            ..
        } = node.1.as_ref()
        {
            out.insert((*name).to_string(), body.clone());
        }
    }
    out
}

fn find_attr_function_body<'a>(
    attr_bodies: &HashMap<String, Output<'a>>,
    name: &str,
) -> Option<Output<'a>> {
    attr_bodies.get(name).cloned()
}

fn is_target_args_spread(args: &Option<Vec<Output<'_>>>) -> bool {
    args.as_ref().is_some_and(|items| {
        items.len() == 1
            && matches!(
                items[0].1.as_ref(),
                Expression::Spread(inner)
                    if matches!(inner.1.as_ref(), Expression::Identifier("args"))
            )
    })
}

fn decoratee_param_names<'a>(decoratee_args: &Output<'a>) -> HashSet<&'static str> {
    fn_param_nodes(decoratee_args)
        .into_iter()
        .map(|(_, name, _)| name)
        .collect()
}

/// Free identifiers in `expr` that must be captured by a lambda wrapping
/// `decoratee_args` parameters (e.g. implicit `self` on methods).
fn collect_lambda_captures<'a>(
    expr: &Output<'a>,
    decoratee_args: &Output<'a>,
) -> Vec<&'static str> {
    let mut bound = decoratee_param_names(decoratee_args);
    let mut free = HashSet::new();
    collect_free_idents(expr, &mut bound, &mut free);
    let mut names: Vec<&'static str> = free.into_iter().map(|n| leak(n)).collect();
    names.sort_unstable();
    names
}

fn should_capture_ident(name: &str) -> bool {
    if name == "self" {
        return true;
    }
    // Type / module names (Constructors, `Point`, …) are not closure captures.
    name.chars().next().is_some_and(|c| c.is_ascii_lowercase())
}

fn collect_free_idents<'a>(
    expr: &Output<'a>,
    bound: &mut HashSet<&'static str>,
    free: &mut HashSet<String>,
) {
    match expr.1.as_ref() {
        Expression::Identifier(name) => {
            if !bound.contains(*name) && should_capture_ident(name) {
                free.insert((*name).to_string());
            }
        }
        Expression::Lambda {
            args,
            captures,
            body,
        } => {
            let mut inner_bound = bound.clone();
            for cap in captures {
                inner_bound.insert(leak((*cap).to_string()));
            }
            if let Expression::Fragment(children) = args.1.as_ref() {
                for child in children {
                    if let Expression::Argument { name, .. } = child.1.as_ref() {
                        inner_bound.insert(leak((*name).to_string()));
                    }
                }
            }
            collect_free_idents(body, &mut inner_bound, free);
        }
        Expression::Defer { captures, body } => {
            let mut inner_bound = bound.clone();
            for cap in captures {
                inner_bound.insert(leak((*cap).to_string()));
            }
            collect_free_idents(body, &mut inner_bound, free);
        }
        Expression::Function { args, body, .. } => {
            let mut inner_bound = bound.clone();
            for (_, name, _) in fn_param_nodes(args) {
                inner_bound.insert(name);
            }
            if let Some(b) = body {
                collect_free_idents(b, &mut inner_bound, free);
            }
        }
        Expression::LetDestructure { pattern, rhs } => {
            collect_free_idents(rhs, bound, free);
            bind_let_pattern_names(pattern, bound);
        }
        Expression::Variable(name, init) => {
            if let Some(rhs) = init {
                collect_free_idents(rhs, bound, free);
            }
            bound.insert(leak((*name).to_string()));
        }
        Expression::Constant(_, init) => {
            if let Some(rhs) = init {
                collect_free_idents(rhs, bound, free);
            }
        }
        Expression::Call { name, args } => {
            // Callee identifiers (`len`, `print`, …) are not capture candidates.
            if !matches!(name.1.as_ref(), Expression::Identifier(_)) {
                collect_free_idents(name, bound, free);
            }
            if let Some(items) = args {
                for a in items {
                    collect_free_idents(a, bound, free);
                }
            }
        }
        Expression::Block(items) | Expression::Fragment(items) | Expression::Program(items) => {
            for item in items {
                collect_free_idents(item, bound, free);
            }
        }
        Expression::If(branches) => {
            for branch in branches {
                collect_free_idents(branch, bound, free);
            }
        }
        Expression::Match { scrutinee, arms } => {
            collect_free_idents(scrutinee, bound, free);
            for arm in arms {
                collect_free_idents(&arm.body, bound, free);
            }
        }
        Expression::Loop {
            identifier,
            iterable,
            body,
        } => {
            if let Some(id) = identifier {
                collect_free_idents(id, bound, free);
            }
            collect_free_idents(iterable, bound, free);
            collect_free_idents(body, bound, free);
        }
        Expression::Return(inner)
        | Expression::ImplicitReturn(inner)
        | Expression::Raise(inner)
        | Expression::Panic(inner)
        | Expression::TypeOf(inner)
        | Expression::Yield(inner)
        | Expression::YieldFrom(inner)
        | Expression::Try(inner)
        | Expression::Negate(inner)
        | Expression::Not(inner)
        | Expression::LogicalNot(inner)
        | Expression::Positive(inner)
        | Expression::Dload(inner)
        | Expression::Done(inner)
        | Expression::Expr(inner)
        | Expression::Group(inner)
        | Expression::Spread(inner)
        | Expression::Noop(inner)
        | Expression::Member(inner) => collect_free_idents(inner, bound, free),
        Expression::Resume(handle, send) => {
            collect_free_idents(handle, bound, free);
            if let Some(s) = send {
                collect_free_idents(s, bound, free);
            }
        }
        Expression::OptionalAccess(receiver, _) => collect_free_idents(receiver, bound, free),
        Expression::Access(receiver, _) => collect_free_idents(receiver, bound, free),
        Expression::Index(receiver, index) => {
            collect_free_idents(receiver, bound, free);
            if let Some(index) = index {
                collect_free_idents(index, bound, free);
            }
        }
        Expression::Cast(expr, ty) => {
            collect_free_idents(expr, bound, free);
            collect_free_idents(ty, bound, free);
        }
        Expression::Assignment(lhs, rhs)
        | Expression::Coalesce(lhs, rhs)
        | Expression::Add(lhs, rhs)
        | Expression::Sub(lhs, rhs)
        | Expression::Mul(lhs, rhs)
        | Expression::Div(lhs, rhs)
        | Expression::Mod(lhs, rhs)
        | Expression::Pow(lhs, rhs)
        | Expression::Shl(lhs, rhs)
        | Expression::Shr(lhs, rhs)
        | Expression::Xor(lhs, rhs)
        | Expression::And(lhs, rhs)
        | Expression::BitAnd(lhs, rhs)
        | Expression::Or(lhs, rhs)
        | Expression::BitOr(lhs, rhs)
        | Expression::Eq(lhs, rhs)
        | Expression::Neq(lhs, rhs)
        | Expression::Leq(lhs, rhs)
        | Expression::Geq(lhs, rhs)
        | Expression::Le(lhs, rhs)
        | Expression::Gt(lhs, rhs) => {
            collect_free_idents(lhs, bound, free);
            collect_free_idents(rhs, bound, free);
        }
        Expression::CompoundAssign(lhs, _, rhs) => {
            collect_free_idents(lhs, bound, free);
            collect_free_idents(rhs, bound, free);
        }
        Expression::Adjust { target: t, .. } => collect_free_idents(t, bound, free),
        Expression::Range { start, end, .. } => {
            collect_free_idents(start, bound, free);
            collect_free_idents(end, bound, free);
        }
        Expression::Branch(cond, body) => {
            if let Some(c) = cond {
                collect_free_idents(c, bound, free);
            }
            collect_free_idents(body, bound, free);
        }
        Expression::List(items)
        | Expression::Array(items)
        | Expression::Tuple(items)
        | Expression::Declare(items)
        | Expression::Invoke(items) => {
            for item in items {
                collect_free_idents(item, bound, free);
            }
        }
        Expression::Dict(fields) => {
            for f in fields {
                collect_free_idents(&f.value, bound, free);
            }
        }
        Expression::Construct { fields, .. } => match fields {
            EnumConstructPayload::Unit => {}
            EnumConstructPayload::Tuple(items) => {
                for item in items {
                    collect_free_idents(item, bound, free);
                }
            }
            EnumConstructPayload::Record(records) => {
                for f in records {
                    collect_free_idents(&f.value, bound, free);
                }
            }
        },
        Expression::NamedArg(_, value) => collect_free_idents(value, bound, free),
        Expression::ExprStatement(inner) | Expression::Statement(inner) => {
            collect_free_idents(inner, bound, free);
        }
        Expression::Instantiate(receiver, type_args) => {
            if !matches!(receiver.1.as_ref(), Expression::Identifier(_)) {
                collect_free_idents(receiver, bound, free);
            }
            if let Some(items) = type_args {
                for t in items {
                    collect_free_idents(t, bound, free);
                }
            }
        }
        Expression::TestCase { name, body } => {
            collect_free_idents(name, bound, free);
            collect_free_idents(body, bound, free);
        }
        Expression::TypeApp { args, .. } | Expression::TypeProjection { args, .. } => {
            for a in args {
                collect_free_idents(a, bound, free);
            }
        }
        Expression::TypeFun(lhs, rhs)
        | Expression::TypeFnSig {
            params: lhs,
            ret: rhs,
        } => {
            collect_free_idents(lhs, bound, free);
            collect_free_idents(rhs, bound, free);
        }
        Expression::Argument { ty, .. } => {
            if let Some(t) = ty {
                collect_free_idents(t, bound, free);
            }
        }
        Expression::AttrDecl {
            docs: _,
            args,
            returns,
            body,
            ..
        } => {
            collect_free_idents(args, bound, free);
            if let Some(r) = returns {
                collect_free_idents(r, bound, free);
            }
            collect_free_idents(body, bound, free);
        }
        Expression::TypeAlias { ty, .. } | Expression::AssocTypeDef { ty, .. } => {
            collect_free_idents(ty, bound, free);
        }
        Expression::Forall { ty, .. } => collect_free_idents(ty, bound, free),
        Expression::Module(_, child) => collect_free_idents(child, bound, free),
        Expression::Field { ty, name, init, .. } => {
            collect_free_idents(ty, bound, free);
            collect_free_idents(name, bound, free);
            if let Some(init) = init {
                collect_free_idents(init, bound, free);
            }
        }
        Expression::Readonly(inner) => collect_free_idents(inner, bound, free),
        Expression::StaticDecl { ty, init, .. } => {
            if let Some(ty) = ty {
                collect_free_idents(ty, bound, free);
            }
            collect_free_idents(init, bound, free);
        }
        Expression::Method(_, method) => collect_free_idents(method, bound, free),
        Expression::Class { fields, .. } => {
            for item in fields {
                collect_free_idents(item, bound, free);
            }
        }
        Expression::Implementation { methods, .. } | Expression::TypeClass { methods, .. } => {
            for m in methods {
                collect_free_idents(m, bound, free);
            }
        }
        Expression::EnumDecl { variants, .. } => {
            for v in variants {
                collect_free_idents(v, bound, free);
            }
        }
        Expression::TypeClassImpl { args, methods, .. } => {
            for a in args {
                collect_free_idents(a, bound, free);
            }
            for m in methods {
                collect_free_idents(m, bound, free);
            }
        }
        Expression::EnumVariant { payload, .. } => match payload {
            EnumVariantPayload::Unit => {}
            EnumVariantPayload::Tuple(items) => {
                for item in items {
                    collect_free_idents(item, bound, free);
                }
            }
            EnumVariantPayload::Record(fields) => {
                for f in fields {
                    collect_free_idents(&f.value, bound, free);
                }
            }
        },
        Expression::ExternStruct(decl) => {
            for (_, ty) in &decl.fields {
                collect_free_idents(ty, bound, free);
            }
        }
        Expression::ExternBlock { declarations, .. } => {
            for d in declarations {
                collect_free_idents(&d.args, bound, free);
                if let Some(r) = &d.returns {
                    collect_free_idents(r, bound, free);
                }
            }
        }
        Expression::Integer(_)
        | Expression::Float(_)
        | Expression::String(_)
        | Expression::Bool(_)
        | Expression::Type(_)
        | Expression::Comment(_)
        | Expression::Default(_)
        | Expression::Break
        | Expression::Continue
        | Expression::Use { .. }
        | Expression::QualifiedAccess { .. }
        | Expression::AssocTypeDecl { .. } => {}
    }
}

fn bind_let_pattern_names<'a>(pattern: &LetPattern<'a>, bound: &mut HashSet<&'static str>) {
    match pattern {
        LetPattern::Wildcard => {}
        LetPattern::Binding { name } => {
            bound.insert(leak((*name).to_string()));
        }
        LetPattern::Tuple(items) => {
            for p in items {
                bind_let_pattern_names(p, bound);
            }
        }
        LetPattern::Record(fields) => {
            for f in fields {
                bind_let_pattern_names(&f.pattern, bound);
            }
        }
    }
}

/// Build call arguments for invoking the decoratee lambda from the enclosing
/// function's parameter names (mirrors the non-inline `Lambda` + `Call` path).
fn forward_call_args<'a>(span: SimpleSpan, decoratee_args: &Output<'a>) -> Vec<Output<'a>> {
    fn_param_nodes(decoratee_args)
        .into_iter()
        .map(|(ty, name, is_rest)| {
            if is_rest {
                if ty.is_none() {
                    // Tuple rest `...xs` — spread the heterogeneous pack.
                    at(span, Expression::Spread(ident(span, name)))
                } else {
                    // Homogeneous `T... xs` — pass the `[T]` pack as one argument.
                    ident(span, name)
                }
            } else {
                ident(span, name)
            }
        })
        .collect()
}

/// Lower `target(...args)` to an anonymous-function call so `return` inside
/// the decoratee body exits the lambda frame and produces a value at the
/// call site (expression-safe, early-return-safe).
fn make_target_invoke<'a>(
    span: SimpleSpan,
    target: &Output<'a>,
    decoratee_args: &Output<'a>,
) -> Output<'a> {
    let captures = collect_lambda_captures(target, decoratee_args);
    let lambda = at(
        span,
        Expression::Lambda {
            args: decoratee_args.clone(),
            captures,
            body: target.clone(),
        },
    );
    at(
        span,
        Expression::Call {
            name: lambda,
            args: Some(forward_call_args(span, decoratee_args)),
        },
    )
}

fn rewrite_outputs<'a>(
    items: &[Output<'a>],
    target: &Output<'a>,
    subs: &HashMap<&str, Output<'a>>,
    decoratee_args: &Output<'a>,
) -> Vec<Output<'a>> {
    items
        .iter()
        .map(|e| rewrite_expr_inline(e, target, subs, decoratee_args))
        .collect()
}

fn rewrite_construct_payload<'a>(
    fields: &EnumConstructPayload<'a>,
    target: &Output<'a>,
    subs: &HashMap<&str, Output<'a>>,
    decoratee_args: &Output<'a>,
) -> EnumConstructPayload<'a> {
    match fields {
        EnumConstructPayload::Unit => EnumConstructPayload::Unit,
        EnumConstructPayload::Tuple(items) => {
            EnumConstructPayload::Tuple(rewrite_outputs(items, target, subs, decoratee_args))
        }
        EnumConstructPayload::Record(records) => EnumConstructPayload::Record(
            records
                .iter()
                .map(|f| RecordFieldValue {
                    name: f.name,
                    value: rewrite_expr_inline(&f.value, target, subs, decoratee_args),
                })
                .collect(),
        ),
    }
}

fn rewrite_enum_variant_payload<'a>(
    payload: &EnumVariantPayload<'a>,
    target: &Output<'a>,
    subs: &HashMap<&str, Output<'a>>,
    decoratee_args: &Output<'a>,
) -> EnumVariantPayload<'a> {
    match payload {
        EnumVariantPayload::Unit => EnumVariantPayload::Unit,
        EnumVariantPayload::Tuple(items) => {
            EnumVariantPayload::Tuple(rewrite_outputs(items, target, subs, decoratee_args))
        }
        EnumVariantPayload::Record(fields) => EnumVariantPayload::Record(
            fields
                .iter()
                .map(|f| RecordFieldDecl {
                    name: f.name,
                    value: rewrite_expr_inline(&f.value, target, subs, decoratee_args),
                })
                .collect(),
        ),
    }
}

fn rewrite_extern_function<'a>(
    decl: &ExternFunction<'a>,
    target: &Output<'a>,
    subs: &HashMap<&str, Output<'a>>,
    decoratee_args: &Output<'a>,
) -> ExternFunction<'a> {
    ExternFunction {
        name: decl.name,
        symbol: decl.symbol,
        args: rewrite_expr_inline(&decl.args, target, subs, decoratee_args),
        returns: decl
            .returns
            .as_ref()
            .map(|r| rewrite_expr_inline(r, target, subs, decoratee_args)),
        variadic: decl.variadic,
    }
}

/// Recursively rewrite every sub-expression in `expr`, lowering
/// `target(...args)` to a lambda call over the decoratee body.
fn rewrite_expr_inline<'a>(
    expr: &Output<'a>,
    target: &Output<'a>,
    subs: &HashMap<&str, Output<'a>>,
    decoratee_args: &Output<'a>,
) -> Output<'a> {
    let span = expr.0;
    let rw = |e: &Output<'a>| rewrite_expr_inline(e, target, subs, decoratee_args);
    match expr.1.as_ref() {
        Expression::Call { name, args } => {
            if let Expression::Identifier(callee) = name.1.as_ref()
                && *callee == "target"
                && is_target_args_spread(args)
            {
                return make_target_invoke(span, target, decoratee_args);
            }
            at(
                span,
                Expression::Call {
                    name: rw(name),
                    args: args
                        .as_ref()
                        .map(|items| rewrite_outputs(items, target, subs, decoratee_args)),
                },
            )
        }
        Expression::Identifier(name) => subs.get(*name).cloned().unwrap_or_else(|| expr.clone()),

        // Literals and leaves with no nested expressions.
        Expression::Integer(_)
        | Expression::Float(_)
        | Expression::String(_)
        | Expression::Bool(_)
        | Expression::Type(_)
        | Expression::Comment(_)
        | Expression::Default(_)
        | Expression::Break
        | Expression::Continue
        | Expression::Use { .. }
        | Expression::AssocTypeDecl { .. } => expr.clone(),

        Expression::Noop(inner)
        | Expression::Spread(inner)
        | Expression::Return(inner)
        | Expression::ImplicitReturn(inner)
        | Expression::Raise(inner)
        | Expression::Panic(inner)
        | Expression::TypeOf(inner)
        | Expression::Yield(inner)
        | Expression::YieldFrom(inner)
        | Expression::Try(inner)
        | Expression::Negate(inner)
        | Expression::Not(inner)
        | Expression::LogicalNot(inner)
        | Expression::Positive(inner)
        | Expression::Dload(inner)
        | Expression::Done(inner)
        | Expression::Expr(inner)
        | Expression::Group(inner)
        | Expression::Member(inner) => at(span, clone_unary(expr.1.as_ref(), rw(inner))),

        Expression::Defer { captures, body } => at(
            span,
            Expression::Defer {
                captures: captures.clone(),
                body: rw(body),
            },
        ),

        Expression::Resume(handle, send) => at(
            span,
            Expression::Resume(rw(handle), send.as_ref().map(|s| rw(s))),
        ),
        Expression::OptionalAccess(receiver, field) => {
            at(span, Expression::OptionalAccess(rw(receiver), *field))
        }
        Expression::CompoundAssign(lhs, op, rhs) => {
            at(span, Expression::CompoundAssign(rw(lhs), *op, rw(rhs)))
        }
        Expression::Adjust {
            op,
            prefix,
            target: adj_target,
        } => at(
            span,
            Expression::Adjust {
                op: *op,
                prefix: *prefix,
                target: rw(adj_target),
            },
        ),
        Expression::Add(l, r)
        | Expression::Sub(l, r)
        | Expression::Mul(l, r)
        | Expression::Div(l, r)
        | Expression::Mod(l, r)
        | Expression::Pow(l, r)
        | Expression::Shl(l, r)
        | Expression::Shr(l, r)
        | Expression::Xor(l, r)
        | Expression::And(l, r)
        | Expression::BitAnd(l, r)
        | Expression::Or(l, r)
        | Expression::BitOr(l, r)
        | Expression::Eq(l, r)
        | Expression::Neq(l, r)
        | Expression::Leq(l, r)
        | Expression::Geq(l, r)
        | Expression::Le(l, r)
        | Expression::Gt(l, r)
        | Expression::Coalesce(l, r) => at(span, clone_binary(expr.1.as_ref(), rw(l), rw(r))),
        Expression::Cast(expr, ty) => at(span, Expression::Cast(rw(expr), rw(ty))),
        Expression::Range {
            start,
            end,
            inclusive,
        } => at(
            span,
            Expression::Range {
                start: rw(start),
                end: rw(end),
                inclusive: *inclusive,
            },
        ),
        Expression::Assignment(lhs, rhs) => at(span, Expression::Assignment(rw(lhs), rw(rhs))),
        Expression::Access(receiver, field) => at(span, Expression::Access(rw(receiver), *field)),
        Expression::Index(receiver, index) => at(
            span,
            Expression::Index(rw(receiver), index.as_ref().map(|idx| rw(idx))),
        ),
        Expression::NamedArg(name, value) => at(span, Expression::NamedArg(*name, rw(value))),
        Expression::Branch(cond, body) => at(
            span,
            Expression::Branch(cond.as_ref().map(|c| rw(c)), rw(body)),
        ),
        Expression::If(branches) => {
            let branches = branches.iter().map(|branch| rw(branch)).collect();
            at(span, Expression::If(branches))
        }
        Expression::Match { scrutinee, arms } => {
            let arms = arms
                .iter()
                .map(|arm| MatchArm {
                    pattern: arm.pattern.clone(),
                    body: rw(&arm.body),
                })
                .collect();
            at(
                span,
                Expression::Match {
                    scrutinee: rw(scrutinee),
                    arms,
                },
            )
        }
        Expression::Loop {
            identifier,
            iterable,
            body,
        } => at(
            span,
            Expression::Loop {
                identifier: identifier.as_ref().map(|id| rw(id)),
                iterable: rw(iterable),
                body: rw(body),
            },
        ),
        Expression::List(items)
        | Expression::Array(items)
        | Expression::Tuple(items)
        | Expression::Fragment(items)
        | Expression::Declare(items)
        | Expression::Invoke(items) => at(
            span,
            clone_expr_list(
                expr.1.as_ref(),
                rewrite_outputs(items, target, subs, decoratee_args),
            ),
        ),
        Expression::Block(items) => {
            let items = items
                .iter()
                .map(|s| rewrite_stmt_inline(s, target, subs, decoratee_args))
                .collect();
            at(span, Expression::Block(items))
        }
        Expression::Program(items) => at(
            span,
            Expression::Program(rewrite_outputs(items, target, subs, decoratee_args)),
        ),
        Expression::Dict(fields) => {
            let fields = fields
                .iter()
                .map(|f| RecordFieldValue {
                    name: f.name,
                    value: rw(&f.value),
                })
                .collect();
            at(span, Expression::Dict(fields))
        }
        Expression::Construct {
            enum_name,
            variant_name,
            fields,
        } => at(
            span,
            Expression::Construct {
                enum_name: *enum_name,
                variant_name: *variant_name,
                fields: rewrite_construct_payload(fields, target, subs, decoratee_args),
            },
        ),
        Expression::Variable(name, init) => at(
            span,
            Expression::Variable(*name, init.as_ref().map(|e| rw(e))),
        ),
        Expression::Constant(ty, init) => at(
            span,
            Expression::Constant(rw(ty), init.as_ref().map(|e| rw(e))),
        ),
        Expression::LetDestructure { pattern, rhs } => at(
            span,
            Expression::LetDestructure {
                pattern: pattern.clone(),
                rhs: rw(rhs),
            },
        ),
        Expression::Argument {
            docs,
            ty,
            name,
            is_rest: rest,
        } => at(
            span,
            Expression::Argument {
                docs: docs.clone(),
                ty: ty.as_ref().map(|t| rw(t)),
                name: *name,
                is_rest: *rest,
            },
        ),
        Expression::TypeFnSig { params, ret } => at(
            span,
            Expression::TypeFnSig {
                params: rw(params),
                ret: rw(ret),
            },
        ),
        Expression::TypeApp { name, args } => at(
            span,
            Expression::TypeApp {
                name: *name,
                args: rewrite_outputs(args, target, subs, decoratee_args),
            },
        ),
        Expression::TypeProjection { owner, name, args } => at(
            span,
            Expression::TypeProjection {
                owner: *owner,
                name: *name,
                args: rewrite_outputs(args, target, subs, decoratee_args),
            },
        ),
        Expression::TypeFun(arg, ret) => at(span, Expression::TypeFun(rw(arg), rw(ret))),
        Expression::Forall { params, ty } => at(
            span,
            Expression::Forall {
                params: params.clone(),
                ty: Box::new(rw(ty)),
            },
        ),
        Expression::TypeAlias {
            docs,
            name,
            type_params,
            ty,
        } => at(
            span,
            Expression::TypeAlias {
                docs: docs.clone(),
                name: *name,
                type_params: type_params.clone(),
                ty: Box::new(rw(ty)),
            },
        ),
        Expression::AssocTypeDef {
            name,
            type_params,
            ty,
        } => at(
            span,
            Expression::AssocTypeDef {
                name: *name,
                type_params: type_params.clone(),
                ty: Box::new(rw(ty)),
            },
        ),
        Expression::AttrDecl {
            docs,
            name,
            type_params,
            args,
            returns,
            where_constraints,
            body,
        } => at(
            span,
            Expression::AttrDecl {
                docs: docs.clone(),
                name: *name,
                type_params: type_params.clone(),
                args: rw(args),
                returns: returns.as_ref().map(|r| rw(r)),
                where_constraints: where_constraints.clone(),
                body: rw(body),
            },
        ),
        Expression::Function {
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
        } => at(
            span,
            Expression::Function {
                docs: docs.clone(),
                attrs: attrs.clone(),
                name: *name,
                is_coro: *is_coro,
                is_static: *is_static,
                type_params: type_params.clone(),
                args: rw(args),
                returns: returns.as_ref().map(|r| rw(r)),
                where_constraints: where_constraints.clone(),
                body: body.as_ref().map(|b| rw(b)),
            },
        ),
        Expression::Lambda {
            args,
            captures,
            body,
        } => at(
            span,
            Expression::Lambda {
                args: rw(args),
                captures: captures.clone(),
                body: rw(body),
            },
        ),
        Expression::Instantiate(receiver, type_args) => at(
            span,
            Expression::Instantiate(
                rw(receiver),
                type_args
                    .as_ref()
                    .map(|items| rewrite_outputs(items, target, subs, decoratee_args)),
            ),
        ),
        Expression::TestCase { name, body } => at(
            span,
            Expression::TestCase {
                name: rw(name),
                body: rw(body),
            },
        ),
        Expression::Module(path, child) => at(span, Expression::Module(path.clone(), rw(child))),
        Expression::Field {
            docs,
            visibility,
            modifier,
            name,
            ty,
            init,
        } => at(
            span,
            Expression::Field {
                docs: docs.clone(),
                visibility: *visibility,
                modifier: *modifier,
                name: rw(name),
                ty: rw(ty),
                init: init.as_ref().map(|e| rw(e)),
            },
        ),
        Expression::Readonly(inner) => at(span, Expression::Readonly(rw(inner))),
        Expression::QualifiedAccess { owner, member } => at(
            span,
            Expression::QualifiedAccess {
                owner: *owner,
                member: *member,
            },
        ),
        Expression::StaticDecl {
            is_const,
            name,
            ty,
            init,
        } => at(
            span,
            Expression::StaticDecl {
                is_const: *is_const,
                name: *name,
                ty: ty.as_ref().map(|t| rw(t)),
                init: rw(init),
            },
        ),
        Expression::Method(vis, method) => at(span, Expression::Method(*vis, rw(method))),
        Expression::Class {
            docs,
            attrs,
            name,
            type_params,
            fields,
        } => at(
            span,
            Expression::Class {
                docs: docs.clone(),
                attrs: attrs.clone(),
                name: *name,
                type_params: type_params.clone(),
                fields: rewrite_outputs(fields, target, subs, decoratee_args),
            },
        ),
        Expression::Implementation {
            what,
            owner,
            type_params,
            methods,
        } => at(
            span,
            Expression::Implementation {
                what: *what,
                owner: *owner,
                type_params: type_params.clone(),
                methods: rewrite_outputs(methods, target, subs, decoratee_args),
            },
        ),
        Expression::EnumDecl {
            docs,
            attrs,
            name,
            type_params,
            variants,
        } => at(
            span,
            Expression::EnumDecl {
                docs: docs.clone(),
                attrs: attrs.clone(),
                name: *name,
                type_params: type_params.clone(),
                variants: rewrite_outputs(variants, target, subs, decoratee_args),
            },
        ),
        Expression::EnumVariant {
            docs,
            name,
            payload,
            discriminant,
        } => at(
            span,
            Expression::EnumVariant {
                docs: docs.clone(),
                name: *name,
                payload: rewrite_enum_variant_payload(payload, target, subs, decoratee_args),
                discriminant: discriminant
                    .as_ref()
                    .map(|d| rw(d)),
            },
        ),
        Expression::ExternStruct(decl) => at(
            span,
            Expression::ExternStruct(ExternStructDecl {
                name: decl.name,
                fields: decl
                    .fields
                    .iter()
                    .map(|(fname, ty)| (fname.clone(), rw(ty)))
                    .collect(),
            }),
        ),
        Expression::ExternBlock {
            library,
            declarations,
        } => at(
            span,
            Expression::ExternBlock {
                library: library.clone(),
                declarations: declarations
                    .iter()
                    .map(|d| rewrite_extern_function(d, target, subs, decoratee_args))
                    .collect(),
            },
        ),
        Expression::TypeClass {
            docs,
            name,
            type_params,
            methods,
        } => at(
            span,
            Expression::TypeClass {
                docs: docs.clone(),
                name: *name,
                type_params: type_params.clone(),
                methods: rewrite_outputs(methods, target, subs, decoratee_args),
            },
        ),
        Expression::TypeClassImpl {
            class,
            args,
            methods,
        } => at(
            span,
            Expression::TypeClassImpl {
                class: *class,
                args: rewrite_outputs(args, target, subs, decoratee_args),
                methods: rewrite_outputs(methods, target, subs, decoratee_args),
            },
        ),
        Expression::Statement(inner) => at(
            span,
            Expression::Statement(rewrite_expr_inline(inner, target, subs, decoratee_args)),
        ),
        Expression::ExprStatement(inner) => at(span, Expression::ExprStatement(rw(inner))),
    }
}

fn clone_unary<'a>(kind: &Expression<'a>, inner: Output<'a>) -> Expression<'a> {
    match kind {
        Expression::Noop(_) => Expression::Noop(inner),
        Expression::Spread(_) => Expression::Spread(inner),
        Expression::Return(_) => Expression::Return(inner),
        Expression::ImplicitReturn(_) => Expression::ImplicitReturn(inner),
        Expression::Raise(_) => Expression::Raise(inner),
        Expression::Panic(_) => Expression::Panic(inner),
        Expression::TypeOf(_) => Expression::TypeOf(inner),
        Expression::Yield(_) => Expression::Yield(inner),
        Expression::YieldFrom(_) => Expression::YieldFrom(inner),
        Expression::Try(_) => Expression::Try(inner),
        Expression::Negate(_) => Expression::Negate(inner),
        Expression::Not(_) => Expression::Not(inner),
        Expression::LogicalNot(_) => Expression::LogicalNot(inner),
        Expression::Positive(_) => Expression::Positive(inner),
        Expression::Dload(_) => Expression::Dload(inner),
        Expression::Done(_) => Expression::Done(inner),
        Expression::Expr(_) => Expression::Expr(inner),
        Expression::Group(_) => Expression::Group(inner),
        Expression::Member(_) => Expression::Member(inner),
        other => panic!("clone_unary: unexpected {:?}", other),
    }
}

fn clone_binary<'a>(kind: &Expression<'a>, left: Output<'a>, right: Output<'a>) -> Expression<'a> {
    match kind {
        Expression::Add(_, _) => Expression::Add(left, right),
        Expression::Sub(_, _) => Expression::Sub(left, right),
        Expression::Mul(_, _) => Expression::Mul(left, right),
        Expression::Div(_, _) => Expression::Div(left, right),
        Expression::Mod(_, _) => Expression::Mod(left, right),
        Expression::Pow(_, _) => Expression::Pow(left, right),
        Expression::Shl(_, _) => Expression::Shl(left, right),
        Expression::Shr(_, _) => Expression::Shr(left, right),
        Expression::Xor(_, _) => Expression::Xor(left, right),
        Expression::And(_, _) => Expression::And(left, right),
        Expression::BitAnd(_, _) => Expression::BitAnd(left, right),
        Expression::Or(_, _) => Expression::Or(left, right),
        Expression::BitOr(_, _) => Expression::BitOr(left, right),
        Expression::Eq(_, _) => Expression::Eq(left, right),
        Expression::Neq(_, _) => Expression::Neq(left, right),
        Expression::Leq(_, _) => Expression::Leq(left, right),
        Expression::Geq(_, _) => Expression::Geq(left, right),
        Expression::Le(_, _) => Expression::Le(left, right),
        Expression::Gt(_, _) => Expression::Gt(left, right),
        Expression::Coalesce(_, _) => Expression::Coalesce(left, right),
        Expression::Cast(_, _) => Expression::Cast(left, right),
        other => panic!("clone_binary: unexpected {:?}", other),
    }
}

fn clone_expr_list<'a>(kind: &Expression<'a>, items: Vec<Output<'a>>) -> Expression<'a> {
    match kind {
        Expression::List(_) => Expression::List(items),
        Expression::Array(_) => Expression::Array(items),
        Expression::Tuple(_) => Expression::Tuple(items),
        Expression::Fragment(_) => Expression::Fragment(items),
        Expression::Declare(_) => Expression::Declare(items),
        Expression::Invoke(_) => Expression::Invoke(items),
        other => panic!("clone_expr_list: unexpected {:?}", other),
    }
}

fn rewrite_stmt_inline<'a>(
    stmt: &Output<'a>,
    target: &Output<'a>,
    subs: &HashMap<&str, Output<'a>>,
    decoratee_args: &Output<'a>,
) -> Output<'a> {
    if let Expression::Statement(inner) = stmt.1.as_ref() {
        let span = stmt.0;
        at(
            span,
            Expression::Statement(rewrite_expr_inline(inner, target, subs, decoratee_args)),
        )
    } else {
        rewrite_expr_inline(stmt, target, subs, decoratee_args)
    }
}

fn inline_attr_body<'a>(
    attr_body: &Output<'a>,
    target: Output<'a>,
    extras: &[Output<'a>],
    extra_param_names: &[String],
    decoratee_args: &Output<'a>,
) -> Output<'a> {
    let mut subs = HashMap::new();
    for (name, expr) in extra_param_names.iter().zip(extras.iter()) {
        subs.insert(name.as_str(), expr.clone());
    }
    rewrite_expr_inline(attr_body, &target, &subs, decoratee_args)
}

fn expand_function_user_attrs<'a>(
    attr_bodies: &HashMap<String, Output<'a>>,
    attrs: &mut Vec<Attribute<'a>>,
    args: &Output<'a>,
    body: &mut Option<Output<'a>>,
    user_attrs: &HashSet<String>,
    attr_extra_names: &HashMap<String, Vec<String>>,
    span: SimpleSpan,
    messages: &mut Vec<Message>,
) {
    let user_attrs_copy: Vec<Attribute<'static>> = user_attrs_on(attrs, user_attrs)
        .iter()
        .map(|a| clone_attr_static(a))
        .collect();
    if user_attrs_copy.is_empty() {
        return;
    }
    let Some(orig_body) = body.take() else {
        return;
    };
    strip_user_attrs(attrs, user_attrs);
    let orig_for_fallback = orig_body.clone();
    let mut wrapped = orig_body;
    for attr in user_attrs_copy.iter().rev() {
        let extra_params: Vec<(&'static str, Option<Output<'a>>)> = attr_extra_names
            .get(attr.name)
            .map(|names| names.iter().map(|n| (leak(n.clone()), None)).collect())
            .unwrap_or_default();
        let extra_names = attr_extra_names.get(attr.name).cloned().unwrap_or_default();
        let Some(extras) = resolve_attr_extras(attr, &extra_params, span, messages) else {
            continue;
        };
        if let Some(attr_body) = find_attr_function_body(attr_bodies, attr.name) {
            wrapped = inline_attr_body(&attr_body, wrapped, &extras, &extra_names, args);
        } else {
            let inner = at(
                span,
                Expression::Lambda {
                    args: args.clone(),
                    captures: collect_lambda_captures(&orig_for_fallback, args),
                    body: orig_for_fallback.clone(),
                },
            );
            let mut call_args = vec![inner];
            call_args.extend(extras);
            call_args.extend(forward_call_args(span, args));
            wrapped = at(
                span,
                Expression::Call {
                    name: ident(span, attr.name),
                    args: Some(call_args),
                },
            );
        }
    }
    *body = Some(block_return(span, wrapped));
}

fn synthesize_class_ctor<'a>(
    span: SimpleSpan,
    class_name: &'a str,
    fields: &[Output<'a>],
) -> Output<'a> {
    let mut args = Vec::new();
    let mut call_args = Vec::new();
    for field in fields {
        if let Expression::Field {
            docs: _,
            name: name_expr,
            ty: ty_expr,
            ..
        } = field.1.as_ref()
        {
            if let Expression::Identifier(name) = name_expr.1.as_ref() {
                args.push(at(
                    span,
                    Expression::Argument {
                        docs: Vec::new(),
                        ty: Some(ty_expr.clone()),
                        name,
                        is_rest: false,
                    },
                ));
                call_args.push(ident(span, name));
            }
        }
    }
    let ctor_name = leak(format!("{class_name}__ctor"));
    let body = block_return(
        span,
        at(
            span,
            Expression::Instantiate(ident(span, class_name), Some(call_args)),
        ),
    );
    at(
        span,
        Expression::Function {
            docs: vec![],
            attrs: vec![],
            name: ctor_name,
            is_coro: false,
            is_static: false,
            type_params: vec![],
            args: at(span, Expression::Fragment(args)),
            returns: Some(ty_name(span, class_name)),
            where_constraints: vec![],
            body: Some(body),
        },
    )
}

/// Shape info needed to synthesize derive methods (no borrow of the decl AST).
#[derive(Clone)]
enum VariantShape<'a> {
    Unit,
    Tuple(usize),
    Record(Vec<&'a str>),
}

#[derive(Clone)]
struct VariantMeta<'a> {
    name: &'a str,
    shape: VariantShape<'a>,
}

fn variant_metas<'a>(variants: &[Output<'a>]) -> Vec<VariantMeta<'a>> {
    variants
        .iter()
        .filter_map(|v| match v.1.as_ref() {
            Expression::EnumVariant { docs: _, name, payload, .. } => Some(VariantMeta {
                name,
                shape: match payload {
                    EnumVariantPayload::Unit => VariantShape::Unit,
                    EnumVariantPayload::Tuple(parts) => VariantShape::Tuple(parts.len()),
                    EnumVariantPayload::Record(fields) => {
                        VariantShape::Record(fields.iter().map(|f| f.name).collect())
                    }
                },
            }),
            _ => None,
        })
        .collect()
}

fn expand_decls<'a>(
    decls: &mut Vec<Output<'a>>,
    user_attrs: &HashSet<String>,
    attr_extra_names: &HashMap<String, Vec<String>>,
    attr_bodies: &HashMap<String, Output<'a>>,
    decorated_class_ctors: &mut HashMap<String, String>,
) -> Vec<Message> {
    let mut messages = Vec::new();
    let mut i = 0;
    while i < decls.len() {
        let span = decls[i].0;

        // Compile-time FFI is `extern "lib" { fn …; }` only.
        if let Expression::Function {
            docs: _,
            attrs,
            name: _,
            is_coro,
            args,
            returns: _,
            body,
            ..
        } = decls[i].1.as_mut()
        {
            let is_ffi_sig = body.is_none();
            validate_attrs(
                attrs,
                "function",
                user_attrs,
                &mut messages,
                span,
                is_ffi_sig,
            );
            if body.is_some() {
                let mut crossing: Vec<String> = Vec::new();
                for attr in user_attrs_on(attrs, user_attrs) {
                    if attr_bodies
                        .get(attr.name)
                        .is_some_and(attr_body_crosses_yield)
                    {
                        crossing.push(attr.name.to_string());
                    }
                }
                for name in &crossing {
                    messages.push(Message::error(
                        ErrorCode::GenericTypeError,
                        format!(
                            "user-defined attribute `{name}` cannot be applied to `async fn` (attribute body contains yield outside `target(...args)`)"
                        ),
                        span.into_range(),
                    ));
                }
                if *is_coro {
                    let drop_attrs: HashSet<&str> = crossing.iter().map(String::as_str).collect();
                    attrs.retain(|a| {
                        !drop_attrs.contains(a.name)
                            || !user_attrs.contains(a.name)
                            || KNOWN_ATTRS.contains(&a.name)
                    });
                }
                if !user_attrs_on(attrs, user_attrs).is_empty() {
                    expand_function_user_attrs(
                        attr_bodies,
                        attrs,
                        args,
                        body,
                        user_attrs,
                        attr_extra_names,
                        span,
                        &mut messages,
                    );
                }
            }
            if attrs.iter().any(|a| a.name == "ffi") {
                messages.push(Message::error(
                    ErrorCode::GenericTypeError,
                    "`#[ffi]` is not supported; use `extern \"lib\" { fn …; }`".to_string(),
                    span.into_range(),
                ));
            } else if is_ffi_sig {
                messages.push(Message::error(
                    ErrorCode::GenericTypeError,
                    "Signature-only function requires `extern \"lib\" { fn …; }`".to_string(),
                    span.into_range(),
                ));
            }
        }

        // Expand user attrs on impl methods.
        if let Expression::Implementation { methods, .. } = decls[i].1.as_mut() {
            for method in methods.iter_mut() {
                if let Expression::Method(_, func_out) = method.1.as_mut() {
                    if let Expression::Function {
                        docs: _,
                        attrs, args, body, ..
                    } = func_out.1.as_mut()
                    {
                        validate_attrs(attrs, "function", user_attrs, &mut messages, span, false);
                        if body.is_some() {
                            expand_function_user_attrs(
                                attr_bodies,
                                attrs,
                                args,
                                body,
                                user_attrs,
                                attr_extra_names,
                                span,
                                &mut messages,
                            );
                        }
                    }
                }
            }
        }

        enum Job<'a> {
            Enum {
                name: &'a str,
                generic: bool,
                derives: Vec<&'a str>,
                variants: Vec<VariantMeta<'a>>,
                variant_nodes: Vec<Output<'a>>,
            },
            Class {
                name: &'a str,
                generic: bool,
                derives: Vec<&'a str>,
                fields: Vec<&'a str>,
            },
        }
        let job = match decls[i].1.as_ref() {
            Expression::EnumDecl {
                docs: _,
                name,
                type_params,
                attrs,
                variants,
            } => {
                validate_attrs(attrs, "enum", user_attrs, &mut messages, span, false);
                let derives = derive_traits_from_attrs(attrs);
                Some(Job::Enum {
                    name,
                    generic: !type_params.is_empty(),
                    derives,
                    variants: variant_metas(variants),
                    variant_nodes: variants.clone(),
                })
            }
            Expression::Class {
                docs: _,
                name,
                type_params,
                attrs,
                fields,
            } => {
                validate_attrs(attrs, "class", user_attrs, &mut messages, span, false);
                let derives = derive_traits_from_attrs(attrs);
                Some(Job::Class {
                    name,
                    generic: !type_params.is_empty(),
                    derives,
                    fields: class_field_names(fields),
                })
            }
            _ => None,
        };

        let mut ctor_insert: Option<Output> = None;
        if let Expression::Class {
            docs: _,
            name,
            attrs,
            fields,
            ..
        } = decls[i].1.as_ref()
        {
            if !user_attrs_on(attrs, user_attrs).is_empty() {
                let class_name = *name;
                let fields_copy = fields.clone();
                let mut ctor = synthesize_class_ctor(span, class_name, &fields_copy);
                if let Expression::Function {
                    docs: _,
                    attrs: ctor_attrs,
                    args,
                    body,
                    ..
                } = ctor.1.as_mut()
                {
                    // User attrs live on the class decl; copy them onto the
                    // synthesized ctor so expansion wraps construction.
                    *ctor_attrs = attrs
                        .iter()
                        .filter(|a| is_user_attr(a, user_attrs))
                        .cloned()
                        .collect();
                    expand_function_user_attrs(
                        attr_bodies,
                        ctor_attrs,
                        args,
                        body,
                        user_attrs,
                        attr_extra_names,
                        span,
                        &mut messages,
                    );
                }
                decorated_class_ctors.insert(class_name.to_string(), format!("{class_name}__ctor"));
                strip_user_attrs(
                    match decls[i].1.as_mut() {
                        Expression::Class { attrs, .. } => attrs,
                        _ => unreachable!(),
                    },
                    user_attrs,
                );
                ctor_insert = Some(ctor);
            }
        }

        let synthesized = match job {
            Some(Job::Enum {
                name,
                generic,
                derives,
                variants,
                variant_nodes,
            }) => Some(expand_enum(
                span,
                name,
                generic,
                &derives,
                &variants,
                &variant_nodes,
                &decls,
                &mut messages,
            )),
            Some(Job::Class {
                name,
                generic,
                derives,
                fields,
            }) => Some(expand_class(
                span,
                name,
                generic,
                &derives,
                &fields,
                &decls,
                &mut messages,
            )),
            None => None,
        };

        if let Some(impls) = synthesized {
            if let Expression::EnumDecl { attrs, .. } | Expression::Class { attrs, .. } =
                decls[i].1.as_mut()
            {
                strip_processed_attrs(attrs);
            }
            let n = impls.len();
            for (offset, impl_node) in impls.into_iter().enumerate() {
                decls.insert(i + 1 + offset, impl_node);
            }
            let mut advance = 1 + n;
            if let Some(ctor) = ctor_insert {
                decls.insert(i + advance, ctor);
                advance += 1;
            }
            i += advance;
        } else if let Some(ctor) = ctor_insert {
            decls.insert(i + 1, ctor);
            i += 2;
        } else {
            i += 1;
        }
    }
    messages
}

fn expand_enum<'a>(
    span: SimpleSpan,
    name: &'a str,
    generic: bool,
    derives: &[&'a str],
    variants: &[VariantMeta<'a>],
    _variant_nodes: &[Output<'a>],
    decls: &[Output<'a>],
    messages: &mut Vec<Message>,
) -> Vec<Output<'a>> {
    if generic {
        if !derives.is_empty() {
            messages.push(Message::error(
                ErrorCode::GenericTypeError,
                format!(
                    "Cannot derive traits for generic enum `{}`; write an explicit `impl`",
                    name
                ),
                span.into_range(),
            ));
        }
        return Vec::new();
    }

    let mut out = Vec::new();
    for &trait_name in derives {
        if let Some(msg) = check_derivable(trait_name, span) {
            messages.push(msg);
            continue;
        }
        match trait_name {
            "Show" => out.push(synth_show_enum(span, name, variants)),
            "Eq" => out.push(synth_eq_enum(span, name, variants)),
            "Ord" => out.extend(synth_ord_enum(span, name, variants)),
            "Default" => out.push(synth_default_enum(span, name, variants)),
            "Hash" => out.push(synth_hash_enum(span, name, variants)),
            "String" => out.push(synth_string_enum(span, name, variants)),
            "Serialize" => out.push(synth_serialize_enum(span, name, variants)),
            "Deserialize" => out.push(synth_deserialize_enum(span, name, variants)),
            "Send" | "Sensitive" => out.push(synth_marker_impl(span, trait_name, name)),
            _ => unreachable!(),
        }
    }
    push_default_display_impls(span, name, derives, decls, &mut out);
    out
}

fn expand_class<'a>(
    span: SimpleSpan,
    name: &'a str,
    generic: bool,
    derives: &[&'a str],
    field_names: &[&'a str],
    decls: &[Output<'a>],
    messages: &mut Vec<Message>,
) -> Vec<Output<'a>> {
    if generic {
        if !derives.is_empty() {
            messages.push(Message::error(
                ErrorCode::GenericTypeError,
                format!(
                    "Cannot derive traits for generic class `{}`; write an explicit `impl`",
                    name
                ),
                span.into_range(),
            ));
        }
        return Vec::new();
    }

    let mut out = Vec::new();
    for &trait_name in derives {
        if let Some(msg) = check_derivable(trait_name, span) {
            messages.push(msg);
            continue;
        }
        match trait_name {
            "Show" => out.push(synth_show_class(span, name, field_names)),
            "Eq" => out.push(synth_eq_class(span, name, field_names)),
            "Ord" => out.extend(synth_ord_class(span, name, field_names)),
            "Default" => out.push(synth_default_class(span, name, field_names)),
            "Hash" => out.push(synth_hash_class(span, name, field_names)),
            "String" => out.push(synth_string_class(span, name, field_names)),
            "Serialize" => out.push(synth_serialize_class(span, name, field_names)),
            "Deserialize" => out.push(synth_deserialize_class(span, name, field_names)),
            "Send" | "Sensitive" => out.push(synth_marker_impl(span, trait_name, name)),
            _ => unreachable!(),
        }
    }
    push_default_display_impls(span, name, derives, decls, &mut out);
    out
}

/// Auto-generate FQN-only `Show`/`String` when neither derive nor an explicit
/// `impl` covers the type. Bodies return the type name as a string literal
/// (same display as `typeof self` for non-generic types).
///
/// Inserted beside the type so typecheck sees the instance before later
/// `fn main` / statements. Script-style top-level match/expr after the type
/// should use `fn main` — these impls bind function entries and would
/// otherwise steal `program_start_offset` (DCE then drops the match body).
fn push_default_display_impls<'a>(
    span: SimpleSpan,
    name: &'a str,
    derives: &[&str],
    decls: &[Output<'a>],
    out: &mut Vec<Output<'a>>,
) {
    if !derives.iter().any(|t| *t == "Show") && !has_explicit_impl(decls, "Show", name) {
        out.push(synth_show_type_name(span, name));
    }
    if !derives.iter().any(|t| *t == "String") && !has_explicit_impl(decls, "String", name) {
        out.push(synth_string_type_name(span, name));
    }
}

fn has_explicit_impl(decls: &[Output<'_>], class: &str, ty_name: &str) -> bool {
    decls.iter().any(|d| {
        matches!(
            d.1.as_ref(),
            Expression::TypeClassImpl {
                class: c,
                args,
                ..
            } if *c == class
                && args.first().is_some_and(|a| {
                    matches!(a.1.as_ref(), Expression::Type(n) | Expression::Identifier(n) if *n == ty_name)
                })
        )
    })
}

fn synth_show_type_name<'a>(span: SimpleSpan, name: &'a str) -> Output<'a> {
    let p = leak(format!("__show_{}", name));
    let body = str_lit(span, name);
    let show_m = method_fn(
        span,
        "show",
        vec![arg(span, name, p)],
        "string",
        block_return(span, body),
    );
    typeclass_impl(span, "Show", name, vec![show_m])
}

fn synth_string_type_name<'a>(span: SimpleSpan, name: &'a str) -> Output<'a> {
    let p = leak(format!("__str_{}", name));
    let body = str_lit(span, name);
    let m = method_fn(
        span,
        "to_string",
        vec![arg(span, name, p)],
        "string",
        block_return(span, body),
    );
    typeclass_impl(span, "String", name, vec![m])
}

fn check_derivable(trait_name: &str, span: SimpleSpan) -> Option<Message> {
    if DERIVABLE.contains(&trait_name) {
        None
    } else {
        let mut msg = Message::error(
            ErrorCode::GenericTypeError,
            format!(
                "Cannot derive unknown or non-derivable trait `{}`",
                trait_name
            ),
            span.into_range(),
        );
        msg.with_help(format!("derivable traits are: {}", DERIVABLE.join(", ")));
        Some(msg)
    }
}

fn class_field_names<'a>(fields: &[Output<'a>]) -> Vec<&'a str> {
    fields
        .iter()
        .filter_map(|f| match f.1.as_ref() {
            Expression::Field {
                docs: _,
                name: name_expr, ..
            } => match name_expr.1.as_ref() {
                Expression::Identifier(n) => Some(*n),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

// ── string interning for synthetic AST ──────────────────────────────────────

fn leak(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}

fn clone_attr_lit_static(lit: &AttrLit<'_>) -> AttrLit<'static> {
    match lit {
        AttrLit::String(s) => AttrLit::String(leak(s.to_string())),
        AttrLit::Int(n) => AttrLit::Int(*n),
        AttrLit::Float(f) => AttrLit::Float(*f),
        AttrLit::Bool(b) => AttrLit::Bool(*b),
    }
}

fn clone_attr_args_static(args: &AttrArgs<'_>) -> AttrArgs<'static> {
    match args {
        AttrArgs::Empty => AttrArgs::Empty,
        AttrArgs::Idents(v) => AttrArgs::Idents(v.iter().map(|s| leak(s.to_string())).collect()),
        AttrArgs::KeyValues(kvs) => AttrArgs::KeyValues(
            kvs.iter()
                .map(|(k, lit)| (leak(k.to_string()), clone_attr_lit_static(lit)))
                .collect(),
        ),
        AttrArgs::Positional(lits) => {
            AttrArgs::Positional(lits.iter().map(clone_attr_lit_static).collect())
        }
        AttrArgs::String(s) => AttrArgs::String(leak(s.to_string())),
    }
}

fn clone_attr_static(attr: &Attribute<'_>) -> Attribute<'static> {
    Attribute {
        name: leak(attr.name.to_string()),
        args: clone_attr_args_static(&attr.args),
    }
}

/// Mint a unique span for each synthetic node.
///
/// Sharing the owning `enum`/`class` span across every derived expression
/// makes span-keyed codegen lookups (`lookup_for_codegen_span`, `%v` Show
/// lowering) collide and pick up the declaration's `unit` type. Unique
/// micro-spans keep the ID/infer caches aligned; expand diagnostics still
/// use the real header span from `expand_decls`.
fn fresh_span() -> SimpleSpan {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static NEXT: AtomicUsize = AtomicUsize::new(0x4000_0000);
    let start = NEXT.fetch_add(1, Ordering::Relaxed);
    SimpleSpan::from(start..start + 1)
}

fn at<'a>(_diag_span: SimpleSpan, expr: Expression<'a>) -> Output<'a> {
    (fresh_span(), Box::new(expr))
}

fn ty_name<'a>(span: SimpleSpan, name: &'a str) -> Output<'a> {
    at(span, Expression::Type(name))
}

/// Parse `int` / `string`, `Vec<elem>`, or legacy dynamic `[elem]` (maps to `Vec`).
fn ty_ret<'a>(span: SimpleSpan, ret: &'a str) -> Output<'a> {
    if let Some(elem) = ret
        .strip_prefix("Vec<")
        .and_then(|s| s.strip_suffix('>'))
    {
        return at(
            span,
            Expression::TypeApp {
                name: "Vec",
                args: vec![ty_name(span, leak(elem.to_string()))],
            },
        );
    }
    if ret.len() >= 2 && ret.starts_with('[') && ret.ends_with(']') && !ret.contains(';') {
        let elem = &ret[1..ret.len() - 1];
        return at(
            span,
            Expression::TypeApp {
                name: "Vec",
                args: vec![ty_name(span, elem)],
            },
        );
    }
    ty_name(span, ret)
}

fn vec_new_call<'a>(span: SimpleSpan) -> Output<'a> {
    at(
        span,
        Expression::Construct {
            enum_name: "Vec",
            variant_name: "new",
            fields: EnumConstructPayload::Unit,
        },
    )
}

fn vec_from_array<'a>(span: SimpleSpan, elems: Vec<Output<'a>>) -> Output<'a> {
    at(
        span,
        Expression::Construct {
            enum_name: "Vec",
            variant_name: "from",
            fields: EnumConstructPayload::Tuple(vec![at(span, Expression::Array(elems))]),
        },
    )
}

fn ident<'a>(span: SimpleSpan, name: &'a str) -> Output<'a> {
    at(span, Expression::Identifier(name))
}

fn str_lit<'a>(span: SimpleSpan, s: &'a str) -> Output<'a> {
    at(span, Expression::String(s))
}

fn string_format_call<'a>(span: SimpleSpan, fmt: &'a str, fmt_args: Vec<Output<'a>>) -> Output<'a> {
    let mut args = Vec::with_capacity(fmt_args.len() + 1);
    args.push(str_lit(span, fmt));
    args.extend(fmt_args);
    at(
        span,
        Expression::Call {
            name: at(
                span,
                Expression::QualifiedAccess {
                    owner: "string",
                    member: "format",
                },
            ),
            args: Some(args),
        },
    )
}

fn stmt<'a>(span: SimpleSpan, inner: Output<'a>) -> Output<'a> {
    at(span, Expression::Statement(inner))
}

fn block_return<'a>(span: SimpleSpan, value: Output<'a>) -> Output<'a> {
    at(
        span,
        Expression::Block(vec![stmt(span, at(span, Expression::Return(value)))]),
    )
}

fn method_fn<'a>(
    span: SimpleSpan,
    name: &'a str,
    args: Vec<Output<'a>>,
    ret: &'a str,
    body: Output<'a>,
) -> Output<'a> {
    let func = at(
        span,
        Expression::Function {
            docs: vec![],
            attrs: vec![],
            name,
            is_coro: false,
            is_static: false,
            type_params: vec![],
            args: at(span, Expression::Fragment(args)),
            returns: Some(ty_ret(span, ret)),
            where_constraints: vec![],
            body: Some(body),
        },
    );
    at(span, Expression::Method(Visibility::Private, func))
}

fn arg<'a>(span: SimpleSpan, ty: &'a str, name: &'a str) -> Output<'a> {
    at(
        span,
        Expression::Argument {
            docs: Vec::new(),
            ty: Some(ty_ret(span, ty)),
            name,
            is_rest: false,
        },
    )
}

fn typeclass_impl<'a>(
    span: SimpleSpan,
    class: &'a str,
    self_ty: &'a str,
    methods: Vec<Output<'a>>,
) -> Output<'a> {
    at(
        span,
        Expression::TypeClassImpl {
            class,
            args: vec![ty_name(span, self_ty)],
            methods,
        },
    )
}

// ── Show (enum) ─────────────────────────────────────────────────────────────

fn synth_show_enum<'a>(
    span: SimpleSpan,
    enum_name: &'a str,
    variants: &[VariantMeta<'a>],
) -> Output<'a> {
    // Unique param name — `codegen_var_types` is a flat map keyed by
    // simple name; two derived `show(p)` methods would clobber each other.
    let p = leak(format!("__show_{}", enum_name));
    let mut arms = Vec::new();
    for v in variants {
        let (pattern, fmt, fmt_args) = show_variant_arm(span, enum_name, v.name, &v.shape, p);
        let body = string_format_call(span, fmt, fmt_args);
        arms.push(MatchArm { pattern, body });
    }
    let match_expr = at(
        span,
        Expression::Match {
            scrutinee: ident(span, p),
            arms,
        },
    );
    let body = block_return(span, match_expr);
    let show_m = method_fn(span, "show", vec![arg(span, enum_name, p)], "string", body);
    typeclass_impl(span, "Show", enum_name, vec![show_m])
}

fn show_variant_arm<'a>(
    span: SimpleSpan,
    enum_name: &'a str,
    vname: &'a str,
    shape: &VariantShape<'a>,
    recv: &'a str,
) -> (PatternOut<'a>, &'static str, Vec<Output<'a>>) {
    match shape {
        VariantShape::Unit => {
            let fmt = leak(format!("{}::{}", enum_name, vname));
            (
                span_pat(
                    span,
                    Pattern::Constructor {
                        enum_name,
                        variant_name: vname,
                        payload: PatternPayload::Unit,
                    },
                ),
                fmt,
                vec![],
            )
        }
        VariantShape::Tuple(arity) => {
            // Wildcard payload + `recv.<i>` Access (synthetic tuple field
            // names `"0"`, `"1"`, …). Match binders in instance methods
            // disagree with JUMP_IF_MATCH push slots once `__dictN` is
            // present; LoadField via Access avoids that bug.
            let mut fmt_args = Vec::new();
            let mut specs = Vec::new();
            for i in 0..*arity {
                let fname = leak(i.to_string());
                fmt_args.push(at(span, Expression::Access(ident(span, recv), fname)));
                specs.push("%v");
            }
            let fmt = leak(format!("{}::{}({})", enum_name, vname, specs.join(", ")));
            (
                span_pat(
                    span,
                    Pattern::Constructor {
                        enum_name,
                        variant_name: vname,
                        payload: wildcard_tuple(span, *arity),
                    },
                ),
                fmt,
                fmt_args,
            )
        }
        VariantShape::Record(fields) => {
            // Wildcard payload + `recv.field` access — avoids match-binding
            // slots overwriting `__dict0` / sibling args in instance methods.
            let mut fmt_args = Vec::new();
            let mut specs = Vec::new();
            for &fname in fields {
                fmt_args.push(at(span, Expression::Access(ident(span, recv), fname)));
                specs.push(format!("{}: %v", fname));
            }
            let fmt = leak(format!(
                "{}::{} {{ {} }}",
                enum_name,
                vname,
                specs.join(", ")
            ));
            (
                span_pat(
                    span,
                    Pattern::Constructor {
                        enum_name,
                        variant_name: vname,
                        payload: record_wildcard_fields(span, fields),
                    },
                ),
                fmt,
                fmt_args,
            )
        }
    }
}

// ── Eq (enum) ───────────────────────────────────────────────────────────────

fn synth_eq_enum<'a>(
    span: SimpleSpan,
    enum_name: &'a str,
    variants: &[VariantMeta<'a>],
) -> Output<'a> {
    let a = leak(format!("__eq_a_{}", enum_name));
    let b = leak(format!("__eq_b_{}", enum_name));
    let mut arms = Vec::new();
    for v in variants {
        let (pat_a, body) = eq_variant_arm(span, enum_name, v.name, &v.shape, a, b);
        arms.push(MatchArm {
            pattern: pat_a,
            body,
        });
    }
    // Defensive fallback (should be unreachable if exhaustive).
    arms.push(MatchArm {
        pattern: span_pat(span, Pattern::Wildcard),
        body: at(span, Expression::Bool(false)),
    });
    let match_expr = at(
        span,
        Expression::Match {
            scrutinee: ident(span, a),
            arms,
        },
    );
    let eq_body = block_return(span, match_expr);
    let eq_m = method_fn(
        span,
        "eq",
        vec![arg(span, enum_name, a), arg(span, enum_name, b)],
        "bool",
        eq_body,
    );

    // ne(a, b) = !(a == b) — uses the Eq instance once `==` is wired.
    let ne_cmp = at(span, Expression::Eq(ident(span, a), ident(span, b)));
    let ne_body = block_return(span, at(span, Expression::LogicalNot(ne_cmp)));
    let ne_m = method_fn(
        span,
        "ne",
        vec![arg(span, enum_name, a), arg(span, enum_name, b)],
        "bool",
        ne_body,
    );

    typeclass_impl(span, "Eq", enum_name, vec![eq_m, ne_m])
}

fn eq_variant_arm<'a>(
    span: SimpleSpan,
    enum_name: &'a str,
    vname: &'a str,
    shape: &VariantShape<'a>,
    a_name: &'a str,
    b_name: &'a str,
) -> (PatternOut<'a>, Output<'a>) {
    match shape {
        VariantShape::Unit => {
            let ctor = Pattern::Constructor {
                enum_name,
                variant_name: vname,
                payload: PatternPayload::Unit,
            };
            let inner_arms = vec![
                MatchArm {
                    pattern: span_pat(span, ctor.clone()),
                    body: at(span, Expression::Bool(true)),
                },
                MatchArm {
                    pattern: span_pat(span, Pattern::Wildcard),
                    body: at(span, Expression::Bool(false)),
                },
            ];
            let body = at(
                span,
                Expression::Match {
                    scrutinee: ident(span, b_name),
                    arms: inner_arms,
                },
            );
            (span_pat(span, ctor), body)
        }
        VariantShape::Tuple(arity) => {
            let mut cmp: Option<Output<'a>> = None;
            for i in 0..*arity {
                let fname = leak(i.to_string());
                let l = at(span, Expression::Access(ident(span, a_name), fname));
                let r = at(span, Expression::Access(ident(span, b_name), fname));
                let eq = at(span, Expression::Eq(l, r));
                cmp = Some(match cmp {
                    None => eq,
                    Some(prev) => at(span, Expression::And(prev, eq)),
                });
            }
            let cmp = cmp.unwrap_or_else(|| at(span, Expression::Bool(true)));
            let wild_tuple = wildcard_tuple(span, *arity);
            let ctor = Pattern::Constructor {
                enum_name,
                variant_name: vname,
                payload: wild_tuple.clone(),
            };
            let inner_arms = vec![
                MatchArm {
                    pattern: span_pat(span, ctor.clone()),
                    body: cmp,
                },
                MatchArm {
                    pattern: span_pat(span, Pattern::Wildcard),
                    body: at(span, Expression::Bool(false)),
                },
            ];
            let body = at(
                span,
                Expression::Match {
                    scrutinee: ident(span, b_name),
                    arms: inner_arms,
                },
            );
            (span_pat(span, ctor), body)
        }
        VariantShape::Record(fields) => {
            let mut cmp: Option<Output<'a>> = None;
            for &fname in fields {
                let l = at(span, Expression::Access(ident(span, a_name), fname));
                let r = at(span, Expression::Access(ident(span, b_name), fname));
                let eq = at(span, Expression::Eq(l, r));
                cmp = Some(match cmp {
                    None => eq,
                    Some(prev) => at(span, Expression::And(prev, eq)),
                });
            }
            let cmp = cmp.unwrap_or_else(|| at(span, Expression::Bool(true)));
            let wild_record = record_wildcard_fields(span, fields);
            let ctor = Pattern::Constructor {
                enum_name,
                variant_name: vname,
                payload: wild_record.clone(),
            };
            let inner_arms = vec![
                MatchArm {
                    pattern: span_pat(span, ctor.clone()),
                    body: cmp,
                },
                MatchArm {
                    pattern: span_pat(span, Pattern::Wildcard),
                    body: at(span, Expression::Bool(false)),
                },
            ];
            let body = at(
                span,
                Expression::Match {
                    scrutinee: ident(span, b_name),
                    arms: inner_arms,
                },
            );
            (span_pat(span, ctor), body)
        }
    }
}

// ── Ord (enum) ──────────────────────────────────────────────────────────────

/// Expand `derive Ord` into the four comparison instances plus an empty
/// `Ord` marker, matching the builtin `int`/`float` layout after PR #14
/// (`Ord` has no methods; `T: Ord` implies `Lt`/`Le`/`Gt`/`Ge`).
fn synth_ord_enum<'a>(
    span: SimpleSpan,
    enum_name: &'a str,
    variants: &[VariantMeta<'a>],
) -> Vec<Output<'a>> {
    // Encode tag order via nested matches: compare tags by walking variants
    // in declaration order. For equal tags, compare payloads field-wise.
    // Per-op param names avoid clobbering the flat `codegen_var_types` map.
    let mut out = Vec::with_capacity(5);
    for op in [OrdOp::Lt, OrdOp::Le, OrdOp::Gt, OrdOp::Ge] {
        let a = leak(format!("__ord_{}_a_{}", op.name(), enum_name));
        let b = leak(format!("__ord_{}_b_{}", op.name(), enum_name));
        let method = ord_method(span, enum_name, variants, a, b, op);
        out.push(typeclass_impl(
            span,
            op.trait_name(),
            enum_name,
            vec![method],
        ));
    }
    out.push(typeclass_impl(span, "Ord", enum_name, vec![]));
    out
}

#[derive(Clone, Copy)]
enum OrdOp {
    Lt,
    Le,
    Gt,
    Ge,
}

impl OrdOp {
    fn name(self) -> &'static str {
        match self {
            OrdOp::Lt => "lt",
            OrdOp::Le => "le",
            OrdOp::Gt => "gt",
            OrdOp::Ge => "ge",
        }
    }

    fn trait_name(self) -> &'static str {
        match self {
            OrdOp::Lt => "Lt",
            OrdOp::Le => "Le",
            OrdOp::Gt => "Gt",
            OrdOp::Ge => "Ge",
        }
    }

    /// Strict field inequality used in the lexicographic fold.
    ///
    /// AST note: `Expression::Le` / `Gt` are the strict `<` / `>` operators
    /// (see `infer_comparison`); inclusive `<=` / `>=` are `Leq` / `Geq`.
    /// Inclusive `Le`/`Ge` still use strict `<`/`>` here so equal prefixes
    /// fall through to the `(== && rest)` arm; the inclusive case is handled
    /// by [`eq_payload_result`] on the final empty-payload base.
    fn primary(self) -> for<'a> fn(SimpleSpan, Output<'a>, Output<'a>) -> Output<'a> {
        match self {
            OrdOp::Lt | OrdOp::Le => |s, l, r| at(s, Expression::Le(l, r)),
            OrdOp::Gt | OrdOp::Ge => |s, l, r| at(s, Expression::Gt(l, r)),
        }
    }

    /// When tags differ and left tag index < right tag index.
    fn when_left_tag_less(self) -> bool {
        matches!(self, OrdOp::Lt | OrdOp::Le)
    }

    /// When tags are equal — use `<=`/`>=`/`</>` on fields; for Le/Ge
    /// equal payloads must return true.
    fn eq_payload_result(self) -> bool {
        matches!(self, OrdOp::Le | OrdOp::Ge)
    }
}

fn ord_method<'a>(
    span: SimpleSpan,
    enum_name: &'a str,
    variants: &[VariantMeta<'a>],
    a: &'a str,
    b: &'a str,
    op: OrdOp,
) -> Output<'a> {
    // Record/Unit: tag-only wildcards + `a.field` / `b.field` after same-tag.
    // Tuple: outer binders → `let` bridge → inner binders (nested match arms
    // do not see outer pattern bindings at codegen time; enums are not
    // indexable, so `a[i]` is not an option).
    let mut arms = Vec::new();
    for (i, v) in variants.iter().enumerate() {
        let (pattern, body) = ord_outer_arm(span, enum_name, variants, i, v, a, b, op);
        arms.push(MatchArm { pattern, body });
    }
    arms.push(MatchArm {
        pattern: span_pat(span, Pattern::Wildcard),
        body: at(span, Expression::Bool(false)),
    });
    let match_expr = at(
        span,
        Expression::Match {
            scrutinee: ident(span, a),
            arms,
        },
    );
    method_fn(
        span,
        op.name(),
        vec![arg(span, enum_name, a), arg(span, enum_name, b)],
        "bool",
        block_return(span, match_expr),
    )
}

fn ord_outer_arm<'a>(
    span: SimpleSpan,
    enum_name: &'a str,
    variants: &[VariantMeta<'a>],
    left_idx: usize,
    left: &VariantMeta<'a>,
    a: &'a str,
    b: &'a str,
    op: OrdOp,
) -> (PatternOut<'a>, Output<'a>) {
    let mut inner_arms = Vec::new();
    for (j, rv) in variants.iter().enumerate() {
        let body = if j == left_idx {
            ord_payload_cmp(span, &left.shape, a, b, op)
        } else if j > left_idx {
            at(span, Expression::Bool(op.when_left_tag_less()))
        } else {
            at(span, Expression::Bool(!op.when_left_tag_less()))
        };
        inner_arms.push(MatchArm {
            pattern: ord_wildcard_pattern(span, enum_name, rv.name, &rv.shape),
            body,
        });
    }
    inner_arms.push(MatchArm {
        pattern: span_pat(span, Pattern::Wildcard),
        body: at(span, Expression::Bool(false)),
    });
    let body = at(
        span,
        Expression::Match {
            scrutinee: ident(span, b),
            arms: inner_arms,
        },
    );
    (
        ord_wildcard_pattern(span, enum_name, left.name, &left.shape),
        body,
    )
}

fn ord_wildcard_pattern<'a>(
    span: SimpleSpan,
    enum_name: &'a str,
    vname: &'a str,
    shape: &VariantShape<'a>,
) -> PatternOut<'a> {
    span_pat(
        span,
        match shape {
            VariantShape::Unit => Pattern::Constructor {
                enum_name,
                variant_name: vname,
                payload: PatternPayload::Unit,
            },
            VariantShape::Tuple(arity) => Pattern::Constructor {
                enum_name,
                variant_name: vname,
                payload: wildcard_tuple(span, *arity),
            },
            VariantShape::Record(fields) => Pattern::Constructor {
                enum_name,
                variant_name: vname,
                payload: record_wildcard_fields(span, fields),
            },
        },
    )
}

fn ord_payload_cmp<'a>(
    span: SimpleSpan,
    left: &VariantShape<'a>,
    a: &'a str,
    b: &'a str,
    op: OrdOp,
) -> Output<'a> {
    // Lexicographic compare via `a.field` / `b.field` (records) or
    // synthetic Access indices `"0"`, `"1"`, … (tuples).
    let primary = op.primary();
    let mut acc = at(span, Expression::Bool(op.eq_payload_result()));
    match left {
        VariantShape::Unit => acc,
        VariantShape::Tuple(arity) => {
            for i in (0..*arity).rev() {
                let fname = leak(i.to_string());
                let l = at(span, Expression::Access(ident(span, a), fname));
                let r = at(span, Expression::Access(ident(span, b), fname));
                let l2 = at(span, Expression::Access(ident(span, a), fname));
                let r2 = at(span, Expression::Access(ident(span, b), fname));
                let prim = primary(span, l, r);
                let eq = at(span, Expression::Eq(l2, r2));
                let and_rest = at(span, Expression::And(eq, acc));
                acc = at(span, Expression::Or(prim, and_rest));
            }
            acc
        }
        VariantShape::Record(fields) => {
            for &fname in fields.iter().rev() {
                let l = at(span, Expression::Access(ident(span, a), fname));
                let r = at(span, Expression::Access(ident(span, b), fname));
                let l2 = at(span, Expression::Access(ident(span, a), fname));
                let r2 = at(span, Expression::Access(ident(span, b), fname));
                let prim = primary(span, l, r);
                let eq = at(span, Expression::Eq(l2, r2));
                let and_rest = at(span, Expression::And(eq, acc));
                acc = at(span, Expression::Or(prim, and_rest));
            }
            acc
        }
    }
}

// ── Show / Eq / Ord (class) ─────────────────────────────────────────────────

fn synth_show_class<'a>(span: SimpleSpan, name: &'a str, fields: &[&'a str]) -> Output<'a> {
    let p = leak(format!("__show_{}", name));
    let mut specs = Vec::new();
    let mut fmt_args = Vec::new();
    for f in fields {
        specs.push(format!("{}: %v", f));
        fmt_args.push(at(span, Expression::Access(ident(span, p), f)));
    }
    let fmt = leak(format!("{} {{ {} }}", name, specs.join(", ")));
    let format = string_format_call(span, fmt, fmt_args);
    let show_m = method_fn(
        span,
        "show",
        vec![arg(span, name, p)],
        "string",
        block_return(span, format),
    );
    typeclass_impl(span, "Show", name, vec![show_m])
}

fn synth_eq_class<'a>(span: SimpleSpan, name: &'a str, fields: &[&'a str]) -> Output<'a> {
    let a = leak(format!("__eq_a_{}", name));
    let b = leak(format!("__eq_b_{}", name));
    let mut cmp: Option<Output<'a>> = None;
    for f in fields {
        let l = at(span, Expression::Access(ident(span, a), f));
        let r = at(span, Expression::Access(ident(span, b), f));
        let eq = at(span, Expression::Eq(l, r));
        cmp = Some(match cmp {
            None => eq,
            Some(prev) => at(span, Expression::And(prev, eq)),
        });
    }
    let cmp = cmp.unwrap_or_else(|| at(span, Expression::Bool(true)));
    let eq_m = method_fn(
        span,
        "eq",
        vec![arg(span, name, a), arg(span, name, b)],
        "bool",
        block_return(span, cmp),
    );
    // ne(a, b) = !(a == b) — same as enum derive (do not call `eq` by name).
    let ne_cmp = at(span, Expression::Eq(ident(span, a), ident(span, b)));
    let ne_m = method_fn(
        span,
        "ne",
        vec![arg(span, name, a), arg(span, name, b)],
        "bool",
        block_return(span, at(span, Expression::LogicalNot(ne_cmp))),
    );
    typeclass_impl(span, "Eq", name, vec![eq_m, ne_m])
}

fn synth_ord_class<'a>(span: SimpleSpan, name: &'a str, fields: &[&'a str]) -> Vec<Output<'a>> {
    let mut out = Vec::with_capacity(5);
    for op in [OrdOp::Lt, OrdOp::Le, OrdOp::Gt, OrdOp::Ge] {
        let a = leak(format!("__ord_{}_a_{}", op.name(), name));
        let b = leak(format!("__ord_{}_b_{}", op.name(), name));
        let body = class_ord_body(span, a, b, fields, op);
        let method = method_fn(
            span,
            op.name(),
            vec![arg(span, name, a), arg(span, name, b)],
            "bool",
            block_return(span, body),
        );
        out.push(typeclass_impl(span, op.trait_name(), name, vec![method]));
    }
    out.push(typeclass_impl(span, "Ord", name, vec![]));
    out
}

fn class_ord_body<'a>(
    span: SimpleSpan,
    a: &'a str,
    b: &'a str,
    fields: &[&'a str],
    op: OrdOp,
) -> Output<'a> {
    if fields.is_empty() {
        return at(span, Expression::Bool(op.eq_payload_result()));
    }
    let primary = op.primary();
    let mut acc = at(span, Expression::Bool(op.eq_payload_result()));
    for f in fields.iter().rev() {
        let l = at(span, Expression::Access(ident(span, a), f));
        let r = at(span, Expression::Access(ident(span, b), f));
        let l2 = at(span, Expression::Access(ident(span, a), f));
        let r2 = at(span, Expression::Access(ident(span, b), f));
        let prim = primary(span, l, r);
        let eq = at(span, Expression::Eq(l2, r2));
        let and_rest = at(span, Expression::And(eq, acc));
        acc = at(span, Expression::Or(prim, and_rest));
    }
    acc
}

// ── Default / Hash / String / Serialize (derive MVP) ─────────────────────────

fn int_zero<'a>(span: SimpleSpan) -> Output<'a> {
    at(span, Expression::Integer(0))
}

fn as_byte<'a>(span: SimpleSpan, expr: Output<'a>) -> Output<'a> {
    at(span, Expression::Cast(expr, ty_name(span, "byte")))
}

fn as_int<'a>(span: SimpleSpan, expr: Output<'a>) -> Output<'a> {
    at(span, Expression::Cast(expr, ty_name(span, "int")))
}

fn hash_combine<'a>(span: SimpleSpan, acc: Output<'a>, field: Output<'a>) -> Output<'a> {
    let scaled = at(
        span,
        Expression::Mul(acc, at(span, Expression::Integer(31))),
    );
    at(span, Expression::Add(scaled, field))
}

/// `value.hash()` — recursive Hash dispatch for derive payloads.
fn hash_of<'a>(span: SimpleSpan, value: Output<'a>) -> Output<'a> {
    at(
        span,
        Expression::Call {
            name: at(span, Expression::Access(value, "hash")),
            args: None,
        },
    )
}

fn default_enum_variant<'a>(
    span: SimpleSpan,
    enum_name: &'a str,
    v: &VariantMeta<'a>,
) -> Output<'a> {
    match &v.shape {
        VariantShape::Unit => at(
            span,
            Expression::Construct {
                enum_name,
                variant_name: v.name,
                fields: EnumConstructPayload::Unit,
            },
        ),
        VariantShape::Tuple(arity) => {
            let items = (0..*arity).map(|_| int_zero(span)).collect();
            at(
                span,
                Expression::Construct {
                    enum_name,
                    variant_name: v.name,
                    fields: EnumConstructPayload::Tuple(items),
                },
            )
        }
        VariantShape::Record(fields) => {
            let records = fields
                .iter()
                .map(|fname| RecordFieldValue {
                    name: fname,
                    value: int_zero(span),
                })
                .collect();
            at(
                span,
                Expression::Construct {
                    enum_name,
                    variant_name: v.name,
                    fields: EnumConstructPayload::Record(records),
                },
            )
        }
    }
}

fn synth_default_enum<'a>(
    span: SimpleSpan,
    enum_name: &'a str,
    variants: &[VariantMeta<'a>],
) -> Output<'a> {
    let value = variants
        .first()
        .map(|v| default_enum_variant(span, enum_name, v))
        .unwrap_or_else(|| int_zero(span));
    let body = block_return(span, value);
    let m = method_fn(span, "default", vec![], enum_name, body);
    typeclass_impl(span, "Default", enum_name, vec![m])
}

fn synth_default_class<'a>(span: SimpleSpan, name: &'a str, fields: &[&'a str]) -> Output<'a> {
    let args: Vec<Output<'a>> = fields.iter().map(|_| int_zero(span)).collect();
    let value = at(span, Expression::Instantiate(ident(span, name), Some(args)));
    let m = method_fn(span, "default", vec![], name, block_return(span, value));
    typeclass_impl(span, "Default", name, vec![m])
}

fn hash_variant_body<'a>(
    span: SimpleSpan,
    tag: usize,
    shape: &VariantShape<'a>,
    recv: &'a str,
) -> Output<'a> {
    let mut acc = at(span, Expression::Integer(tag as i64));
    match shape {
        VariantShape::Unit => {}
        VariantShape::Tuple(arity) => {
            for i in 0..*arity {
                let fname = leak(i.to_string());
                let field = at(span, Expression::Access(ident(span, recv), fname));
                acc = hash_combine(span, acc, hash_of(span, field));
            }
        }
        VariantShape::Record(fields) => {
            for &fname in fields {
                let field = at(span, Expression::Access(ident(span, recv), fname));
                acc = hash_combine(span, acc, hash_of(span, field));
            }
        }
    }
    acc
}

fn synth_hash_enum<'a>(
    span: SimpleSpan,
    enum_name: &'a str,
    variants: &[VariantMeta<'a>],
) -> Output<'a> {
    let p = leak(format!("__hash_{enum_name}"));
    let mut arms = Vec::new();
    for (tag, v) in variants.iter().enumerate() {
        let body = hash_variant_body(span, tag, &v.shape, p);
        arms.push(MatchArm {
            pattern: ord_wildcard_pattern(span, enum_name, v.name, &v.shape),
            body,
        });
    }
    arms.push(MatchArm {
        pattern: span_pat(span, Pattern::Wildcard),
        body: int_zero(span),
    });
    let match_expr = at(
        span,
        Expression::Match {
            scrutinee: ident(span, p),
            arms,
        },
    );
    let m = method_fn(
        span,
        "hash",
        vec![arg(span, enum_name, p)],
        "int",
        block_return(span, match_expr),
    );
    typeclass_impl(span, "Hash", enum_name, vec![m])
}

fn synth_hash_class<'a>(span: SimpleSpan, name: &'a str, fields: &[&'a str]) -> Output<'a> {
    let p = leak(format!("__hash_{name}"));
    let mut acc = int_zero(span);
    for f in fields {
        let field = at(span, Expression::Access(ident(span, p), f));
        acc = hash_combine(span, acc, hash_of(span, field));
    }
    let m = method_fn(
        span,
        "hash",
        vec![arg(span, name, p)],
        "int",
        block_return(span, acc),
    );
    typeclass_impl(span, "Hash", name, vec![m])
}

fn synth_string_enum<'a>(
    span: SimpleSpan,
    enum_name: &'a str,
    variants: &[VariantMeta<'a>],
) -> Output<'a> {
    let p = leak(format!("__str_{enum_name}"));
    let mut arms = Vec::new();
    for v in variants {
        let (pattern, fmt, fmt_args) = show_variant_arm(span, enum_name, v.name, &v.shape, p);
        let body = string_format_call(span, fmt, fmt_args);
        arms.push(MatchArm { pattern, body });
    }
    let match_expr = at(
        span,
        Expression::Match {
            scrutinee: ident(span, p),
            arms,
        },
    );
    let m = method_fn(
        span,
        "to_string",
        vec![arg(span, enum_name, p)],
        "string",
        block_return(span, match_expr),
    );
    typeclass_impl(span, "String", enum_name, vec![m])
}

fn synth_string_class<'a>(span: SimpleSpan, name: &'a str, fields: &[&'a str]) -> Output<'a> {
    let p = leak(format!("__str_{name}"));
    let mut specs = Vec::new();
    let mut fmt_args = Vec::new();
    for f in fields {
        specs.push(format!("{f}: %v"));
        fmt_args.push(at(span, Expression::Access(ident(span, p), f)));
    }
    let fmt = leak(format!("{name} {{ {} }}", specs.join(", ")));
    let format = string_format_call(span, fmt, fmt_args);
    let m = method_fn(
        span,
        "to_string",
        vec![arg(span, name, p)],
        "string",
        block_return(span, format),
    );
    typeclass_impl(span, "String", name, vec![m])
}

fn serialize_variant_body<'a>(
    span: SimpleSpan,
    tag: usize,
    shape: &VariantShape<'a>,
    recv: &'a str,
) -> Output<'a> {
    let mut elems = vec![as_byte(span, at(span, Expression::Integer(tag as i64)))];
    match shape {
        VariantShape::Unit => {}
        VariantShape::Tuple(arity) => {
            for i in 0..*arity {
                let fname = leak(i.to_string());
                let field = at(span, Expression::Access(ident(span, recv), fname));
                elems.push(as_byte(span, field));
            }
        }
        VariantShape::Record(fields) => {
            for &fname in fields {
                let field = at(span, Expression::Access(ident(span, recv), fname));
                elems.push(as_byte(span, field));
            }
        }
    }
    vec_from_array(span, elems)
}

fn synth_serialize_enum<'a>(
    span: SimpleSpan,
    enum_name: &'a str,
    variants: &[VariantMeta<'a>],
) -> Output<'a> {
    let p = leak(format!("__ser_{enum_name}"));
    let mut arms = Vec::new();
    for (tag, v) in variants.iter().enumerate() {
        let body = serialize_variant_body(span, tag, &v.shape, p);
        arms.push(MatchArm {
            pattern: ord_wildcard_pattern(span, enum_name, v.name, &v.shape),
            body,
        });
    }
    arms.push(MatchArm {
        pattern: span_pat(span, Pattern::Wildcard),
        body: vec_new_call(span),
    });
    let match_expr = at(
        span,
        Expression::Match {
            scrutinee: ident(span, p),
            arms,
        },
    );
    let m = method_fn(
        span,
        "serialize",
        vec![arg(span, enum_name, p)],
        "Vec<byte>",
        block_return(span, match_expr),
    );
    typeclass_impl(span, "Serialize", enum_name, vec![m])
}

fn synth_marker_impl<'a>(span: SimpleSpan, trait_name: &'a str, self_ty: &'a str) -> Output<'a> {
    typeclass_impl(span, trait_name, self_ty, vec![])
}

fn data_at<'a>(span: SimpleSpan, data: &Output<'a>, index: usize) -> Output<'a> {
    at(
        span,
        Expression::Index(
            data.clone(),
            Some(at(span, Expression::Integer(index as i64))),
        ),
    )
}

fn deserialize_variant_value<'a>(
    span: SimpleSpan,
    enum_name: &'a str,
    v: &VariantMeta<'a>,
    data: &Output<'a>,
    base_index: usize,
) -> Output<'a> {
    match &v.shape {
        VariantShape::Unit => at(
            span,
            Expression::Construct {
                enum_name,
                variant_name: v.name,
                fields: EnumConstructPayload::Unit,
            },
        ),
        VariantShape::Tuple(arity) => {
            let items = (0..*arity)
                .map(|i| as_int(span, data_at(span, data, base_index + i)))
                .collect();
            at(
                span,
                Expression::Construct {
                    enum_name,
                    variant_name: v.name,
                    fields: EnumConstructPayload::Tuple(items),
                },
            )
        }
        VariantShape::Record(fields) => {
            let records = fields
                .iter()
                .enumerate()
                .map(|(i, fname)| RecordFieldValue {
                    name: fname,
                    value: as_int(span, data_at(span, data, base_index + i)),
                })
                .collect();
            at(
                span,
                Expression::Construct {
                    enum_name,
                    variant_name: v.name,
                    fields: EnumConstructPayload::Record(records),
                },
            )
        }
    }
}

fn if_tag_equals<'a>(
    span: SimpleSpan,
    data: &Output<'a>,
    tag: usize,
    body: Output<'a>,
) -> Output<'a> {
    let cond = at(
        span,
        Expression::Eq(
            as_int(span, data_at(span, data, 0)),
            at(span, Expression::Integer(tag as i64)),
        ),
    );
    at(span, Expression::Branch(Some(cond), body))
}

fn synth_deserialize_enum<'a>(
    span: SimpleSpan,
    enum_name: &'a str,
    variants: &[VariantMeta<'a>],
) -> Output<'a> {
    let data = leak(format!("__de_{enum_name}"));
    let panic_msg = leak(format!("deserialize: invalid tag for `{enum_name}`"));
    let err_body = at(span, Expression::Panic(str_lit(span, panic_msg)));
    let mut branches: Vec<Output<'a>> = variants
        .iter()
        .enumerate()
        .map(|(tag, v)| {
            if_tag_equals(
                span,
                &ident(span, data),
                tag,
                deserialize_variant_value(span, enum_name, v, &ident(span, data), 1),
            )
        })
        .collect();
    branches.push(at(span, Expression::Branch(None, err_body)));
    let body = block_return(span, at(span, Expression::If(branches)));
    let m = method_fn(
        span,
        "deserialize",
        vec![arg(span, "Vec<byte>", data)],
        enum_name,
        body,
    );
    typeclass_impl(span, "Deserialize", enum_name, vec![m])
}

fn synth_serialize_class<'a>(span: SimpleSpan, name: &'a str, fields: &[&'a str]) -> Output<'a> {
    let p = leak(format!("__ser_{name}"));
    let mut elems = Vec::new();
    for f in fields {
        let field = at(span, Expression::Access(ident(span, p), f));
        elems.push(as_byte(span, field));
    }
    let arr = vec_from_array(span, elems);
    let m = method_fn(
        span,
        "serialize",
        vec![arg(span, name, p)],
        "Vec<byte>",
        block_return(span, arr),
    );
    typeclass_impl(span, "Serialize", name, vec![m])
}

fn synth_deserialize_class<'a>(span: SimpleSpan, name: &'a str, fields: &[&'a str]) -> Output<'a> {
    let data = leak(format!("__de_{name}"));
    let args: Vec<Output<'a>> = fields
        .iter()
        .enumerate()
        .map(|(i, _)| as_int(span, data_at(span, &ident(span, data), i)))
        .collect();
    let value = at(span, Expression::Instantiate(ident(span, name), Some(args)));
    let m = method_fn(
        span,
        "deserialize",
        vec![arg(span, "Vec<byte>", data)],
        name,
        block_return(span, value),
    );
    typeclass_impl(span, "Deserialize", name, vec![m])
}


#[cfg(test)]
#[path = "attrs.tests.rs"]
mod tests;
