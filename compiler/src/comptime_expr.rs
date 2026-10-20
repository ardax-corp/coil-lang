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
        let mut consts = Vec::new();
        collect(&mut items[at], &mut consts);
        let n = consts.len();
        items.splice(at..at, consts);
        at += n + 1;
    }
}

fn collect<'a>(node: &mut Output<'a>, consts: &mut Vec<Output<'a>>) {
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
