//! Early freeing (`feature/escape-memory` step 2): free an object at the
//! end of its scope, or right after the call it was passed to, when the
//! escape facts prove nothing else can reach it.
//!
//! Two shapes are rewritten on a body after the HIR optimizer ran:
//!
//! - **Call temporaries.** `f(g(x))` where `g`'s result is fresh (nothing
//!   else reaches anything in it) and `f` neither keeps, returns nor
//!   changes that argument: the argument is bound to a temporary and its
//!   whole graph is freed after the call (`item_check(bottom_up(d))`).
//! - **Block-end locals.** `let x = …` whose every value is an object this
//!   body allocated, that no other local, store, capture, `yield` or result
//!   reaches: `x`'s object is freed (not what it holds) when the block that
//!   declares it ends, and before every `return` after the `let`
//!   (`let b = to_bytes(s)`). `break`, `continue` and a panic leave it to
//!   the collector.
//!
//! Each free is a `Builtin::FreeEarly` of the local, then a `Clear` of it.
//! The VM skips what it cannot free safely (interned strings, shared unit
//! variants, objects with a `drop`, anything while a collection is under
//! way), so a free never needs more than the escape facts. The tracing GC
//! stays the backstop for everything else.

use std::collections::HashMap;

use super::escape::{no_heap, result_exprs, CallLookup, Escape, Src};
use super::{is_vec, Builtin, Callee, HirBody, HirId, HirKind, HirModule, LocalId, LocalKind, VEC_MUTATORS};
use crate::hir::{HirExpr, HirFlags, HirLocal, Span};
use crate::typechecking::ty::{self, Ty};

/// `body` with early frees added, or `None` when it has no place for one.
pub(super) fn rewrite(module: &HirModule, body: &HirBody, lookup: &CallLookup<'_>) -> Option<HirBody> {
    if body.is_coro || body.root.is_none() {
        return None;
    }
    // A lambda's captures are read from `module`; a body spliced from
    // another module may name lambdas it does not hold.
    if body.exprs.iter().any(|x| matches!(x.kind, HirKind::Lambda { body: i } if i >= module.bodies.len())) {
        return None;
    }
    // `defer` bodies run on every exit and lower against the block layout
    // as built.
    if body.exprs.iter().any(|x| matches!(x.kind, HirKind::Defer { .. })) {
        return None;
    }
    let mut e = Escape::new(module, body, lookup);
    e.solve_locals();
    let mut escaped = Src::EMPTY;
    let mut returned = Src::EMPTY;
    for i in 0..body.exprs.len() {
        e.sink(HirId(i as u32), &mut escaped, &mut returned);
    }
    if let Some(root) = body.root {
        for v in result_exprs(body, root) {
            returned = returned.union(e.src(v));
        }
    }
    let kept = e.close(escaped.union(returned)).union(e.held());
    let temps = call_temps(body, &mut e);
    let locals = block_locals(body, &mut e, kept);
    if temps.is_empty() && locals.is_empty() {
        return None;
    }
    let mut out = body.clone();
    for (call, arg, deep) in temps {
        free_temp(&mut out, call, arg, deep);
    }
    for (block, local) in locals {
        free_at_end(&mut out, block, local);
    }
    Some(out)
}

/// Whether a value of `ty` may be one heap object the VM can free.
fn freeable(ty: Option<&Ty>) -> bool {
    let Some(t) = ty else {
        return false;
    };
    let t = ty::strip_readonly(t);
    if no_heap(Some(t)) {
        return false;
    }
    match t {
        Ty::Con(n) => n != "Range" && n != "never",
        Ty::App(head, _) => matches!(&**head, Ty::Con(n) if n != "Range"),
        Ty::Sum { .. } | Ty::Tuple(_) | Ty::Array { .. } => true,
        _ => false,
    }
}

/// `(call, argument index, deep)` for every argument that is a fresh call
/// result its call neither keeps nor returns.
fn call_temps(body: &HirBody, e: &mut Escape<'_, '_>) -> Vec<(HirId, usize, bool)> {
    let mut out = Vec::new();
    for (i, x) in body.exprs.iter().enumerate() {
        let HirKind::Call { callee, args } = &x.kind else {
            continue;
        };
        if vec_mutator(body, callee, args) {
            continue;
        }
        let Some(f) = e.callee(HirId(i as u32), callee, args) else {
            continue;
        };
        for (j, a) in args.iter().enumerate().take(64) {
            let bit = 1u64 << j;
            if f.escapes & bit != 0 || f.returns & bit != 0 {
                continue;
            }
            let arg = body.expr(*a);
            let HirKind::Call { callee: g_callee, args: g_args } = &arg.kind else {
                continue;
            };
            if !freeable(arg.ty.as_ref()) || vec_mutator(body, g_callee, g_args) {
                continue;
            }
            if e.callee(*a, g_callee, g_args).is_some_and(|g| !g.shared_ret) {
                // A callee that changes the argument may have stored
                // something it does not own there: free only the top.
                out.push((HirId(i as u32), j, f.mutates & bit == 0));
            }
        }
    }
    out
}

fn vec_mutator(body: &HirBody, callee: &Callee, args: &[HirId]) -> bool {
    matches!(callee, Callee::Method { name } if VEC_MUTATORS.contains(&name.as_str()))
        && args.first().is_some_and(|r| is_vec(body.expr(*r).ty.as_ref()))
}

/// `(block, local)` for every `let` local whose object nothing else
/// reaches, with the block that declares it.
fn block_locals(body: &HirBody, e: &mut Escape<'_, '_>, kept: Src) -> Vec<(HirId, LocalId)> {
    let parents = parents(body);
    // Each local's `let`s, and every node that names it.
    let mut lets: HashMap<LocalId, Vec<HirId>> = HashMap::new();
    let mut uses: HashMap<LocalId, Vec<HirId>> = HashMap::new();
    for (i, x) in body.exprs.iter().enumerate() {
        match &x.kind {
            HirKind::Let { local, .. } => lets.entry(*local).or_default().push(HirId(i as u32)),
            HirKind::Local(l) => uses.entry(*l).or_default().push(HirId(i as u32)),
            _ => {}
        }
    }
    let srcs: Vec<(LocalId, Src)> = (0..body.locals.len()).map(|i| (LocalId(i as u32), e.local(LocalId(i as u32)))).collect();
    let mut out = Vec::new();
    for &(l, s) in &srcs {
        let local = body.local(l);
        // `let _ = …` binds nothing to read back.
        if !matches!(local.kind, LocalKind::Let | LocalKind::Temp) || local.captured || local.name == "_" || !freeable(local.ty.as_ref()) {
            continue;
        }
        if s.params != 0 || s.other || s.sites == 0 || s.sites & kept.sites != 0 {
            continue;
        }
        if srcs.iter().any(|&(m, t)| m != l && t.sites & s.sites != 0) {
            continue;
        }
        let Some([decl]) = lets.get(&l).map(Vec::as_slice) else {
            continue;
        };
        let Some(&block) = parents.get(decl) else {
            continue;
        };
        let HirKind::Block { stmts, tail } = &body.expr(block).kind else {
            continue;
        };
        if !stmts.contains(decl) {
            continue;
        }
        // `break` / `continue` out of the block leave it to the GC; a
        // `return` never hands it back (`kept` holds what is returned).
        if matches!(stmts.last().map(|s| &body.expr(*s).kind), Some(HirKind::Break | HirKind::Continue)) {
            continue;
        }
        // Every read of it is inside the block, and the block's value is
        // not (part of) its object.
        let inside = |mut at: HirId| loop {
            if at == block {
                return true;
            }
            match parents.get(&at) {
                Some(&p) => at = p,
                None => return false,
            }
        };
        if !uses.get(&l).is_none_or(|us| us.iter().all(|u| inside(*u))) {
            continue;
        }
        if let Some(t) = tail
            && e.src(*t).sites & s.sites != 0
        {
            continue;
        }
        // A local typed inlining clears is freed at its clear, which must
        // then be a statement of the same block.
        let clears = body.exprs.iter().enumerate().filter(|(_, x)| matches!(&x.kind, HirKind::Clear(ls) if ls.contains(&l)));
        if !clears.map(|(i, _)| HirId(i as u32)).all(|c| stmts.contains(&c)) {
            continue;
        }
        out.push((block, l));
    }
    out
}

fn parents(body: &HirBody) -> HashMap<HirId, HirId> {
    let mut out = HashMap::new();
    for i in 0..body.exprs.len() {
        let id = HirId(i as u32);
        for c in super::super::lower::children(body, id) {
            out.insert(c, id);
        }
    }
    out
}

fn push(body: &mut HirBody, kind: HirKind, ty: Option<Ty>, span: Span) -> HirId {
    let id = HirId(body.exprs.len() as u32);
    body.exprs.push(HirExpr {
        kind,
        ty,
        layout: crate::layout::Layout::Word,
        span,
        node: None,
        flags: HirFlags::default(),
    });
    id
}

fn temp(body: &mut HirBody, ty: Option<Ty>) -> LocalId {
    let id = LocalId(body.locals.len() as u32);
    body.locals.push(HirLocal {
        name: format!("$free{}", id.0),
        ty,
        kind: LocalKind::Temp,
        captured: false,
    });
    id
}

/// Move node `id` to a new node and return it, so `id` can be rewritten
/// while its parent still points at it.
fn relocate(body: &mut HirBody, id: HirId) -> HirId {
    let moved = body.exprs[id.0 as usize].clone();
    let new = HirId(body.exprs.len() as u32);
    body.exprs.push(moved);
    new
}

/// The free of `local` and the clear of its frame word.
fn free_stmts(body: &mut HirBody, local: LocalId, deep: bool, span: Span) -> [HirId; 2] {
    let ty = body.local(local).ty.clone();
    let read = push(body, HirKind::Local(local), ty, span);
    let free = push(body, HirKind::Builtin { op: Builtin::FreeEarly { deep }, args: vec![read] }, Some(ty::unit()), span);
    let clear = push(body, HirKind::Clear(vec![local]), Some(ty::unit()), span);
    [free, clear]
}

/// `f(…, g(x), …)` becomes `{ let r = f(…, { let t = g(x); t }, …); free t; r }`.
fn free_temp(body: &mut HirBody, call: HirId, arg: usize, deep: bool) {
    let HirKind::Call { args, .. } = &body.expr(call).kind else {
        return;
    };
    let a = args[arg];
    let span = body.expr(call).span;
    let arg_ty = body.expr(a).ty.clone();
    let t = temp(body, arg_ty.clone());
    let g = relocate(body, a);
    let bind = push(body, HirKind::Let { local: t, init: Some(g) }, Some(ty::unit()), span);
    let read = push(body, HirKind::Local(t), arg_ty, span);
    let x = &mut body.exprs[a.0 as usize];
    x.kind = HirKind::Block { stmts: vec![bind], tail: Some(read) };
    x.node = None;
    let call_ty = body.expr(call).ty.clone();
    let layout = body.expr(call).layout.clone();
    let inner = relocate(body, call);
    let frees = free_stmts(body, t, deep, span);
    let unit = call_ty.as_ref().is_none_or(|t| *t == ty::unit());
    let (stmts, tail) = if unit {
        (vec![inner, frees[0], frees[1]], None)
    } else {
        let r = temp(body, call_ty.clone());
        let keep = push(body, HirKind::Let { local: r, init: Some(inner) }, Some(ty::unit()), span);
        let read = push(body, HirKind::Local(r), call_ty.clone(), span);
        (vec![keep, frees[0], frees[1]], Some(read))
    };
    let x = &mut body.exprs[call.0 as usize];
    x.kind = HirKind::Block { stmts, tail };
    x.node = None;
    x.layout = layout;
}

/// Free `local` when `block` ends: before every `return` after its `let`,
/// and after the block's statements (and its tail, when the tail reads it)
/// when the block can fall through.
fn free_at_end(body: &mut HirBody, block: HirId, local: LocalId) {
    let HirKind::Block { mut stmts, tail } = body.expr(block).kind.clone() else {
        return;
    };
    let span = body.expr(block).span;
    // Cleared by typed inlining after its last read: free it there.
    if let Some(at) = stmts.iter().position(|s| matches!(&body.expr(*s).kind, HirKind::Clear(ls) if ls.contains(&local))) {
        let ty = body.local(local).ty.clone();
        let read = push(body, HirKind::Local(local), ty, span);
        let free = push(body, HirKind::Builtin { op: Builtin::FreeEarly { deep: false }, args: vec![read] }, Some(ty::unit()), span);
        stmts.insert(at, free);
        body.exprs[block.0 as usize].kind = HirKind::Block { stmts, tail };
        return;
    }
    // A block that always leaves early has no end to free at.
    let exits = |k: &HirKind| matches!(k, HirKind::Return(_) | HirKind::Break | HirKind::Continue);
    let diverges = stmts.last().is_some_and(|s| exits(&body.expr(*s).kind)) || tail.is_some_and(|t| exits(&body.expr(t).kind));
    // Every `return` after the `let`, nested or not, frees on its way out.
    let decl = stmts.iter().position(|s| matches!(body.expr(*s).kind, HirKind::Let { local: l, .. } if l == local));
    let after = decl.map_or(&stmts[..0], |d| &stmts[d + 1..]);
    let mut work: Vec<HirId> = after.iter().copied().chain(tail).collect();
    let mut returns = Vec::new();
    while let Some(at) = work.pop() {
        if matches!(body.expr(at).kind, HirKind::Return(_)) {
            returns.push(at);
        }
        work.extend(super::super::lower::children(body, at));
    }
    for ret in returns {
        free_before_return(body, ret, local);
    }
    if diverges {
        return;
    }
    let tail = match tail {
        Some(t) if reads(body, t, local) => {
            let ty = body.expr(t).ty.clone();
            let r = temp(body, ty.clone());
            let keep = push(body, HirKind::Let { local: r, init: Some(t) }, Some(ty::unit()), span);
            stmts.push(keep);
            Some(push(body, HirKind::Local(r), ty, span))
        }
        t => t,
    };
    stmts.extend(free_stmts(body, local, false, span));
    body.exprs[block.0 as usize].kind = HirKind::Block { stmts, tail };
}

/// `return v` becomes `{ let r = v; free local; return r }`.
fn free_before_return(body: &mut HirBody, ret: HirId, local: LocalId) {
    let HirKind::Return(value) = body.expr(ret).kind.clone() else {
        return;
    };
    let span = body.expr(ret).span;
    let mut stmts = Vec::new();
    let value = match value {
        Some(v) if reads(body, v, local) => {
            let ty = body.expr(v).ty.clone();
            let r = temp(body, ty.clone());
            stmts.push(push(body, HirKind::Let { local: r, init: Some(v) }, Some(ty::unit()), span));
            Some(push(body, HirKind::Local(r), ty, span))
        }
        v => v,
    };
    stmts.extend(free_stmts(body, local, false, span));
    let inner = relocate(body, ret);
    body.exprs[inner.0 as usize].kind = HirKind::Return(value);
    let x = &mut body.exprs[ret.0 as usize];
    x.kind = HirKind::Block { stmts, tail: Some(inner) };
    x.node = None;
}

/// Whether the subtree at `id` reads `local`.
fn reads(body: &HirBody, id: HirId, local: LocalId) -> bool {
    let mut work = vec![id];
    while let Some(at) = work.pop() {
        if matches!(body.expr(at).kind, HirKind::Local(l) if l == local) {
            return true;
        }
        work.extend(super::super::lower::children(body, at));
    }
    false
}
