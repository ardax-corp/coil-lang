//! Structural checks on built HIR (the Phase 1 gate).
//!
//! A problem is a node the builder could not express
//! ([`HirKind::Unsupported`]) or a node whose span differs from the span the
//! checker recorded for its AST node, which would make diagnostics and
//! debug locations point at the wrong source.

use super::{HirKind, HirModule};
use crate::typechecking::infer::Checker;

/// One line per problem, `body: what at start..end`.
pub fn problems(module: &HirModule, checker: &Checker) -> Vec<String> {
    let ids = checker.id_table();
    let mut out = Vec::new();
    for body in &module.bodies {
        for expr in &body.exprs {
            let (start, end) = expr.span;
            if let HirKind::Unsupported(what) = expr.kind {
                out.push(format!("{}: unsupported {what} at {start}..{end}", body.name));
            }
            if let Some(id) = expr.node
                && let Some(recorded) = ids.span_of(id)
                && recorded != expr.span
            {
                out.push(format!(
                    "{}: span {start}..{end} differs from its AST node's {}..{}",
                    body.name, recorded.0, recorded.1
                ));
            }
        }
    }
    out
}
