//! Loop-invariant code motion.
//!
//! An expression inside a loop that reads only locals the loop never
//! writes, and computes nothing that can trap or that has an identity
//! (`n * 3 + 1`, `(y as float)`, a pure call on scalars returning a
//! scalar), is computed once into a temp before the loop. Loops are
//! visited outermost first, so an expression invariant in every loop of a
//! nest leaves the whole nest at once.
//!
//! An expression that may trap (a pure call, a division by a variable)
//! moves only when every iteration runs it, before any exit, and the loop
//! is sure to run it once: a `loop` with no condition, or a `while` whose
//! cheap condition is tested again in front of the hoisted code.

use std::collections::HashSet;

use super::lower::children;
use super::tuple_sroa::{fresh, push};
use super::{BinOp, Callee, HirBody, HirId, HirKind, HirPat, Lit, LocalId, MakeKind, UnOp};
use crate::typechecking::ty::{self, Ty};

/// `body` with invariant expressions moved out of its loops, or `None`
/// when nothing moves. `pure(name)` admits a call to `name`.
pub fn hoist(body: &HirBody, pure: impl Fn(&str) -> bool) -> Option<HirBody> {
    let root = body.root?;
    let mut loops = Vec::new();
    preorder(body, root, &mut |id| {
        if matches!(body.expr(id).kind, HirKind::Loop { .. } | HirKind::ForIn { .. }) {
            loops.push(id);
        }
    });
    let mut out: Option<HirBody> = None;
    for lp in loops {
        let cur = out.as_ref().unwrap_or(body);
        let plan = Plan::new(cur, lp, &pure);
        let picks = plan.picks();
        if picks.is_empty() {
            continue;
        }
        let guard = plan.guard.flatten().filter(|_| picks.iter().any(|&(_, s)| s == Safety::Traps));
        let picks: Vec<HirId> = picks.into_iter().map(|(p, _)| p).collect();
        let target = out.get_or_insert_with(|| body.clone());
        apply(target, lp, &picks, guard);
    }
    out
}

/// How safe an expression is to compute ahead of its loop.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Safety {
    /// Never traps: computing it early changes nothing.
    Free,
    /// May trap: only where the loop is sure to compute it.
    Traps,
}

struct Plan<'a, P> {
    body: &'a HirBody,
    pure: &'a P,
    /// Locals the loop writes or binds.
    written: HashSet<LocalId>,
    /// The loop changes nothing on the heap: no field, element or append
    /// write and no call that is not pure.
    heap_quiet: bool,
    /// Expressions every iteration computes before it can leave.
    every: HashSet<HirId>,
    /// The `while` condition to test before trapping code, or `None` when
    /// the loop needs none (`Some(None)` admits no trapping code).
    guard: Option<Option<HirId>>,
    loop_body: HirId,
}

impl<'a, P: Fn(&str) -> bool> Plan<'a, P> {
    fn new(body: &'a HirBody, lp: HirId, pure: &'a P) -> Self {
        let mut written = HashSet::new();
        let (loop_body, pat) = match &body.expr(lp).kind {
            HirKind::Loop { body: b } => (*b, None),
            HirKind::ForIn { body: b, pat, .. } => (*b, Some(pat.clone())),
            _ => unreachable!("loop ids only"),
        };
        if let Some(p) = &pat {
            binds(p, &mut written);
        }
        preorder(body, loop_body, &mut |id| match &body.expr(id).kind {
            HirKind::Let { local, .. } => {
                written.insert(*local);
            }
            HirKind::LetPat { pat, .. } => binds(pat, &mut written),
            HirKind::Match { arms, .. } => arms.iter().for_each(|a| binds(&a.pat, &mut written)),
            HirKind::ForIn { pat, .. } => binds(pat, &mut written),
            HirKind::Assign { place, .. } | HirKind::Append { base: place, .. } => {
                if let HirKind::Local(l) = body.expr(*place).kind {
                    written.insert(l);
                }
            }
            HirKind::Clear(ls) => written.extend(ls),
            _ => {}
        });
        let mut heap_quiet = true;
        preorder(body, loop_body, &mut |id| {
            heap_quiet &= match &body.expr(id).kind {
                HirKind::Assign { place, .. } => matches!(body.expr(*place).kind, HirKind::Local(_)),
                HirKind::Append { .. } | HirKind::Resume { .. } | HirKind::Yield { .. } => false,
                HirKind::Call { callee: Callee::Named { name, .. }, .. } => pure(name),
                HirKind::Call { .. } => false,
                HirKind::Builtin { op, .. } => {
                    matches!(op, super::Builtin::Panic | super::Builtin::TypeOf | super::Builtin::Readonly | super::Builtin::Default)
                }
                _ => true,
            };
        });
        let mut plan = Plan {
            body,
            pure,
            written,
            heap_quiet,
            every: HashSet::new(),
            guard: Some(None),
            loop_body,
        };
        // A `for` loop may run no iteration and has no condition to test.
        let (spine, guard) = match &body.expr(lp).kind {
            HirKind::ForIn { .. } => (None, Some(None)),
            _ => match plan.while_cond() {
                Some((cond, then)) if plan.cheap(cond) => (Some(then), Some(Some(cond))),
                Some(_) => (None, Some(None)),
                None => (Some(loop_body), None),
            },
        };
        plan.guard = guard;
        if let Some(s) = spine {
            plan.every_from(s);
        }
        plan
    }

    /// `loop { if cond { then } else { break } }`, the shape of `while`.
    fn while_cond(&self) -> Option<(HirId, HirId)> {
        let HirKind::Block { stmts, tail: None } = &self.body.expr(self.loop_body).kind else {
            return None;
        };
        let [only] = stmts.as_slice() else { return None };
        match &self.body.expr(*only).kind {
            HirKind::If { cond, then, els: Some(e) } if matches!(self.body.expr(*e).kind, HirKind::Break) => {
                Some((*cond, *then))
            }
            _ => None,
        }
    }

    /// Mark what runs whenever `id` runs, stopping at the first statement
    /// that can leave the iteration.
    fn every_from(&mut self, id: HirId) {
        self.every.insert(id);
        match &self.body.expr(id).kind {
            HirKind::Block { stmts, tail } => {
                for &s in stmts.iter().chain(tail) {
                    self.every_from(s);
                    if self.leaves(s) {
                        break;
                    }
                }
            }
            HirKind::If { cond, .. } => self.every_from(*cond),
            HirKind::Logic { lhs, .. } => self.every_from(*lhs),
            HirKind::Match { scrutinee, .. } => self.every_from(*scrutinee),
            HirKind::Loop { .. } | HirKind::Defer { .. } | HirKind::Lambda { .. } => {}
            HirKind::ForIn { iterable, .. } => self.every_from(*iterable),
            _ => {
                for k in children(self.body, id) {
                    self.every_from(k);
                }
            }
        }
    }

    /// Whether `id` may jump out of the iteration.
    fn leaves(&self, id: HirId) -> bool {
        let mut out = false;
        preorder(self.body, id, &mut |k| {
            out |= matches!(
                self.body.expr(k).kind,
                HirKind::Break | HirKind::Continue | HirKind::Return(_) | HirKind::Yield { .. }
            );
        });
        out
    }

    /// A condition worth testing twice: locals, literals and free arithmetic.
    fn cheap(&self, cond: HirId) -> bool {
        let mut n = 0;
        let mut ok = true;
        preorder(self.body, cond, &mut |k| {
            n += 1;
            ok &= match &self.body.expr(k).kind {
                HirKind::Lit(_) | HirKind::Local(_) | HirKind::Logic { .. } | HirKind::Un { .. } => true,
                HirKind::Bin { op, .. } => op_safety(self.body, *op, k) == Some(Safety::Free),
                _ => false,
            };
        });
        ok && n <= 16
    }

    /// The expressions to move: maximal invariant subtrees worth a temp.
    fn picks(&self) -> Vec<(HirId, Safety)> {
        let mut picks = Vec::new();
        self.collect(self.loop_body, &mut picks);
        picks
    }

    fn collect(&self, id: HirId, picks: &mut Vec<(HirId, Safety)>) {
        let e = self.body.expr(id);
        if let Some(safety) = self.invariant(id)
            && worth(self.body, &e.kind)
            && (safety == Safety::Free || self.guard != Some(None) && self.every.contains(&id))
        {
            picks.push((id, safety));
            return;
        }
        match &e.kind {
            // The places an assignment writes stay where they are.
            HirKind::Assign { value, .. } | HirKind::Append { value, .. } => self.collect(*value, picks),
            HirKind::Lambda { .. } | HirKind::Defer { .. } => {}
            _ => {
                for k in children(self.body, id) {
                    self.collect(k, picks);
                }
            }
        }
    }

    /// `Some(safety)` when `id` reads nothing the loop writes.
    fn invariant(&self, id: HirId) -> Option<Safety> {
        let body = self.body;
        let e = body.expr(id);
        match &e.kind {
            HirKind::Lit(_) => Some(Safety::Free),
            HirKind::Local(l) => {
                let local = body.local(*l);
                (!self.written.contains(l) && !local.captured).then_some(Safety::Free)
            }
            HirKind::Bin { op, lhs, rhs } => {
                let s = op_safety(body, *op, id)?;
                Some(s.max(self.invariant(*lhs)?).max(self.invariant(*rhs)?))
            }
            HirKind::Logic { lhs, rhs, .. } => Some(self.invariant(*lhs)?.max(self.invariant(*rhs)?)),
            // A scalar field of an object nothing in the loop writes to.
            HirKind::Field { base, .. }
                if self.heap_quiet && e.ty.as_ref().is_some_and(|t| scalar(t) || is_named(t, "string")) =>
            {
                match body.expr(*base).kind {
                    HirKind::Local(l) if !self.written.contains(&l) && !body.local(l).captured => Some(Safety::Free),
                    _ => None,
                }
            }
            HirKind::Un { op: UnOp::Neg | UnOp::BitNot | UnOp::Not, operand } => self.invariant(*operand),
            HirKind::Cast { value } => {
                let from = body.expr(*value).ty.as_ref().map(ty::strip_readonly)?;
                let to = e.ty.as_ref().map(ty::strip_readonly)?;
                let s = if is_int(from) && is_named(to, "float") { Safety::Free } else if scalar(from) && scalar(to) { Safety::Traps } else { return None };
                Some(s.max(self.invariant(*value)?))
            }
            HirKind::Call { callee: Callee::Named { name, .. }, args } => {
                if !(self.pure)(name) || !e.ty.as_ref().is_some_and(|t| scalar(t) || is_named(t, "string")) {
                    return None;
                }
                let mut s = Safety::Traps;
                for &a in args {
                    s = s.max(self.argument(a)?);
                }
                Some(s)
            }
            _ => None,
        }
    }

    /// `Some(safety)` when `a`, an argument of a pure call, is invariant.
    /// A heap value is, when the loop changes nothing on the heap; a range
    /// or tuple built from invariant parts is a value the call only reads.
    fn argument(&self, a: HirId) -> Option<Safety> {
        let e = self.body.expr(a);
        if e.ty.as_ref().is_some_and(scalar) {
            return self.invariant(a);
        }
        if !self.heap_quiet {
            return None;
        }
        match &e.kind {
            HirKind::Local(l) => (!self.written.contains(l) && !self.body.local(*l).captured).then_some(Safety::Free),
            HirKind::Make { kind: MakeKind::Range { .. } | MakeKind::Tuple, args } => {
                args.iter().try_fold(Safety::Free, |s, &x| Some(s.max(self.argument(x)?)))
            }
            HirKind::Lit(_) => Some(Safety::Free),
            _ => None,
        }
    }
}

/// Whether `op` at `id` can trap; `None` for operators that call code.
fn op_safety(body: &HirBody, op: BinOp, id: HirId) -> Option<Safety> {
    Some(match op {
        BinOp::Overloaded(_) => return None,
        BinOp::IntDiv | BinOp::IntRem => {
            let HirKind::Bin { rhs, .. } = body.expr(id).kind else { return None };
            match body.expr(rhs).kind {
                HirKind::Lit(Lit::Int(k)) if k != 0 && k != -1 => Safety::Free,
                _ => Safety::Traps,
            }
        }
        BinOp::IntPow => Safety::Traps,
        _ => Safety::Free,
    })
}

/// Worth a temp: computes something, not just a read. A unary operator or
/// a comparison on reads stays, since it fuses with the branch that uses it.
fn worth(body: &HirBody, kind: &HirKind) -> bool {
    let read = |id: &HirId| matches!(body.expr(*id).kind, HirKind::Local(_) | HirKind::Lit(_));
    match kind {
        HirKind::Un { operand, .. } => !read(operand),
        HirKind::Bin {
            op: BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge,
            lhs,
            rhs,
        } => !(read(lhs) && read(rhs)),
        HirKind::Bin { .. } | HirKind::Logic { .. } | HirKind::Cast { .. } | HirKind::Call { .. } | HirKind::Field { .. } => true,
        _ => false,
    }
}

fn is_named(t: &Ty, name: &str) -> bool {
    matches!(ty::strip_readonly(t), Ty::Con(n) if n == name)
}

fn is_int(t: &Ty) -> bool {
    matches!(ty::strip_readonly(t), Ty::Con(n) if matches!(n.as_str(), "int" | "i8" | "i16" | "i32" | "i64" | "u8" | "u16" | "u32" | "u64" | "byte"))
}

/// A value with no identity: copying it is the same as recomputing it.
fn scalar(t: &Ty) -> bool {
    is_int(t) || matches!(ty::strip_readonly(t), Ty::Con(n) if matches!(n.as_str(), "float" | "f32" | "f64" | "bool" | "char"))
}

fn binds(pat: &HirPat, out: &mut HashSet<LocalId>) {
    match pat {
        HirPat::Bind(l) => {
            out.insert(*l);
        }
        HirPat::Variant {
            fields: super::HirPatFields::Tuple(ps),
            ..
        }
        | HirPat::Tuple(ps) => ps.iter().for_each(|p| binds(p, out)),
        HirPat::Variant {
            fields: super::HirPatFields::Record(fs),
            ..
        }
        | HirPat::Record(fs) => fs.iter().for_each(|(_, p)| binds(p, out)),
        _ => {}
    }
}

fn preorder(body: &HirBody, id: HirId, f: &mut impl FnMut(HirId)) {
    f(id);
    for k in children(body, id) {
        preorder(body, k, f);
    }
}

/// Move each of `picks` into a temp set in front of loop `lp`, behind a
/// copy of the `guard` condition when one is given.
fn apply(body: &mut HirBody, lp: HirId, picks: &[HirId], guard: Option<HirId>) {
    let span = body.expr(lp).span;
    let mut lets = Vec::new();
    for &p in picks {
        let ty = body.expr(p).ty.clone().unwrap_or_else(ty::unit);
        let tmp = fresh(body, "__licm", ty);
        let moved = HirId(body.exprs.len() as u32);
        let copy = body.expr(p).clone();
        body.exprs.push(copy);
        lets.push(push(body, HirKind::Let { local: tmp, init: Some(moved) }, Some(ty::unit()), span));
        let e = &mut body.exprs[p.0 as usize];
        e.kind = HirKind::Local(tmp);
        e.node = None;
        e.flags = super::HirFlags::default();
    }
    // The loop moves to a fresh id; `lp` becomes the block that sets the
    // temps and then runs it.
    let moved_loop = HirId(body.exprs.len() as u32);
    let lp_expr = body.expr(lp).clone();
    let loop_ty = lp_expr.ty.clone();
    let layout = lp_expr.layout.clone();
    body.exprs.push(lp_expr);
    let inner = match guard {
        Some(cond) => {
            let test = copy_tree(body, cond);
            let block = push(body, HirKind::Block { stmts: lets, tail: Some(moved_loop) }, loop_ty.clone(), span);
            body.exprs[block.0 as usize].layout = layout;
            HirKind::If { cond: test, then: block, els: None }
        }
        None => HirKind::Block { stmts: lets, tail: Some(moved_loop) },
    };
    let e = &mut body.exprs[lp.0 as usize];
    e.kind = inner;
    e.node = None;
    if matches!(e.kind, HirKind::If { .. }) {
        e.ty = Some(ty::unit());
    }
}

/// A fresh copy of the tree at `id` (locals, literals and operators only).
fn copy_tree(body: &mut HirBody, id: HirId) -> HirId {
    let mut e = body.expr(id).clone();
    e.kind = match e.kind {
        HirKind::Bin { op, lhs, rhs } => HirKind::Bin { op, lhs: copy_tree(body, lhs), rhs: copy_tree(body, rhs) },
        HirKind::Logic { and, lhs, rhs } => HirKind::Logic { and, lhs: copy_tree(body, lhs), rhs: copy_tree(body, rhs) },
        HirKind::Un { op, operand } => HirKind::Un { op, operand: copy_tree(body, operand) },
        k => k,
    };
    e.node = None;
    let at = HirId(body.exprs.len() as u32);
    body.exprs.push(e);
    at
}
