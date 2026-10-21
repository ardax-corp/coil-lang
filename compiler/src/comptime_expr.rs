//! `comptime expr` (comptime step 5): each expression becomes a hidden
//! module `const`, so it is computed by the same evaluator, under the same
//! rules, as a written `const`, and the expression reads that const.

use parser::ast::{Expression, Output};

/// Name prefix of the hidden const that holds a `comptime` expression's value.
pub const PREFIX: &str = "__comptime_";

/// Whether `name` (short or qualified) is the const of a `comptime` expression.
pub fn is_comptime_const(name: &str) -> bool {
    name.rsplit("::").next().unwrap_or(name).starts_with(PREFIX)
}

/// "const `NAME`", or "this `comptime` expression" for a hidden one, for
/// diagnostics about a const.
pub fn describe(short: &str) -> String {
    if is_comptime_const(short) {
        "this `comptime` expression".to_string()
    } else {
        format!("const `{short}`")
    }
}

/// Replace every `comptime e` in `program` with a read of a new module
/// `const __comptime_<offset> = e;`, placed just before the item that holds
/// it (a const is visible from the items after it). Nested `comptime`
/// expressions get their own consts first. Idempotent.
pub fn hoist<'a>(program: &mut Output<'a>) {
    if let Expression::Program(items) = program.1.as_mut() {
        hoist_items(items);
    }
}

fn hoist_items<'a>(items: &mut Vec<Output<'a>>) {
    let mut at = 0;
    while at < items.len() {
        // A top-level block is code generation, expanded with the macros.
        if matches!(items[at].1.as_ref(), Expression::ComptimeBlock(_)) {
            at += 1;
            continue;
        }
        let mut consts = Vec::new();
        collect(&mut items[at], &mut consts);
        let n = consts.len();
        items.splice(at..at, consts);
        at += n + 1;
    }
}

fn collect<'a>(node: &mut Output<'a>, consts: &mut Vec<Output<'a>>) {
    if let Expression::ComptimeBlock(body) = node.1.as_mut() {
        // `comptime { … }` in an expression: `fn __comptime_<offset>_body()
        // { … }` (its return type inferred) and a `comptime` call of it.
        // Its own `comptime` expressions are hoisted first.
        let mut items = vec![std::mem::replace(body, (body.0, Box::new(Expression::Block(Vec::new()))))];
        hoist_items(&mut items);
        let body = items.pop().expect("the body");
        consts.extend(items);
        let fresh = crate::attrs::fresh_span;
        let name = crate::attrs::leak(format!("{PREFIX}{}_body", node.0.start));
        consts.push((
            fresh(),
            Box::new(Expression::Function {
                docs: Vec::new(),
                attrs: Vec::new(),
                name,
                is_coro: false,
                is_static: false,
                type_params: Vec::new(),
                args: (fresh(), Box::new(Expression::Fragment(Vec::new()))),
                returns: None,
                where_constraints: Vec::new(),
                effects: None,
                contracts: Vec::new(),
                body: Some(body),
            }),
        ));
        let call = Expression::Call {
            name: (fresh(), Box::new(Expression::Identifier(name))),
            args: Some(Vec::new()),
        };
        *node.1 = Expression::Comptime((fresh(), Box::new(call)));
    }
    node.1.for_each_child_mut(&mut |c| collect(c, consts));
    if !matches!(node.1.as_ref(), Expression::Comptime(_)) {
        return;
    }
    let span = node.0;
    let name = crate::attrs::leak(format!("{PREFIX}{}", span.start));
    let read = Box::new(Expression::Identifier(name));
    let Expression::Comptime(value) = *std::mem::replace(&mut node.1, read) else { unreachable!() };
    // Span-keyed checker tables need each synthetic node to have its own span.
    let fresh = crate::attrs::fresh_span;
    let binder = (fresh(), Box::new(Expression::Identifier(name)));
    let head = (fresh(), Box::new(Expression::Constant(binder, None)));
    consts.push((fresh(), Box::new(Expression::Fragment(vec![head, value]))));
}

/// Adjust the checker's messages for `program`'s `comptime` expressions: a
/// name it cannot find there is most likely a local variable, and the
/// hidden const's binding warnings speak of the expression.
pub fn explain_messages(program: &Output<'_>, messages: &mut Vec<reporting::Message>) {
    let Expression::Program(items) = program.1.as_ref() else { return };
    let ranges: Vec<(&str, std::ops::Range<usize>)> = items
        .iter()
        .filter_map(|item| match item.1.as_ref() {
            Expression::StaticDecl { name, init, .. } if is_comptime_const(name) => Some((*name, init.0.into_range())),
            // A `comptime { … }` block's hidden function.
            Expression::Function { name, body: Some(body), .. } if is_comptime_const(name) => {
                Some((*name, body.0.into_range()))
            }
            Expression::Fragment(parts) => match parts.as_slice() {
                [binder, value] => match binder.1.as_ref() {
                    Expression::Constant(name, _) => match name.1.as_ref() {
                        Expression::Identifier(n) if is_comptime_const(n) => Some((*n, value.0.into_range())),
                        _ => None,
                    },
                    _ => None,
                },
                _ => None,
            },
            _ => None,
        })
        .collect();
    if ranges.is_empty() {
        return;
    }
    for m in messages.iter_mut() {
        // The const's shallow-mutability warning, said of the expression.
        if let Some(rest) = m.message().strip_prefix("binding `")
            && let Some((_, at)) = ranges.iter().find(|(n, _)| rest.starts_with(&format!("{n}`")))
            && let Some(ty) = rest.split("of type `").nth(1).and_then(|t| t.strip_suffix("` is still mutable"))
        {
            let mut w = reporting::Message::warn(
                reporting::ErrorCode::GenericTypeError,
                format!("this `comptime` value of type `{ty}` is made once and shared by every use"),
                at.clone(),
            );
            w.with_help("changes through fields, indices, or `vec.push` are seen by later uses".to_string());
            *m = w;
            continue;
        }
        let r = m.range();
        if m.code() == Some(reporting::ErrorCode::UnknownValue)
            && m.help().is_none()
            && ranges.iter().any(|(_, c)| c.start <= r.start && r.end <= c.end)
        {
            m.with_help(
                "a `comptime` expression is computed while compiling, so it cannot use local variables or parameters"
                    .to_string(),
            );
        }
    }
}
