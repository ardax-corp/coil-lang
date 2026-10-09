//! Scalar replacement of local tuples.
//!
//! A local tuple whose every value is a tuple built in place (`let t = (a,
//! b)`, `t = (c, d)`) and whose only reads are constant indices (`t[0]`) or
//! a destructuring `let (x, y) = t` becomes one local per element. Nothing
//! is allocated.

use super::{HirBody, HirExpr, HirFlags, HirId, HirKind, HirLocal, HirPat, IndexKind, Lit, LocalId, LocalKind, MakeKind};
use crate::typechecking::ty::{self, Ty};

/// `body` with every scalarizable local tuple split, or `None` when there
/// is none. `ok(ty)` admits an element type.
pub fn scalarize(body: &HirBody, ok: impl Fn(&Ty) -> bool) -> Option<HirBody> {
    let mut out = destructure(body);
    let body = &out.clone().unwrap_or_else(|| body.clone());
    let parent = parents(body);
    for (k, l) in body.locals.iter().enumerate() {
        let local = LocalId(k as u32);
        if l.captured || !matches!(l.kind, LocalKind::Let | LocalKind::Temp) {
            continue;
        }
        let Some(Ty::Tuple(items)) = l.ty.as_ref().map(ty::strip_readonly) else { continue };
        if items.is_empty() || !items.iter().all(&ok) {
            continue;
        }
        let n = items.len();
        let make = |v: HirId| matches!(&body.expr(v).kind, HirKind::Make { kind: MakeKind::Tuple, args } if args.len() == n);
        let mut built = false;
        let mut read = false;
        let fits = body.exprs.iter().enumerate().all(|(i, e)| match &e.kind {
            HirKind::Let { local: l, init } if *l == local => {
                built = true;
                init.is_some_and(make)
            }
            HirKind::Local(l) if *l == local => match parent[i].map(|p| &body.expr(p).kind) {
                Some(HirKind::Assign { place, value }) if place.0 as usize == i => {
                    !body.expr(parent[i].unwrap()).flags.contains(HirFlags::COMPOUND) && make(*value)
                }
                Some(HirKind::Index { base, index, kind: IndexKind::Tuple }) if base.0 as usize == i => {
                    read = true;
                    let at = parent[i].unwrap();
                    constant(body, *index).is_some_and(|k| k < n)
                        && !matches!(parent[at.0 as usize].map(|p| &body.expr(p).kind), Some(HirKind::Assign { place, .. }) if *place == at)
                }
                Some(HirKind::LetPat { pat: HirPat::Tuple(ps), init }) if init.0 as usize == i => {
                    read = true;
                    ps.len() == n && ps.iter().all(|p| matches!(p, HirPat::Wild | HirPat::Bind(_)))
                }
                _ => false,
            },
            HirKind::Defer { captures, .. } => !captures.contains(&Some(local)),
            HirKind::LetPat { pat, .. } => !binds(pat, local),
            HirKind::Match { arms, .. } => !arms.iter().any(|a| binds(&a.pat, local)),
            HirKind::ForIn { pat, .. } => !binds(pat, local),
            _ => true,
        });
        if !(fits && built && read) {
            continue;
        }
        let target = out.get_or_insert_with(|| body.clone());
        let fields: Vec<LocalId> = items
            .iter()
            .enumerate()
            .map(|(i, t)| fresh(target, &format!("__{}_{i}", l.name), t.clone()))
            .collect();
        rewrite(target, local, &fields);
    }
    out
}

/// `let (a, (b, c)) = (x, (y, z))` (or a record pattern on a record
/// built in place) as one `let` per element, in argument order.
fn destructure(body: &HirBody) -> Option<HirBody> {
    let fits = |pat: &HirPat, init: HirId| match (pat, &body.expr(init).kind) {
        (HirPat::Tuple(ps), HirKind::Make { kind: MakeKind::Tuple, args }) => ps.len() == args.len(),
        (HirPat::Record(_), HirKind::Make { kind: MakeKind::Record(_), .. }) => true,
        _ => false,
    };
    let sites: Vec<HirId> = (0..body.exprs.len())
        .map(|i| HirId(i as u32))
        .filter(|&id| matches!(&body.expr(id).kind, HirKind::LetPat { pat, init } if fits(pat, *init)))
        .collect();
    if sites.is_empty() {
        return None;
    }
    let mut out = body.clone();
    for at in sites {
        let HirKind::LetPat { pat, init } = out.expr(at).kind.clone() else { unreachable!() };
        let span = out.expr(at).span;
        let mut stmts = Vec::new();
        split_pat(&mut out, &pat, init, &mut stmts, span);
        set_block(&mut out, at, stmts);
    }
    Some(out)
}

fn split_pat(body: &mut HirBody, pat: &HirPat, value: HirId, stmts: &mut Vec<HirId>, span: super::Span) {
    match (pat, body.expr(value).kind.clone()) {
        (HirPat::Bind(b), _) => stmts.push(push(body, HirKind::Let { local: *b, init: Some(value) }, Some(ty::unit()), span)),
        (HirPat::Wild, HirKind::Lit(_) | HirKind::Local(_)) => {}
        (HirPat::Wild, _) => {
            let ty = body.expr(value).ty.clone().unwrap_or_else(ty::unit);
            let tmp = fresh(body, "__discard", ty);
            stmts.push(push(body, HirKind::Let { local: tmp, init: Some(value) }, Some(ty::unit()), span));
        }
        (HirPat::Tuple(ps), HirKind::Make { kind: MakeKind::Tuple, args }) if ps.len() == args.len() => {
            for (p, a) in ps.iter().zip(args) {
                split_pat(body, p, a, stmts, span);
            }
        }
        (HirPat::Record(fs), HirKind::Make { kind: MakeKind::Record(names), args }) => {
            for (n, a) in names.iter().zip(args) {
                let p = fs.iter().find(|(f, _)| f == n).map_or(HirPat::Wild, |(_, p)| p.clone());
                split_pat(body, &p, a, stmts, span);
            }
        }
        _ => stmts.push(push(body, HirKind::LetPat { pat: pat.clone(), init: value }, Some(ty::unit()), span)),
    }
}

fn constant(body: &HirBody, index: HirId) -> Option<usize> {
    match body.expr(index).kind {
        HirKind::Lit(Lit::Int(k)) => usize::try_from(k).ok(),
        _ => None,
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

pub(super) fn parents(body: &HirBody) -> Vec<Option<HirId>> {
    let mut parent = vec![None; body.exprs.len()];
    for i in 0..body.exprs.len() {
        for c in super::lower::children(body, HirId(i as u32)) {
            parent[c.0 as usize] = Some(HirId(i as u32));
        }
    }
    parent
}

/// Whether the tree at `root` reads `local`.
pub(super) fn mentions(body: &HirBody, root: HirId, local: LocalId) -> bool {
    let mut stack = vec![root];
    while let Some(id) = stack.pop() {
        if body.expr(id).kind == HirKind::Local(local) {
            return true;
        }
        stack.extend(super::lower::children(body, id));
    }
    false
}

pub(super) fn fresh(body: &mut HirBody, name: &str, ty: Ty) -> LocalId {
    let id = LocalId(body.locals.len() as u32);
    body.locals.push(HirLocal {
        name: name.to_string(),
        ty: Some(ty),
        kind: LocalKind::Temp,
        captured: false,
    });
    id
}

/// Replace every build and read of `local` with its element locals.
fn rewrite(body: &mut HirBody, local: LocalId, fields: &[LocalId]) {
    let parent = parents(body);
    let stage = self_reads(body, local);
    for (i, &up) in parent.iter().enumerate() {
        let id = HirId(i as u32);
        match body.exprs[i].kind.clone() {
            HirKind::Let { local: l, init: Some(v) } if l == local => {
                let stmts = build(body, v, false, fields, true);
                set_block(body, id, stmts);
            }
            HirKind::Local(l) if l == local => {
                let Some(p) = up else { continue };
                match body.expr(p).kind.clone() {
                    HirKind::Assign { place, value } if place == id => {
                        let stmts = build(body, value, stage.contains(&value), fields, false);
                        set_block(body, p, stmts);
                    }
                    HirKind::Index { index, .. } => {
                        let k = constant(body, index).expect("checked constant index");
                        body.exprs[p.0 as usize].kind = HirKind::Local(fields[k]);
                        body.exprs[p.0 as usize].node = None;
                    }
                    HirKind::LetPat { pat: HirPat::Tuple(ps), .. } => {
                        let span = body.expr(p).span;
                        let mut stmts = Vec::new();
                        for (q, &f) in ps.iter().zip(fields) {
                            if let HirPat::Bind(b) = q {
                                let ty = body.local(f).ty.clone();
                                let read = push(body, HirKind::Local(f), ty, span);
                                stmts.push(push(body, HirKind::Let { local: *b, init: Some(read) }, Some(ty::unit()), span));
                            }
                        }
                        set_block(body, p, stmts);
                    }
                    _ => {}
                }
            }
            HirKind::Clear(xs) if xs.contains(&local) => {
                body.exprs[i].kind = HirKind::Clear(xs.into_iter().filter(|&x| x != local).collect());
            }
            _ => {}
        }
    }
}

/// The statements of a tuple build `v`: each element into its local. A
/// reassignment whose elements read the tuple (`stage`) computes them all
/// first.
fn build(body: &mut HirBody, v: HirId, stage: bool, fields: &[LocalId], define: bool) -> Vec<HirId> {
    let span = body.expr(v).span;
    let HirKind::Make { args, .. } = body.expr(v).kind.clone() else {
        unreachable!("checked tuple build")
    };
    let mut stmts = Vec::new();
    let args = if stage {
        staged(body, &args, &mut stmts, span)
    } else {
        args
    };
    for (&f, &a) in fields.iter().zip(&args) {
        stmts.push(set(body, f, a, define, span));
    }
    body.exprs[v.0 as usize].kind = HirKind::Lit(Lit::Unit);
    body.exprs[v.0 as usize].ty = Some(ty::unit());
    stmts
}

/// The values assigned to `local` that read it. Taken before any rewrite,
/// which replaces those reads with the split locals.
pub(super) fn self_reads(body: &HirBody, local: LocalId) -> Vec<HirId> {
    body.exprs
        .iter()
        .filter_map(|e| match &e.kind {
            HirKind::Assign { place, value } if body.expr(*place).kind == HirKind::Local(local) && mentions(body, *value, local) => {
                Some(*value)
            }
            _ => None,
        })
        .collect()
}

/// Each of `args` into a fresh temp (pushed to `stmts`), as reads of them.
pub(super) fn staged(body: &mut HirBody, args: &[HirId], stmts: &mut Vec<HirId>, span: super::Span) -> Vec<HirId> {
    args.iter()
        .map(|&a| {
            let ty = body.expr(a).ty.clone().unwrap_or_else(ty::unit);
            let tmp = fresh(body, "__staged", ty.clone());
            stmts.push(push(body, HirKind::Let { local: tmp, init: Some(a) }, Some(ty::unit()), span));
            push(body, HirKind::Local(tmp), Some(ty), span)
        })
        .collect()
}

/// `local = value` as a statement: a `let` on its first definition.
pub(super) fn set(body: &mut HirBody, local: LocalId, value: HirId, define: bool, span: super::Span) -> HirId {
    let kind = if define {
        HirKind::Let { local, init: Some(value) }
    } else {
        let ty = body.local(local).ty.clone();
        let place = push(body, HirKind::Local(local), ty, span);
        HirKind::Assign { place, value }
    };
    push(body, kind, Some(ty::unit()), span)
}

pub(super) fn set_block(body: &mut HirBody, at: HirId, stmts: Vec<HirId>) {
    let e = &mut body.exprs[at.0 as usize];
    e.kind = HirKind::Block { stmts, tail: None };
    e.ty = Some(ty::unit());
    e.layout = super::layout::Layout::Word;
    e.node = None;
}

pub(super) fn push(body: &mut HirBody, kind: HirKind, ty: Option<Ty>, span: super::Span) -> HirId {
    let id = HirId(body.exprs.len() as u32);
    body.exprs.push(HirExpr {
        kind,
        ty,
        layout: super::layout::Layout::Word,
        span,
        node: None,
        flags: HirFlags::default(),
    });
    id
}
