//! Compile-time scalar constant evaluation for codegen optimizations.

use std::collections::HashMap;

use parser::{
    ast::{Expression, Output},
    SimpleSpan,
};

/// A scalar value known at compile time.
#[derive(Debug, Clone, PartialEq)]
pub enum ConstValue {
    Int(i64),
    Float(f64),
    Bool(bool),
    Str(String),
}

/// Evaluate a pure expression using `env` for const identifiers.
pub fn eval_expr<'a>(
    ast: &(SimpleSpan, Box<Expression<'a>>),
    env: &HashMap<String, ConstValue>,
) -> Option<ConstValue> {
    match ast.1.as_ref() {
        Expression::Integer(n) => Some(ConstValue::Int(*n)),
        Expression::Float(n) => Some(ConstValue::Float(*n)),
        Expression::Bool(b) => Some(ConstValue::Bool(*b)),
        Expression::String(s) => Some(ConstValue::Str(
            s.replace("\\n", "\n")
                .replace("\\r", "\r")
                .replace("\\t", "\t")
                .replace("\\0", "\0"),
        )),
        Expression::Identifier(name) => env.get(*name).cloned(),
        Expression::Group(inner) | Expression::Expr(inner) | Expression::Statement(inner) => {
            eval_expr(inner, env)
        }
        Expression::Positive(inner) => eval_expr(inner, env),
        Expression::Negate(inner) => {
            let v = eval_expr(inner, env)?;
            match v {
                ConstValue::Int(n) => Some(ConstValue::Int(-n)),
                ConstValue::Float(n) => Some(ConstValue::Float(-n)),
                _ => None,
            }
        }
        Expression::Not(inner) | Expression::LogicalNot(inner) => {
            let v = eval_expr(inner, env)?;
            match v {
                ConstValue::Int(n) => Some(ConstValue::Bool(n == 0)),
                ConstValue::Bool(b) => Some(ConstValue::Bool(!b)),
                _ => None,
            }
        }
        // Each operand is evaluated once: trying string concat and then the
        // numeric op re-evaluated both sides, doubling the work per level of
        // a left-nested `a + b + c + …` chain.
        Expression::Add(lhs, rhs) => {
            let a = eval_expr(lhs, env)?;
            let b = eval_expr(rhs, env)?;
            match (a, b) {
                (ConstValue::Str(x), ConstValue::Str(y)) => Some(ConstValue::Str(format!("{x}{y}"))),
                (ConstValue::Int(x), ConstValue::Int(y)) => Some(ConstValue::Int(x + y)),
                (ConstValue::Float(x), ConstValue::Float(y)) => Some(ConstValue::Float(x + y)),
                _ => None,
            }
        }
        Expression::Sub(lhs, rhs) => eval_binop(lhs, rhs, env, |a, b| a - b, |a, b| a - b),
        Expression::Mul(lhs, rhs) => eval_binop(lhs, rhs, env, |a, b| a * b, |a, b| a * b),
        Expression::Div(lhs, rhs) => {
            let a = eval_expr(lhs, env)?;
            let b = eval_expr(rhs, env)?;
            match (a, b) {
                (ConstValue::Int(x), ConstValue::Int(y)) if y != 0 => Some(ConstValue::Int(x / y)),
                (ConstValue::Float(x), ConstValue::Float(y)) if y != 0.0 && y.is_finite() => {
                    Some(ConstValue::Float(x / y))
                }
                _ => None,
            }
        }
        Expression::Mod(lhs, rhs) => {
            let a = eval_expr(lhs, env)?;
            let b = eval_expr(rhs, env)?;
            match (a, b) {
                (ConstValue::Int(x), ConstValue::Int(y)) if y != 0 => Some(ConstValue::Int(x % y)),
                _ => None,
            }
        }
        Expression::Le(lhs, rhs) => eval_cmp(lhs, rhs, env, |a, b| a < b),
        Expression::Gt(lhs, rhs) => eval_cmp(lhs, rhs, env, |a, b| a > b),
        Expression::Leq(lhs, rhs) => eval_cmp(lhs, rhs, env, |a, b| a <= b),
        Expression::Geq(lhs, rhs) => eval_cmp(lhs, rhs, env, |a, b| a >= b),
        Expression::Eq(lhs, rhs) => eval_eq(lhs, rhs, env),
        Expression::Neq(lhs, rhs) => {
            eval_eq(lhs, rhs, env).map(|b| ConstValue::Bool(!matches!(b, ConstValue::Bool(true))))
        }
        Expression::BitAnd(lhs, rhs) => eval_int_bit(lhs, rhs, env, |a, b| a & b),
        Expression::BitOr(lhs, rhs) => eval_int_bit(lhs, rhs, env, |a, b| a | b),
        Expression::Xor(lhs, rhs) => eval_int_bit(lhs, rhs, env, |a, b| a ^ b),
        Expression::Shl(lhs, rhs) => eval_int_shift(lhs, rhs, env, true),
        Expression::Shr(lhs, rhs) => eval_int_shift(lhs, rhs, env, false),
        Expression::Call { name, args } => eval_len_call(name, args.as_deref(), env),
        Expression::TypeOf(_) => None,
        _ => None,
    }
}

/// Fold `len(...)` when the operand's length is known from a literal shape
/// or a const string binding.
fn eval_len_call<'a>(
    name: &Output<'a>,
    args: Option<&[Output<'a>]>,
    env: &HashMap<String, ConstValue>,
) -> Option<ConstValue> {
    let Expression::Identifier("len") = name.1.as_ref() else {
        return None;
    };
    let args = args?;
    if args.len() != 1 {
        return None;
    }
    eval_len_operand(&args[0], env)
}

fn eval_len_operand<'a>(ast: &Output<'a>, env: &HashMap<String, ConstValue>) -> Option<ConstValue> {
    match ast.1.as_ref() {
        Expression::String(s) => {
            let unescaped = s
                .replace("\\n", "\n")
                .replace("\\r", "\r")
                .replace("\\t", "\t")
                .replace("\\0", "\0");
            Some(ConstValue::Int(unescaped.len() as i64))
        }
        Expression::Array(items) | Expression::Tuple(items) => {
            Some(ConstValue::Int(items.len() as i64))
        }
        Expression::Dict(fields) => Some(ConstValue::Int(fields.len() as i64)),
        Expression::Group(inner) | Expression::Expr(inner) | Expression::Statement(inner) => {
            eval_len_operand(inner, env)
        }
        Expression::Identifier(name) => match env.get(*name)? {
            ConstValue::Str(s) => Some(ConstValue::Int(s.len() as i64)),
            _ => None,
        },
        _ => None,
    }
}

fn eval_binop<'a>(
    lhs: &Output<'a>,
    rhs: &Output<'a>,
    env: &HashMap<String, ConstValue>,
    int_op: fn(i64, i64) -> i64,
    float_op: fn(f64, f64) -> f64,
) -> Option<ConstValue> {
    let a = eval_expr(lhs, env)?;
    let b = eval_expr(rhs, env)?;
    match (a, b) {
        (ConstValue::Int(x), ConstValue::Int(y)) => Some(ConstValue::Int(int_op(x, y))),
        (ConstValue::Float(x), ConstValue::Float(y)) => Some(ConstValue::Float(float_op(x, y))),
        _ => None,
    }
}

fn eval_int_bit<'a>(
    lhs: &Output<'a>,
    rhs: &Output<'a>,
    env: &HashMap<String, ConstValue>,
    op: fn(i64, i64) -> i64,
) -> Option<ConstValue> {
    let ConstValue::Int(a) = eval_expr(lhs, env)? else {
        return None;
    };
    let ConstValue::Int(b) = eval_expr(rhs, env)? else {
        return None;
    };
    Some(ConstValue::Int(op(a, b)))
}

/// Fold `<<` / `>>` only when the shift is in `0..32` (VM `i32` shift).
fn eval_int_shift<'a>(
    lhs: &Output<'a>,
    rhs: &Output<'a>,
    env: &HashMap<String, ConstValue>,
    left: bool,
) -> Option<ConstValue> {
    let ConstValue::Int(a) = eval_expr(lhs, env)? else {
        return None;
    };
    let ConstValue::Int(b) = eval_expr(rhs, env)? else {
        return None;
    };
    if !(0..32).contains(&b) {
        return None;
    }
    let a32 = a as i32;
    let n = b as u32;
    let r = if left {
        a32.wrapping_shl(n)
    } else {
        a32.wrapping_shr(n)
    };
    Some(ConstValue::Int(r as i64))
}

fn eval_cmp<'a>(
    lhs: &Output<'a>,
    rhs: &Output<'a>,
    env: &HashMap<String, ConstValue>,
    cmp: fn(i64, i64) -> bool,
) -> Option<ConstValue> {
    let a = eval_expr(lhs, env)?;
    let b = eval_expr(rhs, env)?;
    match (a, b) {
        (ConstValue::Int(x), ConstValue::Int(y)) => Some(ConstValue::Bool(cmp(x, y))),
        _ => None,
    }
}

fn eval_eq<'a>(
    lhs: &Output<'a>,
    rhs: &Output<'a>,
    env: &HashMap<String, ConstValue>,
) -> Option<ConstValue> {
    let a = eval_expr(lhs, env)?;
    let b = eval_expr(rhs, env)?;
    Some(ConstValue::Bool(a == b))
}

/// Integer strength-reduction hint: `x * k` when k is a positive power of
/// two → shift left by `trailing_zeros(k)`.
pub fn strength_mul_int(k: i64) -> Option<u32> {
    if k > 0 && (k & (k - 1)) == 0 {
        Some(k.trailing_zeros())
    } else {
        None
    }
}

/// Integer strength-reduction hint: `x / k` when k is a positive power of
/// two → shift right by `trailing_zeros(k)`.
pub fn strength_div_int(k: i64) -> Option<u32> {
    strength_mul_int(k).filter(|&shift| shift > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use parser::ast::Expression;

    fn int_expr(n: i64) -> Output<'static> {
        (SimpleSpan::from(0..1), Box::new(Expression::Integer(n)))
    }

    fn id_expr(name: &'static str) -> Output<'static> {
        (
            SimpleSpan::from(0..1),
            Box::new(Expression::Identifier(name)),
        )
    }

    /// A long left-nested `"s" + x + "s" + x …` chain with an unknown operand
    /// must not re-evaluate operands (it was exponential in the chain length).
    #[test]
    fn long_add_chain_with_unknown_operand_is_linear() {
        let mut e: Output<'static> = (SimpleSpan::from(0..1), Box::new(Expression::String("s")));
        for i in 0..200 {
            let rhs = if i % 2 == 0 { id_expr("x") } else { (SimpleSpan::from(0..1), Box::new(Expression::String("s"))) };
            e = (SimpleSpan::from(0..1), Box::new(Expression::Add(e, rhs)));
        }
        let start = std::time::Instant::now();
        assert_eq!(eval_expr(&e, &HashMap::new()), None);
        assert!(start.elapsed() < std::time::Duration::from_secs(1));
    }

    fn float_expr(n: f64) -> Output<'static> {
        (SimpleSpan::from(0..1), Box::new(Expression::Float(n)))
    }

    #[test]
    fn fold_float_unary_neg() {
        let env = HashMap::new();
        let neg = (
            SimpleSpan::from(0..4),
            Box::new(Expression::Negate(float_expr(0.55))),
        );
        assert_eq!(eval_expr(&neg, &env), Some(ConstValue::Float(-0.55)));
    }

    #[test]
    fn fold_add_and_cmp() {
        let env = HashMap::new();
        let add = (
            SimpleSpan::from(0..3),
            Box::new(Expression::Add(int_expr(5), int_expr(5))),
        );
        assert_eq!(eval_expr(&add, &env), Some(ConstValue::Int(10)));
        let cmp = (
            SimpleSpan::from(0..3),
            Box::new(Expression::Le(int_expr(4), int_expr(5))),
        );
        assert_eq!(eval_expr(&cmp, &env), Some(ConstValue::Bool(true)));
    }

    /// `Expression::Le` is `<` (not `<=`). Equality must stay false.
    #[test]
    fn le_is_strict_less_than_not_leq() {
        let env = HashMap::new();
        let eq_boundary = (
            SimpleSpan::from(0..3),
            Box::new(Expression::Le(int_expr(5), int_expr(5))),
        );
        assert_eq!(
            eval_expr(&eq_boundary, &env),
            Some(ConstValue::Bool(false)),
            "`5 < 5` must fold to false (Le is strict <)"
        );
        let leq = (
            SimpleSpan::from(0..3),
            Box::new(Expression::Leq(int_expr(5), int_expr(5))),
        );
        assert_eq!(
            eval_expr(&leq, &env),
            Some(ConstValue::Bool(true)),
            "`5 <= 5` must fold to true"
        );
    }

    #[test]
    fn fold_strict_lt_boundary() {
        let env = HashMap::new();
        let cmp = (
            SimpleSpan::from(0..3),
            Box::new(Expression::Le(int_expr(5), int_expr(5))),
        );
        assert_eq!(eval_expr(&cmp, &env), Some(ConstValue::Bool(false)));
    }

    #[test]
    fn const_ident_in_env() {
        let mut env = HashMap::new();
        env.insert("x".into(), ConstValue::Int(5));
        let id: Output = (
            SimpleSpan::from(0..1),
            Box::new(Expression::Identifier("x")),
        );
        let add = (
            SimpleSpan::from(0..3),
            Box::new(Expression::Add(id, int_expr(5))),
        );
        assert_eq!(eval_expr(&add, &env), Some(ConstValue::Int(10)));
    }

    #[test]
    fn div_and_mod_by_zero_do_not_fold() {
        let env = HashMap::new();
        let div0 = (
            SimpleSpan::from(0..3),
            Box::new(Expression::Div(int_expr(10), int_expr(0))),
        );
        let mod0 = (
            SimpleSpan::from(0..3),
            Box::new(Expression::Mod(int_expr(10), int_expr(0))),
        );
        assert_eq!(eval_expr(&div0, &env), None);
        assert_eq!(eval_expr(&mod0, &env), None);
    }

    #[test]
    fn strength_mul_int_only_powers_of_two() {
        assert_eq!(strength_mul_int(8), Some(3));
        assert_eq!(strength_mul_int(1), Some(0));
        assert_eq!(strength_mul_int(6), None);
        assert_eq!(strength_mul_int(0), None);
        assert_eq!(strength_mul_int(-4), None);
    }

    #[test]
    fn strength_div_int_only_positive_powers_of_two() {
        assert_eq!(strength_div_int(2), Some(1));
        assert_eq!(strength_div_int(4), Some(2));
        assert_eq!(strength_div_int(3), None);
        assert_eq!(strength_div_int(0), None);
        assert_eq!(strength_div_int(-2), None);
        assert_eq!(strength_div_int(1), None);
    }

    #[test]
    fn eval_bitand_const() {
        let env = HashMap::new();
        let and = (
            SimpleSpan::from(0..3),
            Box::new(Expression::BitAnd(int_expr(0xFF), int_expr(3))),
        );
        assert_eq!(eval_expr(&and, &env), Some(ConstValue::Int(3)));
    }

    #[test]
    fn string_add_folds_concatenation() {
        let env = HashMap::new();
        let lhs = (SimpleSpan::from(0..1), Box::new(Expression::String("he")));
        let rhs = (SimpleSpan::from(0..1), Box::new(Expression::String("llo")));
        let add = (SimpleSpan::from(0..1), Box::new(Expression::Add(lhs, rhs)));
        assert_eq!(eval_expr(&add, &env), Some(ConstValue::Str("hello".into())));
    }

    #[test]
    fn len_folds_string_array_tuple_literals() {
        let env = HashMap::new();
        let call = |arg: Output<'static>| -> Output<'static> {
            (
                SimpleSpan::from(0..8),
                Box::new(Expression::Call {
                    name: (
                        SimpleSpan::from(0..3),
                        Box::new(Expression::Identifier("len")),
                    ),
                    args: Some(vec![arg]),
                }),
            )
        };
        assert_eq!(
            eval_expr(
                &call((SimpleSpan::from(0..3), Box::new(Expression::String("foo")))),
                &env
            ),
            Some(ConstValue::Int(3))
        );
        assert_eq!(
            eval_expr(
                &call((
                    SimpleSpan::from(0..5),
                    Box::new(Expression::Array(vec![int_expr(1), int_expr(2)])),
                )),
                &env
            ),
            Some(ConstValue::Int(2))
        );
        assert_eq!(
            eval_expr(
                &call((
                    SimpleSpan::from(0..5),
                    Box::new(Expression::Tuple(vec![
                        int_expr(1),
                        int_expr(2),
                        int_expr(3)
                    ])),
                )),
                &env
            ),
            Some(ConstValue::Int(3))
        );
    }

    #[test]
    fn len_folds_dict_escapes_grouped_and_env_string() {
        let call = |arg: Output<'static>| -> Output<'static> {
            (
                SimpleSpan::from(0..8),
                Box::new(Expression::Call {
                    name: (
                        SimpleSpan::from(0..3),
                        Box::new(Expression::Identifier("len")),
                    ),
                    args: Some(vec![arg]),
                }),
            )
        };
        let env = HashMap::new();
        let dict = (
            SimpleSpan::from(0..9),
            Box::new(Expression::Dict(vec![
                parser::ast::RecordFieldValue {
                    name: "a",
                    value: int_expr(1),
                },
                parser::ast::RecordFieldValue {
                    name: "b",
                    value: int_expr(2),
                },
            ])),
        );
        assert_eq!(eval_expr(&call(dict), &env), Some(ConstValue::Int(2)));

        assert_eq!(
            eval_expr(
                &call((
                    SimpleSpan::from(0..4),
                    Box::new(Expression::String("a\\nb")),
                )),
                &env
            ),
            Some(ConstValue::Int(3)),
            "escape sequences count as one byte each after unescape"
        );

        let grouped = (
            SimpleSpan::from(0..5),
            Box::new(Expression::Group((
                SimpleSpan::from(0..3),
                Box::new(Expression::String("hi")),
            ))),
        );
        assert_eq!(eval_expr(&call(grouped), &env), Some(ConstValue::Int(2)));

        let mut env = HashMap::new();
        env.insert("s".into(), ConstValue::Str("xyz".into()));
        assert_eq!(
            eval_expr(&call(id_expr("s")), &env),
            Some(ConstValue::Int(3))
        );
        env.insert("n".into(), ConstValue::Int(9));
        assert_eq!(
            eval_expr(&call(id_expr("n")), &env),
            None,
            "non-string const bindings must not fold"
        );
    }
}
