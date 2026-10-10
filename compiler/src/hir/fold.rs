//! Constant folding and algebraic identities.
//!
//! Operators on literals become literals (`int` and `float` only, and an
//! `int` result only when it fits 32 bits, as the stack IL encodes it),
//! `x + 0`, `x * 1`, `x | 0` and friends become `x`, `x * 0` becomes `0`
//! when `x` is a plain read, `x ** 2` becomes `x * x`, `(x + 1) + 2` becomes
//! `x + 3` (and so do `x = x + 1; x = x + 2;` in a row), `!!b` becomes `b`,
//! and `if` on a literal keeps only the branch it takes. Folds run bottom-up, so
//! they cascade.

use super::lower::children;
use super::{BinOp, HirBody, HirExpr, HirId, HirKind, Lit, UnOp};
use crate::typechecking::ty::{self, Ty};

/// `body` with every fold applied, or `None` when nothing folds.
pub fn fold(body: &HirBody) -> Option<HirBody> {
    let root = body.root?;
    let mut out = body.clone();
    let mut hits = 0;
    post(&mut out, root, &mut hits);
    (hits > 0).then_some(out)
}

fn post(body: &mut HirBody, id: HirId, hits: &mut usize) {
    for k in children(body, id) {
        post(body, k, hits);
    }
    merge_steps(body, id, hits);
    if let Some(kind) = rewrite(body, id).or_else(|| square(body, id)).or_else(|| regroup(body, id)) {
        let e = &mut body.exprs[id.0 as usize];
        e.kind = kind;
        e.node = None;
        *hits += 1;
    }
}

/// What `id` becomes, when it folds.
fn rewrite(body: &HirBody, id: HirId) -> Option<HirKind> {
    let e = body.expr(id);
    match &e.kind {
        HirKind::Bin { op, lhs, rhs } => {
            let (l, r) = (body.expr(*lhs), body.expr(*rhs));
            if let (HirKind::Lit(a), HirKind::Lit(b)) = (&l.kind, &r.kind) {
                return constant(*op, a, b, e.ty.as_ref()?).map(HirKind::Lit);
            }
            identity(body, *op, *lhs, *rhs, e)
        }
        HirKind::Un { op: UnOp::Not, operand } => match &body.expr(*operand).kind {
            HirKind::Un { op: UnOp::Not, operand: inner } => Some(body.expr(*inner).kind.clone()),
            HirKind::Lit(Lit::Bool(b)) => Some(HirKind::Lit(Lit::Bool(!b))),
            _ => None,
        },
        HirKind::Un { op: UnOp::Neg, operand } if named(e.ty.as_ref()?, "int") => match body.expr(*operand).kind {
            HirKind::Lit(Lit::Int(a)) => fits(a.checked_neg()?).map(|v| HirKind::Lit(Lit::Int(v))),
            _ => None,
        },
        HirKind::Logic { and, lhs, rhs } => match body.expr(*lhs).kind {
            // `true && b` is `b`, `false || b` is `b`; the other two are the literal.
            HirKind::Lit(Lit::Bool(a)) if a == *and => Some(body.expr(*rhs).kind.clone()),
            HirKind::Lit(Lit::Bool(a)) => Some(HirKind::Lit(Lit::Bool(a))),
            _ => None,
        },
        // The branch taken stands in for the `if` when it has the same type.
        HirKind::If { cond, then, els } => {
            let taken = match body.expr(*cond).kind {
                HirKind::Lit(Lit::Bool(true)) => Some(*then),
                HirKind::Lit(Lit::Bool(false)) => *els,
                _ => return None,
            };
            match taken {
                Some(b) => (body.expr(b).ty == e.ty).then(|| body.expr(b).kind.clone()),
                None => e.ty.as_ref().is_some_and(|t| named(t, "unit") || *t == ty::unit()).then(|| HirKind::Block { stmts: Vec::new(), tail: None }),
            }
        }
        _ => None,
    }
}

/// `x ** 2` on a plain read is `x * x`.
fn square(body: &mut HirBody, id: HirId) -> Option<HirKind> {
    let HirKind::Bin { op: BinOp::IntPow, lhs, rhs } = body.expr(id).kind else {
        return None;
    };
    if !matches!(body.expr(rhs).kind, HirKind::Lit(Lit::Int(2))) || !matches!(body.expr(lhs).kind, HirKind::Local(_)) {
        return None;
    }
    let mut copy = body.expr(lhs).clone();
    copy.node = None;
    let again = HirId(body.exprs.len() as u32);
    body.exprs.push(copy);
    Some(HirKind::Bin { op: BinOp::IntMul, lhs, rhs: again })
}

/// `(x + a) + b` is `x + (a + b)` for `int` literals of one sign, so a
/// chain of constant steps (an unrolled counter, say) adds once. Same sign
/// keeps every overflow the chain had: the sum overflows exactly when some
/// step did.
fn regroup(body: &mut HirBody, id: HirId) -> Option<HirKind> {
    let HirKind::Bin { op: BinOp::IntAdd, lhs, rhs } = body.expr(id).kind else { return None };
    let HirKind::Lit(Lit::Int(b)) = body.expr(rhs).kind else { return None };
    let HirKind::Bin { op: BinOp::IntAdd, lhs: x, rhs: inner } = body.expr(lhs).kind else { return None };
    let HirKind::Lit(Lit::Int(a)) = body.expr(inner).kind else { return None };
    if (a < 0) != (b < 0) || body.expr(x).ty != body.expr(id).ty {
        return None;
    }
    let sum = fits(a.checked_add(b)?)?;
    // `inner` belongs to the `x + a` this replaces, so it can take the sum.
    let e = &mut body.exprs[inner.0 as usize];
    e.kind = HirKind::Lit(Lit::Int(sum));
    e.node = None;
    Some(HirKind::Bin { op: BinOp::IntAdd, lhs: x, rhs: inner })
}

/// In a block, `x = x + a; x = x + b;` is `x = x + (a + b);` for `int`
/// literals of one sign, as [`regroup`] does inside one expression.
fn merge_steps(body: &mut HirBody, id: HirId, hits: &mut usize) {
    let HirKind::Block { stmts, tail } = &body.expr(id).kind else { return };
    let (mut stmts, tail) = (stmts.clone(), *tail);
    let mut k = 0;
    let mut merged = false;
    while k + 1 < stmts.len() {
        if let (Some((x, a, lit_a)), Some((y, b, _))) = (step(body, stmts[k]), step(body, stmts[k + 1]))
            && x == y
            && (a < 0) == (b < 0)
            && let Some(sum) = a.checked_add(b).and_then(fits)
        {
            let e = &mut body.exprs[lit_a.0 as usize];
            e.kind = HirKind::Lit(Lit::Int(sum));
            e.node = None;
            stmts.remove(k + 1);
            merged = true;
            *hits += 1;
            continue;
        }
        k += 1;
    }
    if merged {
        let e = &mut body.exprs[id.0 as usize];
        e.kind = HirKind::Block { stmts, tail };
        e.node = None;
    }
}

/// `x = x + c` on an `int` local: `x`, `c` and the literal's node.
fn step(body: &HirBody, s: HirId) -> Option<(super::LocalId, i64, HirId)> {
    let HirKind::Assign { place, value } = body.expr(s).kind else { return None };
    let HirKind::Local(x) = body.expr(place).kind else { return None };
    let HirKind::Bin { op: BinOp::IntAdd, lhs, rhs } = body.expr(value).kind else { return None };
    if !matches!(body.expr(lhs).kind, HirKind::Local(y) if y == x) || body.local(x).captured {
        return None;
    }
    match body.expr(rhs).kind {
        HirKind::Lit(Lit::Int(c)) => Some((x, c, rhs)),
        _ => None,
    }
}

/// `x op c` / `c op x` that is just `x` (or just `0`).
fn identity(body: &HirBody, op: BinOp, lhs: HirId, rhs: HirId, e: &HirExpr) -> Option<HirKind> {
    let t = e.ty.as_ref()?;
    let int = named(t, "int");
    let float = named(t, "float");
    let lit = |id: HirId| match body.expr(id).kind {
        HirKind::Lit(Lit::Int(v)) if int => Some(v as f64),
        HirKind::Lit(Lit::Float(v)) if float => Some(v),
        _ => None,
    };
    // Only an operand of the result's own type stands in for it.
    let same = |id: HirId| body.expr(id).ty.as_ref() == Some(t);
    let keep = |id: HirId| same(id).then(|| body.expr(id).kind.clone());
    let (a, b) = (lit(lhs), lit(rhs));
    match op {
        BinOp::IntAdd if b == Some(0.0) => keep(lhs),
        BinOp::IntAdd if a == Some(0.0) => keep(rhs),
        BinOp::IntSub if b == Some(0.0) => keep(lhs),
        BinOp::IntMul | BinOp::FloatMul if b == Some(1.0) => keep(lhs),
        BinOp::IntMul | BinOp::FloatMul if a == Some(1.0) => keep(rhs),
        BinOp::IntDiv | BinOp::FloatDiv if b == Some(1.0) => keep(lhs),
        BinOp::BitOr | BinOp::BitXor | BinOp::Shl | BinOp::Shr if b == Some(0.0) => keep(lhs),
        BinOp::BitOr | BinOp::BitXor if a == Some(0.0) => keep(rhs),
        BinOp::BitAnd if b == Some(-1.0) => keep(lhs),
        BinOp::BitAnd if a == Some(-1.0) => keep(rhs),
        BinOp::IntPow if b == Some(1.0) => keep(lhs),
        BinOp::IntPow if b == Some(0.0) && read(body, lhs) => Some(HirKind::Lit(Lit::Int(1))),
        BinOp::IntSub
            if matches!((&body.expr(lhs).kind, &body.expr(rhs).kind), (HirKind::Local(x), HirKind::Local(y)) if x == y) =>
        {
            Some(HirKind::Lit(Lit::Int(0)))
        }
        // A plain read times zero: nothing to keep.
        BinOp::IntMul | BinOp::BitAnd
            if (b == Some(0.0) && read(body, lhs)) || (a == Some(0.0) && read(body, rhs)) =>
        {
            Some(HirKind::Lit(Lit::Int(0)))
        }
        _ => None,
    }
}

fn read(body: &HirBody, id: HirId) -> bool {
    matches!(body.expr(id).kind, HirKind::Local(_) | HirKind::Lit(_))
}

/// `a op b` on literals, for an `int`, `float` or `bool` result.
fn constant(op: BinOp, a: &Lit, b: &Lit, t: &Ty) -> Option<Lit> {
    match (a, b) {
        (Lit::Int(a), Lit::Int(b)) => {
            let (a, b) = (*a, *b);
            if named(t, "bool") {
                return Some(Lit::Bool(match op {
                    BinOp::Eq => a == b,
                    BinOp::Ne => a != b,
                    BinOp::Lt => a < b,
                    BinOp::Le => a <= b,
                    BinOp::Gt => a > b,
                    BinOp::Ge => a >= b,
                    _ => return None,
                }));
            }
            if !named(t, "int") {
                return None;
            }
            let r = match op {
                BinOp::IntAdd => a.checked_add(b)?,
                BinOp::IntSub => a.checked_sub(b)?,
                BinOp::IntMul => a.checked_mul(b)?,
                BinOp::IntDiv if b != 0 => a.checked_div(b)?,
                // An index lowers `i % n` as a Euclidean rem; the two agree
                // only on non-negative operands.
                BinOp::IntRem if a >= 0 && b > 0 => a % b,
                BinOp::BitAnd => a & b,
                BinOp::BitOr => a | b,
                BinOp::BitXor => a ^ b,
                BinOp::Shl if (0..32).contains(&b) => a.checked_shl(b as u32)?,
                BinOp::Shr if (0..64).contains(&b) => a >> b,
                BinOp::IntPow if (0..32).contains(&b) => a.checked_pow(b as u32)?,
                _ => return None,
            };
            fits(r).map(Lit::Int)
        }
        (Lit::Float(a), Lit::Float(b)) => {
            let (a, b) = (*a, *b);
            if named(t, "bool") {
                return Some(Lit::Bool(match op {
                    BinOp::Eq => a == b,
                    BinOp::Ne => a != b,
                    BinOp::Lt => a < b,
                    BinOp::Le => a <= b,
                    BinOp::Gt => a > b,
                    BinOp::Ge => a >= b,
                    _ => return None,
                }));
            }
            if !named(t, "float") {
                return None;
            }
            let r = match op {
                BinOp::FloatAdd => a + b,
                BinOp::FloatSub => a - b,
                BinOp::FloatMul => a * b,
                BinOp::FloatDiv if b != 0.0 => a / b,
                _ => return None,
            };
            r.is_finite().then_some(Lit::Float(r))
        }
        (Lit::Bool(a), Lit::Bool(b)) if named(t, "bool") => Some(Lit::Bool(match op {
            BinOp::Eq => a == b,
            BinOp::Ne => a != b,
            _ => return None,
        })),
        _ => None,
    }
}

/// An `int` literal the stack IL encodes inline.
fn fits(v: i64) -> Option<i64> {
    i32::try_from(v).ok().map(i64::from)
}

fn named(t: &Ty, name: &str) -> bool {
    matches!(ty::strip_readonly(t), Ty::Con(n) if n == name)
}
