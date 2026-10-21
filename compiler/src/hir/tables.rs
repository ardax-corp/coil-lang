//! Constant lookup tables (#1561): reads of a `readonly` array no code can
//! change, at an index known at compile time, become the element.
//!
//! A table is a module `const T = readonly [e0, …]` (the caller resolves a
//! `Global` read to its elements) or a function-local
//! `const t = readonly [lit, …]` whose elements are all literals. `T[k]`
//! with a literal `k` in range reads as `ek` when that element folds, and
//! `T.len()` as the element count. A local table nothing reads any more
//! goes, with its array.

use std::collections::{HashMap, HashSet};

use super::lower::children;
use crate::typechecking::ty::{self, Ty};

use super::{Callee, HirBody, HirId, HirKind, Lit, LocalId, LocalKind, MakeKind};

/// What the caller knows of the `Global` read `id`: how many elements the
/// table it names has, and the literal element `k` folds to (already
/// checked against the read's type).
pub trait GlobalTables {
    fn len(&self, id: HirId) -> Option<usize>;
    fn elem(&self, id: HirId, k: usize, read: HirId) -> Option<Lit>;
}

/// A function-local `const` table: its `let`s (an unrolled loop repeats
/// one), and the first one's literal element nodes.
struct LocalTable {
    stmts: Vec<HirId>,
    elems: Vec<HirId>,
}

/// The `let`s of `body` inside a loop.
pub fn lets_in_loops(body: &HirBody) -> HashSet<HirId> {
    let mut out = HashSet::new();
    let Some(root) = body.root else {
        return out;
    };
    let mut stack = vec![(root, false)];
    while let Some((k, inside)) = stack.pop() {
        let e = body.expr(k);
        if inside && matches!(e.kind, HirKind::Let { .. }) {
            out.insert(k);
        }
        let deeper = inside || matches!(e.kind, HirKind::Loop { .. } | HirKind::ForIn { .. });
        stack.extend(children(body, k).into_iter().map(|c| (c, deeper)));
    }
    out
}

/// The `const` local `let` of an all-literal array: `let` node and elements.
pub fn local_table(body: &HirBody, id: HirId) -> Option<(LocalId, Vec<HirId>)> {
    let HirKind::Let { local, init: Some(init) } = body.expr(id).kind else {
        return None;
    };
    let l = body.local(local);
    if l.kind != LocalKind::Const || l.captured {
        return None;
    }
    let HirKind::Make { kind: MakeKind::Array, args } = &body.expr(init).kind else {
        return None;
    };
    let literal = |a: &HirId| matches!(body.expr(*a).kind, HirKind::Lit(Lit::Int(_) | Lit::Float(_) | Lit::Bool(_) | Lit::Str(_)));
    args.iter().all(literal).then(|| (local, args.clone()))
}

/// The locals no code can write through: declared `readonly`, or read
/// only as `readonly` (a `const t = readonly [..]` binds the array's own
/// type and reads as `readonly`). `is_readonly`: whether a type is
/// (after the checker's substitution).
pub fn sealed_locals(body: &HirBody, is_readonly: &dyn Fn(&Ty) -> bool) -> HashSet<LocalId> {
    let mut reads: HashMap<LocalId, bool> = HashMap::new();
    for e in &body.exprs {
        if let HirKind::Local(l) = e.kind {
            let sealed = e.ty.as_ref().is_some_and(is_readonly);
            *reads.entry(l).or_insert(true) &= sealed;
        }
    }
    (0..body.locals.len() as u32)
        .map(LocalId)
        .filter(|&l| body.local(l).ty.as_ref().is_some_and(is_readonly) || reads.get(&l) == Some(&true))
        .collect()
}

/// `body` with every table read that folds folded, or `None` when none
/// does. `frozen(local)`: the local's type is `readonly`, so the array is
/// never written.
pub fn fold_tables(body: &HirBody, globals: &dyn GlobalTables, frozen: &dyn Fn(LocalId) -> bool) -> Option<HirBody> {
    let root = body.root?;
    let mut locals: HashMap<LocalId, LocalTable> = HashMap::new();
    let mut places = HashSet::new();
    let mut nodes = Vec::new();
    let mut stack = vec![root];
    while let Some(k) = stack.pop() {
        nodes.push(k);
        match &body.expr(k).kind {
            HirKind::Let { .. } => {
                if let Some((local, elems)) = local_table(body, k).filter(|(l, _)| frozen(*l)) {
                    locals.entry(local).or_insert(LocalTable { stmts: Vec::new(), elems }).stmts.push(k);
                }
            }
            HirKind::Assign { place, .. } | HirKind::Append { base: place, .. } => {
                places.insert(*place);
            }
            _ => {}
        }
        stack.extend(children(body, k).into_iter().rev());
    }
    let local_of = |id: HirId| match body.expr(id).kind {
        HirKind::Local(l) => locals.get(&l).map(|t| (l, t)),
        _ => None,
    };
    let mut out = body.clone();
    let mut hits = 0;
    let mut folded_reads: HashMap<LocalId, usize> = HashMap::new();
    for &k in &nodes {
        let e = body.expr(k);
        let lit = match &e.kind {
            HirKind::Index { base, index, .. } if !places.contains(&k) => {
                let HirKind::Lit(Lit::Int(at)) = body.expr(*index).kind else {
                    continue;
                };
                let Ok(at) = usize::try_from(at) else {
                    continue;
                };
                if let Some((l, t)) = local_of(*base) {
                    let Some(&el) = t.elems.get(at) else { continue };
                    let elem = body.expr(el);
                    if elem.ty.is_none() || elem.ty != e.ty {
                        continue;
                    }
                    let HirKind::Lit(lit) = &elem.kind else { continue };
                    *folded_reads.entry(l).or_default() += 1;
                    lit.clone()
                } else if matches!(body.expr(*base).kind, HirKind::Global { .. }) {
                    match globals.elem(*base, at, k) {
                        Some(lit) => lit,
                        None => continue,
                    }
                } else {
                    continue;
                }
            }
            HirKind::Call { callee: Callee::Method { name }, args } if name == "len" && args.len() == 1 => {
                let n = if let Some((l, t)) = local_of(args[0]) {
                    *folded_reads.entry(l).or_default() += 1;
                    t.elems.len()
                } else if matches!(body.expr(args[0]).kind, HirKind::Global { .. }) {
                    match globals.len(args[0]) {
                        Some(n) => n,
                        None => continue,
                    }
                } else {
                    continue;
                };
                if !e.ty.as_ref().is_some_and(is_int) {
                    continue;
                }
                Lit::Int(n as i64)
            }
            _ => continue,
        };
        let slot = &mut out.exprs[k.0 as usize];
        slot.kind = HirKind::Lit(lit);
        slot.node = None;
        hits += 1;
    }
    // A local table whose every read folded is dead: its `let` goes (a
    // `defer` that captures it still reads it).
    let deferred: HashSet<LocalId> = nodes
        .iter()
        .filter_map(|&k| match &body.expr(k).kind {
            HirKind::Defer { captures, .. } => Some(captures.iter().flatten().copied()),
            _ => None,
        })
        .flatten()
        .collect();
    for (l, t) in &locals {
        let reads = nodes.iter().filter(|&&k| matches!(body.expr(k).kind, HirKind::Local(x) if x == *l)).count();
        if !deferred.contains(l) && folded_reads.get(l).copied().unwrap_or(0) == reads {
            for &stmt in &t.stmts {
                empty(&mut out, stmt);
            }
            hits += 1;
        }
    }
    (hits > 0).then_some(out)
}

/// `id` becomes an empty block.
fn empty(out: &mut HirBody, id: HirId) {
    let slot = &mut out.exprs[id.0 as usize];
    slot.kind = HirKind::Block { stmts: Vec::new(), tail: None };
    slot.node = None;
}

fn is_int(t: &Ty) -> bool {
    matches!(ty::strip_readonly(t), Ty::Con(n) if n == ty::INT)
}

#[cfg(test)]
#[path = "tables.tests.rs"]
mod tests;
