//! Cold code the program makes clear (`docs/internals/opt-hints.md`): a
//! function every path of which fails, and the arms that end in failure.
//! Nothing here guesses at a workload: code is cold only where it is rare
//! in every program.

use std::collections::{HashMap, HashSet};

use super::{Builtin, Callee, HirBody, HirId, HirKind, HirModule};

/// Whether evaluating `id` always fails: it panics, or calls a function
/// `diverging` names, on every path. An expression that may `return`
/// first is not known to fail here; [`diverges`] rules those out.
pub fn fails(body: &HirBody, id: HirId, diverging: &dyn Fn(&str) -> bool) -> bool {
    let f = |k: HirId| fails(body, k, diverging);
    match &body.expr(id).kind {
        HirKind::Builtin { op: Builtin::Panic, .. } => true,
        HirKind::Call { callee, args } => {
            args.iter().any(|&a| f(a)) || matches!(callee, Callee::Named { name, .. } if diverging(name))
        }
        HirKind::Block { stmts, tail } => stmts.iter().chain(tail).any(|&s| f(s)),
        HirKind::Let { init: Some(init), .. } => f(*init),
        HirKind::LetPat { init, .. } => f(*init),
        HirKind::If { cond, then, els } => f(*cond) || els.is_some_and(|e| f(*then) && f(e)),
        HirKind::Match { scrutinee, arms } => f(*scrutinee) || (!arms.is_empty() && arms.iter().all(|a| f(a.body))),
        _ => false,
    }
}

/// Whether `body` never returns: it has no `return` (or `yield`) and its
/// root always fails.
pub fn diverges(body: &HirBody, diverging: &dyn Fn(&str) -> bool) -> bool {
    let Some(root) = body.root else { return false };
    !body.is_coro
        && !body.exprs.iter().any(|e| matches!(e.kind, HirKind::Return(_) | HirKind::Yield { .. }))
        && fails(body, root, diverging)
}

/// The function and method bodies of `module` that never return, by
/// index. `known` names a callee outside the module that never returns
/// (one an earlier module found, or `exit`); calls between the module's
/// own bodies are followed to a fixpoint.
pub fn diverging_bodies(module: &HirModule, known: &dyn Fn(&str) -> bool) -> HashSet<usize> {
    use super::BodyKind;
    let by_name: HashMap<&str, usize> = module
        .bodies
        .iter()
        .enumerate()
        .filter(|(_, b)| matches!(b.kind, BodyKind::Function | BodyKind::Method))
        .map(|(i, b)| (b.name.as_str(), i))
        .collect();
    let mut out = HashSet::new();
    loop {
        let before = out.len();
        for (&_, &i) in &by_name {
            if out.contains(&i) {
                continue;
            }
            let is = |name: &str| by_name.get(name).is_some_and(|j| out.contains(j)) || known(name);
            if diverges(&module.bodies[i], &is) {
                out.insert(i);
            }
        }
        if out.len() == before {
            return out;
        }
    }
}

#[cfg(test)]
#[path = "cold.tests.rs"]
mod tests;
