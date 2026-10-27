//! Early freeing (`feature/escape-memory` step 2): free an object at the
//! end of its scope, or right after the call it was passed to, when the
//! escape facts prove nothing else can reach it.
//!
//! Four shapes are rewritten on a body after the HIR optimizer ran:
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
//! - **Reassigned locals** (step 3). `x = v` frees `x`'s old value once
//!   `v` is computed, when every value `x` holds is fresh and every read
//!   of it leaves nothing behind (`s = s + piece`, `list = build(n)`).
//! - **Match scrutinees** (step 3). `match g(x) { … }` frees the fresh
//!   value it matched once the arms are done, when only the arms'
//!   bindings read it and the match does not hand it back.
//!
//! Each free is a `Builtin::FreeEarly` of the local, then a `Clear` of it.
//! The VM skips what it cannot free safely (interned strings, shared unit
//! variants, objects with a `drop`, anything while a collection is under
//! way), so a free never needs more than the escape facts. The tracing GC
//! stays the backstop for everything else.

use std::collections::HashMap;

use super::escape::{leaf, no_heap, result_exprs, CallLookup, Escape, Src};
use super::{is_vec, BinOp, Builtin, Callee, HirBody, HirId, HirKind, HirModule, LocalId, LocalKind, MakeKind, VEC_MUTATORS};
use crate::hir::{HirExpr, HirFlags, HirLocal, IndexKind, Span};
use crate::typechecking::ty::{self, Ty};

/// `body` with early frees added, or `None` when it has no place for one.
pub(super) fn rewrite(module: &HirModule, body: &HirBody, lookup: &CallLookup<'_>, host: &dyn Fn(HirId) -> Option<usize>) -> Option<HirBody> {
    if body.is_coro || body.root.is_none() {
        return None;
    }
    let viewed = byte_views(body, host);
    let changed = viewed.is_some();
    let body = viewed.as_ref().unwrap_or(body);
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
    let stored = e.close(escaped).union(e.held());
    let parents = parents(body);
    let temps = call_temps(body, &mut e);
    let linear = linear_locals(body, &mut e, &parents, stored);
    let locals = block_locals(body, &mut e, kept, &linear);
    let chains = concat_temps(body, &mut e, &parents);
    let scrutinees = match_temps(body, &mut e, &parents, kept);
    let takes = take_calls(body, &mut e, &parents, stored, host);
    let reassigns: Vec<(HirId, bool)> = (0..body.exprs.len())
        .map(|i| HirId(i as u32))
        .filter_map(|a| match &body.expr(a).kind {
            HirKind::Assign { place, .. } => match body.expr(*place).kind {
                HirKind::Local(l) => linear.get(&l).map(|&(_, deep)| (a, deep)),
                _ => None,
            },
            _ => None,
        })
        .collect();
    if temps.is_empty() && locals.is_empty() && chains.is_empty() && reassigns.is_empty() && scrutinees.is_empty() && takes.is_empty() {
        return changed.then(|| body.clone());
    }
    let mut out = body.clone();
    // Flags first: the rewrites below move nodes, flags and all.
    for c in takes {
        out.exprs[c.0 as usize].flags.insert(HirFlags::TAKE);
    }
    for (call, arg, deep) in temps {
        free_temp(&mut out, call, arg, deep);
    }
    for (head, leaves) in chains {
        free_concat_temps(&mut out, head, &leaves);
    }
    for m in scrutinees {
        free_match_temp(&mut out, m);
    }
    for (assign, deep) in reassigns {
        free_on_reassign(&mut out, assign, deep);
    }
    for (block, local) in locals {
        free_at_end(&mut out, block, local);
    }
    Some(out)
}

/// `let b = to_bytes(s)` whose `b` is only indexed and measured, with `s`
/// never set again: those reads go to `s`'s own bytes (`s[i]`, `s.len()`)
/// and the copy is never made.
fn byte_views(body: &HirBody, host: &dyn Fn(HirId) -> Option<usize>) -> Option<HirBody> {
    let to_bytes = common::host_native_id("to_bytes")?;
    let parents = parents(body);
    let mut lets: HashMap<LocalId, Vec<HirId>> = HashMap::new();
    let mut reads: HashMap<LocalId, Vec<HirId>> = HashMap::new();
    let mut set = std::collections::HashSet::new();
    for (i, x) in body.exprs.iter().enumerate() {
        match &x.kind {
            HirKind::Let { local, .. } => lets.entry(*local).or_default().push(HirId(i as u32)),
            HirKind::Local(l) => reads.entry(*l).or_default().push(HirId(i as u32)),
            HirKind::Assign { place, .. } => {
                if let HirKind::Local(l) = body.expr(*place).kind {
                    set.insert(l);
                }
            }
            _ => {}
        }
    }
    let mut out: Option<HirBody> = None;
    for (&b, decls) in &lets {
        let [decl] = decls.as_slice() else {
            continue;
        };
        let HirKind::Let { init: Some(init), .. } = body.expr(*decl).kind else {
            continue;
        };
        let HirKind::Call { args, .. } = &body.expr(init).kind else {
            continue;
        };
        let [arg] = args.as_slice() else {
            continue;
        };
        let HirKind::Local(s) = body.expr(*arg).kind else {
            continue;
        };
        let string = body.local(s).ty.as_ref().is_some_and(|t| matches!(ty::strip_readonly(t), Ty::Con(n) if n == "string"));
        // `let _ = to_bytes(s)` runs it on purpose.
        if host(init) != Some(to_bytes) || !string || body.local(b).name == "_" || set.contains(&b) || set.contains(&s) || body.local(b).captured || body.local(s).captured {
            continue;
        }
        let uses = reads.get(&b).cloned().unwrap_or_default();
        let ok = uses.iter().all(|&r| {
            let Some(&p) = parents.get(&r) else {
                return false;
            };
            match &body.expr(p).kind {
                HirKind::Index { base, kind: IndexKind::Array, .. } => {
                    *base == r && !parents.get(&p).is_some_and(|&q| matches!(body.expr(q).kind, HirKind::Assign { place, .. } if place == p))
                }
                HirKind::Call { callee: Callee::Method { name }, args } => name == "len" && args.as_slice() == [r],
                _ => false,
            }
        });
        if !ok {
            continue;
        }
        let o = out.get_or_insert_with(|| body.clone());
        let s_ty = o.local(s).ty.clone();
        for &r in &uses {
            let x = &mut o.exprs[r.0 as usize];
            x.kind = HirKind::Local(s);
            x.ty = s_ty.clone();
            x.node = None;
            let p = parents[&r];
            if let HirKind::Index { kind, .. } = &mut o.exprs[p.0 as usize].kind {
                *kind = IndexKind::String;
            }
        }
        if let HirKind::Let { init, .. } = &mut o.exprs[decl.0 as usize].kind {
            *init = None;
        }
    }
    out
}

/// `from_bytes(x)` calls whose buffer nothing reads again: the call sits in
/// a `return` (no loop in between), `x`'s buffer was allocated here and
/// nothing stores it, and inside that `return` it is read only by this call
/// (or copied into another local on the way, as inlining binds a parameter).
fn take_calls(body: &HirBody, e: &mut Escape<'_, '_>, parents: &HashMap<HirId, HirId>, stored: Src, host: &dyn Fn(HirId) -> Option<usize>) -> Vec<HirId> {
    let Some(from_bytes) = common::host_native_id("from_bytes") else {
        return Vec::new();
    };
    let mut reads: HashMap<LocalId, Vec<HirId>> = HashMap::new();
    for (i, x) in body.exprs.iter().enumerate() {
        if let HirKind::Local(l) = x.kind {
            reads.entry(l).or_default().push(HirId(i as u32));
        }
    }
    let under = |mut at: HirId, root: HirId| loop {
        if at == root {
            return true;
        }
        match parents.get(&at) {
            Some(&p) => at = p,
            None => return false,
        }
    };
    let mut out = Vec::new();
    for i in 0..body.exprs.len() {
        let c = HirId(i as u32);
        let HirKind::Call { args, .. } = &body.expr(c).kind else {
            continue;
        };
        let [arg] = args.as_slice() else {
            continue;
        };
        let HirKind::Local(x) = body.expr(*arg).kind else {
            continue;
        };
        if host(c) != Some(from_bytes) || !is_vec(body.local(x).ty.as_ref()) {
            continue;
        }
        // The `return` it sits in, with no loop on the way.
        let mut at = c;
        let ret = loop {
            match parents.get(&at) {
                Some(&p) => match body.expr(p).kind {
                    HirKind::Return(_) => break Some(p),
                    HirKind::Loop { .. } | HirKind::ForIn { .. } | HirKind::Lambda { .. } => break None,
                    _ => at = p,
                },
                None => break None,
            }
        };
        let Some(ret) = ret else {
            continue;
        };
        let s = e.local(x);
        if s.params != 0 || s.other || s.sites == 0 || s.sites & stored.sites != 0 {
            continue;
        }
        let holders: Vec<LocalId> = (0..body.locals.len())
            .map(|j| LocalId(j as u32))
            .filter(|&m| {
                let h = e.local(m);
                e.close(h).sites & s.sites != 0
            })
            .collect();
        let ok = holders.iter().all(|&m| {
            let local = body.local(m);
            matches!(local.kind, LocalKind::Let | LocalKind::Temp)
                && !local.captured
                && reads.get(&m).is_none_or(|rs| {
                    rs.iter().filter(|&&r| under(r, ret)).all(|&r| {
                        r == *arg || parents.get(&r).is_some_and(|&p| matches!(&body.expr(p).kind, HirKind::Let { local, init: Some(v) } if *v == r && holders.contains(local)))
                    })
                })
        });
        if ok {
            out.push(c);
        }
    }
    out
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
        // Fixed arrays and tuples are left out: MIR replaces many by their
        // elements (a free would keep the object), and they are small.
        Ty::Sum { .. } => true,
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
fn block_locals(body: &HirBody, e: &mut Escape<'_, '_>, kept: Src, linear: &HashMap<LocalId, (Src, bool)>) -> Vec<(HirId, LocalId)> {
    let parents = parents(body);
    // Each local's `let`s, and every node that names it.
    let mut lets: HashMap<LocalId, Vec<HirId>> = HashMap::new();
    let mut uses: HashMap<LocalId, Vec<HirId>> = HashMap::new();
    let mut bases = std::collections::HashSet::new();
    for (i, x) in body.exprs.iter().enumerate() {
        match &x.kind {
            HirKind::Field { base, .. } => {
                bases.insert(*base);
            }
            HirKind::Let { local, .. } => lets.entry(*local).or_default().push(HirId(i as u32)),
            HirKind::Local(l) => uses.entry(*l).or_default().push(HirId(i as u32)),
            _ => {}
        }
    }
    // A linear local's literals are interned (the VM never frees one), so
    // only what it allocated counts.
    let srcs: Vec<(LocalId, Src)> = (0..body.locals.len())
        .map(|i| {
            let l = LocalId(i as u32);
            (l, linear.get(&l).map_or_else(|| e.local(l), |&(s, _)| s))
        })
        .collect();
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
        // A class local read only through its fields lives in frame slots
        // (lowering's SROA); freeing it would make it a whole-object use.
        if let HirKind::Let { init: Some(i), .. } = body.expr(*decl).kind
            && matches!(body.expr(i).kind, HirKind::Make { kind: MakeKind::Class(_), .. })
            && uses.get(&l).is_none_or(|us| us.iter().all(|u| bases.contains(u)))
        {
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

/// Whether `v` always yields an object nothing else holds, or an interned
/// literal: a concatenation, a fresh call result, or a string literal.
fn fresh_top(body: &HirBody, e: &mut Escape<'_, '_>, v: HirId) -> bool {
    match &body.expr(v).kind {
        HirKind::Lit(super::super::Lit::Str(_)) | HirKind::Bin { op: BinOp::StrConcat, .. } => true,
        HirKind::Block { tail: Some(t), .. } => fresh_top(body, e, *t),
        HirKind::Call { callee, args } => {
            !vec_mutator(body, callee, args) && e.callee(v, callee, args).is_some_and(|g| !g.shared_ret && g.returns == 0)
        }
        _ => false,
    }
}

/// Whether `v` (an inlined body, say) builds a graph of objects of its own
/// that, once it is assigned to `target`, nothing else holds: no store
/// keeps any of it, and every other local that may reach it is declared
/// inside `v` (dead once `v` is computed).
#[allow(clippy::too_many_arguments)]
fn fresh_graph(
    body: &HirBody,
    e: &mut Escape<'_, '_>,
    parents: &HashMap<HirId, HirId>,
    lets: &HashMap<LocalId, Vec<HirId>>,
    stored: Src,
    target: LocalId,
    v: HirId,
) -> bool {
    let s = e.src(v);
    if s.params != 0 || s.other || s.sites == 0 || s.sites & stored.sites != 0 {
        return false;
    }
    let within = |mut at: HirId| loop {
        if at == v {
            return true;
        }
        match parents.get(&at) {
            Some(&p) => at = p,
            None => return false,
        }
    };
    (0..body.locals.len()).map(|i| LocalId(i as u32)).filter(|&m| m != target).all(|m| {
        let h = e.local(m);
        if e.close(h).sites & s.sites == 0 {
            return true;
        }
        !body.local(m).captured && (lets.get(&m).is_some_and(|ds| ds.iter().all(|d| within(*d))) || cleared_after(body, parents, v, m))
    })
}

/// Whether the statement right after the one that computes `v` clears
/// `m` (typed inlining clears its locals after their last read).
fn cleared_after(body: &HirBody, parents: &HashMap<HirId, HirId>, v: HirId, m: LocalId) -> bool {
    let Some(&stmt) = parents.get(&v) else {
        return false;
    };
    let Some(&block) = parents.get(&stmt) else {
        return false;
    };
    let HirKind::Block { stmts, .. } = &body.expr(block).kind else {
        return false;
    };
    stmts
        .iter()
        .position(|s| *s == stmt)
        .and_then(|i| stmts.get(i + 1))
        .is_some_and(|n| matches!(&body.expr(*n).kind, HirKind::Clear(ls) if ls.contains(&m)))
}

/// Locals whose value is only ever read in place (copied out of by a
/// concatenation, a read of a plain element or field, or a call that keeps
/// and returns none of it) or handed on as it dies (`return x`, the tail of
/// the block that declares it), and set only to fresh objects or literals.
/// On such a local, `x = e` may free the old value once `e` is computed:
/// nothing else can hold it. Each maps to what it holds other than literals,
/// and whether the free takes the whole graph: a string or vector of plain
/// values holds nothing, and any other value is read only by callees that
/// leave it as it was (a fresh graph stays fresh).
fn linear_locals(body: &HirBody, e: &mut Escape<'_, '_>, parents: &HashMap<HirId, HirId>, stored: Src) -> HashMap<LocalId, (Src, bool)> {
    let mut lets: HashMap<LocalId, Vec<HirId>> = HashMap::new();
    let mut defs: HashMap<LocalId, Vec<HirId>> = HashMap::new();
    let mut reads: HashMap<LocalId, Vec<HirId>> = HashMap::new();
    let mut places = std::collections::HashSet::new();
    let mut bad = std::collections::HashSet::new();
    for (i, x) in body.exprs.iter().enumerate() {
        match &x.kind {
            HirKind::Let { local, init } => {
                lets.entry(*local).or_default().push(HirId(i as u32));
                match init {
                    Some(v) => defs.entry(*local).or_default().push(*v),
                    None => {
                        bad.insert(*local);
                    }
                }
            }
            HirKind::Assign { place, value } => {
                if let HirKind::Local(l) = body.expr(*place).kind {
                    places.insert(*place);
                    if x.flags.contains(HirFlags::ADJUST) {
                        bad.insert(l);
                    }
                    defs.entry(l).or_default().push(*value);
                }
            }
            HirKind::Local(l) => reads.entry(*l).or_default().push(HirId(i as u32)),
            _ => {}
        }
    }
    let mut out = HashMap::new();
    for (i, local) in body.locals.iter().enumerate() {
        let l = LocalId(i as u32);
        let flat = leaf(local.ty.as_ref());
        if !matches!(local.kind, LocalKind::Let | LocalKind::Temp) || local.captured || local.name == "_" || !(flat || freeable(local.ty.as_ref())) || bad.contains(&l) {
            continue;
        }
        let Some([decl]) = lets.get(&l).map(Vec::as_slice) else {
            continue;
        };
        let Some(&block) = parents.get(decl) else {
            continue;
        };
        let vals = defs.get(&l).cloned().unwrap_or_default();
        if !vals.iter().all(|&v| fresh_top(body, e, v) || (!flat && fresh_graph(body, e, parents, &lets, stored, l, v))) {
            continue;
        }
        // Its facts must come from these definitions alone.
        let all = vals.iter().fold(Src::EMPTY, |m, &v| m.union(e.src(v)));
        if all != e.local(l) {
            continue;
        }
        let own = vals
            .iter()
            .filter(|&&v| !matches!(body.expr(v).kind, HirKind::Lit(super::super::Lit::Str(_))))
            .fold(Src::EMPTY, |m, &v| m.union(e.src(v)));
        if own.params != 0 || own.other {
            continue;
        }
        let ok = reads.get(&l).is_none_or(|rs| {
            rs.iter().filter(|r| !places.contains(*r)).all(|&r| {
                let Some(&p) = parents.get(&r) else {
                    return false;
                };
                match &body.expr(p).kind {
                    HirKind::Bin { op, .. } => flat || !matches!(op, BinOp::Overloaded(_)),
                    HirKind::Index { base, .. } | HirKind::Field { base, .. } => *base == r && no_heap(body.expr(p).ty.as_ref()),
                    HirKind::Return(Some(v)) => *v == r,
                    HirKind::Block { tail: Some(t), .. } => p == block && *t == r,
                    HirKind::Call { callee, args } => {
                        let Some(j) = args.iter().position(|a| *a == r) else {
                            return false;
                        };
                        if j == 0 && vec_mutator(body, callee, args) {
                            return flat;
                        }
                        let bit = 1u64 << j.min(63);
                        j < 64
                            && e.callee(p, callee, args)
                                .is_some_and(|f| f.escapes & bit == 0 && f.returns & bit == 0 && (flat || f.mutates & bit == 0))
                    }
                    _ => false,
                }
            })
        });
        if ok {
            out.insert(l, (own, !flat));
        }
    }
    out
}

/// `x = v` on a linear local becomes `{ let t = v; free x; x = t }`.
fn free_on_reassign(body: &mut HirBody, assign: HirId, deep: bool) {
    let HirKind::Assign { place, value } = body.expr(assign).kind.clone() else {
        return;
    };
    let HirKind::Local(local) = body.expr(place).kind else {
        return;
    };
    let span = body.expr(assign).span;
    let ty = body.expr(value).ty.clone();
    let t = temp(body, ty.clone());
    let bind = push(body, HirKind::Let { local: t, init: Some(value) }, Some(ty::unit()), span);
    // No `Clear`: `x` is set again right after.
    let local_ty = body.local(local).ty.clone();
    let old_read = push(body, HirKind::Local(local), local_ty, span);
    let free = push(body, HirKind::Builtin { op: Builtin::FreeEarly { deep }, args: vec![old_read] }, Some(ty::unit()), span);
    let read = push(body, HirKind::Local(t), ty, span);
    let inner = relocate(body, assign);
    body.exprs[inner.0 as usize].kind = HirKind::Assign { place, value: read };
    let x = &mut body.exprs[assign.0 as usize];
    x.kind = HirKind::Block { stmts: vec![bind, free, inner], tail: None };
    x.node = None;
}

/// `match` nodes on a fresh value (a call result, an inlined body) that
/// only the arms' bindings read, and that the match does not hand back:
/// its object is freed once the match is done
/// (`match step(i) { Ok(n) => n.v, … }`). Locals declared inside the
/// scrutinee are dead once it is computed.
fn match_temps(body: &HirBody, e: &mut Escape<'_, '_>, parents: &HashMap<HirId, HirId>, kept: Src) -> Vec<HirId> {
    let mut lets: HashMap<LocalId, Vec<HirId>> = HashMap::new();
    for (i, x) in body.exprs.iter().enumerate() {
        if let HirKind::Let { local, .. } = &x.kind {
            lets.entry(*local).or_default().push(HirId(i as u32));
        }
    }
    let mut out = Vec::new();
    for i in 0..body.exprs.len() {
        let m = HirId(i as u32);
        let HirKind::Match { scrutinee, arms } = &body.expr(m).kind else {
            continue;
        };
        let x = body.expr(*scrutinee);
        if matches!(x.kind, HirKind::Local(_)) || !freeable(x.ty.as_ref()) || !sized_by_value(x.ty.as_ref()) {
            continue;
        }
        let s = e.src(*scrutinee);
        if s.params != 0 || s.other || s.sites == 0 || s.sites & kept.sites != 0 {
            continue;
        }
        let mut binds = Vec::new();
        for arm in arms {
            super::pat_binds(&arm.pat, &mut binds);
        }
        if binds.iter().any(|b| body.local(*b).captured) {
            continue;
        }
        let inside = |mut at: HirId| loop {
            if at == *scrutinee {
                return true;
            }
            match parents.get(&at) {
                Some(&p) => at = p,
                None => return false,
            }
        };
        let held = (0..body.locals.len()).map(|j| LocalId(j as u32)).any(|l| {
            if binds.contains(&l) {
                return false;
            }
            let h = e.local(l);
            if e.close(h).sites & s.sites == 0 {
                return false;
            }
            body.local(l).captured || !lets.get(&l).is_some_and(|ds| ds.iter().all(|d| inside(*d)))
        });
        let value = e.src(m);
        if held || e.close(value).sites & s.sites != 0 {
            continue;
        }
        out.push(m);
    }
    out
}

/// Whether the object a value of `ty` points at has a size its value
/// decides (a string, a vector), possibly behind a pointer-niche
/// `Option`. Freeing one small fixed-size object (a class instance, an enum
/// box) costs more than the collector's sweep of it: `result_heap_churn`
/// ran 17% slower freeing each of its 10M nodes.
fn sized_by_value(ty: Option<&Ty>) -> bool {
    let Some(t) = ty else {
        return false;
    };
    match ty::strip_readonly(t) {
        Ty::Con(n) => n == "string",
        t @ Ty::App(head, args) => is_vec(Some(t)) || matches!(&**head, Ty::Con(n) if n == "Option") && sized_by_value(args.first()),
        _ => false,
    }
}

/// `match g(x) { … }` becomes `{ let t = g(x); let r = match t { … }; free t; r }`.
fn free_match_temp(body: &mut HirBody, m: HirId) {
    let HirKind::Match { scrutinee, .. } = body.expr(m).kind.clone() else {
        return;
    };
    let span = body.expr(m).span;
    let ty = body.expr(scrutinee).ty.clone();
    let t = temp(body, ty);
    let call = relocate(body, scrutinee);
    let bind = push(body, HirKind::Let { local: t, init: Some(call) }, Some(ty::unit()), span);
    let x = &mut body.exprs[scrutinee.0 as usize];
    x.kind = HirKind::Local(t);
    x.node = None;
    let match_ty = body.expr(m).ty.clone();
    let layout = body.expr(m).layout.clone();
    let inner = relocate(body, m);
    // No `Clear`: a dead word that still holds the freed address marks
    // nothing (root scans resolve it through the slab), and clearing every
    // register the value was copied to costs more than a small object.
    let free = free_stmts(body, t, false, span)[0];
    let unit = match_ty.as_ref().is_none_or(|t| *t == ty::unit());
    let (stmts, tail) = if unit {
        (vec![bind, inner, free], None)
    } else {
        let r = temp(body, match_ty.clone());
        let keep = push(body, HirKind::Let { local: r, init: Some(inner) }, Some(ty::unit()), span);
        let read = push(body, HirKind::Local(r), match_ty, span);
        (vec![bind, keep, free], Some(read))
    };
    let x = &mut body.exprs[m.0 as usize];
    x.kind = HirKind::Block { stmts, tail };
    x.node = None;
    x.layout = layout;
}

/// `(chain head, leaves)` for each string concatenation chain with a fresh
/// call result among its operands: the call results are freed once the
/// chain is built. Every other operand must be a local or a literal, so
/// computing the calls first keeps the order of effects.
fn concat_temps(body: &HirBody, e: &mut Escape<'_, '_>, parents: &HashMap<HirId, HirId>) -> Vec<(HirId, Vec<HirId>)> {
    let mut out = Vec::new();
    for i in 0..body.exprs.len() {
        let head = HirId(i as u32);
        if !matches!(body.expr(head).kind, HirKind::Bin { op: BinOp::StrConcat, .. }) {
            continue;
        }
        if parents.get(&head).is_some_and(|p| matches!(body.expr(*p).kind, HirKind::Bin { op: BinOp::StrConcat, .. })) {
            continue;
        }
        let leaves = body.concat_leaves(head);
        let mut fresh = Vec::new();
        let mut simple = true;
        for &leaf in &leaves {
            match &body.expr(leaf).kind {
                HirKind::Local(_) | HirKind::Lit(_) => {}
                HirKind::Call { callee, args } if freeable(body.expr(leaf).ty.as_ref()) && !vec_mutator(body, callee, args) => {
                    if e.callee(leaf, callee, args).is_some_and(|g| !g.shared_ret && g.returns == 0) {
                        fresh.push(leaf);
                    } else {
                        simple = false;
                    }
                }
                _ => simple = false,
            }
        }
        if simple && !fresh.is_empty() {
            out.push((head, fresh));
        }
    }
    out
}

/// `a + g(x) + b` becomes `{ let t = g(x); let r = a + t + b; free t; r }`.
fn free_concat_temps(body: &mut HirBody, head: HirId, leaves: &[HirId]) {
    let span = body.expr(head).span;
    let mut stmts = Vec::new();
    let mut temps = Vec::new();
    for &leaf in leaves {
        let ty = body.expr(leaf).ty.clone();
        let t = temp(body, ty.clone());
        let call = relocate(body, leaf);
        stmts.push(push(body, HirKind::Let { local: t, init: Some(call) }, Some(ty::unit()), span));
        let x = &mut body.exprs[leaf.0 as usize];
        x.kind = HirKind::Local(t);
        x.node = None;
        temps.push(t);
    }
    let ty = body.expr(head).ty.clone();
    let layout = body.expr(head).layout.clone();
    let chain = relocate(body, head);
    let r = temp(body, ty.clone());
    stmts.push(push(body, HirKind::Let { local: r, init: Some(chain) }, Some(ty::unit()), span));
    for t in temps {
        stmts.extend(free_stmts(body, t, false, span));
    }
    let read = push(body, HirKind::Local(r), ty, span);
    let x = &mut body.exprs[head.0 as usize];
    x.kind = HirKind::Block { stmts, tail: Some(read) };
    x.node = None;
    x.layout = layout;
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
