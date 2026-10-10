//! Lightweight const evaluation for bool loop conditions.

use parser::ast::{Expression, Output};

/// Foldable compile-time value used for loop-condition const eval.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ConstVal {
    Bool(bool),
    Int(i64),
    Float(f64),
}

impl ConstVal {
    pub fn as_bool(self) -> Option<bool> {
        match self {
            ConstVal::Bool(b) => Some(b),
            ConstVal::Int(i) => Some(i != 0),
            ConstVal::Float(f) => Some(f != 0.0),
        }
    }

    pub fn as_int(self) -> Option<i64> {
        match self {
            ConstVal::Int(i) => Some(i),
            ConstVal::Bool(b) => Some(if b { 1 } else { 0 }),
            ConstVal::Float(f) if f.fract() == 0.0 && f.is_finite() => Some(f as i64),
            _ => None,
        }
    }
}

/// Evaluate `expr` to a [`ConstVal`] when it is a pure compile-time form.
///
/// `lookup` resolves `const` bindings by name. Mutable `let` bindings are not folded.
pub fn eval_const(
    expr: &Output<'_>,
    lookup: &dyn Fn(&str) -> Option<ConstVal>,
) -> Option<ConstVal> {
    match expr.1.as_ref() {
        Expression::Statement(inner)
        | Expression::Expr(inner)
        | Expression::ExprStatement(inner)
        | Expression::Group(inner) => eval_const(inner, lookup),

        Expression::Bool(b) => Some(ConstVal::Bool(*b)),
        Expression::Integer(n) => Some(ConstVal::Int(*n)),
        Expression::Float(f) => Some(ConstVal::Float(*f)),

        Expression::Identifier(name) => lookup(name),

        Expression::Not(inner) | Expression::LogicalNot(inner) => {
            Some(ConstVal::Bool(!eval_const(inner, lookup)?.as_bool()?))
        }
        Expression::Negate(inner) => match eval_const(inner, lookup)? {
            ConstVal::Int(i) => Some(ConstVal::Int(i.checked_neg()?)),
            ConstVal::Float(f) => Some(ConstVal::Float(-f)),
            ConstVal::Bool(_) => None,
        },
        Expression::Positive(inner) => eval_const(inner, lookup),

        Expression::And(l, r) => {
            let left = eval_const(l, lookup)?.as_bool()?;
            if !left {
                return Some(ConstVal::Bool(false));
            }
            Some(ConstVal::Bool(eval_const(r, lookup)?.as_bool()?))
        }
        Expression::Or(l, r) => {
            let left = eval_const(l, lookup)?.as_bool()?;
            if left {
                return Some(ConstVal::Bool(true));
            }
            Some(ConstVal::Bool(eval_const(r, lookup)?.as_bool()?))
        }

        Expression::Eq(l, r) => cmp(l, r, lookup, |a, b| a == b, |a, b| a == b, |a, b| a == b),
        Expression::Neq(l, r) => cmp(l, r, lookup, |a, b| a != b, |a, b| a != b, |a, b| a != b),
        Expression::Le(l, r) => cmp(l, r, lookup, |a, b| a < b, |a, b| a < b, |_, _| false),
        Expression::Gt(l, r) => cmp(l, r, lookup, |a, b| a > b, |a, b| a > b, |_, _| false),
        Expression::Leq(l, r) => cmp(l, r, lookup, |a, b| a <= b, |a, b| a <= b, |_, _| false),
        Expression::Geq(l, r) => cmp(l, r, lookup, |a, b| a >= b, |a, b| a >= b, |_, _| false),

        Expression::Add(l, r) => arith(l, r, lookup, |a, b| a.checked_add(b), |a, b| a + b),
        Expression::Sub(l, r) => arith(l, r, lookup, |a, b| a.checked_sub(b), |a, b| a - b),
        Expression::Mul(l, r) => arith(l, r, lookup, |a, b| a.checked_mul(b), |a, b| a * b),
        Expression::Div(l, r) => arith(
            l,
            r,
            lookup,
            |a, b| a.checked_div(b),
            |a, b| if b != 0.0 { a / b } else { f64::NAN },
        ),
        Expression::Mod(l, r) => arith(
            l,
            r,
            lookup,
            |a, b| (b != 0).then_some(a.wrapping_rem(b)),
            |_, _| f64::NAN,
        ),

        Expression::Tuple(items) if items.is_empty() => Some(ConstVal::Int(0)),
        _ => None,
    }
}

/// Evaluate a condition to `Some(true)` / `Some(false)` when foldable.
pub fn eval_bool_const(
    expr: &Output<'_>,
    lookup: &dyn Fn(&str) -> Option<ConstVal>,
) -> Option<bool> {
    eval_const(expr, lookup)?.as_bool()
}

fn cmp(
    l: &Output<'_>,
    r: &Output<'_>,
    lookup: &dyn Fn(&str) -> Option<ConstVal>,
    on_int: impl Fn(i64, i64) -> bool,
    on_float: impl Fn(f64, f64) -> bool,
    on_bool: impl Fn(bool, bool) -> bool,
) -> Option<ConstVal> {
    let lv = eval_const(l, lookup)?;
    let rv = eval_const(r, lookup)?;
    if let (Some(a), Some(b)) = (lv.as_int(), rv.as_int()) {
        return Some(ConstVal::Bool(on_int(a, b)));
    }
    match (lv, rv) {
        (ConstVal::Float(a), ConstVal::Float(b)) => Some(ConstVal::Bool(on_float(a, b))),
        (ConstVal::Float(a), ConstVal::Int(b)) => Some(ConstVal::Bool(on_float(a, b as f64))),
        (ConstVal::Int(a), ConstVal::Float(b)) => Some(ConstVal::Bool(on_float(a as f64, b))),
        (ConstVal::Bool(a), ConstVal::Bool(b)) => Some(ConstVal::Bool(on_bool(a, b))),
        _ => None,
    }
}

fn arith(
    l: &Output<'_>,
    r: &Output<'_>,
    lookup: &dyn Fn(&str) -> Option<ConstVal>,
    on_int: impl Fn(i64, i64) -> Option<i64>,
    on_float: impl Fn(f64, f64) -> f64,
) -> Option<ConstVal> {
    let lv = eval_const(l, lookup)?;
    let rv = eval_const(r, lookup)?;
    if let (Some(a), Some(b)) = (lv.as_int(), rv.as_int()) {
        return Some(ConstVal::Int(on_int(a, b)?));
    }
    let (a, b) = match (lv, rv) {
        (ConstVal::Float(a), ConstVal::Float(b)) => (a, b),
        (ConstVal::Float(a), ConstVal::Int(b)) => (a, b as f64),
        (ConstVal::Int(a), ConstVal::Float(b)) => (a as f64, b),
        _ => return None,
    };
    let v = on_float(a, b);
    if v.is_nan() {
        None
    } else {
        Some(ConstVal::Float(v))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use parser::SimpleSpan;
    use parser::ast::Expression;

    fn out<'a>(expr: Expression<'a>) -> Output<'a> {
        (SimpleSpan::from(0..0), Box::new(expr))
    }

    #[test]
    fn folds_true_literal() {
        let e = out(Expression::Bool(true));
        assert_eq!(eval_bool_const(&e, &|_| None), Some(true));
    }

    #[test]
    fn folds_eq_ints() {
        let e = out(Expression::Eq(
            out(Expression::Integer(1)),
            out(Expression::Integer(1)),
        ));
        assert_eq!(eval_bool_const(&e, &|_| None), Some(true));
    }

    #[test]
    fn folds_const_binding() {
        let e = out(Expression::Identifier("flag"));
        assert_eq!(
            eval_bool_const(&e, &|n| (n == "flag").then_some(ConstVal::Bool(true))),
            Some(true)
        );
    }

    #[test]
    fn and_short_circuits_on_false_left() {
        // Right side is non-foldable; left false must still yield false.
        let e = out(Expression::And(
            out(Expression::Bool(false)),
            out(Expression::Identifier("unknown")),
        ));
        assert_eq!(eval_bool_const(&e, &|_| None), Some(false));
    }

    #[test]
    fn or_short_circuits_on_true_left() {
        let e = out(Expression::Or(
            out(Expression::Bool(true)),
            out(Expression::Identifier("unknown")),
        ));
        assert_eq!(eval_bool_const(&e, &|_| None), Some(true));
    }

    #[test]
    fn int_nonzero_is_truthy_for_loop_conds() {
        let e = out(Expression::Integer(1));
        assert_eq!(eval_bool_const(&e, &|_| None), Some(true));
        let z = out(Expression::Integer(0));
        assert_eq!(eval_bool_const(&z, &|_| None), Some(false));
    }
}
