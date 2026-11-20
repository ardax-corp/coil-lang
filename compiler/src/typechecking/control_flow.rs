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
    out.always_exits = walk(body, lookup, &mut pending_defers, &mut out.messages);
    out
}

/// True when `expr` never transfers control to a following sibling.
#[allow(dead_code)] // unit tests; production uses analyze_fn_body
pub fn always_exits(expr: &Output<'_>, lookup: &dyn Fn(&str) -> Option<ConstVal>) -> bool {
    walk(expr, lookup, &mut Vec::new(), &mut Vec::new())
}

/// Walk `expr`. Returns whether it always exits the enclosing function.
///
fn walk(
    expr: &Output<'_>,
    lookup: &dyn Fn(&str) -> Option<ConstVal>,
    pending_defers: &mut Vec<Range<usize>>,
    messages: &mut Vec<Message>,
) -> bool {
    match expr.1.as_ref() {
        Expression::Statement(inner)
        | Expression::Expr(inner)
        | Expression::ExprStatement(inner)
        | Expression::Group(inner) => {
            walk(inner, lookup, pending_defers, messages)
        }

        Expression::Return(_)
        | Expression::ImplicitReturn(_)
        | Expression::Raise(_)
        | Expression::Panic(_) => true,

        Expression::Block(children) => {
            // A `defer` runs when its block ends: past the block, an
            // infinite loop does not keep it from running.
            let open = pending_defers.len();
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
                    let _ = walk(child, lookup, pending_defers, messages);
                    i += 1;
                    continue;
                }
                // Function-body stmts are `Statement`-wrapped; nested block
                // items from the recursive statement parser are bare. Peel so
                // both shapes register defers / infinite loops the same way.
                let peeled = peel_stmt(child);
                match peeled.1.as_ref() {
                    Expression::Defer { .. } => {
                        pending_defers.push(peeled.0.into_range());
                        let _ = walk(child, lookup, pending_defers, messages);
                    }
                    _ => {
                        let child_exits =
                            walk(child, lookup, pending_defers, messages);
                        if is_infinite_loop(child, lookup) {
                            for d in pending_defers.drain(..) {
                                warn_defer_never(messages, d, child.0.into_range());
                            }
                            exited = true;
                        } else if child_exits {
                            exited = true;
                        }
                    }
                }
                i += 1;
            }
            pending_defers.truncate(open.min(pending_defers.len()));
            exited
        }

        Expression::Branch(cond, body) => {
            // Single branch node: used inside If; completeness checked at If level.
            if let Some(c) = cond {
                let _ = c;
            }
            walk(body, lookup, pending_defers, messages)
        }

        Expression::If(branches) => {
            let mut all_exit = true;
            let mut has_else = false;
            for branch in branches {
                if let Expression::Branch(cond, body) = branch.1.as_ref() {
                    if cond.is_none() {
                        has_else = true;
                    }
                    if !walk(body, lookup, pending_defers, messages) {
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
                .all(|arm| walk(&arm.body, lookup, pending_defers, messages))
        }

        Expression::IfLet {
            then_arm, else_arm, ..
        } => {
            walk(
                &then_arm.body,
                lookup,
                pending_defers,
                messages,
            ) && walk(
                &else_arm.body,
                lookup,
                pending_defers,
                messages,
            )
        }

        Expression::WhileLet {
            then_arm, on_miss, ..
        } => {
            walk(
                &then_arm.body,
                lookup,
                pending_defers,
                messages,
            );
            walk(
                &on_miss.body,
                lookup,
                pending_defers,
                messages,
            );
            false
        }

        Expression::Loop {
            contracts: _,
            identifier,
            pattern,
            iterable,
            body,
        } => {
            // for-in never proven infinite.
            if identifier.is_some() || pattern.is_some() {
                walk(body, lookup, pending_defers, messages);
                return false;
            }
            // while cond { body }
            let infinite = eval_bool_const(iterable, lookup) == Some(true) && !may_break(body, 0);
            walk(
                body,
                lookup,
                pending_defers,
                messages,
            );
            infinite
        }

        Expression::Defer { body, .. } => {
            // Body of defer is a separate thunk; not function-exit for the outer fn.
            let _ = walk(body, lookup, &mut Vec::new(), messages);
            false
        }

        _ => false,
    }
}

/// `return`, `raise`, `?`, and `break` / `continue` outside a loop of the
/// body itself, inside a `defer` body: each would leave a function that is
/// already leaving. Lambdas and nested `fn`s are their own functions.
pub fn defer_body_exits(body: &Output<'_>) -> Vec<(Range<usize>, &'static str)> {
    fn visit(expr: &Output<'_>, loops: usize, out: &mut Vec<(Range<usize>, &'static str)>) {
        let what = match expr.1.as_ref() {
            Expression::Lambda { .. } | Expression::Function { .. } => return,
            Expression::Loop { .. } | Expression::WhileLet { .. } => {
                expr.1.for_each_child(&mut |c| visit(c, loops + 1, out));
                return;
            }
            Expression::Return(_) => Some("`return`"),
            Expression::Raise(_) => Some("`raise`"),
            Expression::Try(_) => Some("`?`"),
            Expression::Break if loops == 0 => Some("`break`"),
            Expression::Continue if loops == 0 => Some("`continue`"),
            _ => None,
        };
        if let Some(what) = what {
            out.push((expr.0.into_range(), what));
        }
        expr.1.for_each_child(&mut |c| visit(c, loops, out));
    }
    let mut out = Vec::new();
    visit(body, 0, &mut out);
    out
}

/// The names a `defer` body assigns (`x = ..`, `x += ..`, `x++`), with
/// the assignment's span, among `names`. A lambda's body and a name the
/// body binds again (`let x`) are its own.
pub fn defer_body_assigns<'a>(body: &Output<'_>, names: &[&'a str]) -> Vec<(Range<usize>, &'a str)> {
    fn bound(expr: &Output<'_>, out: &mut Vec<String>) {
        match expr.1.as_ref() {
            Expression::Lambda { .. } | Expression::Function { .. } => return,
            Expression::Variable(name, _) => out.push((*name).to_string()),
            _ => {}
        }
        expr.1.for_each_child(&mut |c| bound(c, out));
    }
    fn visit<'a>(expr: &Output<'_>, names: &[&'a str], out: &mut Vec<(Range<usize>, &'a str)>) {
        let target = match expr.1.as_ref() {
            Expression::Lambda { .. } | Expression::Function { .. } => return,
            Expression::Assignment(t, _) | Expression::CompoundAssign(t, _, _) | Expression::Adjust { target: t, .. } => Some(t),
            _ => None,
        };
        if let Some(t) = target
            && let Expression::Identifier(id) = t.1.as_ref()
            && let Some(name) = names.iter().find(|n| **n == *id)
        {
            out.push((expr.0.into_range(), name));
        }
        expr.1.for_each_child(&mut |c| visit(c, names, out));
    }
    let mut rebound = Vec::new();
    bound(body, &mut rebound);
    let names: Vec<&'a str> = names.iter().copied().filter(|n| !rebound.iter().any(|r| r == n)).collect();
    let mut out = Vec::new();
    visit(body, &names, &mut out);
    out
}

/// True when `expr` is a proven non-terminating `while`/`for` (const-true cond, no break).
pub fn is_infinite_loop(expr: &Output<'_>, lookup: &dyn Fn(&str) -> Option<ConstVal>) -> bool {
    match expr.1.as_ref() {
        Expression::Statement(inner)
        | Expression::Expr(inner)
        | Expression::ExprStatement(inner)
        | Expression::Group(inner) => is_infinite_loop(inner, lookup),
        Expression::Loop {
            contracts: _,
            identifier: None,
            pattern: None,
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
        Expression::IfLet {
            then_arm, else_arm, ..
        } => may_break(&then_arm.body, depth) || may_break(&else_arm.body, depth),
        Expression::WhileLet {
            then_arm, on_miss, ..
        } => may_break(&then_arm.body, depth + 1) || may_break(&on_miss.body, depth + 1),
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

fn warn_defer_never(messages: &mut Vec<Message>, defer_span: Range<usize>, loop_span: Range<usize>) {
    let mut msg = Message::warn(
        ErrorCode::DeferNeverRuns,
        "defer will never run".to_string(),
        defer_span,
    );
    msg.push(Label::new(
        "its block never ends: this loop does not terminate".to_string(),
        loop_span,
    ));
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
            pattern: None,
            iterable: out(cond),
            body,
            contracts: Vec::new(),
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
            borrows: vec![],
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
            borrows: vec![],
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

    fn defer_of(body: Output<'_>) -> Output<'_> {
        stmt(out(Expression::Defer {
            captures: vec![],
            borrows: vec![],
            body,
        }))
    }

    fn never_runs(body: &Output<'_>) -> bool {
        analyze_fn_body(body, &|_| None)
            .messages
            .iter()
            .any(|m| m.code() == Some(ErrorCode::DeferNeverRuns))
    }

    #[test]
    fn defer_inside_a_while_true_body_runs_each_iteration() {
        let body = block(vec![stmt(while_loop(
            Expression::Bool(true),
            block(vec![defer_of(block(vec![]))]),
        ))]);
        assert!(!never_runs(&body));
    }

    #[test]
    fn defer_of_an_ended_block_does_not_warn() {
        let inner = out(Expression::Block(vec![defer_of(block(vec![]))]));
        let body = block(vec![stmt(inner), stmt(while_loop(Expression::Bool(true), block(vec![])))]);
        assert!(!never_runs(&body));
    }

    #[test]
    fn defer_before_an_infinite_loop_in_an_inner_block_warns() {
        let inner = out(Expression::Block(vec![stmt(while_loop(Expression::Bool(true), block(vec![])))]));
        let body = block(vec![defer_of(block(vec![])), stmt(inner)]);
        assert!(never_runs(&body));
    }

    #[test]
    fn defer_body_exits_skips_loops_of_the_body() {
        let ret = || stmt(out(Expression::Return(out(Expression::Integer(1)))));
        assert_eq!(defer_body_exits(&block(vec![ret()])).len(), 1);
        assert_eq!(defer_body_exits(&block(vec![stmt(out(Expression::Break))])).len(), 1);
        let inner = while_loop(Expression::Bool(false), block(vec![stmt(out(Expression::Break))]));
        assert!(defer_body_exits(&block(vec![stmt(inner)])).is_empty());
    }
}
