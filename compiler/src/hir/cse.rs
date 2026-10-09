//! Local common-subexpression elimination.
//!
//! In a block, `let x = e` followed by another `e` reads `x` instead, as
//! long as nothing between them wrote a local `e` reads, wrote `x`, or (for
//! an `e` that reads the heap: a field, an element, a call with a heap
//! argument) wrote to the heap. `e` is pure: operators, casts, field and
//! element reads, and pure calls returning a scalar or a string.
//!
//! A statement with control flow or writes inside it hands its nested
//! blocks only what nothing in the whole statement can change.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;

use super::lower::children;
use super::{BinOp, Builtin, Callee, HirBody, HirId, HirKind, HirPat, LocalId};
use crate::typechecking::ty::{self, Ty};

/// `body` with repeated pure expressions read from the local that holds
/// the first, or `None` when nothing changes. `pure(name)` admits a call.
pub fn eliminate(body: &HirBody, pure: impl Fn(&str) -> bool) -> Option<HirBody> {
    let root = body.root?;
    let mut cx = Cx { body: body.clone(), pure: &pure, hits: 0 };
    cx.walk(root, &Avail::default());
    (cx.hits > 0).then_some(cx.body)
}

/// One available expression: its key, the local holding it, and what it
/// reads.
#[derive(Clone)]
struct Entry {
    holder: LocalId,
    reads: HashSet<LocalId>,
    heap: bool,
}

#[derive(Clone, Default)]
struct Avail {
    map: HashMap<String, Entry>,
}

impl Avail {
    fn kill(&mut self, w: &Writes) {
        self.map
            .retain(|_, e| !w.locals.contains(&e.holder) && !e.reads.iter().any(|l| w.locals.contains(l)) && !(w.heap && e.heap));
    }
}

/// What a tree may change.
#[derive(Default)]
struct Writes {
    locals: HashSet<LocalId>,
    heap: bool,
}

struct Cx<'p, P> {
    body: HirBody,
    pure: &'p P,
    hits: usize,
}

impl<P: Fn(&str) -> bool> Cx<'_, P> {
    /// Rewrite inside `id`, entering with `avail`.
    fn walk(&mut self, id: HirId, avail: &Avail) {
        match self.body.expr(id).kind.clone() {
            HirKind::Block { stmts, tail } => {
                let mut avail = avail.clone();
                for s in stmts.into_iter().chain(tail) {
                    self.statement(s, &mut avail);
                }
            }
            _ => {
                let mut avail = avail.clone();
                self.statement(id, &mut avail);
            }
        }
    }

    fn statement(&mut self, s: HirId, avail: &mut Avail) {
        let (value, defines) = match self.body.expr(s).kind.clone() {
            HirKind::Let { local, init: Some(v) } => (Some(v), Some(local)),
            HirKind::Assign { value, .. } => (Some(value), None),
            _ => (None, None),
        };
        let w = self.writes(s);
        let simple = value.is_some_and(|v| self.simple(v)) || (value.is_none() && self.simple(s));
        if simple {
            match value {
                Some(v) => self.reuse(v, avail),
                None => self.reuse(s, avail),
            }
            // An element or field written to: the index and base still read
            // what they read before the write.
            if let HirKind::Assign { place, .. } = self.body.expr(s).kind.clone()
                && !matches!(self.body.expr(place).kind, HirKind::Local(_))
            {
                for k in children(&self.body, place) {
                    if self.simple(k) {
                        self.reuse(k, avail);
                    }
                }
            }
        } else {
            let mut inner = avail.clone();
            inner.kill(&w);
            for k in children(&self.body, s) {
                self.walk(k, &inner);
            }
        }
        avail.kill(&w);
        if let (Some(x), Some(v)) = (defines, value)
            && !self.body.local(x).captured
            && let Some((key, reads, heap)) = self.key(v)
            && worth(&self.body.expr(v).kind)
            && !reads.contains(&x)
            && self.body.local(x).ty == self.body.expr(v).ty
        {
            avail.map.insert(key, Entry { holder: x, reads, heap });
        }
    }

    /// Replace each maximal subtree of `id` that is available.
    fn reuse(&mut self, id: HirId, avail: &Avail) {
        if worth(&self.body.expr(id).kind)
            && let Some((key, _, _)) = self.key(id)
            && let Some(e) = avail.map.get(&key)
        {
            let at = &mut self.body.exprs[id.0 as usize];
            at.kind = HirKind::Local(e.holder);
            at.node = None;
            at.flags = super::HirFlags::default();
            self.hits += 1;
            return;
        }
        for k in children(&self.body, id) {
            self.reuse(k, avail);
        }
    }

    /// No control flow, no writes, no calls that are not pure.
    fn simple(&self, id: HirId) -> bool {
        let mut ok = true;
        visit(&self.body, id, &mut |e| {
            ok &= match &e.kind {
                HirKind::Lit(_)
                | HirKind::Local(_)
                | HirKind::Global { .. }
                | HirKind::Field { .. }
                | HirKind::Index { .. }
                | HirKind::Bin { .. }
                | HirKind::Un { .. }
                | HirKind::Cast { .. }
                | HirKind::Make { .. }
                | HirKind::Named { .. } => true,
                HirKind::Call { callee: Callee::Named { name, .. }, .. } => (self.pure)(name),
                _ => false,
            };
        });
        ok
    }

    /// A key naming the value of `id`, the locals it reads, and whether it
    /// reads the heap; `None` when `id` is not a pure expression.
    fn key(&self, id: HirId) -> Option<(String, HashSet<LocalId>, bool)> {
        let mut out = String::new();
        let mut reads = HashSet::new();
        let mut heap = false;
        self.key_into(id, &mut out, &mut reads, &mut heap)?;
        Some((out, reads, heap))
    }

    fn key_into(&self, id: HirId, out: &mut String, reads: &mut HashSet<LocalId>, heap: &mut bool) -> Option<()> {
        let e = self.body.expr(id);
        match &e.kind {
            HirKind::Lit(l) => write!(out, "{l:?}").ok()?,
            HirKind::Local(l) => {
                if self.body.local(*l).captured {
                    return None;
                }
                reads.insert(*l);
                write!(out, "%{}", l.0).ok()?;
            }
            HirKind::Bin { op, lhs, rhs } => {
                if matches!(op, BinOp::Overloaded(_)) {
                    return None;
                }
                let mut l = String::new();
                let mut r = String::new();
                self.key_into(*lhs, &mut l, reads, heap)?;
                self.key_into(*rhs, &mut r, reads, heap)?;
                // `a * b` and `b * a` are one value.
                if commutes(*op) && r < l {
                    std::mem::swap(&mut l, &mut r);
                }
                write!(out, "({op:?} {l} {r})").ok()?;
            }
            HirKind::Un { op, operand } => {
                write!(out, "({op:?} ").ok()?;
                self.key_into(*operand, out, reads, heap)?;
                out.push(')');
            }
            HirKind::Cast { value } => {
                write!(out, "(as {:?} ", e.ty.as_ref().map(ty::strip_readonly)?).ok()?;
                self.key_into(*value, out, reads, heap)?;
                out.push(')');
            }
            HirKind::Field { base, name } => {
                *heap = true;
                write!(out, "(. {name} ").ok()?;
                self.key_into(*base, out, reads, heap)?;
                out.push(')');
            }
            HirKind::Index { base, index, kind } => {
                *heap = true;
                write!(out, "([] {kind:?} ").ok()?;
                self.key_into(*base, out, reads, heap)?;
                out.push(' ');
                self.key_into(*index, out, reads, heap)?;
                out.push(')');
            }
            HirKind::Call { callee: Callee::Named { name, overload, .. }, args } => {
                if !(self.pure)(name) || !e.ty.as_ref().is_some_and(value_ty) {
                    return None;
                }
                write!(out, "(call {name}#{overload:?}").ok()?;
                for &a in args {
                    if !self.body.expr(a).ty.as_ref().is_some_and(value_ty) {
                        *heap = true;
                    }
                    out.push(' ');
                    self.key_into(a, out, reads, heap)?;
                }
                out.push(')');
            }
            _ => return None,
        }
        Some(())
    }

    /// Everything `id` may change.
    fn writes(&self, id: HirId) -> Writes {
        let body = &self.body;
        let mut w = Writes::default();
        visit(body, id, &mut |e| match &e.kind {
            HirKind::Let { local, .. } => {
                w.locals.insert(*local);
            }
            HirKind::LetPat { pat, .. } => binds(pat, &mut w.locals),
            HirKind::Match { arms, .. } => arms.iter().for_each(|a| binds(&a.pat, &mut w.locals)),
            HirKind::ForIn { pat, .. } => binds(pat, &mut w.locals),
            HirKind::Assign { place, .. } | HirKind::Append { base: place, .. } => match body.expr(*place).kind {
                HirKind::Local(l) if matches!(e.kind, HirKind::Assign { .. }) => {
                    w.locals.insert(l);
                }
                HirKind::Local(l) => {
                    w.locals.insert(l);
                    w.heap = true;
                }
                _ => w.heap = true,
            },
            HirKind::Clear(ls) => w.locals.extend(ls),
            HirKind::Call { callee: Callee::Named { name, .. }, .. } if (self.pure)(name) => {}
            HirKind::Call { .. } | HirKind::Yield { .. } | HirKind::Resume { .. } | HirKind::Lambda { .. } | HirKind::Defer { .. } => {
                w.heap = true
            }
            HirKind::Builtin { op, .. } if !matches!(op, Builtin::Panic | Builtin::TypeOf | Builtin::Readonly | Builtin::Default) => {
                w.heap = true
            }
            _ => {}
        });
        w
    }
}

fn commutes(op: BinOp) -> bool {
    matches!(
        op,
        BinOp::IntAdd | BinOp::IntMul | BinOp::FloatAdd | BinOp::FloatMul | BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor | BinOp::Eq | BinOp::Ne
    )
}

/// Worth reading from a local instead: computes something.
fn worth(kind: &HirKind) -> bool {
    matches!(
        kind,
        HirKind::Bin { .. } | HirKind::Un { .. } | HirKind::Cast { .. } | HirKind::Field { .. } | HirKind::Index { .. } | HirKind::Call { .. }
    )
}

/// A value with no identity of its own.
fn value_ty(t: &Ty) -> bool {
    matches!(ty::strip_readonly(t), Ty::Con(n) if matches!(n.as_str(),
        "int" | "i8" | "i16" | "i32" | "i64" | "u8" | "u16" | "u32" | "u64" | "byte" | "float" | "f32" | "f64" | "bool" | "char" | "string"))
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

fn visit(body: &HirBody, id: HirId, f: &mut impl FnMut(&super::HirExpr)) {
    f(body.expr(id));
    for k in children(body, id) {
        visit(body, k, f);
    }
}
