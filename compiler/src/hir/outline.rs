//! Partial inlining (`docs/internals/opt-hints.md`): cut a body's cold arms
//! out into helper functions, so the rest of it is small enough to inline.
//!
//! A cold arm is an arm of an `if` statement that always fails (a panic,
//! with any statements before it) or that calls a cold function. The arm
//! must be plain code ([`super::query::plain`]: no loop, closure, `defer`,
//! coroutine step or `break` / `continue`) with no `return`. Its free
//! locals become the helper's parameters. It may assign at most one of
//! them, by name only (`x = ..`): the helper takes that local's value and
//! returns its new one, and the arm becomes `x = helper(..)`. A local the
//! arm writes through (`v[i] = ..`, `o.f = ..`, or a copy-on-write
//! `unique` before a mutating call) or borrows keeps the arm in place,
//! since a parameter is only a share of it.

use std::collections::{HashMap, HashSet};

use super::inline::map_ids;
use super::query::plain;
use super::{BodyKind, Builtin, Callee, HirBody, HirExpr, HirFlags, HirId, HirKind, HirLocal, HirPat, LocalId, LocalKind};
use crate::typechecking::ty;

/// Arms smaller than this stay in place: the call costs as much as they do.
pub const MIN_NODES: usize = 4;

/// One helper cut out of a body: its body, then the parameter and result
/// types of its signature.
#[derive(Debug, Clone)]
pub struct Helper {
    pub body: HirBody,
    pub params: Vec<ty::Ty>,
    pub ret: ty::Ty,
}

/// `body` with its cold arms cut out into helpers named by `name` (given
/// the helper's index), when it has any. `cold_call` says whether a call
/// node goes to a cold function; `diverging` names a function that never
/// returns.
pub fn outline(
    body: &HirBody,
    cold_call: &dyn Fn(HirId) -> bool,
    diverging: &dyn Fn(&str) -> bool,
    name: &dyn Fn(usize) -> String,
) -> Option<(HirBody, Vec<Helper>)> {
    if body.is_coro || body.is_generic || !body.captures.is_empty() || !matches!(body.kind, BodyKind::Function | BodyKind::Method) {
        return None;
    }
    let root = body.root?;
    let mut arms = Vec::new();
    body.visit(root, &mut |e| {
        if let HirKind::Block { stmts, .. } = &e.kind {
            for &s in stmts {
                if let HirKind::If { then, els, .. } = &body.expr(s).kind {
                    arms.push(*then);
                    arms.extend(*els);
                }
            }
        }
    });
    let mut out = body.clone();
    let mut helpers = Vec::new();
    for arm in arms {
        if !cold_arm(body, arm, cold_call, diverging) {
            continue;
        }
        let Some((helper, call)) = cut(body, &mut out, arm, &name(helpers.len())) else {
            continue;
        };
        out.exprs[arm.0 as usize].kind = HirKind::Block { stmts: vec![call], tail: None };
        helpers.push(helper);
    }
    if helpers.is_empty() {
        return None;
    }
    compact(&mut out);
    Some((out, helpers))
}

/// An arm that always fails or calls a cold function, plain, with no
/// `return` and no failed `requires` (which names the frame's caller), and
/// big enough to be worth a call.
fn cold_arm(body: &HirBody, arm: HirId, cold_call: &dyn Fn(HirId) -> bool, diverging: &dyn Fn(&str) -> bool) -> bool {
    if !matches!(body.expr(arm).kind, HirKind::Block { .. }) || !plain(body, arm) {
        return false;
    }
    let mut nodes = 0;
    let mut returns = false;
    let mut clears = false;
    let mut blames = false;
    let mut calls_cold = false;
    body.visit(arm, &mut |e| {
        nodes += 1;
        returns |= matches!(e.kind, HirKind::Return(_));
        // A failed `requires` blames the caller of the frame it runs in.
        blames |= matches!(&e.kind, HirKind::Builtin { op: Builtin::Panic, args } if args.len() == 2);
        clears |= matches!(e.kind, HirKind::Clear(_));
    });
    let mut stack = vec![arm];
    while let Some(at) = stack.pop() {
        calls_cold |= matches!(body.expr(at).kind, HirKind::Call { .. }) && cold_call(at);
        stack.extend(body.children(at));
    }
    nodes >= MIN_NODES && !returns && !clears && !blames && (calls_cold || super::cold::fails(body, arm, diverging))
}

/// The helper running `arm` of `body`, and the statement in `out` that
/// calls it in its place.
fn cut(body: &HirBody, out: &mut HirBody, arm: HirId, name: &str) -> Option<(Helper, HirId)> {
    // The arm's nodes in order, and the locals it binds.
    let mut nodes = Vec::new();
    let mut bound = HashSet::new();
    body.visit(arm, &mut |e| match &e.kind {
        HirKind::Let { local, .. } => {
            bound.insert(*local);
        }
        HirKind::LetPat { pat, .. } => binders(pat, &mut bound),
        HirKind::Match { arms, .. } => arms.iter().for_each(|a| binders(&a.pat, &mut bound)),
        _ => {}
    });
    let mut stack = vec![arm];
    while let Some(at) = stack.pop() {
        nodes.push(at);
        stack.extend(body.children(at).rev());
    }
    // Free locals in order of first use, and the ones the arm writes.
    let mut free: Vec<LocalId> = Vec::new();
    let mut assigned: HashSet<LocalId> = HashSet::new();
    let mut through: HashSet<LocalId> = HashSet::new();
    for &id in &nodes {
        match &body.expr(id).kind {
            HirKind::Local(l) if !bound.contains(l) && !free.contains(l) => free.push(*l),
            HirKind::Assign { place, .. } | HirKind::Append { base: place, .. } => match body.expr(*place).kind {
                HirKind::Local(l) => {
                    assigned.insert(l);
                }
                _ => {
                    if let Some(l) = place_root(body, *place) {
                        through.insert(l);
                    }
                }
            },
            // Copy-on-write makes a place unique before a write through it
            // or a mutating call on it.
            HirKind::Builtin { op: Builtin::Cow { unique: true }, args } => {
                if let Some(l) = args.first().and_then(|&a| place_root(body, a)) {
                    through.insert(l);
                }
            }
            _ => {}
        }
    }
    if free.iter().any(|l| through.contains(l)) {
        return None;
    }
    if free.iter().any(|&l| {
        let local = body.local(l);
        local.captured || local.borrow || local.ty.as_ref().is_none_or(|t| matches!(t, ty::Ty::Array { .. } | ty::Ty::Tuple(_) | ty::Ty::Record { .. }) || !ty::ftv_ty(t).is_empty())
    }) {
        return None;
    }
    let written: Vec<LocalId> = free.iter().copied().filter(|l| assigned.contains(l)).collect();
    if written.len() > 1 {
        return None;
    }
    let written = written.first().copied();
    // The helper: parameters for the free locals, the arm's own locals,
    // then its nodes renumbered.
    let mut helper = body.clone();
    helper.name = name.to_string();
    helper.kind = BodyKind::Function;
    helper.params.clear();
    helper.locals.clear();
    helper.exprs.clear();
    helper.hints = super::OptHints { cold: true, ..Default::default() };
    helper.contract_spans.clear();
    helper.aliased_fn_values.clear();
    helper.fn_key_sites.clear();
    helper.stray_muts.clear();
    helper.result_mode = false;
    helper.declared = None;
    helper.stated_escape = None;
    let mut locals: HashMap<LocalId, LocalId> = HashMap::new();
    for &l in &free {
        let id = LocalId(helper.locals.len() as u32);
        helper.locals.push(HirLocal { kind: LocalKind::Param, ..body.local(l).clone() });
        helper.params.push(id);
        locals.insert(l, id);
    }
    let mut own: Vec<LocalId> = bound.iter().copied().collect();
    own.sort_by_key(|l| l.0);
    for l in own {
        locals.insert(l, LocalId(helper.locals.len() as u32));
        helper.locals.push(body.local(l).clone());
    }
    let ids: HashMap<HirId, HirId> = nodes.iter().enumerate().map(|(i, &id)| (id, HirId(i as u32))).collect();
    for &id in &nodes {
        let e = body.expr(id);
        let kind = map_ids(&e.kind, &|h| ids[&h], &|l| locals[&l]);
        helper.exprs.push(HirExpr { kind, ..e.clone() });
    }
    let unit = || HirExpr {
        kind: HirKind::Lit(super::Lit::Unit),
        ty: Some(ty::unit()),
        layout: crate::layout::Layout::Word,
        span: body.expr(arm).span,
        node: None,
        flags: HirFlags::default(),
        scope: body.expr(arm).scope,
        src: body.expr(arm).src,
    };
    let push = |b: &mut HirBody, e: HirExpr| {
        let id = HirId(b.exprs.len() as u32);
        b.exprs.push(e);
        id
    };
    let arm_ty = |t: Option<ty::Ty>| HirExpr { ty: t, ..unit() };
    let ret_local = written.map(|w| {
        let read = body.exprs.iter().find(|e| matches!(e.kind, HirKind::Local(l) if l == w)).cloned();
        read.map_or_else(|| HirExpr { kind: HirKind::Local(w), ty: body.local(w).ty.clone(), ..unit() }, |e| HirExpr { flags: HirFlags::default(), ..e })
    });
    let tail = ret_local.as_ref().map(|r| push(&mut helper, HirExpr { kind: HirKind::Local(locals[&written.unwrap()]), ..r.clone() }));
    let ret = written.and_then(|w| body.local(w).ty.clone()).unwrap_or_else(ty::unit);
    helper.ret_layout = ret_local.as_ref().map_or(crate::layout::Layout::Word, |r| r.layout.clone());
    helper.ret = Some(ret.clone());
    let block = push(&mut helper, HirExpr { kind: HirKind::Block { stmts: vec![ids[&arm]], tail }, ..arm_ty(Some(ret.clone())) });
    helper.root = Some(block);
    // The call in the arm's place.
    let args: Vec<HirId> = free
        .iter()
        .map(|&l| push(out, HirExpr { kind: HirKind::Local(l), ty: body.local(l).ty.clone(), layout: layout_of(body, l), ..unit() }))
        .collect();
    let callee = Callee::Named { name: name.to_string(), def: None, overload: None };
    let call = push(out, HirExpr { kind: HirKind::Call { callee, args }, ty: Some(ret.clone()), layout: helper.ret_layout.clone(), ..unit() });
    let stmt = match written {
        Some(w) => {
            let place = push(out, HirExpr { kind: HirKind::Local(w), ty: body.local(w).ty.clone(), layout: layout_of(body, w), ..unit() });
            push(out, HirExpr { kind: HirKind::Assign { place, value: call }, ..arm_ty(Some(ty::unit())) })
        }
        None => call,
    };
    let params = free.iter().map(|&l| body.local(l).ty.clone().expect("typed free local")).collect();
    Some((Helper { body: helper, params, ret }, stmt))
}

/// The layout of a read of `l`: as another read of it has it.
fn layout_of(body: &HirBody, l: LocalId) -> crate::layout::Layout {
    body.exprs
        .iter()
        .find(|e| matches!(e.kind, HirKind::Local(x) if x == l))
        .map_or(crate::layout::Layout::Word, |e| e.layout.clone())
}

/// The local a place (`x`, `x.f`, `x[i]`, …) is rooted at.
fn place_root(body: &HirBody, mut id: HirId) -> Option<LocalId> {
    loop {
        match &body.expr(id).kind {
            HirKind::Local(l) => return Some(*l),
            HirKind::Field { base, .. } | HirKind::Index { base, .. } => id = *base,
            _ => return None,
        }
    }
}

fn binders(pat: &HirPat, out: &mut HashSet<LocalId>) {
    match pat {
        HirPat::Bind(l) => {
            out.insert(*l);
        }
        HirPat::Variant { fields, .. } => match fields {
            super::HirPatFields::Unit => {}
            super::HirPatFields::Tuple(ps) => ps.iter().for_each(|p| binders(p, out)),
            super::HirPatFields::Record(fs) => fs.iter().for_each(|(_, p)| binders(p, out)),
        },
        HirPat::Tuple(ps) => ps.iter().for_each(|p| binders(p, out)),
        HirPat::Record(fs) => fs.iter().for_each(|(_, p)| binders(p, out)),
        HirPat::Wild | HirPat::Int(_) => {}
    }
}

/// `body` with only the nodes its root reaches, renumbered: an inline cost
/// counts every node a body holds.
fn compact(body: &mut HirBody) {
    let Some(root) = body.root else { return };
    let mut order = Vec::new();
    let mut seen = vec![false; body.exprs.len()];
    let mut stack = vec![root];
    while let Some(id) = stack.pop() {
        if std::mem::replace(&mut seen[id.0 as usize], true) {
            continue;
        }
        order.push(id);
        stack.extend(body.children(id));
    }
    order.sort_by_key(|id| id.0);
    let ids: HashMap<HirId, HirId> = order.iter().enumerate().map(|(i, &id)| (id, HirId(i as u32))).collect();
    let exprs = order
        .iter()
        .map(|&id| {
            let e = body.expr(id);
            HirExpr { kind: map_ids(&e.kind, &|h| ids[&h], &|l| l), ..e.clone() }
        })
        .collect();
    body.exprs = exprs;
    body.root = Some(ids[&root]);
    body.aliased_fn_values = body.aliased_fn_values.iter().filter_map(|h| ids.get(h).copied()).collect();
    body.fn_key_sites = body.fn_key_sites.iter().filter_map(|(h, k)| Some((*ids.get(h)?, k.clone()))).collect();
}

#[cfg(test)]
#[path = "outline.tests.rs"]
mod tests;
