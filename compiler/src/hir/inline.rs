//! Typed inlining (HIR phase 7): copy a small callee's HIR into its caller.
//!
//! A callee's body is spliced in before lowering: its locals become caller locals, its parameters become
//! `let`s (or the argument itself, when that is a literal or a local the
//! callee never rebinds), and the call becomes its result. The caller then
//! lowers the inlined code with its own context, so a receiver that is
//! only read through fields can stay in frame slots (SROA) and a callee
//! with locals inlines at any depth.
//!
//! The callee must be straight-line code plus `if` values with at most a
//! trailing `return`; see [`inlinable`]. A call site whose operands
//! evaluated before the call are pure locals and literals hoists the
//! callee's statements ahead of the enclosing statement, which keeps the
//! evaluation order; a call evaluated with no operand below it otherwise
//! runs in place as a block value. Calls inside inlined code inline in the
//! next round (`Compiler::hir_inline_replan`).

use super::{Callee, HirArm, HirBody, HirExpr, HirFlags, HirId, HirKind, HirLocal, HirPat, HirPatFields, LocalId, LocalKind};
use super::{BinOp, BodyKind, Lit};
use crate::typechecking::ty;

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
    /// Callee locals holding heap values: the caller's frame would keep
    /// them alive past the call, so they are cleared after the statement.
    heap_locals: Vec<LocalId>,
    /// The callee is another module's: its nodes take the call's span.
    foreign: bool,
    /// In a loop, `heap_locals` keep their value until the next trip
    /// rebinds them, as the loop's own locals do (nothing observes when
    /// those values die), and are not cleared.
    loop_inert: bool,
    /// The callee has a loop: it inlines only at a call inside a loop,
    /// where the call is paid per trip. Elsewhere the call runs once and
    /// the loop keeps its own body (and tier).
    looping: bool,
}

impl Shape {
    /// The callee has a loop.
    pub fn looping(&self) -> bool {
        self.looping
    }

    pub fn with_foreign(mut self, foreign: bool) -> Self {
        self.foreign = foreign;
        self
    }

    pub fn with_heap_result(mut self, heap: bool) -> Self {
        self.heap_result = heap;
        self
    }

    pub fn with_heap_locals(mut self, locals: Vec<LocalId>) -> Self {
        self.heap_locals = locals;
        self
    }

    pub fn with_loop_inert(mut self, inert: bool) -> Self {
        self.loop_inert = inert;
        self
    }
}

/// HIR call weight in [`inlinable`]'s cost.
const CALL_COST: usize = 25;

/// HIR loop weight in [`inlinable`]'s cost.
const LOOP_COST: usize = 4;

/// Whether `callee` can be inlined within `budget`, and how.
pub fn inlinable(callee: &HirBody, budget: usize) -> Result<Shape, &'static str> {
    if !matches!(callee.kind, BodyKind::Function | BodyKind::Method | BodyKind::Lambda) {
        return Err("kind");
    }
    // A Result-mode body's returns are explicit `Ok` / `Err` values.
    if callee.is_coro || callee.is_generic || !callee.captures.is_empty() {
        return Err("body");
    }
    // A fixed array, tuple or record argument is shared with the callee,
    // but a `let` of it copies: only a parameter the callee just reads from
    // can be bound by a `let`.
    if callee.params.iter().any(|&p| value_ty(&callee.local(p).ty) && !reads_only(callee, p)) || value_ty(&callee.ret) {
        return Err("value-param");
    }
    let root = callee.root.ok_or("no-body")?;
    let HirKind::Block { stmts, tail } = &callee.expr(root).kind else {
        return Err("root");
    };
    let mut cost = 0;
    let mut returns = 0;
    let mut looping = false;
    for e in &callee.exprs {
        looping |= matches!(e.kind, HirKind::Loop { .. } | HirKind::ForIn { .. });
        cost += match &e.kind {
            HirKind::Lambda { .. }
            | HirKind::Defer { .. }
            | HirKind::Yield { .. }
            | HirKind::Resume { .. }
            | HirKind::Builtin { .. }
            | HirKind::Named { .. }
            | HirKind::Spread(_)
            | HirKind::Unsupported(_) => return Err("construct"),
            HirKind::Return(_) => {
                returns += 1;
                0
            }
            HirKind::Call { .. } => CALL_COST,
            // A loop's body splices whole (its `break` / `continue` still
            // name the callee's loop); the loop itself costs its test.
            HirKind::Loop { .. } | HirKind::ForIn { .. } => LOOP_COST,
            HirKind::Break | HirKind::Continue => 0,
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
        heap_locals: Vec::new(),
        foreign: false,
        looping,
        loop_inert: false,
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
    match b.expr(branch).kind.clone() {
        HirKind::Block { stmts, tail } => fold_exit(b, &stmts, tail, folded),
        // `else if`: the branch is the `if` itself.
        HirKind::If { .. } => fold_exit(b, &[branch], None, folded),
        _ => None,
    }
}

/// `branch` (a block, or an `else if`) ends in a `return` on every path.
fn exits(b: &HirBody, branch: HirId) -> bool {
    let last = match &b.expr(branch).kind {
        HirKind::Block { stmts, tail } => tail.or_else(|| stmts.last().copied()),
        HirKind::If { .. } => Some(branch),
        _ => return false,
    };
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
        layout: crate::layout::Layout::Word,
        span,
        node: None,
        flags: HirFlags::default(),
    });
    id
}

/// Rewrite every eligible call site of `caller`. `callee_of(call)` names
/// the callee body and its [`Shape`] for a direct call the planner may
/// inline; `growth` caps how many nodes the rewrite may add. Returns the
/// body and the name and span of the callee at each site it inlined; `None`
/// when nothing was.
pub fn inline_calls<'a>(
    caller: &HirBody,
    callee_of: impl Fn(HirId) -> Option<(&'a HirBody, Shape)>,
    growth: usize,
    in_place: bool,
    opaque: impl Fn(HirId) -> bool,
) -> Option<(HirBody, Vec<(String, super::Span)>)> {
    // Defer thunks are planned against the caller's own statements.
    if caller.exprs.iter().any(|e| matches!(e.kind, HirKind::Defer { .. })) {
        return None;
    }
    let mut b = Inliner {
        body: caller.clone(),
        original: caller.exprs.len(),
        sites: 0,
        spliced: Vec::new(),
        clear: Vec::new(),
        inert: Vec::new(),
        in_place,
        opaque: &opaque,
    };
    // Only the blocks the body still reaches: a block an earlier round left
    // behind (`single_exit` rebuilds the blocks it folds) can share a
    // statement with a live one, and a temp's `let` hoisted into it would
    // never run.
    let mut live = vec![false; b.original];
    // Nodes in a loop's body: a spliced heap local there is rebound on
    // the next trip, as the loop's own locals are, and is not cleared.
    let mut looped = vec![false; b.original];
    let mut todo: Vec<(HirId, bool)> = caller.root.into_iter().map(|r| (r, false)).collect();
    while let Some((id, in_loop)) = todo.pop() {
        if !std::mem::replace(&mut live[id.0 as usize], true) {
            looped[id.0 as usize] = in_loop;
            let inner = in_loop || matches!(caller.expr(id).kind, HirKind::Loop { .. } | HirKind::ForIn { .. });
            todo.extend(super::lower::children(caller, id).into_iter().map(|c| (c, inner)));
        }
    }
    for (i, live) in live.into_iter().enumerate() {
        if b.body.exprs.len() - b.original > growth {
            break;
        }
        if !live {
            continue;
        }
        let HirKind::Block { stmts, tail } = b.body.exprs[i].kind.clone() else {
            continue;
        };
        let mut out = Vec::with_capacity(stmts.len());
        let here = looped[i];
        let callee_here = |id: HirId| callee_of(id).filter(|(_, shape)| here || !shape.looping);
        for s in stmts {
            let s = b.statement(s, &callee_here, growth, &mut out);
            if let Some(s) = s {
                out.push(s);
            }
            // The spliced heap locals die with the statement that read them,
            // as they did when the callee returned.
            let mut clear = std::mem::take(&mut b.clear);
            let inert = std::mem::take(&mut b.inert);
            if !looped[i] {
                clear.extend(inert);
            }
            let returns = s.is_some_and(|s| matches!(b.body.expr(s).kind, HirKind::Return(_)));
            if !clear.is_empty() && !returns {
                let span = b.body.exprs[i].span;
                out.push(b.push(HirKind::Clear(clear), Some(ty::unit()), span));
            }
        }
        // A tail's calls hoist into the statements; the tail itself stays,
        // and its spliced locals live to the end of the frame.
        let tail = tail.and_then(|t| b.statement(t, &callee_here, growth, &mut out));
        b.clear.clear();
        b.inert.clear();
        b.body.exprs[i].kind = HirKind::Block { stmts: out, tail };
    }
    (!b.spliced.is_empty()).then_some((b.body, b.spliced))
}

struct Inliner<'o> {
    body: HirBody,
    /// Nodes before the rewrite; only these call sites are inlined.
    original: usize,
    sites: usize,
    /// Each inlined callee's name and span.
    spliced: Vec<(String, super::Span)>,
    /// Heap locals spliced into the current statement.
    clear: Vec<LocalId>,
    /// Those of [`Shape::loop_inert`] callees: cleared only outside loops.
    inert: Vec<LocalId>,
    /// Splice calls that cannot hoist where they stand, as a block value
    /// (see [`Inliner::in_place_site`]).
    in_place: bool,
    /// A call whose arguments do not run as written (a literal's `len`):
    /// nothing in them moves.
    opaque: &'o dyn Fn(HirId) -> bool,
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

impl Inliner<'_> {
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
            let Some((call, callee, shape, spills)) = self.site_in(s, callee_of) else {
                match self.in_place.then(|| self.in_place_site(s, callee_of, true)).flatten() {
                    Some((call, callee, shape)) => {
                        self.splice_in_place(call, callee, &shape);
                        continue;
                    }
                    None => break,
                }
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
            for a in spills {
                out.push(self.spill(a));
            }
            let mut hoisted = Vec::new();
            let result = self.splice(call, callee, &shape, site, &mut hoisted);
            // A parameter's `let` holds the call's argument, whose own calls
            // inline like any statement's.
            for h in hoisted {
                if let Some(h) = self.statement(h, callee_of, growth, out) {
                    out.push(h);
                }
            }
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
    /// hoisted: every operand evaluated before it is pure, or may only trap
    /// and goes into a temp first (the returned operands, in order).
    fn site_in<'a>(
        &self,
        s: HirId,
        callee_of: &impl Fn(HirId) -> Option<(&'a HirBody, Shape)>,
    ) -> Option<(HirId, &'a HirBody, Shape, Vec<HirId>)> {
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
        let mut spills = Vec::new();
        let _ = self.search(root, callee_of, &mut found, &mut spills);
        found
            .filter(|(call, _, shape)| *call == root || !shape.heap_result)
            .map(|(call, callee, shape)| (call, callee, shape, spills))
    }

    /// Walk `e` in evaluation order. `Err` stops at an operand that is not
    /// pure, since a later call cannot move ahead of it. An operand that
    /// only may trap (int `+`, ...) goes to `spills`: evaluated into a temp
    /// first, it still traps before the call runs (coil-lang#856).
    fn search<'a>(
        &self,
        e: HirId,
        callee_of: &impl Fn(HirId) -> Option<(&'a HirBody, Shape)>,
        found: &mut Option<(HirId, &'a HirBody, Shape)>,
        spills: &mut Vec<HirId>,
    ) -> Result<(), ()> {
        let b = &self.body;
        let seq = |ids: &[HirId], found: &mut Option<_>, spills: &mut Vec<HirId>| -> Result<(), ()> {
            for &a in ids {
                self.search(a, callee_of, found, spills)?;
                if found.is_some() {
                    return Ok(());
                }
                if !self.pure(a) {
                    if !self.spillable(a) {
                        return Err(());
                    }
                    spills.push(a);
                }
            }
            Ok(())
        };
        match &b.expr(e).kind {
            HirKind::Lit(_) | HirKind::Local(_) => Ok(()),
            HirKind::Call { .. } if (self.opaque)(e) => Err(()),
            HirKind::Call { callee, args }
                if matches!(callee, Callee::Named { .. } | Callee::Method { .. })
                    || matches!(callee, Callee::Value(v) if matches!(b.expr(*v).kind, HirKind::Local(_))) =>
            {
                // A call's own arguments are bound in order ahead of the
                // spliced body, so they need not be pure; only a nested
                // call past an impure argument cannot move.
                let mark = spills.len();
                for &a in args {
                    let before = spills.len();
                    let nested = self.search(a, callee_of, found, spills);
                    if found.is_some() {
                        return Ok(());
                    }
                    if nested.is_err() {
                        spills.truncate(before);
                        break;
                    }
                    if !self.pure(a) {
                        if !self.spillable(a) {
                            break;
                        }
                        spills.push(a);
                    }
                }
                // The call's own arguments bind ahead of its body anyway.
                spills.truncate(mark);
                if (e.0 as usize) < self.original
                    && let Some((callee, shape)) = callee_of(e)
                    && callee.params.len() == args.len()
                {
                    *found = Some((e, callee, shape));
                    return Ok(());
                }
                Err(())
            }
            HirKind::Bin { lhs, rhs, .. } => seq(&[*lhs, *rhs], found, spills),
            HirKind::Index { base, index, .. } => seq(&[*base, *index], found, spills),
            HirKind::Make { args, .. } => seq(args, found, spills),
            HirKind::Un { operand: x, .. } | HirKind::Cast { value: x } | HirKind::Field { base: x, .. } => {
                self.search(*x, callee_of, found, spills)
            }
            // The scrutinee or condition runs first; the arms and branches
            // are not pure, which the enclosing operand checks.
            HirKind::Match { scrutinee: x, .. } | HirKind::If { cond: x, .. } => {
                self.search(*x, callee_of, found, spills)
            }
            _ if self.pure(e) => Ok(()),
            _ => Err(()),
        }
    }

    /// The first call evaluated with no operand below it in statement `s`
    /// (an operand the hoisting in [`Inliner::site_in`] cannot move ahead
    /// of: past an effect, under `&&` / `||`, ...), whose callee then runs
    /// in place as a block value: its parameters' `let`s and statements,
    /// then its result. Those `let`s need the empty operand stack, so above
    /// live operands (`zero` false) only an [`Inliner::expression_only`]
    /// callee splices, as its result expression.
    fn in_place_site<'a>(
        &self,
        s: HirId,
        callee_of: &impl Fn(HirId) -> Option<(&'a HirBody, Shape)>,
        zero: bool,
    ) -> Option<(HirId, &'a HirBody, Shape)> {
        let b = &self.body;
        let scalar = |e: HirId| b.expr(e).ty.as_ref().and_then(super::lower::primitive).is_some();
        let at = |x: HirId, zero: bool| self.in_place_site(x, callee_of, zero);
        match &b.expr(s).kind {
            HirKind::Call { callee, args }
                if (matches!(callee, Callee::Named { .. } | Callee::Method { .. })
                    || matches!(callee, Callee::Value(v) if matches!(b.expr(*v).kind, HirKind::Local(_))))
                    && (s.0 as usize) < self.original
                && let Some((callee, shape)) = callee_of(s)
                && callee.params.len() == args.len()
                && (zero || self.expression_only(callee, &shape, args)) =>
            {
                Some((s, callee, shape))
            }
            HirKind::Let { init: Some(x), .. } | HirKind::Return(Some(x)) => at(*x, zero),
            HirKind::Assign { place, value } if matches!(b.expr(*place).kind, HirKind::Local(_)) => at(*value, zero),
            HirKind::Logic { lhs, rhs, .. } => at(*lhs, zero).or_else(|| at(*rhs, zero)),
            HirKind::Un { operand: x, .. } | HirKind::Cast { value: x } if scalar(s) && scalar(*x) => at(*x, zero),
            HirKind::Bin { op, lhs, rhs }
                if !matches!(op, BinOp::Overloaded(_) | BinOp::StrConcat) && scalar(*lhs) && scalar(*rhs) =>
            {
                at(*lhs, zero).or_else(|| at(*rhs, false))
            }
            HirKind::If { cond: x, .. } | HirKind::Match { scrutinee: x, .. } if zero => {
                at(*x, true).or_else(|| self.operands(s, callee_of))
            }
            // A block spliced in place: its statements run in turn.
            HirKind::Block { stmts, tail } if (s.0 as usize) >= self.original => {
                stmts.iter().chain(tail).find_map(|&x| at(x, zero))
            }
            HirKind::Lambda { .. } | HirKind::Defer { .. } => None,
            // An original block's statements are inlined as its own.
            HirKind::Block { .. } | HirKind::Loop { .. } => None,
            _ => self.operands(s, callee_of),
        }
    }

    /// [`Inliner::in_place_site`] in the operands of `e`, above live ones.
    fn operands<'a>(
        &self,
        e: HirId,
        callee_of: &impl Fn(HirId) -> Option<(&'a HirBody, Shape)>,
    ) -> Option<(HirId, &'a HirBody, Shape)> {
        super::lower::children(&self.body, e)
            .into_iter()
            .find_map(|x| self.in_place_site(x, callee_of, false))
    }

    /// `callee` splices as its result expression alone: no statements, and
    /// every argument stands in for its parameter. That runs above any
    /// operands, as the call did.
    fn expression_only(&self, callee: &HirBody, shape: &Shape, args: &[HirId]) -> bool {
        shape.stmts.is_empty()
            && shape.result.is_some()
            && shape.heap_locals.iter().all(|l| callee.params.contains(l))
            && callee.params.iter().zip(args).all(|(&p, &a)| {
                matches!(self.body.expr(a).kind, HirKind::Lit(_) | HirKind::Local(_)) && !rebinds(callee, p)
            })
    }

    /// Splice `callee` in for `call` as a block value in the call's place:
    /// see [`Inliner::in_place_site`].
    fn splice_in_place(&mut self, call: HirId, callee: &HirBody, shape: &Shape) {
        let span = self.body.expr(call).span;
        let mut stmts = Vec::new();
        let result = self.splice(call, callee, shape, Site::Direct, &mut stmts);
        let tail = result.unwrap_or_else(|| self.push(HirKind::Lit(Lit::Unit), Some(ty::unit()), span));
        // The call's node becomes the block (or the bare result), so its
        // parent still reads it. A result packed into the call's trait
        // object stays a node of its own: its pack is keyed on it (#1757).
        let packed = existential(&self.body.expr(call).ty) && !existential(&self.body.expr(tail).ty);
        let kind = if stmts.is_empty() && !packed {
            let kind = self.body.expr(tail).kind.clone();
            self.tombstone(tail);
            kind
        } else {
            HirKind::Block { stmts, tail: Some(tail) }
        };
        let e = &mut self.body.exprs[call.0 as usize];
        e.kind = kind;
        e.node = None;
    }

    /// Reads only locals and literals, with no effect and no trap.
    fn pure(&self, e: HirId) -> bool {
        match &self.body.expr(e).kind {
            HirKind::Lit(_) | HirKind::Local(_) => true,
            HirKind::Bin { op, lhs, rhs } => {
                !super::op_may_trap(&self.body, e) && !matches!(op, BinOp::Overloaded(_)) && self.pure(*lhs) && self.pure(*rhs)
            }
            HirKind::Un { operand, .. } => !super::op_may_trap(&self.body, e) && self.pure(*operand),
            _ => false,
        }
    }

    /// Reads only locals and literals, with no effect, but int arithmetic
    /// that may trap: a call cannot run before it, but it can run first
    /// into a temp ([`Inliner::spill`]).
    fn spillable(&self, e: HirId) -> bool {
        match &self.body.expr(e).kind {
            HirKind::Lit(_) | HirKind::Local(_) => true,
            HirKind::Bin { op, lhs, rhs } => {
                !matches!(op, BinOp::Overloaded(_)) && self.spillable(*lhs) && self.spillable(*rhs)
            }
            HirKind::Un { operand, .. } => self.spillable(*operand),
            _ => false,
        }
    }

    /// `let t = e;` for the hoisted statements, with `e`'s node now reading
    /// `t`, so `e` (and its trap) runs before the call that hoists.
    fn spill(&mut self, e: HirId) -> HirId {
        let moved = self.body.expr(e).clone();
        let local = LocalId(self.body.locals.len() as u32);
        self.body.locals.push(HirLocal {
            name: format!("__inl_spill{}", local.0),
            ty: moved.ty.clone(),
            kind: LocalKind::Temp,
            captured: false,
            borrow: false,
        });
        let span = moved.span;
        let init = HirId(self.body.exprs.len() as u32);
        self.body.exprs.push(moved);
        let node = &mut self.body.exprs[e.0 as usize];
        node.kind = HirKind::Local(local);
        node.node = None;
        node.flags = HirFlags::default();
        self.push(HirKind::Let { local, init: Some(init) }, Some(ty::unit()), span)
    }

    /// Splice `callee` in for `call`: hoisted statements go to `out`. For
    /// [`Site::Stmt`] and [`Site::Direct`] returns the result node, which
    /// the caller puts in place of the statement or the operand.
    fn splice(&mut self, call: HirId, callee: &HirBody, shape: &Shape, site: Site, out: &mut Vec<HirId>) -> Option<HirId> {
        self.sites += 1;
        self.spliced.push((callee.name.clone(), callee.span));
        // Unique across rounds: the caller's locals so far.
        let tag = self.body.locals.len();
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
                captured: l.captured,
                borrow: false,
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
            let (span, node) = if shape.foreign { (span, None) } else { (e.span, e.node) };
            self.body.exprs.push(HirExpr { kind, span, node, ..e.clone() });
        }
        // A substituted parameter has no slot of its own.
        let substituted = |l: &LocalId| param_of(*l).is_some_and(|k| subst[k].is_some());
        let heap = shape.heap_locals.iter().filter(|l| !substituted(l)).map(|l| LocalId(l.0 + loff));
        if shape.loop_inert {
            self.inert.extend(heap);
        } else {
            self.clear.extend(heap);
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
                            captured: false,
                            borrow: false,
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
        e.layout = crate::layout::Layout::Word;
        e.node = None;
    }

    fn push(&mut self, kind: HirKind, ty: Option<crate::typechecking::ty::Ty>, span: super::Span) -> HirId {
        let id = HirId(self.body.exprs.len() as u32);
        self.body.exprs.push(HirExpr {
            kind,
            ty,
            layout: crate::layout::Layout::Word,
            span,
            node: None,
            flags: HirFlags::default(),
        });
        id
    }
}

/// A bare-class existential (`Sum`): a `[value, dictionary]` pack.
fn existential(t: &Option<ty::Ty>) -> bool {
    t.as_ref().is_some_and(|t| matches!(ty::strip_readonly(t), ty::Ty::Existential { .. }))
}

/// Whether `callee` assigns parameter `p` or a place rooted at it by index.
fn value_ty(t: &Option<ty::Ty>) -> bool {
    matches!(t, Some(ty::Ty::Array { .. } | ty::Ty::Tuple(_) | ty::Ty::Record { .. }))
}

/// Whether every use of the value-typed parameter `p` reads a non-value
/// element or field out of it (`xs[i]`, `p.x`), so a copy of the argument
/// behaves like the shared original.
fn reads_only(callee: &HirBody, p: LocalId) -> bool {
    let mut parent = vec![None; callee.exprs.len()];
    for i in 0..callee.exprs.len() {
        for c in super::lower::children(callee, HirId(i as u32)) {
            parent[c.0 as usize] = Some(HirId(i as u32));
        }
    }
    let written = |id: HirId| {
        parent[id.0 as usize].is_some_and(|q| match &callee.expr(q).kind {
            HirKind::Assign { place, .. } | HirKind::Append { base: place, .. } => *place == id,
            _ => false,
        })
    };
    callee.exprs.iter().enumerate().all(|(i, e)| {
        if e.kind != HirKind::Local(p) {
            return true;
        }
        let mut at = HirId(i as u32);
        loop {
            match parent[at.0 as usize].map(|q| (q, &callee.expr(q).kind)) {
                Some((q, HirKind::Index { base, .. } | HirKind::Field { base, .. })) if *base == at => {
                    if written(q) {
                        return false;
                    }
                    if !value_ty(&callee.expr(q).ty) {
                        return true;
                    }
                    at = q;
                }
                _ => return false,
            }
        }
    })
}

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
    map_ids(kind, &|id| HirId(id.0 + off), &|id| LocalId(id.0 + loff))
}

/// `kind` with each expression id passed through `hf` and each local
/// through `lf` (a lambda's body, another body, stays as it is).
pub(crate) fn map_ids(kind: &HirKind, hf: &dyn Fn(HirId) -> HirId, lf: &dyn Fn(LocalId) -> LocalId) -> HirKind {
    let h = |id: &HirId| hf(*id);
    let l = |id: &LocalId| lf(*id);
    let hs = |ids: &[HirId]| ids.iter().map(h).collect::<Vec<_>>();
    match kind {
        HirKind::Lit(_) | HirKind::Global { .. } | HirKind::Break | HirKind::Continue | HirKind::Unsupported(_) => {
            kind.clone()
        }
        HirKind::Local(x) => HirKind::Local(l(x)),
        HirKind::Clear(xs) => HirKind::Clear(xs.iter().map(l).collect()),
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
            pat: map_pat(pat, lf),
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
            pat: map_pat(pat, lf),
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
                    pat: map_pat(&a.pat, lf),
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

fn map_pat(pat: &HirPat, lf: &dyn Fn(LocalId) -> LocalId) -> HirPat {
    let fields = |fs: &[(String, HirPat)]| fs.iter().map(|(n, p)| (n.clone(), map_pat(p, lf))).collect();
    match pat {
        HirPat::Wild | HirPat::Int(_) => pat.clone(),
        HirPat::Bind(l) => HirPat::Bind(lf(*l)),
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
                HirPatFields::Tuple(ps) => HirPatFields::Tuple(ps.iter().map(|p| map_pat(p, lf)).collect()),
                HirPatFields::Record(fs) => HirPatFields::Record(fields(fs)),
            },
        },
        HirPat::Tuple(ps) => HirPat::Tuple(ps.iter().map(|p| map_pat(p, lf)).collect()),
        HirPat::Record(fs) => HirPat::Record(fields(fs)),
    }
}

/// Locals of `body` that hold one value for the whole body: parameters, and
/// `let`s outside every loop, that nothing assigns. A pure call whose
/// arguments are these (or literals) computes the same value on every trip
/// of a loop, so LICM hoists it.
pub fn invariant_locals(body: &HirBody) -> std::collections::HashSet<LocalId> {
    let mut varying = std::collections::HashSet::new();
    let mut lets = Vec::new();
    let mut todo: Vec<(HirId, bool)> = body.root.into_iter().map(|r| (r, false)).collect();
    while let Some((id, in_loop)) = todo.pop() {
        match &body.expr(id).kind {
            HirKind::Assign { place, .. } | HirKind::Append { base: place, .. } => {
                if let HirKind::Local(l) = body.expr(*place).kind {
                    varying.insert(l);
                }
            }
            HirKind::Let { local, .. } => {
                if in_loop {
                    varying.insert(*local);
                } else {
                    lets.push(*local);
                }
            }
            _ => {}
        }
        let inner = in_loop || matches!(body.expr(id).kind, HirKind::Loop { .. } | HirKind::ForIn { .. });
        todo.extend(super::lower::children(body, id).into_iter().map(|c| (c, inner)));
    }
    body.params
        .iter()
        .copied()
        .chain(lets)
        .filter(|l| !varying.contains(l) && !body.local(*l).captured)
        .collect()
}

/// `id` computes one value for the whole body: literals and
/// [`invariant_locals`] under operators.
pub fn invariant_expr(body: &HirBody, id: HirId, invariant: &std::collections::HashSet<LocalId>) -> bool {
    match &body.expr(id).kind {
        HirKind::Lit(_) => true,
        HirKind::Local(l) => invariant.contains(l),
        HirKind::Bin { lhs, rhs, .. } => invariant_expr(body, *lhs, invariant) && invariant_expr(body, *rhs, invariant),
        HirKind::Un { operand, .. } => invariant_expr(body, *operand, invariant),
        HirKind::Cast { value } => invariant_expr(body, *value, invariant),
        HirKind::Make { kind: super::MakeKind::Range { .. }, args } => args.iter().all(|&a| invariant_expr(body, a, invariant)),
        _ => false,
    }
}

/// Most trips a callee's loop may run for the callee to inline. A longer
/// loop pays for its call many times over, and keeps its own body: its tier,
/// its SIMD form, and invariants hoisted once per call.
pub const SHORT_TRIPS: i64 = 16;

/// Whether every loop of `callee` is short: bounded by a literal of at most
/// [`SHORT_TRIPS`] or by a parameter nothing writes. `Some` lists those
/// parameters; a call inlines only when it passes each one such a literal.
pub fn short_loops(callee: &HirBody) -> Option<Vec<usize>> {
    let mut params = Vec::new();
    let bound = |id: HirId, params: &mut Vec<usize>| -> bool {
        match callee.expr(id).kind {
            HirKind::Lit(Lit::Int(k)) => k <= SHORT_TRIPS,
            HirKind::Local(l) => match callee.params.iter().position(|&p| p == l) {
                Some(k) if !rebinds(callee, l) => {
                    params.push(k);
                    true
                }
                _ => false,
            },
            _ => false,
        }
    };
    for e in &callee.exprs {
        match &e.kind {
            HirKind::Loop { body } => {
                // `while i < K`: the loop opens with `if i < K { .. } else { break }`.
                let HirKind::Block { stmts, .. } = &callee.expr(*body).kind else { return None };
                let HirKind::If { cond, els: Some(els), .. } = callee.expr(*stmts.first()?).kind else { return None };
                if !matches!(callee.expr(els).kind, HirKind::Break) {
                    return None;
                }
                let HirKind::Bin { op, lhs, rhs } = callee.expr(cond).kind else { return None };
                let k = match op {
                    BinOp::Lt | BinOp::Le => rhs,
                    BinOp::Gt | BinOp::Ge => lhs,
                    _ => return None,
                };
                if !bound(k, &mut params) {
                    return None;
                }
            }
            HirKind::ForIn { iterable, .. } => {
                let HirKind::Make { kind: super::MakeKind::Range { .. }, args } = &callee.expr(*iterable).kind else { return None };
                if !bound(*args.last()?, &mut params) {
                    return None;
                }
            }
            _ => {}
        }
    }
    Some(params)
}

/// `callee` as a call with constant arguments runs it: each parameter at
/// `consts` that the callee never writes reads as its literal. `None` when
/// none could be bound, or a lambda in the callee might capture one.
pub fn bind_params(callee: &HirBody, consts: &[(usize, Lit)]) -> Option<HirBody> {
    if callee.exprs.iter().any(|e| matches!(e.kind, HirKind::Lambda { .. })) {
        return None;
    }
    let mut out = callee.clone();
    let mut bound = false;
    for (k, lit) in consts {
        let Some(&p) = callee.params.get(*k) else { continue };
        if rebinds(callee, p) {
            continue;
        }
        for e in &mut out.exprs {
            if matches!(e.kind, HirKind::Local(l) if l == p) {
                e.kind = HirKind::Lit(lit.clone());
                bound = true;
            }
        }
    }
    bound.then_some(out)
}

/// Blank the nodes no longer reachable from `body`'s root (an arm or a
/// branch a fold dropped), so [`inlinable`] neither charges nor refuses
/// code that cannot run.
pub fn blank_unreachable(body: &mut HirBody) {
    let Some(root) = body.root else { return };
    // Statements after one that always exits never run: a block cut there
    // (its exiting nested blocks spliced in, so the exit is its own last
    // statement, as a folded `if true { return a; }` leaves it).
    for i in 0..body.exprs.len() {
        let HirKind::Block { stmts, tail } = &body.exprs[i].kind else { continue };
        let all: Vec<HirId> = stmts.iter().chain(tail).copied().collect();
        let mut kept = Vec::new();
        let mut cut = false;
        for s in all {
            flatten_exit(body, s, &mut kept);
            if kept.last().is_some_and(|&k| exits_always(body, k)) {
                cut = true;
                break;
            }
        }
        if cut {
            body.exprs[i].kind = HirKind::Block { stmts: kept, tail: None };
        }
    }
    let mut live = vec![false; body.exprs.len()];
    let mut stack = vec![root];
    while let Some(id) = stack.pop() {
        if std::mem::replace(&mut live[id.0 as usize], true) {
            continue;
        }
        stack.extend(super::lower::children(body, id));
    }
    for (e, live) in body.exprs.iter_mut().zip(live) {
        if !live {
            e.kind = HirKind::Lit(Lit::Unit);
        }
    }
}

/// Push `s` onto `out`, a nested block that always exits as its own
/// statements.
fn flatten_exit(body: &HirBody, s: HirId, out: &mut Vec<HirId>) {
    match &body.expr(s).kind {
        HirKind::Block { stmts, tail } if exits_always(body, s) => {
            for &k in stmts.iter().chain(tail) {
                flatten_exit(body, k, out);
            }
        }
        _ => out.push(s),
    }
}

/// `id` never completes normally: a `return`, `break` or `continue`, a
/// block ending in one, or an `if` whose branches both are.
fn exits_always(body: &HirBody, id: HirId) -> bool {
    match &body.expr(id).kind {
        HirKind::Return(_) | HirKind::Break | HirKind::Continue => true,
        HirKind::Block { stmts, tail } => stmts.iter().chain(tail).any(|&k| exits_always(body, k)),
        HirKind::If { then, els: Some(els), .. } => exits_always(body, *then) && exits_always(body, *els),
        _ => false,
    }
}
