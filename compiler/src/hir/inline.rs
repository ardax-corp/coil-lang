//! Typed inlining (HIR phase 7): copy a small callee's HIR into its caller.
//!
//! The IL tiny-inline copies emitted instructions and so only takes leaf
//! bodies at depth zero with no locals. Here a callee's body is spliced in
//! before lowering: its locals become caller locals, its parameters become
//! `let`s (or the argument itself, when that is a literal or a local the
//! callee never rebinds), and the call becomes its result. The caller then
//! lowers the inlined code with its own context, so a receiver that is
//! only read through fields can stay in frame slots (SROA) and a callee
//! with locals inlines at any depth.
//!
//! The callee must be straight-line code plus `if` values with at most a
//! trailing `return`; see [`inlinable`]. Only call sites whose operands
//! evaluated before the call are pure locals and literals are rewritten, so
//! hoisting the callee's statements ahead of the enclosing statement keeps
//! the evaluation order. One round: calls inside inlined code stay calls.

use super::{Callee, HirArm, HirBody, HirExpr, HirFlags, HirId, HirKind, HirLocal, HirPat, HirPatFields, LocalId, LocalKind};
use super::{BinOp, BodyKind, Lit};
use crate::typechecking::ty;

/// Typed inlining is on unless `COIL_HIR_INLINE=0` (or `false` / `off` / `no`).
pub(crate) fn inline_from_env() -> bool {
    !matches!(
        std::env::var("COIL_HIR_INLINE").as_deref(),
        Ok("0" | "false" | "off" | "no")
    )
}

/// What a callee's body splices in as: its statements and its result.
#[derive(Debug, Clone)]
pub struct Shape {
    stmts: Vec<HirId>,
    result: Option<HirId>,
    /// The trailing `return` (dropped in the copy).
    ret: Option<HirId>,
    /// The result is a heap value: a temp holding it past the statement
    /// would keep it alive, so the call must be the statement's operand.
    heap_result: bool,
}

impl Shape {
    pub fn with_heap_result(mut self, heap: bool) -> Self {
        self.heap_result = heap;
        self
    }
}

/// HIR call weight in [`inlinable`]'s cost, as the IL tiny-inline counts a call.
const CALL_COST: usize = 25;

/// Whether `callee` can be inlined within `budget`, and how.
pub fn inlinable(callee: &HirBody, budget: usize) -> Result<Shape, &'static str> {
    if !matches!(callee.kind, BodyKind::Function | BodyKind::Method) {
        return Err("kind");
    }
    if callee.is_coro || callee.is_generic || callee.result_mode || !callee.captures.is_empty() {
        return Err("body");
    }
    // A fixed array, tuple or record argument is shared with the callee,
    // but a `let` of it copies.
    let value_ty = |t: &Option<ty::Ty>| matches!(t, Some(ty::Ty::Array { .. } | ty::Ty::Tuple(_) | ty::Ty::Record { .. }));
    if callee.params.iter().any(|&p| value_ty(&callee.local(p).ty)) || value_ty(&callee.ret) {
        return Err("value-param");
    }
    let root = callee.root.ok_or("no-body")?;
    let HirKind::Block { stmts, tail } = &callee.expr(root).kind else {
        return Err("root");
    };
    let mut cost = 0;
    let mut returns = 0;
    for e in &callee.exprs {
        cost += match &e.kind {
            HirKind::Lambda { .. }
            | HirKind::Defer { .. }
            | HirKind::Yield { .. }
            | HirKind::Resume { .. }
            | HirKind::Loop { .. }
            | HirKind::ForIn { .. }
            | HirKind::Match { .. }
            | HirKind::Builtin { .. }
            | HirKind::Break
            | HirKind::Continue
            | HirKind::Named { .. }
            | HirKind::Spread(_)
            | HirKind::Unsupported(_) => return Err("construct"),
            HirKind::Return(_) => {
                returns += 1;
                0
            }
            HirKind::Call { .. } => CALL_COST,
            HirKind::Block { .. } | HirKind::Lit(_) | HirKind::Local(_) => 0,
            _ => 1,
        };
    }
    if cost > budget {
        return Err("cost");
    }
    let mut stmts = stmts.clone();
    let (result, ret) = match (tail, stmts.last().map(|&s| &callee.expr(s).kind)) {
        (Some(t), _) if returns == 0 => (Some(*t), None),
        (Some(t), _) if returns == 1 && let HirKind::Return(v) = callee.expr(*t).kind => (v, Some(*t)),
        (None, Some(HirKind::Return(v))) if returns == 1 => {
            let v = *v;
            let ret = stmts.pop();
            (v, ret)
        }
        (None, _) if returns == 0 => (None, None),
        _ => return Err("return"),
    };
    Ok(Shape {
        stmts,
        result,
        ret,
        heap_result: false,
    })
}

/// `callee` with its guard returns folded into `if` values, so it has a
/// single exit [`inlinable`] can splice: `if c { ..; return a; } rest`
/// becomes `if c { ..; a } else { rest }`, and a last `if` whose branches
/// both return becomes the tail. `None` when some `return` sits anywhere
/// else (in a loop, a match, an operand) or there is nothing to fold.
pub fn single_exit(callee: &HirBody) -> Option<HirBody> {
    let root = callee.root?;
    let returns = callee.exprs.iter().filter(|e| matches!(e.kind, HirKind::Return(_))).count();
    if returns < 2 {
        return None;
    }
    let mut b = callee.clone();
    let HirKind::Block { stmts, tail } = b.expr(root).kind.clone() else {
        return None;
    };
    let mut folded = 0;
    let value = fold_exit(&mut b, &stmts, tail, &mut folded)?;
    if folded != returns {
        return None;
    }
    b.exprs[root.0 as usize].kind = HirKind::Block {
        stmts: Vec::new(),
        tail: Some(value),
    };
    b.exprs[root.0 as usize].ty = ret_ty(&b);
    Some(b)
}

/// The value of running `stmts` then `tail` up to the function's exit, as
/// one expression; `folded` counts the `return`s it absorbed.
fn fold_exit(b: &mut HirBody, stmts: &[HirId], tail: Option<HirId>, folded: &mut usize) -> Option<HirId> {
    let span = b.expr(b.root?).span;
    // A tail `if` folds like a last statement.
    if let Some(t) = tail
        && matches!(b.expr(t).kind, HirKind::If { .. })
        && has_return(b, t)
    {
        let mut all = stmts.to_vec();
        all.push(t);
        return fold_exit(b, &all, None, folded);
    }
    for (i, &s) in stmts.iter().enumerate() {
        let HirKind::If { cond, then, els } = b.expr(s).kind.clone() else {
            if has_return(b, s) {
                return None;
            }
            continue;
        };
        if has_return(b, cond) {
            return None;
        }
        let last = i + 1 == stmts.len() && tail.is_none();
        let then_exits = exits(b, then);
        let else_exits = els.is_some_and(|e| exits(b, e));
        let value = match (then_exits, els) {
            // `if c { ..; return a; }` then the rest of the block.
            (true, None) => {
                let t = fold_branch(b, then, folded)?;
                let rest = fold_exit(b, &stmts[i + 1..], tail, folded)?;
                (t, rest)
            }
            // A last `if` whose branches both return.
            (true, Some(e)) if last && else_exits => (fold_branch(b, then, folded)?, fold_branch(b, e, folded)?),
            _ if !has_return(b, s) => continue,
            _ => return None,
        };
        let ty = ret_ty(b);
        b.exprs[s.0 as usize].kind = HirKind::If {
            cond,
            then: value.0,
            els: Some(value.1),
        };
        b.exprs[s.0 as usize].ty = ty.clone();
        return Some(push_block(b, stmts[..i].to_vec(), Some(s), ty, span));
    }
    // No guard: at most a trailing `return`.
    let (stmts, value) = match (tail, stmts.last().map(|&s| b.expr(s).kind.clone())) {
        (Some(t), _) => match b.expr(t).kind.clone() {
            HirKind::Return(v) => {
                *folded += 1;
                b.exprs[t.0 as usize].kind = HirKind::Lit(Lit::Unit);
                b.exprs[t.0 as usize].ty = Some(ty::unit());
                (stmts.to_vec(), v)
            }
            _ if has_return(b, t) => return None,
            _ => (stmts.to_vec(), Some(t)),
        },
        (None, Some(HirKind::Return(v))) => {
            let r = *stmts.last().expect("a last statement");
            *folded += 1;
            b.exprs[r.0 as usize].kind = HirKind::Lit(Lit::Unit);
            b.exprs[r.0 as usize].ty = Some(ty::unit());
            (stmts[..stmts.len() - 1].to_vec(), v)
        }
        (None, _) => (stmts.to_vec(), None),
    };
    let ty = ret_ty(b);
    let value = value.unwrap_or_else(|| push_expr(b, HirKind::Lit(Lit::Unit), Some(ty::unit()), span));
    Some(push_block(b, stmts, Some(value), ty, span))
}

/// The type the folded exits produce.
fn ret_ty(b: &HirBody) -> Option<ty::Ty> {
    b.ret.clone().or_else(|| Some(ty::unit()))
}

/// A branch block that ends in `return`, as a block that ends in its value.
fn fold_branch(b: &mut HirBody, branch: HirId, folded: &mut usize) -> Option<HirId> {
    let HirKind::Block { stmts, tail } = b.expr(branch).kind.clone() else {
        return None;
    };
    fold_exit(b, &stmts, tail, folded)
}

/// `branch` is a block whose last step is a `return`.
fn exits(b: &HirBody, branch: HirId) -> bool {
    let HirKind::Block { stmts, tail } = &b.expr(branch).kind else {
        return false;
    };
    let last = tail.or_else(|| stmts.last().copied());
    last.is_some_and(|l| match &b.expr(l).kind {
        HirKind::Return(_) => true,
        HirKind::If { then, els: Some(e), .. } => exits(b, *then) && exits(b, *e),
        _ => false,
    })
}

/// Some `return` under `e`.
fn has_return(b: &HirBody, e: HirId) -> bool {
    let mut found = false;
    super::lower::visit(b, e, &mut |x| found |= matches!(x.kind, HirKind::Return(_)));
    found
}

fn push_block(b: &mut HirBody, stmts: Vec<HirId>, tail: Option<HirId>, ty: Option<ty::Ty>, span: super::Span) -> HirId {
    push_expr(b, HirKind::Block { stmts, tail }, ty, span)
}

fn push_expr(b: &mut HirBody, kind: HirKind, ty: Option<ty::Ty>, span: super::Span) -> HirId {
    let id = HirId(b.exprs.len() as u32);
    b.exprs.push(HirExpr {
        kind,
        ty,
        layout: super::layout::Layout::Word,
        span,
        node: None,
        flags: HirFlags::default(),
    });
    id
}

/// Rewrite every eligible call site of `caller`. `callee_of(call)` names
/// the callee body and its [`Shape`] for a direct call the planner may
/// inline; `growth` caps how many nodes the rewrite may add. Returns the
/// body and how many sites it inlined; `None` when nothing was.
pub fn inline_calls<'a>(
    caller: &HirBody,
    callee_of: impl Fn(HirId) -> Option<(&'a HirBody, Shape)>,
    growth: usize,
) -> Option<(HirBody, usize)> {
    // Defer thunks are planned against the caller's own statements.
    if caller.exprs.iter().any(|e| matches!(e.kind, HirKind::Defer { .. })) {
        return None;
    }
    let mut b = Inliner {
        body: caller.clone(),
        original: caller.exprs.len(),
        sites: 0,
    };
    for i in 0..b.original {
        if b.body.exprs.len() - b.original > growth {
            break;
        }
        let HirKind::Block { stmts, tail } = b.body.exprs[i].kind.clone() else {
            continue;
        };
        let mut out = Vec::with_capacity(stmts.len());
        for s in stmts {
            let s = b.statement(s, &callee_of, growth, &mut out);
            if let Some(s) = s {
                out.push(s);
            }
        }
        // A tail's calls hoist into the statements; the tail itself stays.
        let tail = tail.and_then(|t| b.statement(t, &callee_of, growth, &mut out));
        b.body.exprs[i].kind = HirKind::Block { stmts: out, tail };
    }
    (b.sites > 0).then_some((b.body, b.sites))
}

struct Inliner {
    body: HirBody,
    /// Nodes before the rewrite; only these call sites are inlined.
    original: usize,
    sites: usize,
}

/// Where the inlined call sits in its statement.
#[derive(Clone, Copy, PartialEq)]
enum Site {
    /// The statement is the call.
    Stmt,
    /// `let x = call` / `return call`: the result replaces the call operand.
    Direct,
    /// Deeper: the result goes through a temp.
    Nested,
}

impl Inliner {
    /// Inline the sites of statement `s`, pushing hoisted statements to
    /// `out`. Returns the statement to keep (`None` when it was the call
    /// and the callee has no result).
    fn statement<'a>(
        &mut self,
        mut s: HirId,
        callee_of: &impl Fn(HirId) -> Option<(&'a HirBody, Shape)>,
        growth: usize,
        out: &mut Vec<HirId>,
    ) -> Option<HirId> {
        while self.body.exprs.len() - self.original <= growth {
            let Some((call, callee, shape)) = self.site_in(s, callee_of) else {
                break;
            };
            let site = if call == s {
                Site::Stmt
            } else {
                match &self.body.expr(s).kind {
                    HirKind::Let { init: Some(i), .. } if *i == call => Site::Direct,
                    HirKind::Return(Some(v)) if *v == call => Site::Direct,
                    _ => Site::Nested,
                }
            };
            let result = self.splice(call, callee, &shape, site, out);
            match site {
                // The statement is gone; its result (if any) is the new one.
                Site::Stmt => s = result?,
                Site::Direct => {
                    let span = self.body.expr(call).span;
                    let value = result.unwrap_or_else(|| self.push(HirKind::Lit(Lit::Unit), Some(ty::unit()), span));
                    match &mut self.body.exprs[s.0 as usize].kind {
                        HirKind::Let { init: Some(i), .. } => *i = value,
                        HirKind::Return(Some(v)) => *v = value,
                        _ => unreachable!(),
                    }
                    self.tombstone(call);
                }
                Site::Nested => {}
            }
        }
        Some(s)
    }

    /// The first call in statement `s`, in evaluation order, that can be
    /// hoisted: every operand evaluated before it is pure.
    fn site_in<'a>(
        &self,
        s: HirId,
        callee_of: &impl Fn(HirId) -> Option<(&'a HirBody, Shape)>,
    ) -> Option<(HirId, &'a HirBody, Shape)> {
        let b = &self.body;
        let root = match &b.expr(s).kind {
            HirKind::Let { init: Some(i), .. } => *i,
            HirKind::Return(Some(v)) => *v,
            HirKind::Assign { place, value } => {
                // The place is computed after the value only when it has
                // no effects of its own; a compound place is also read.
                let compound = b.expr(s).flags.contains(HirFlags::COMPOUND);
                match &b.expr(*place).kind {
                    HirKind::Local(_) => *value,
                    HirKind::Field { base, .. }
                        if !compound && matches!(b.expr(*base).kind, HirKind::Local(_)) =>
                    {
                        *value
                    }
                    _ => return None,
                }
            }
            _ => s,
        };
        let mut found = None;
        let _ = self.search(root, callee_of, &mut found);
        found.filter(|(call, _, shape)| *call == root || !shape.heap_result)
    }

    /// Walk `e` in evaluation order. `Err` stops at an operand that is not
    /// pure, since a later call cannot move ahead of it.
    fn search<'a>(
        &self,
        e: HirId,
        callee_of: &impl Fn(HirId) -> Option<(&'a HirBody, Shape)>,
        found: &mut Option<(HirId, &'a HirBody, Shape)>,
    ) -> Result<(), ()> {
        let b = &self.body;
        let seq = |ids: &[HirId], found: &mut Option<_>| -> Result<(), ()> {
            for &a in ids {
                self.search(a, callee_of, found)?;
                if found.is_some() {
                    return Ok(());
                }
                if !self.pure(a) {
                    return Err(());
                }
            }
            Ok(())
        };
        match &b.expr(e).kind {
            HirKind::Lit(_) | HirKind::Local(_) => Ok(()),
            HirKind::Call {
                callee: Callee::Named { .. } | Callee::Method { .. },
                args,
            } => {
                // A call's own arguments are bound in order ahead of the
                // spliced body, so they need not be pure; only a nested
                // call past an impure argument cannot move.
                for &a in args {
                    let nested = self.search(a, callee_of, found);
                    if found.is_some() {
                        return Ok(());
                    }
                    if nested.is_err() || !self.pure(a) {
                        break;
                    }
                }
                if (e.0 as usize) < self.original
                    && let Some((callee, shape)) = callee_of(e)
                    && callee.params.len() == args.len()
                {
                    *found = Some((e, callee, shape));
                    return Ok(());
                }
                Err(())
            }
            HirKind::Bin { lhs, rhs, .. } => seq(&[*lhs, *rhs], found),
            HirKind::Index { base, index, .. } => seq(&[*base, *index], found),
            HirKind::Make { args, .. } => seq(args, found),
            HirKind::Un { operand: x, .. } | HirKind::Cast { value: x } | HirKind::Field { base: x, .. } => {
                self.search(*x, callee_of, found)
            }
            _ if self.pure(e) => Ok(()),
            _ => Err(()),
        }
    }

    /// Reads only locals and literals, with no trap and no effect.
    fn pure(&self, e: HirId) -> bool {
        match &self.body.expr(e).kind {
            HirKind::Lit(_) | HirKind::Local(_) => true,
            HirKind::Bin { op, lhs, rhs } => {
                !matches!(
                    op,
                    BinOp::IntDiv | BinOp::IntRem | BinOp::IntPow | BinOp::Overloaded(_)
                ) && self.pure(*lhs)
                    && self.pure(*rhs)
            }
            HirKind::Un { operand, .. } => self.pure(*operand),
            _ => false,
        }
    }

    /// Splice `callee` in for `call`: hoisted statements go to `out`. For
    /// [`Site::Stmt`] and [`Site::Direct`] returns the result node, which
    /// the caller puts in place of the statement or the operand.
    fn splice(&mut self, call: HirId, callee: &HirBody, shape: &Shape, site: Site, out: &mut Vec<HirId>) -> Option<HirId> {
        self.sites += 1;
        let tag = self.sites;
        let off = self.body.exprs.len() as u32;
        let loff = self.body.locals.len() as u32;
        let span = self.body.expr(call).span;
        let HirKind::Call { args, .. } = self.body.expr(call).kind.clone() else {
            unreachable!()
        };
        // A literal or local argument stands in for its parameter when the
        // callee never rebinds that parameter.
        let subst: Vec<Option<HirKind>> = callee
            .params
            .iter()
            .zip(&args)
            .map(|(&p, &a)| {
                let arg = &self.body.expr(a).kind;
                let substitutable = matches!(arg, HirKind::Lit(_) | HirKind::Local(_));
                (substitutable && !rebinds(callee, p)).then(|| arg.clone())
            })
            .collect();
        for l in &callee.locals {
            self.body.locals.push(HirLocal {
                name: format!("__inl{tag}_{}", l.name),
                ty: l.ty.clone(),
                kind: if l.kind == LocalKind::Param { LocalKind::Let } else { l.kind },
            });
        }
        let param_of = |l: LocalId| callee.params.iter().position(|&p| p == l);
        for e in &callee.exprs {
            let kind = match &e.kind {
                HirKind::Local(l) => match param_of(*l).and_then(|k| subst[k].clone()) {
                    Some(arg) => arg,
                    None => HirKind::Local(LocalId(l.0 + loff)),
                },
                k => remap(k, off, loff),
            };
            self.body.exprs.push(HirExpr { kind, ..e.clone() });
        }
        let mut hoisted = Vec::new();
        for (k, (&p, &a)) in callee.params.iter().zip(&args).enumerate() {
            if subst[k].is_some() {
                self.tombstone(a);
            } else {
                let local = LocalId(p.0 + loff);
                hoisted.push(self.push(HirKind::Let { local, init: Some(a) }, Some(ty::unit()), span));
            }
        }
        hoisted.extend(shape.stmts.iter().map(|s| HirId(s.0 + off)));
        if let Some(root) = callee.root {
            self.tombstone(HirId(root.0 + off));
        }
        if let Some(r) = shape.ret {
            self.tombstone(HirId(r.0 + off));
        }
        let result = shape.result.map(|r| HirId(r.0 + off));
        out.extend(hoisted);
        match site {
            Site::Stmt => {
                self.tombstone(call);
                result
            }
            Site::Direct => result,
            Site::Nested => {
                match result {
                    Some(r) => {
                        let ty = self.body.expr(call).ty.clone();
                        let local = LocalId(self.body.locals.len() as u32);
                        self.body.locals.push(HirLocal {
                            name: format!("__inl{tag}_ret"),
                            ty,
                            kind: LocalKind::Temp,
                        });
                        out.push(self.push(HirKind::Let { local, init: Some(r) }, Some(ty::unit()), span));
                        self.body.exprs[call.0 as usize].kind = HirKind::Local(local);
                    }
                    None => self.body.exprs[call.0 as usize].kind = HirKind::Lit(Lit::Unit),
                }
                None
            }
        }
    }

    /// An orphaned node: whole-arena scans (field-only bases, stack arrays,
    /// assigned locals) must not see what it used to read.
    fn tombstone(&mut self, id: HirId) {
        let e = &mut self.body.exprs[id.0 as usize];
        e.kind = HirKind::Lit(Lit::Unit);
        e.ty = Some(ty::unit());
        e.layout = super::layout::Layout::Word;
        e.node = None;
    }

    fn push(&mut self, kind: HirKind, ty: Option<crate::typechecking::ty::Ty>, span: super::Span) -> HirId {
        let id = HirId(self.body.exprs.len() as u32);
        self.body.exprs.push(HirExpr {
            kind,
            ty,
            layout: super::layout::Layout::Word,
            span,
            node: None,
            flags: HirFlags::default(),
        });
        id
    }
}

/// Whether `callee` assigns parameter `p` or a place rooted at it by index.
pub fn rebinds(callee: &HirBody, p: LocalId) -> bool {
    let rooted = |mut place: HirId| loop {
        match &callee.expr(place).kind {
            HirKind::Local(l) => return *l == p,
            HirKind::Index { base, .. } => place = *base,
            _ => return false,
        }
    };
    callee.exprs.iter().any(|e| match &e.kind {
        HirKind::Assign { place, .. } | HirKind::Append { base: place, .. } => rooted(*place),
        _ => false,
    })
}

/// `kind` with every child id shifted by `off` and local by `loff`.
fn remap(kind: &HirKind, off: u32, loff: u32) -> HirKind {
    let h = |id: &HirId| HirId(id.0 + off);
    let l = |id: &LocalId| LocalId(id.0 + loff);
    let hs = |ids: &[HirId]| ids.iter().map(h).collect::<Vec<_>>();
    match kind {
        HirKind::Lit(_) | HirKind::Global { .. } | HirKind::Break | HirKind::Continue | HirKind::Unsupported(_) => {
            kind.clone()
        }
        HirKind::Local(x) => HirKind::Local(l(x)),
        HirKind::Field { base, name } => HirKind::Field {
            base: h(base),
            name: name.clone(),
        },
        HirKind::Index { base, index, kind } => HirKind::Index {
            base: h(base),
            index: h(index),
            kind: *kind,
        },
        HirKind::Bin { op, lhs, rhs } => HirKind::Bin {
            op: *op,
            lhs: h(lhs),
            rhs: h(rhs),
        },
        HirKind::Logic { and, lhs, rhs } => HirKind::Logic {
            and: *and,
            lhs: h(lhs),
            rhs: h(rhs),
        },
        HirKind::Un { op, operand } => HirKind::Un {
            op: *op,
            operand: h(operand),
        },
        HirKind::Cast { value } => HirKind::Cast { value: h(value) },
        HirKind::Call { callee, args } => HirKind::Call {
            callee: match callee {
                Callee::Value(v) => Callee::Value(h(v)),
                c => c.clone(),
            },
            args: hs(args),
        },
        HirKind::Named { name, value } => HirKind::Named {
            name: name.clone(),
            value: h(value),
        },
        HirKind::Spread(v) => HirKind::Spread(h(v)),
        HirKind::Make { kind, args } => HirKind::Make {
            kind: kind.clone(),
            args: hs(args),
        },
        HirKind::Block { stmts, tail } => HirKind::Block {
            stmts: hs(stmts),
            tail: tail.as_ref().map(h),
        },
        HirKind::Let { local, init } => HirKind::Let {
            local: l(local),
            init: init.as_ref().map(h),
        },
        HirKind::LetPat { pat, init } => HirKind::LetPat {
            pat: remap_pat(pat, loff),
            init: h(init),
        },
        HirKind::Assign { place, value } => HirKind::Assign {
            place: h(place),
            value: h(value),
        },
        HirKind::Append { base, value } => HirKind::Append {
            base: h(base),
            value: h(value),
        },
        HirKind::If { cond, then, els } => HirKind::If {
            cond: h(cond),
            then: h(then),
            els: els.as_ref().map(h),
        },
        HirKind::Loop { body } => HirKind::Loop { body: h(body) },
        HirKind::ForIn {
            pat,
            iterable,
            body,
            kind,
        } => HirKind::ForIn {
            pat: remap_pat(pat, loff),
            iterable: h(iterable),
            body: h(body),
            kind: kind.clone(),
        },
        HirKind::Return(v) => HirKind::Return(v.as_ref().map(h)),
        HirKind::Match { scrutinee, arms } => HirKind::Match {
            scrutinee: h(scrutinee),
            arms: arms
                .iter()
                .map(|a| HirArm {
                    pat: remap_pat(&a.pat, loff),
                    body: h(&a.body),
                })
                .collect(),
        },
        HirKind::Lambda { body } => HirKind::Lambda { body: *body },
        HirKind::Yield { value, from } => HirKind::Yield {
            value: h(value),
            from: *from,
        },
        HirKind::Resume { handle, value } => HirKind::Resume {
            handle: h(handle),
            value: value.as_ref().map(h),
        },
        HirKind::Defer { captures, body } => HirKind::Defer {
            captures: captures.iter().map(|c| c.as_ref().map(l)).collect(),
            body: h(body),
        },
        HirKind::Builtin { op, args } => HirKind::Builtin { op: *op, args: hs(args) },
    }
}

fn remap_pat(pat: &HirPat, loff: u32) -> HirPat {
    let fields = |fs: &[(String, HirPat)]| fs.iter().map(|(n, p)| (n.clone(), remap_pat(p, loff))).collect();
    match pat {
        HirPat::Wild | HirPat::Int(_) => pat.clone(),
        HirPat::Bind(l) => HirPat::Bind(LocalId(l.0 + loff)),
        HirPat::Variant {
            enum_name,
            variant,
            tag,
            fields: f,
        } => HirPat::Variant {
            enum_name: enum_name.clone(),
            variant: variant.clone(),
            tag: *tag,
            fields: match f {
                HirPatFields::Unit => HirPatFields::Unit,
                HirPatFields::Tuple(ps) => HirPatFields::Tuple(ps.iter().map(|p| remap_pat(p, loff)).collect()),
                HirPatFields::Record(fs) => HirPatFields::Record(fields(fs)),
            },
        },
        HirPat::Tuple(ps) => HirPat::Tuple(ps.iter().map(|p| remap_pat(p, loff)).collect()),
        HirPat::Record(fs) => HirPat::Record(fields(fs)),
    }
}
