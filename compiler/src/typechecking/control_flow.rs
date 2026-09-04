//! Control-flow facts for path completeness, unreachable code, and dead defers.

use std::ops::Range;

use parser::ast::{Expression, Output};
use reporting::{ErrorCode, Label, Message};

use super::const_eval::{ConstVal, eval_bool_const};

/// Result of analyzing a function body for exits / warnings.
#[derive(Debug, Default)]
pub struct CfAnalysis {
    pub always_exits: bool,
    pub messages: Vec<Message>,
}

/// Analyze `body` for path completeness helpers and soft warnings.
///
/// `lookup` folds `const` bindings for infinite-loop proofs.
pub fn analyze_fn_body(body: &Output<'_>, lookup: &dyn Fn(&str) -> Option<ConstVal>) -> CfAnalysis {
    let mut out = CfAnalysis::default();
    let mut pending_defers: Vec<Range<usize>> = Vec::new();
    out.always_exits = walk(body, lookup, &mut pending_defers, &mut out.messages, false);
    out
}

/// True when `expr` never transfers control to a following sibling.
#[allow(dead_code)] // unit tests; production uses analyze_fn_body
pub fn always_exits(expr: &Output<'_>, lookup: &dyn Fn(&str) -> Option<ConstVal>) -> bool {
    walk(expr, lookup, &mut Vec::new(), &mut Vec::new(), false)
}

/// Walk `expr`. Returns whether it always exits the enclosing function.
///
/// `inside_infinite` is true when nested under a proven non-terminating loop.
fn walk(
    expr: &Output<'_>,
    lookup: &dyn Fn(&str) -> Option<ConstVal>,
    pending_defers: &mut Vec<Range<usize>>,
    messages: &mut Vec<Message>,
    inside_infinite: bool,
) -> bool {
    match expr.1.as_ref() {
        Expression::Statement(inner)
        | Expression::Expr(inner)
        | Expression::ExprStatement(inner)
        | Expression::Group(inner) => {
            walk(inner, lookup, pending_defers, messages, inside_infinite)
        }

        Expression::Return(_)
        | Expression::ImplicitReturn(_)
        | Expression::Raise(_)
        | Expression::Panic(_) => true,

        Expression::Block(children) => {
            let mut exited = false;
            let mut i = 0;
            while i < children.len() {
                let child = &children[i];
                if exited {
                    warn_unreachable(
                        messages,
                        child.0.into_range(),
                        children[i - 1].0.into_range(),
                    );
                    // Still walk for nested defer warnings / typecheck later.
                    let _ = walk(child, lookup, pending_defers, messages, inside_infinite);
                    i += 1;
                    continue;
                }
                // Function-body stmts are `Statement`-wrapped; nested block
                // items from the recursive statement parser are bare. Peel so
                // both shapes register defers / infinite loops the same way.
                let peeled = peel_stmt(child);
                match peeled.1.as_ref() {
                    Expression::Defer { .. } => {
                        let span = peeled.0.into_range();
                        if inside_infinite {
                            warn_defer_never(messages, span.clone(), None);
                        } else {
                            pending_defers.push(span);
                        }
                        let _ = walk(child, lookup, pending_defers, messages, inside_infinite);
                    }
                    _ => {
                        let child_exits =
                            walk(child, lookup, pending_defers, messages, inside_infinite);
                        if is_infinite_loop(child, lookup) {
                            for d in pending_defers.drain(..) {
                                warn_defer_never(messages, d, Some(child.0.into_range()));
                            }
                            exited = true;
                        } else if child_exits {
                            exited = true;
                        }
                    }
                }
                i += 1;
            }
            exited
        }

        Expression::Branch(cond, body) => {
            // Single branch node: used inside If; completeness checked at If level.
            if let Some(c) = cond {
                let _ = c;
            }
            walk(body, lookup, pending_defers, messages, inside_infinite)
        }

        Expression::If(branches) => {
            let mut all_exit = true;
            let mut has_else = false;
            for branch in branches {
                if let Expression::Branch(cond, body) = branch.1.as_ref() {
                    if cond.is_none() {
                        has_else = true;
                    }
                    if !walk(body, lookup, pending_defers, messages, inside_infinite) {
                        all_exit = false;
                    }
                } else {
                    all_exit = false;
                }
            }
            all_exit && has_else && !branches.is_empty()
        }

        Expression::Match { arms, .. } => {
            if arms.is_empty() {
                return false;
            }
            arms.iter()
                .all(|arm| walk(&arm.body, lookup, pending_defers, messages, inside_infinite))
        }

        Expression::Loop {
            identifier,
            iterable,
            body,
        } => {
            // for-in never proven infinite.
            if identifier.is_some() {
                walk(body, lookup, pending_defers, messages, inside_infinite);
                return false;
            }
            // while cond { body }
            let infinite = eval_bool_const(iterable, lookup) == Some(true) && !may_break(body, 0);
            walk(
                body,
                lookup,
                pending_defers,
                messages,
                inside_infinite || infinite,
            );
            infinite
        }

        Expression::Defer { body, .. } => {
            // Body of defer is a separate thunk; not function-exit for the outer fn.
            let _ = walk(body, lookup, &mut Vec::new(), messages, false);
            false
        }

        _ => false,
    }
}

/// True when `expr` is a proven non-terminating `while`/`for` (const-true cond, no break).
pub fn is_infinite_loop(expr: &Output<'_>, lookup: &dyn Fn(&str) -> Option<ConstVal>) -> bool {
    match expr.1.as_ref() {
        Expression::Statement(inner)
        | Expression::Expr(inner)
        | Expression::ExprStatement(inner)
        | Expression::Group(inner) => is_infinite_loop(inner, lookup),
        Expression::Loop {
            identifier: None,
            iterable,
            body,
        } => eval_bool_const(iterable, lookup) == Some(true) && !may_break(body, 0),
        _ => false,
    }
}

/// Peel `Statement` / expr wrappers so block item classification matches nested bodies.
fn peel_stmt<'a>(expr: &'a Output<'a>) -> &'a Output<'a> {
    match expr.1.as_ref() {
        Expression::Statement(inner)
        | Expression::Expr(inner)
        | Expression::ExprStatement(inner)
        | Expression::Group(inner) => peel_stmt(inner),
        _ => expr,
    }
}

/// Unlabeled `break` exits the innermost loop only (`depth` 0 = current).
fn may_break(expr: &Output<'_>, depth: usize) -> bool {
    match expr.1.as_ref() {
        Expression::Statement(inner)
        | Expression::Expr(inner)
        | Expression::ExprStatement(inner)
        | Expression::Group(inner) => may_break(inner, depth),
        Expression::Break => depth == 0,
        Expression::Block(children) => children.iter().any(|c| may_break(c, depth)),
        Expression::If(branches) => branches.iter().any(|b| may_break(b, depth)),
        Expression::Branch(_, body) => may_break(body, depth),
        Expression::Match { arms, .. } => arms.iter().any(|a| may_break(&a.body, depth)),
        Expression::Loop { body, .. } => may_break(body, depth + 1),
        Expression::Defer { body, .. } => may_break(body, depth),
        _ => false,
    }
}

fn warn_unreachable(messages: &mut Vec<Message>, dead: Range<usize>, cause: Range<usize>) {
    let mut msg = Message::warn(
        ErrorCode::UnreachableCode,
        "unreachable code".to_string(),
        dead,
    );
    msg.push(Label::new(
        "any code after this is unreachable".to_string(),
        cause,
    ));
    messages.push(msg);
}

fn warn_defer_never(
    messages: &mut Vec<Message>,
    defer_span: Range<usize>,
    loop_span: Option<Range<usize>>,
) {
    let mut msg = Message::warn(
        ErrorCode::DeferNeverRuns,
        "defer will never run on function exit".to_string(),
        defer_span,
    );
    if let Some(loop_span) = loop_span {
        msg.push(Label::new(
            "dominated by this non-terminating loop".to_string(),
            loop_span,
        ));
    } else {
        msg.with_help(
            "this defer is inside a non-terminating loop and never runs when the function exits"
                .to_string(),
        );
    }
    messages.push(msg);
}

#[cfg(test)]
mod tests {
    use super::*;
    use parser::SimpleSpan;
    use parser::ast::Expression;

    fn out<'a>(expr: Expression<'a>) -> Output<'a> {
        (SimpleSpan::from(0..0), Box::new(expr))
    }

    fn block<'a>(children: Vec<Output<'a>>) -> Output<'a> {
        out(Expression::Block(children))
    }

    fn while_loop<'a>(cond: Expression<'a>, body: Output<'a>) -> Output<'a> {
        out(Expression::Loop {
            identifier: None,
            iterable: out(cond),
            body,
        })
    }

    #[test]
    fn return_always_exits() {
        let body = block(vec![out(Expression::Return(out(Expression::Integer(1))))]);
        assert!(always_exits(&body, &|_| None));
    }

    #[test]
    fn while_true_without_break_always_exits() {
        let body = block(vec![while_loop(Expression::Bool(true), block(vec![]))]);
        assert!(always_exits(&body, &|_| None));
    }

    #[test]
    fn while_true_with_break_does_not_always_exit() {
        let body = block(vec![while_loop(
            Expression::Bool(true),
            block(vec![out(Expression::Break)]),
        )]);
        assert!(!always_exits(&body, &|_| None));
    }

    #[test]
    fn nested_break_does_not_defeat_outer_infinite_while() {
        // break in an inner loop only exits depth 0 of that loop; outer while
        // true {} with no outer break remains infinite.
        let inner = while_loop(Expression::Bool(false), block(vec![out(Expression::Break)]));
        let body = block(vec![while_loop(Expression::Bool(true), block(vec![inner]))]);
        assert!(always_exits(&body, &|_| None));
    }

    #[test]
    fn if_without_else_does_not_always_exit() {
        let then = out(Expression::Branch(
            Some(out(Expression::Bool(true))),
            block(vec![out(Expression::Return(out(Expression::Integer(1))))]),
        ));
        let body = block(vec![out(Expression::If(vec![then]))]);
        assert!(!always_exits(&body, &|_| None));
    }

    #[test]
    fn analyze_fn_body_warns_unreachable_after_return() {
        let body = block(vec![
            out(Expression::Return(out(Expression::Integer(1)))),
            out(Expression::Integer(2)),
        ]);
        let cf = analyze_fn_body(&body, &|_| None);
        assert!(cf.always_exits);
        assert!(
            cf.messages
                .iter()
                .any(|m| m.code() == Some(ErrorCode::UnreachableCode))
        );
    }

    #[test]
    fn analyze_fn_body_warns_defer_before_infinite_loop() {
        let defer = out(Expression::Defer {
            captures: vec![],
            body: block(vec![]),
        });
        let body = block(vec![
            defer,
            while_loop(Expression::Bool(true), block(vec![])),
        ]);
        let cf = analyze_fn_body(&body, &|_| None);
        assert!(cf.always_exits);
        assert!(
            cf.messages
                .iter()
                .any(|m| m.code() == Some(ErrorCode::DeferNeverRuns))
        );
    }

    fn stmt(inner: Output<'_>) -> Output<'_> {
        out(Expression::Statement(inner))
    }

    #[test]
    fn statement_wrapped_defer_before_while_true_warns() {
        let defer = stmt(out(Expression::Defer {
            captures: vec![],
            body: block(vec![]),
        }));
        let loop_ = stmt(while_loop(Expression::Bool(true), block(vec![])));
        let body = block(vec![defer, loop_]);
        let cf = analyze_fn_body(&body, &|_| None);
        assert!(cf.always_exits, "while true should still exit");
        assert!(
            cf.messages
                .iter()
                .any(|m| m.code() == Some(ErrorCode::DeferNeverRuns)),
            "Statement-wrapped defer before infinite while must warn; msgs={:?}",
            cf.messages.iter().map(|m| m.message()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn statement_wrapped_defer_inside_while_true_warns() {
        let defer = stmt(out(Expression::Defer {
            captures: vec![],
            body: block(vec![]),
        }));
        let loop_ = stmt(while_loop(Expression::Bool(true), block(vec![defer])));
        let body = block(vec![loop_]);
        let cf = analyze_fn_body(&body, &|_| None);
        assert!(cf.always_exits);
        assert!(
            cf.messages
                .iter()
                .any(|m| m.code() == Some(ErrorCode::DeferNeverRuns)),
            "Statement-wrapped defer inside infinite while must warn; msgs={:?}",
            cf.messages.iter().map(|m| m.message()).collect::<Vec<_>>()
        );
    }
}
