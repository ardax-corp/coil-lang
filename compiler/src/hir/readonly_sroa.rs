//! Scalar replacement of `readonly class` locals that escape (#1562).
//!
//! An instance of a readonly class never changes after `new`, compares
//! structurally and has no `drop` or weak handle, so two objects with the
//! same field values cannot be told apart. A local `let p = new P(a, b)`
//! of such a class whose whole value is used (passed, returned, stored)
//! therefore need not be one object from the start: its fields become
//! locals, each field read `p.x` reads its local, and each whole use is a
//! fresh `new P(x, y)` of those locals, built only where it runs. A hot
//! path that only reads fields allocates nothing; a cold call that takes
//! the whole value pays for its own object.
//!
//! A whole use inside a loop the `let` is outside of would build an object
//! per iteration where the local's one object was built once, and two
//! whole uses on one path would build two. So every whole use is outside
//! any loop that does not hold the `let`, and any two of them are in
//! different arms of an `if` or a `match`. A reassignment `p = new P(..)`
//! stores the fields; any other value stored to `p` keeps it an object.

use std::collections::HashSet;

use super::tuple_sroa::{fresh, mentions, parents, push, set, set_block, staged};
use super::{HirBody, HirFlags, HirId, HirKind, HirPat, LocalId, LocalKind, MakeKind};
use crate::typechecking::ty::Ty;

/// The most whole uses one local is rebuilt at.
const MAX_USES: usize = 8;

/// `body` with every readonly class local that fits split into field
/// locals, or `None` when there is none. `fields(class)` gives the fields
/// of a class `new` may make this way (a non-generic readonly class with
/// 1 to 32 admitted field types), in argument order.
pub fn scalarize(body: &HirBody, fields: impl Fn(&str) -> Option<Vec<(String, Ty)>>) -> Option<HirBody> {
    // Only a local some `let` makes a class into can split; checking just
    // those keeps a long body with many locals linear (#1857).
    let made: HashSet<LocalId> = body
        .exprs
        .iter()
        .filter_map(|e| match &e.kind {
            HirKind::Let { local, init: Some(v) }
                if matches!(body.expr(*v).kind, HirKind::Make { kind: MakeKind::Class(_), .. }) =>
            {
                Some(*local)
            }
            _ => None,
        })
        .collect();
    if made.is_empty() {
        return None;
    }
    let parent = parents(body);
    let mut out: Option<HirBody> = None;
    for (k, l) in body.locals.iter().enumerate() {
        let local = LocalId(k as u32);
        if l.captured || !matches!(l.kind, LocalKind::Let | LocalKind::Temp) || !made.contains(&local) {
            continue;
        }
        let Some((class, decl)) = candidate(body, &parent, local, &fields) else {
            continue;
        };
        let target = out.get_or_insert_with(|| body.clone());
        let locals: Vec<LocalId> = decl
            .iter()
            .map(|(f, t)| fresh(target, &format!("__{}_{f}", l.name), t.clone()))
            .collect();
        let names: Vec<String> = decl.into_iter().map(|(f, _)| f).collect();
        rewrite(target, local, &class, &names, &locals);
    }
    out
}

/// The class and fields of `local` when it splits: one `let` of a `new`
/// of a class `fields` admits, reassigned only to `new`s of it, read
/// whole at most [`MAX_USES`] times on exclusive paths outside loops it
/// is not bound in, and at least once (a local only read field by field
/// is already kept in frame slots).
fn candidate(
    body: &HirBody,
    parent: &[Option<HirId>],
    local: LocalId,
    fields: &impl Fn(&str) -> Option<Vec<(String, Ty)>>,
) -> Option<(String, Vec<(String, Ty)>)> {
    let mut class: Option<(String, Vec<(String, Ty)>)> = None;
    let mut lets = Vec::new();
    let mut uses = Vec::new();
    // Whether `v` is a `new` of the local's class, the first one naming it.
    let new_of = |v: HirId, class: &mut Option<(String, Vec<(String, Ty)>)>| {
        let HirKind::Make {
            kind: MakeKind::Class(name),
            args,
        } = &body.expr(v).kind
        else {
            return false;
        };
        if class.is_none() {
            match fields(name) {
                Some(decl) => *class = Some((name.clone(), decl)),
                None => return false,
            }
        }
        class.as_ref().is_some_and(|(seen, decl)| seen == name && args.len() == decl.len())
    };
    for (i, e) in body.exprs.iter().enumerate() {
        let id = HirId(i as u32);
        match &e.kind {
            HirKind::Let { local: l, init } if *l == local => {
                if !init.is_some_and(|v| new_of(v, &mut class)) {
                    return None;
                }
                lets.push(id);
            }
            HirKind::Local(l) if *l == local => match parent[i].map(|p| (p, &body.expr(p).kind)) {
                Some((p, HirKind::Assign { place, value })) if *place == id => {
                    if body.expr(p).flags.contains(HirFlags::COMPOUND) || !new_of(*value, &mut class) {
                        return None;
                    }
                }
                // Checked against the class below, once it is known.
                Some((_, HirKind::Field { base, .. })) if *base == id => {}
                _ => uses.push(id),
            },
            HirKind::Defer { captures, .. } if captures.contains(&Some(local)) => return None,
            HirKind::LetPat { pat, .. } | HirKind::ForIn { pat, .. } if binds(pat, local) => return None,
            HirKind::Match { arms, .. } if arms.iter().any(|a| binds(&a.pat, local)) => return None,
            _ => {}
        }
    }
    let (name, decl) = class?;
    // Every field read names a field of the class.
    let reads_fields = body.exprs.iter().all(|e| match &e.kind {
        HirKind::Field { base, name } if body.expr(*base).kind == HirKind::Local(local) => decl.iter().any(|(f, _)| f == name),
        _ => true,
    });
    let [at] = lets.as_slice() else { return None };
    if !reads_fields || uses.is_empty() || uses.len() > MAX_USES {
        return None;
    }
    // The block the `let` is a statement of.
    let scope = parent[at.0 as usize]?;
    // Each use's ancestors up to `scope`, nearest first, none of them a
    // loop body.
    let mut chains = Vec::with_capacity(uses.len());
    for &u in &uses {
        let mut chain = vec![u];
        let mut at = u;
        while at != scope {
            let up = parent[at.0 as usize]?;
            match &body.expr(up).kind {
                HirKind::Loop { .. } => return None,
                HirKind::ForIn { body: b, .. } if *b == at => return None,
                _ => {}
            }
            chain.push(up);
            at = up;
        }
        chains.push(chain);
    }
    for (i, a) in chains.iter().enumerate() {
        for b in &chains[i + 1..] {
            if !exclusive(body, a, b) {
                return None;
            }
        }
    }
    Some((name, decl))
}

/// Whether the two uses whose ancestor chains are `a` and `b` (each nearest
/// first, ending at the same node) are on different arms of the `if` or
/// `match` that is their nearest common ancestor.
fn exclusive(body: &HirBody, a: &[HirId], b: &[HirId]) -> bool {
    // Walk both chains from the shared top down to where they part.
    let (mut i, mut j) = (a.len(), b.len());
    while i > 0 && j > 0 && a[i - 1] == b[j - 1] {
        i -= 1;
        j -= 1;
    }
    // `a[i]` is the common ancestor; `a[i - 1]` / `b[j - 1]` its children
    // on each side (none when one use is the other's ancestor).
    if i == 0 || j == 0 || i == a.len() {
        return false;
    }
    let (top, x, y) = (a[i], a[i - 1], b[j - 1]);
    match &body.expr(top).kind {
        HirKind::If { then, els: Some(els), .. } => (x == *then && y == *els) || (x == *els && y == *then),
        HirKind::Match { arms, .. } => {
            let arm = |c: HirId| arms.iter().position(|a| a.body == c);
            matches!((arm(x), arm(y)), (Some(p), Some(q)) if p != q)
        }
        _ => false,
    }
}

fn binds(pat: &HirPat, local: LocalId) -> bool {
    match pat {
        HirPat::Bind(l) => *l == local,
        HirPat::Variant {
            fields: super::HirPatFields::Tuple(ps),
            ..
        }
        | HirPat::Tuple(ps) => ps.iter().any(|p| binds(p, local)),
        HirPat::Variant {
            fields: super::HirPatFields::Record(fs),
            ..
        }
        | HirPat::Record(fs) => fs.iter().any(|(_, p)| binds(p, local)),
        _ => false,
    }
}

/// Replace `local`'s `let`, reassignments, field reads and whole uses
/// with its field locals.
fn rewrite(body: &mut HirBody, local: LocalId, class: &str, names: &[String], fields: &[LocalId]) {
    let parent = parents(body);
    // Values stored to `local` that read it compute every field first.
    let stage: Vec<HirId> = body
        .exprs
        .iter()
        .filter_map(|e| match &e.kind {
            HirKind::Assign { place, value } if body.expr(*place).kind == HirKind::Local(local) && mentions(body, *value, local) => {
                Some(*value)
            }
            _ => None,
        })
        .collect();
    for (i, &up) in parent.iter().enumerate() {
        let id = HirId(i as u32);
        match body.exprs[i].kind.clone() {
            HirKind::Let { local: l, init: Some(v) } if l == local => {
                let stmts = build(body, v, false, fields, true);
                set_block(body, id, stmts);
            }
            HirKind::Local(l) if l == local => match up.map(|p| (p, body.expr(p).kind.clone())) {
                Some((p, HirKind::Assign { place, value })) if place == id => {
                    let stmts = build(body, value, stage.contains(&value), fields, false);
                    set_block(body, p, stmts);
                }
                Some((p, HirKind::Field { base, name })) if base == id => {
                    let k = names.iter().position(|n| *n == name).expect("checked field");
                    let e = &mut body.exprs[p.0 as usize];
                    e.kind = HirKind::Local(fields[k]);
                    e.node = None;
                }
                // A whole use: a fresh object of the fields' values.
                _ => {
                    let span = body.expr(id).span;
                    let args = fields
                        .iter()
                        .map(|&f| {
                            let ty = body.local(f).ty.clone();
                            push(body, HirKind::Local(f), ty, span)
                        })
                        .collect();
                    let e = &mut body.exprs[i];
                    e.kind = HirKind::Make {
                        kind: MakeKind::Class(class.to_string()),
                        args,
                    };
                    e.node = None;
                }
            },
            // An inlined call's clear of the split local clears its fields.
            HirKind::Clear(xs) if xs.contains(&local) => {
                let mut out: Vec<LocalId> = xs.into_iter().filter(|&x| x != local).collect();
                for &f in fields {
                    if !out.contains(&f) {
                        out.push(f);
                    }
                }
                body.exprs[i].kind = HirKind::Clear(out);
            }
            _ => {}
        }
    }
}

/// The statements of a `new` stored to the local: each argument into its
/// field local. A reassignment whose arguments read the local (`stage`)
/// computes them all first.
fn build(body: &mut HirBody, v: HirId, stage: bool, fields: &[LocalId], define: bool) -> Vec<HirId> {
    let span = body.expr(v).span;
    let HirKind::Make { args, .. } = body.expr(v).kind.clone() else {
        unreachable!("checked class build")
    };
    let mut stmts = Vec::new();
    let args = if stage { staged(body, &args, &mut stmts, span) } else { args };
    for (&f, &a) in fields.iter().zip(&args) {
        stmts.push(set(body, f, a, define, span));
    }
    body.exprs[v.0 as usize].kind = HirKind::Lit(super::Lit::Unit);
    body.exprs[v.0 as usize].ty = Some(crate::typechecking::ty::unit());
    stmts
}
