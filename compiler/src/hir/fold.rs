//! Constant folding and algebraic identities.
//!
//! Operators on literals become literals (`int` and `float` only; an `int`
//! op that would trap at run time stays, so the trap still happens),
//! `x + 0`, `x * 1`, `x | 0` and friends become `x`, `x * 0` becomes `0`
//! when `x` is a plain read, `x ** 2` becomes `x * x`, `(x + 1) + 2` becomes
//! `x + 3` (and so do `x = x + 1; x = x + 2;` in a row), `x = x` goes, a
//! `match` on a constructor takes its arm, `2 * x` becomes `x * 2` (and
//! `1 < x` becomes `x > 1`), `!!b` becomes `b`, and `if` on a literal keeps
//! only the branch it takes. Folds run bottom-up, so they cascade.

use common::int_arith;

use super::lower::children;
use super::{BinOp, HirBody, HirExpr, HirId, HirKind, HirPat, HirPatFields, Lit, MakeKind, UnOp};
use crate::typechecking::ty::{self, Ty};

/// Fold rounds: a local read as a literal ([`propagate_lets`]) can make
/// another local's `let` a literal, which the next round reads through.
const ROUNDS: usize = 4;

/// `body` with every fold applied, or `None` when nothing folds.
pub fn fold(body: &HirBody) -> Option<HirBody> {
    let root = body.root?;
    let mut out = body.clone();
    let mut hits = 0;
    for _ in 0..ROUNDS {
        let before = hits;
        post(&mut out, root, &mut hits);
        drop_unread(&mut out, root, &mut hits);
        // A match that folded can bind a constructor the next round's
        // `post` folds a match on.
        if propagate_lets(&mut out, root, &mut hits) == 0 && hits == before {
            break;
        }
    }
    (hits > 0).then_some(out)
}

/// A scalar local bound once by a `let` of a literal (or of a block that
/// is just one, a folded `if`'s value) and never written again reads as
/// that literal, so what it feeds folds too: an inlined callee's
/// parameter bound to the caller's `let m = 1`, say. A local bound the
/// same way to a [`constant_variant`] stands in as a copy of it where a
/// `match` reads it, so [`known_variant`] takes the arm. Returns how many
/// reads it rewrote.
fn propagate_lets(body: &mut HirBody, root: HirId, hits: &mut usize) -> usize {
    use std::collections::HashMap;
    let mut lets: HashMap<super::LocalId, Option<Lit>> = HashMap::new();
    let mut variants: HashMap<super::LocalId, HirId> = HashMap::new();
    let mut written = std::collections::HashSet::new();
    let mut places = std::collections::HashSet::new();
    let mut scrutinees = std::collections::HashSet::new();
    let mut cleared = std::collections::HashSet::new();
    let mut reads = Vec::new();
    let mut stack = vec![root];
    while let Some(k) = stack.pop() {
        match &body.expr(k).kind {
            HirKind::Let { local, init } => {
                let local_ty = body.local(*local).ty.as_ref();
                let same_ty = init.is_some_and(|v| body.expr(v).ty.as_ref() == local_ty);
                let lit = init.and_then(|v| literal_value(body, v)).filter(|_| same_ty);
                // A constructor's own type may name its variant: the read keeps
                // the local's.
                if let Some(v) = init.filter(|&v| constant_variant(body, v)) {
                    variants.insert(*local, v);
                }
                // A second `let` of the same local is a second write.
                if lets.insert(*local, lit).is_some() {
                    written.insert(*local);
                }
            }
            HirKind::Match { scrutinee, .. } => {
                scrutinees.insert(*scrutinee);
            }
            HirKind::Assign { place, .. } | HirKind::Append { base: place, .. } => {
                places.insert(*place);
                if let HirKind::Local(l) = body.expr(*place).kind {
                    written.insert(l);
                }
            }
            // A spliced heap local is cleared after its last read: no read
            // sees the cleared slot, so a constructor still stands in for it.
            HirKind::Clear(ls) => cleared.extend(ls.iter().copied()),
            HirKind::Local(l) if !places.contains(&k) => reads.push((k, *l)),
            _ => {}
        }
        stack.extend(children(body, k).into_iter().rev());
    }
    let mut n = 0;
    for (k, l) in reads {
        if written.contains(&l) || body.local(l).captured {
            continue;
        }
        if scrutinees.contains(&k)
            && let Some(&v) = variants.get(&l)
            && variant_args_fixed(body, v, &written)
        {
            let kind = copy_variant(body, v);
            let e = &mut body.exprs[k.0 as usize];
            e.kind = kind;
            e.node = None;
            n += 1;
            continue;
        }
        if cleared.contains(&l) {
            continue;
        }
        let Some(Some(lit)) = lets.get(&l) else { continue };
        let e = &mut body.exprs[k.0 as usize];
        e.kind = HirKind::Lit(lit.clone());
        e.node = None;
        n += 1;
    }
    *hits += n;
    n
}

/// The scalar literal `id` is, looking through blocks that hold only it.
fn literal_value(body: &HirBody, id: HirId) -> Option<Lit> {
    match &body.expr(id).kind {
        HirKind::Lit(l @ (Lit::Int(_) | Lit::Float(_) | Lit::Bool(_))) => Some(l.clone()),
        HirKind::Block { stmts, tail: Some(t) } if stmts.iter().all(|&s| is_empty_block(body, s)) => literal_value(body, *t),
        _ => None,
    }
}

/// An enum constructor whose fields are literals, local reads or unit
/// variants: building it twice builds the same value, with nothing run.
fn constant_variant(body: &HirBody, id: HirId) -> bool {
    let HirKind::Make { kind: MakeKind::Variant { .. }, args } = &body.expr(id).kind else { return false };
    args.iter().all(|&a| matches!(body.expr(a).kind, HirKind::Lit(_) | HirKind::Local(_)) || unit_variant(body, a))
}

/// A variant with no fields.
fn unit_variant(body: &HirBody, id: HirId) -> bool {
    matches!(&body.expr(id).kind, HirKind::Make { kind: MakeKind::Variant { .. }, args } if args.is_empty())
}

/// No local the constructor `id` reads is written, so a copy of it read
/// later builds the same value.
fn variant_args_fixed(body: &HirBody, id: HirId, written: &std::collections::HashSet<super::LocalId>) -> bool {
    let HirKind::Make { args, .. } = &body.expr(id).kind else { return false };
    args.iter().all(|&a| match body.expr(a).kind {
        HirKind::Local(l) => !written.contains(&l) && !body.local(l).captured,
        _ => true,
    })
}

/// A fresh copy of the constructor `id` (its fields are leaves), for a
/// reader of the local it was bound to.
fn copy_variant(body: &mut HirBody, id: HirId) -> HirKind {
    let HirKind::Make { kind, args } = body.expr(id).kind.clone() else { unreachable!("a constructor") };
    let args = args
        .into_iter()
        .map(|a| {
            let mut e = body.expr(a).clone();
            e.node = None;
            body.exprs.push(e);
            HirId(body.exprs.len() as u32 - 1)
        })
        .collect();
    HirKind::Make { kind, args }
}

fn is_empty_block(body: &HirBody, id: HirId) -> bool {
    matches!(&body.expr(id).kind, HirKind::Block { stmts, tail: None } if stmts.is_empty())
}

/// Statements that do nothing go: `x = x` (an identity folded away,
/// `t = t + 0`), and the `let` and assignments of a literal or a local read
/// to a local nothing reads (an unrolled counter's steps, say).
fn drop_unread(body: &mut HirBody, root: HirId, hits: &mut usize) {
    let mut places = std::collections::HashSet::new();
    let mut read = std::collections::HashSet::new();
    let mut stmts = std::collections::HashSet::new();
    let mut clears = Vec::new();
    let mut nodes = Vec::new();
    let mut stack = vec![root];
    while let Some(k) = stack.pop() {
        nodes.push(k);
        match &body.expr(k).kind {
            HirKind::Assign { place, .. } => {
                places.insert(*place);
            }
            HirKind::Local(l) if !places.contains(&k) => {
                read.insert(*l);
            }
            HirKind::Block { stmts: ss, .. } => stmts.extend(ss.iter().copied()),
            // Clearing a local is not a read: a dropped `let` leaves it too.
            HirKind::Clear(_) => clears.push(k),
            HirKind::Defer { captures, .. } => read.extend(captures.iter().flatten().copied()),
            _ => {}
        }
        stack.extend(children(body, k).into_iter().rev());
    }
    let mut dropped = std::collections::HashSet::new();
    // An `Option` / `Result` or unit-variant constructor of constant fields
    // builds a value no finalizer watches (a user enum's may have one).
    let pure = |body: &HirBody, v: HirId| {
        matches!(body.expr(v).kind, HirKind::Lit(_) | HirKind::Local(_))
            || literal_value(body, v).is_some()
            || unit_variant(body, v)
            || constant_variant(body, v)
                && matches!(&body.expr(v).kind, HirKind::Make { kind: MakeKind::Variant { enum_name, .. }, .. }
                    if common::is_builtin_option_enum(enum_name) || common::is_builtin_result_enum(enum_name))
    };
    for k in nodes.into_iter().filter(|k| stmts.contains(k)) {
        let gone = match body.expr(k).kind {
            HirKind::Let { local, init } => {
                !read.contains(&local) && !body.local(local).captured && init.is_none_or(|v| pure(body, v))
            }
            HirKind::Assign { place, value } => match (&body.expr(place).kind, &body.expr(value).kind) {
                (HirKind::Local(x), HirKind::Local(y)) if x == y => true,
                (HirKind::Local(l), _) => !read.contains(l) && !body.local(*l).captured && pure(body, value),
                _ => false,
            },
            _ => false,
        };
        if gone {
            if let HirKind::Let { local, .. } = body.expr(k).kind {
                dropped.insert(local);
            }
            let e = &mut body.exprs[k.0 as usize];
            e.kind = HirKind::Block { stmts: Vec::new(), tail: None };
            e.node = None;
            *hits += 1;
        }
    }
    for k in clears {
        if let HirKind::Clear(ls) = &mut body.exprs[k.0 as usize].kind {
            ls.retain(|l| !dropped.contains(l));
        }
    }
}

fn post(body: &mut HirBody, id: HirId, hits: &mut usize) {
    for k in children(body, id) {
        post(body, k, hits);
    }
    merge_steps(body, id, hits);
    if let Some(kind) = rewrite(body, id)
        .or_else(|| square(body, id))
        .or_else(|| regroup(body, id))
        .or_else(|| known_variant(body, id))
    {
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
            identity(body, *op, *lhs, *rhs, e).or_else(|| literal_right(body, *op, *lhs, *rhs))
        }
        HirKind::Un { op: UnOp::Not, operand } => match &body.expr(*operand).kind {
            // `!!b` is `b` only for a bool: `!!n` on an int is `n != 0`.
            HirKind::Un { op: UnOp::Not, operand: inner } if body.expr(*inner).ty.as_ref().is_some_and(|t| named(t, "bool")) => {
                Some(body.expr(*inner).kind.clone())
            }
            HirKind::Lit(Lit::Bool(b)) => Some(HirKind::Lit(Lit::Bool(!b))),
            HirKind::Lit(Lit::Int(n)) if named(e.ty.as_ref()?, "bool") => Some(HirKind::Lit(Lit::Bool(*n == 0))),
            _ => None,
        },
        HirKind::Un { op: UnOp::Neg, operand } if named(e.ty.as_ref()?, "int") => match body.expr(*operand).kind {
            HirKind::Lit(Lit::Int(a)) => int_arith::neg(a).ok().map(|v| HirKind::Lit(Lit::Int(v))),
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

/// `match V(a, b) { V(x, _) => e, … }` is `e` with `x` read as `a`: the
/// arm the constructor takes, with no enum built. Arms of other variants
/// are skipped. Every field must be a literal, a local read or a unit
/// variant, and the arm
/// must not assign a binding or a field's local (so each read of `x` still
/// sees `a`).
fn known_variant(body: &mut HirBody, id: HirId) -> Option<HirKind> {
    let HirKind::Match { scrutinee, arms } = &body.expr(id).kind else { return None };
    let HirKind::Make { kind: MakeKind::Variant { enum_name, variant, .. }, args } = &body.expr(*scrutinee).kind else {
        return None;
    };
    if !args.iter().all(|&a| matches!(body.expr(a).kind, HirKind::Lit(_) | HirKind::Local(_)) || unit_variant(body, a)) {
        return None;
    }
    let (pats, arm) = arms.iter().find_map(|arm| match &arm.pat {
        HirPat::Wild => Some(Some((Vec::new(), arm.body))),
        HirPat::Variant { enum_name: e, variant: v, fields, .. } if e == enum_name && v == variant => {
            Some(match fields {
                HirPatFields::Unit if args.is_empty() => Some((Vec::new(), arm.body)),
                HirPatFields::Tuple(ps) if ps.len() == args.len() && ps.iter().all(|p| matches!(p, HirPat::Bind(_) | HirPat::Wild)) => {
                    Some((ps.clone(), arm.body))
                }
                _ => None,
            })
        }
        HirPat::Variant { enum_name: e, .. } if e == enum_name => None,
        _ => Some(None),
    })??;
    if body.expr(arm).ty != body.expr(id).ty || body.expr(arm).layout != body.expr(id).layout {
        return None;
    }
    let mut subst = Vec::new();
    let mut fixed = Vec::new();
    for (p, &a) in pats.iter().zip(args) {
        if let HirPat::Bind(x) = p {
            if body.local(*x).captured {
                return None;
            }
            subst.push((*x, body.expr(a).kind.clone()));
            fixed.push(*x);
            if let HirKind::Local(l) = body.expr(a).kind {
                fixed.push(l);
            }
        }
    }
    let mut reads = Vec::new();
    let mut stack = vec![arm];
    while let Some(k) = stack.pop() {
        match &body.expr(k).kind {
            HirKind::Assign { place, .. }
                if matches!(body.expr(*place).kind, HirKind::Local(l) if fixed.contains(&l)) =>
            {
                return None;
            }
            HirKind::Lambda { .. } | HirKind::Defer { .. } => return None,
            HirKind::Local(l) if subst.iter().any(|(x, _)| x == l) => reads.push(k),
            _ => {}
        }
        stack.extend(children(body, k));
    }
    for k in reads {
        let HirKind::Local(l) = body.expr(k).kind else { unreachable!() };
        let e = &mut body.exprs[k.0 as usize];
        e.kind = subst.iter().find(|(x, _)| *x == l).map(|(_, v)| v.clone()).expect("a bound read");
        e.node = None;
    }
    Some(body.expr(arm).kind.clone())
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
    let sum = a.checked_add(b)?;
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
            && let Some(sum) = a.checked_add(b)
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

/// `c op x` is `x op' c` for an `int` literal `c` and a commutative op (or a
/// flipped compare), so lowering fuses the slot-immediate form.
fn literal_right(body: &HirBody, op: BinOp, lhs: HirId, rhs: HirId) -> Option<HirKind> {
    if !matches!(body.expr(lhs).kind, HirKind::Lit(Lit::Int(_))) || matches!(body.expr(rhs).kind, HirKind::Lit(_)) {
        return None;
    }
    let op = match op {
        BinOp::IntAdd | BinOp::IntMul | BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor | BinOp::Eq | BinOp::Ne => op,
        BinOp::Lt => BinOp::Gt,
        BinOp::Gt => BinOp::Lt,
        BinOp::Le => BinOp::Ge,
        BinOp::Ge => BinOp::Le,
        _ => return None,
    };
    Some(HirKind::Bin { op, lhs: rhs, rhs: lhs })
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
            // A trap (overflow, division by zero, a bad shift count or
            // exponent) does not fold: the op stays and traps at run time.
            let r = match op {
                BinOp::IntAdd => int_arith::add(a, b),
                BinOp::IntSub => int_arith::sub(a, b),
                BinOp::IntMul => int_arith::mul(a, b),
                BinOp::IntDiv => int_arith::div(a, b),
                // An index lowers `i % n` as a Euclidean rem; the two agree
                // only on non-negative operands.
                BinOp::IntRem if a >= 0 && b > 0 => int_arith::rem(a, b),
                BinOp::BitAnd => Ok(a & b),
                BinOp::BitOr => Ok(a | b),
                BinOp::BitXor => Ok(a ^ b),
                BinOp::Shl => int_arith::shl(a, b),
                BinOp::Shr => int_arith::shr(a, b),
                BinOp::IntPow => int_arith::pow(a, b),
                _ => return None,
            };
            r.ok().map(Lit::Int)
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

fn named(t: &Ty, name: &str) -> bool {
    matches!(ty::strip_readonly(t), Ty::Con(n) if n == name)
}

#[cfg(test)]
#[path = "fold.tests.rs"]
mod tests;
