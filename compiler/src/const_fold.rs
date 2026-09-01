//! Compile-time scalar constant evaluation for codegen optimizations.

use std::collections::HashMap;

use parser::{
    SimpleSpan,
    ast::{Expression, Output},
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
        Expression::Add(lhs, rhs) => eval_string_add(lhs, rhs, env)
            .or_else(|| eval_binop(lhs, rhs, env, |a, b| a + b, |a, b| a + b)),
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

fn eval_len_operand<'a>(
    ast: &Output<'a>,
    env: &HashMap<String, ConstValue>,
) -> Option<ConstValue> {
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

/// String concatenation when both sides are known strings.
pub fn eval_string_add<'a>(
    lhs: &Output<'a>,
    rhs: &Output<'a>,
    env: &HashMap<String, ConstValue>,
) -> Option<ConstValue> {
    let a = eval_expr(lhs, env)?;
    let b = eval_expr(rhs, env)?;
    match (a, b) {
        (ConstValue::Str(x), ConstValue::Str(y)) => Some(ConstValue::Str(format!("{x}{y}"))),
        _ => None,
    }
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

/// If `expr` is `x * 2^n` or `2^n * x` (n ≥ 1), return `(x, n)` so codegen
/// can emit `SHL` instead of `MUL`. `n == 0` (`* 1`) is left to
/// [`strength_reduced_inner`].
pub fn strength_mul_to_shl<'a>(
    expr: &'a (SimpleSpan, Box<Expression<'a>>),
    env: &HashMap<String, ConstValue>,
) -> Option<(&'a Output<'a>, u32)> {
    let Expression::Mul(lhs, rhs) = expr.1.as_ref() else {
        return None;
    };
    let shift_for = |side: &Output<'a>| -> Option<u32> {
        match eval_expr(side, env)? {
            ConstValue::Int(k) => {
                let shift = strength_mul_int(k)?;
                if shift == 0 { None } else { Some(shift) }
            }
            _ => None,
        }
    };
    if let Some(shift) = shift_for(rhs) {
        return Some((lhs, shift));
    }
    if let Some(shift) = shift_for(lhs) {
        return Some((rhs, shift));
    }
    None
}

/// Integer strength-reduction hint: `x / k` when k is a positive power of
/// two → shift right by `trailing_zeros(k)`.
pub fn strength_div_int(k: i64) -> Option<u32> {
    strength_mul_int(k).filter(|&shift| shift > 0)
}

/// If `expr` is `x / 2^n` (n ≥ 1), return `(x, n)` so codegen can emit `SHR`
/// when the dividend is provably non-negative. Divisor-on-the-left (`k / x`)
/// is not a shift.
pub fn strength_div_to_shr<'a>(
    expr: &'a (SimpleSpan, Box<Expression<'a>>),
    env: &HashMap<String, ConstValue>,
) -> Option<(&'a Output<'a>, u32)> {
    let Expression::Div(lhs, rhs) = expr.1.as_ref() else {
        return None;
    };
    let ConstValue::Int(k) = eval_expr(rhs, env)? else {
        return None;
    };
    let shift = strength_div_int(k)?;
    Some((lhs, shift))
}

/// `true` when `expr` folds to an integer ≥ 0 (safe for signed `>>` ≡ `/ 2^n`).
pub fn strength_div_dividend_nonneg(
    expr: &Output<'_>,
    env: &HashMap<String, ConstValue>,
) -> bool {
    matches!(eval_expr(expr, env), Some(ConstValue::Int(k)) if k >= 0)
}

/// Result of [`strength_reduce_bitops`]. Does not return [`crate::il::IlOp`]:
/// const-fold stays AST-level; codegen emits CONST / the inner expr.
#[derive(Debug, Clone, Copy)]
pub enum StrengthBitop<'a> {
    /// Drop the bitop; the remaining operand is the result.
    Identity(&'a Output<'a>),
    /// Fold to an integer constant (operand is a trivial identifier / literal).
    Const(i64),
}

fn int_imm(expr: &Output<'_>, env: &HashMap<String, ConstValue>) -> Option<i64> {
    match eval_expr(expr, env)? {
        ConstValue::Int(k) => Some(k),
        _ => None,
    }
}

fn is_trivial_operand(expr: &Output<'_>) -> bool {
    match expr.1.as_ref() {
        Expression::Integer(_) | Expression::Identifier(_) | Expression::Bool(_) => true,
        Expression::Group(inner) | Expression::Expr(inner) | Expression::Positive(inner) => {
            is_trivial_operand(inner)
        }
        _ => false,
    }
}

fn same_ident<'a>(a: &Output<'a>, b: &Output<'a>) -> bool {
    match (a.1.as_ref(), b.1.as_ref()) {
        (Expression::Identifier(x), Expression::Identifier(y)) => x == y,
        _ => false,
    }
}

/// i32 all-ones (`-1` or `0xFFFF_FFFF` as a positive i64 literal).
fn is_i32_all_ones(k: i64) -> bool {
    k == -1 || k == 0xFFFF_FFFF
}

/// Common bitwise identities / annihilators. Side-effecting operands are refused
/// so `f() & 0` still evaluates `f()`.
pub fn strength_reduce_bitops<'a>(
    expr: &'a (SimpleSpan, Box<Expression<'a>>),
    env: &HashMap<String, ConstValue>,
) -> Option<StrengthBitop<'a>> {
    match expr.1.as_ref() {
        Expression::BitAnd(lhs, rhs) => bitand_reduce(lhs, rhs, env),
        Expression::BitOr(lhs, rhs) => bitor_reduce(lhs, rhs, env),
        Expression::Xor(lhs, rhs) => xor_reduce(lhs, rhs, env),
        Expression::Shl(lhs, rhs) | Expression::Shr(lhs, rhs) => {
            if int_imm(rhs, env) == Some(0) {
                Some(StrengthBitop::Identity(lhs))
            } else {
                None
            }
        }
        _ => None,
    }
}

fn bitand_reduce<'a>(
    lhs: &'a Output<'a>,
    rhs: &'a Output<'a>,
    env: &HashMap<String, ConstValue>,
) -> Option<StrengthBitop<'a>> {
    if same_ident(lhs, rhs) {
        return Some(StrengthBitop::Identity(lhs));
    }
    let (x, k) = match (int_imm(lhs, env), int_imm(rhs, env)) {
        (Some(k), None) if is_trivial_operand(rhs) => (rhs, k),
        (None, Some(k)) if is_trivial_operand(lhs) => (lhs, k),
        _ => return None,
    };
    if k == 0 {
        return Some(StrengthBitop::Const(0));
    }
    if is_i32_all_ones(k) {
        return Some(StrengthBitop::Identity(x));
    }
    None
}

fn bitor_reduce<'a>(
    lhs: &'a Output<'a>,
    rhs: &'a Output<'a>,
    env: &HashMap<String, ConstValue>,
) -> Option<StrengthBitop<'a>> {
    if same_ident(lhs, rhs) {
        return Some(StrengthBitop::Identity(lhs));
    }
    let (x, k) = match (int_imm(lhs, env), int_imm(rhs, env)) {
        (Some(k), None) if is_trivial_operand(rhs) => (rhs, k),
        (None, Some(k)) if is_trivial_operand(lhs) => (lhs, k),
        _ => return None,
    };
    if k == 0 {
        return Some(StrengthBitop::Identity(x));
    }
    if is_i32_all_ones(k) {
        return Some(StrengthBitop::Const(-1));
    }
    None
}

fn xor_reduce<'a>(
    lhs: &'a Output<'a>,
    rhs: &'a Output<'a>,
    env: &HashMap<String, ConstValue>,
) -> Option<StrengthBitop<'a>> {
    if same_ident(lhs, rhs) {
        return Some(StrengthBitop::Const(0));
    }
    let (x, k) = match (int_imm(lhs, env), int_imm(rhs, env)) {
        (Some(k), None) if is_trivial_operand(rhs) => (rhs, k),
        (None, Some(k)) if is_trivial_operand(lhs) => (lhs, k),
        _ => return None,
    };
    if k == 0 {
        return Some(StrengthBitop::Identity(x));
    }
    None
}

/// If `expr` is `x + 0`, `x - 0`, `x * 1`, `x / 1`, `x % 1` (when defined), return inner.
pub fn strength_reduced_inner<'a>(
    expr: &'a (SimpleSpan, Box<Expression<'a>>),
) -> Option<&'a Output<'a>> {
    match expr.1.as_ref() {
        Expression::Add(lhs, rhs) => zero_int(rhs)
            .map(|_| lhs)
            .or_else(|| zero_int(lhs).map(|_| rhs)),
        Expression::Sub(lhs, rhs) if zero_int(rhs).is_some() => Some(lhs),
        Expression::Mul(lhs, rhs) => one_int(rhs)
            .map(|_| lhs)
            .or_else(|| one_int(lhs).map(|_| rhs)),
        Expression::Div(lhs, rhs) if one_int(rhs).is_some() => Some(lhs),
        // `x ** 1` → `x` (exponent identity).
        Expression::Pow(lhs, rhs) if one_int(rhs).is_some() => Some(lhs),
        _ => None,
    }
}

/// Integer `x ** k` strength-reduction kind for codegen / IL peeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StrengthPow {
    /// `x ** 0` → `1` (including `0 ** 0`, matching VM `i32::pow`).
    ConstOne,
    /// `x ** 2` → `x * x` (caller must ensure base is side-effect free / dup-safe).
    Square,
}

/// If `expr` is `base ** k` with small constant `k`, return `(base, kind)`.
pub fn strength_pow_int<'a>(
    expr: &'a (SimpleSpan, Box<Expression<'a>>),
    env: &HashMap<String, ConstValue>,
) -> Option<(&'a Output<'a>, StrengthPow)> {
    let Expression::Pow(lhs, rhs) = expr.1.as_ref() else {
        return None;
    };
    let ConstValue::Int(k) = eval_expr(rhs, env)? else {
        return None;
    };
    match k {
        0 => Some((lhs, StrengthPow::ConstOne)),
        2 => Some((lhs, StrengthPow::Square)),
        _ => None,
    }
}

fn zero_int<'a>(expr: &Output<'a>) -> Option<()> {
    eval_expr(expr, &HashMap::new()).and_then(|v| match v {
        ConstValue::Int(0) => Some(()),
        _ => None,
    })
}

fn one_int<'a>(expr: &Output<'a>) -> Option<()> {
    eval_expr(expr, &HashMap::new()).and_then(|v| match v {
        ConstValue::Int(1) => Some(()),
        _ => None,
    })
}

/// Range `start..end` inclusive/exclusive trip count (cap 8).
pub fn range_trip_count<'a>(start: &Output<'a>, end: &Output<'a>, inclusive: bool) -> Option<u32> {
    let ConstValue::Int(s) = eval_expr(start, &HashMap::new())? else {
        return None;
    };
    let ConstValue::Int(e) = eval_expr(end, &HashMap::new())? else {
        return None;
    };
    let count = if inclusive {
        e.saturating_sub(s).saturating_add(1)
    } else {
        e.saturating_sub(s)
    };
    if count < 0 || count > 8 {
        return None;
    }
    Some(count as u32)
}

/// Body contains break/continue — skip unroll.
pub fn body_has_loop_control<'a>(body: &Output<'a>) -> bool {
    body_has_loop_control_walk(body)
}

fn body_has_loop_control_walk<'a>(node: &Output<'a>) -> bool {
    use parser::ast::Expression;
    match node.1.as_ref() {
        Expression::Break | Expression::Continue => true,
        Expression::Block(children) => children.iter().any(body_has_loop_control_walk),
        Expression::Fragment(children) => children.iter().any(body_has_loop_control_walk),
        Expression::ExprStatement(inner)
        | Expression::Statement(inner)
        | Expression::Expr(inner)
        | Expression::Group(inner) => body_has_loop_control_walk(inner),
        Expression::If(branches) => branches.iter().any(|b| {
            if let Expression::Branch(cond, body) = b.1.as_ref() {
                cond.as_ref().is_some_and(body_has_loop_control_walk)
                    || body_has_loop_control_walk(body)
            } else {
                false
            }
        }),
        Expression::Match { scrutinee, arms } => {
            body_has_loop_control_walk(scrutinee)
                || arms.iter().any(|arm| body_has_loop_control_walk(&arm.body))
        }
        Expression::Loop { body, .. } => body_has_loop_control_walk(body),
        _ => false,
    }
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
    fn strength_mul_to_shl_rewrites_power_of_two_factor() {
        let env = HashMap::new();
        let mul8 = (
            SimpleSpan::from(0..3),
            Box::new(Expression::Mul(id_expr("x"), int_expr(8))),
        );
        let mul2_lhs = (
            SimpleSpan::from(0..3),
            Box::new(Expression::Mul(int_expr(2), id_expr("x"))),
        );
        let mul6 = (
            SimpleSpan::from(0..3),
            Box::new(Expression::Mul(id_expr("x"), int_expr(6))),
        );
        let mul1 = (
            SimpleSpan::from(0..3),
            Box::new(Expression::Mul(id_expr("x"), int_expr(1))),
        );
        let (inner, shift) = strength_mul_to_shl(&mul8, &env).expect("x*8");
        assert!(matches!(inner.1.as_ref(), Expression::Identifier("x")));
        assert_eq!(shift, 3);
        let (inner, shift) = strength_mul_to_shl(&mul2_lhs, &env).expect("2*x");
        assert!(matches!(inner.1.as_ref(), Expression::Identifier("x")));
        assert_eq!(shift, 1);
        assert_eq!(strength_mul_to_shl(&mul6, &env), None);
        // `* 1` stays with strength_reduced_inner
        assert_eq!(strength_mul_to_shl(&mul1, &env), None);
        let mul0 = (
            SimpleSpan::from(0..3),
            Box::new(Expression::Mul(id_expr("x"), int_expr(0))),
        );
        let mul_neg = (
            SimpleSpan::from(0..3),
            Box::new(Expression::Mul(id_expr("x"), int_expr(-8))),
        );
        assert_eq!(strength_mul_to_shl(&mul0, &env), None);
        assert_eq!(strength_mul_to_shl(&mul_neg, &env), None);
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
    fn strength_div_to_shr_rewrites_power_of_two_divisor() {
        let env = HashMap::new();
        let div2 = (
            SimpleSpan::from(0..3),
            Box::new(Expression::Div(id_expr("x"), int_expr(2))),
        );
        let div4 = (
            SimpleSpan::from(0..3),
            Box::new(Expression::Div(id_expr("x"), int_expr(4))),
        );
        let div3 = (
            SimpleSpan::from(0..3),
            Box::new(Expression::Div(id_expr("x"), int_expr(3))),
        );
        let div0 = (
            SimpleSpan::from(0..3),
            Box::new(Expression::Div(id_expr("x"), int_expr(0))),
        );
        let div_neg = (
            SimpleSpan::from(0..3),
            Box::new(Expression::Div(id_expr("x"), int_expr(-2))),
        );
        let (inner, shift) = strength_div_to_shr(&div2, &env).expect("x/2");
        assert!(matches!(inner.1.as_ref(), Expression::Identifier("x")));
        assert_eq!(shift, 1);
        let (inner, shift) = strength_div_to_shr(&div4, &env).expect("x/4");
        assert!(matches!(inner.1.as_ref(), Expression::Identifier("x")));
        assert_eq!(shift, 2);
        assert_eq!(strength_div_to_shr(&div3, &env), None);
        assert_eq!(strength_div_to_shr(&div0, &env), None);
        assert_eq!(strength_div_to_shr(&div_neg, &env), None);
        assert!(!strength_div_dividend_nonneg(&id_expr("x"), &env));
        let mut env_pos = HashMap::new();
        env_pos.insert("x".into(), ConstValue::Int(8));
        assert!(strength_div_dividend_nonneg(&id_expr("x"), &env_pos));
        env_pos.insert("x".into(), ConstValue::Int(-1));
        assert!(!strength_div_dividend_nonneg(&id_expr("x"), &env_pos));
    }

    #[test]
    fn strength_reduce_bitops_identities_and_zeros() {
        let env = HashMap::new();
        let or0 = (
            SimpleSpan::from(0..3),
            Box::new(Expression::BitOr(id_expr("x"), int_expr(0))),
        );
        let xor0 = (
            SimpleSpan::from(0..3),
            Box::new(Expression::Xor(id_expr("x"), int_expr(0))),
        );
        let and_ones = (
            SimpleSpan::from(0..3),
            Box::new(Expression::BitAnd(id_expr("x"), int_expr(-1))),
        );
        let and_ff = (
            SimpleSpan::from(0..3),
            Box::new(Expression::BitAnd(id_expr("x"), int_expr(0xFF))),
        );
        let and0 = (
            SimpleSpan::from(0..3),
            Box::new(Expression::BitAnd(id_expr("x"), int_expr(0))),
        );
        let xor_x = (
            SimpleSpan::from(0..3),
            Box::new(Expression::Xor(id_expr("x"), id_expr("x"))),
        );
        let or_x = (
            SimpleSpan::from(0..3),
            Box::new(Expression::BitOr(id_expr("x"), id_expr("x"))),
        );
        let shl0 = (
            SimpleSpan::from(0..3),
            Box::new(Expression::Shl(id_expr("x"), int_expr(0))),
        );
        let shr0 = (
            SimpleSpan::from(0..3),
            Box::new(Expression::Shr(id_expr("x"), int_expr(0))),
        );
        let or_ones = (
            SimpleSpan::from(0..3),
            Box::new(Expression::BitOr(id_expr("x"), int_expr(-1))),
        );
        let call_and0 = (
            SimpleSpan::from(0..3),
            Box::new(Expression::BitAnd(
                (
                    SimpleSpan::from(0..1),
                    Box::new(Expression::Call {
                        name: id_expr("f"),
                        args: None,
                    }),
                ),
                int_expr(0),
            )),
        );
        assert!(matches!(
            strength_reduce_bitops(&or0, &env),
            Some(StrengthBitop::Identity(e)) if matches!(e.1.as_ref(), Expression::Identifier("x"))
        ));
        assert!(matches!(
            strength_reduce_bitops(&xor0, &env),
            Some(StrengthBitop::Identity(_))
        ));
        assert!(matches!(
            strength_reduce_bitops(&and_ones, &env),
            Some(StrengthBitop::Identity(_))
        ));
        assert!(strength_reduce_bitops(&and_ff, &env).is_none());
        assert!(strength_reduce_bitops(&call_and0, &env).is_none());
        assert!(matches!(
            strength_reduce_bitops(&and0, &env),
            Some(StrengthBitop::Const(0))
        ));
        assert!(matches!(
            strength_reduce_bitops(&xor_x, &env),
            Some(StrengthBitop::Const(0))
        ));
        assert!(matches!(
            strength_reduce_bitops(&or_x, &env),
            Some(StrengthBitop::Identity(_))
        ));
        assert!(matches!(
            strength_reduce_bitops(&shl0, &env),
            Some(StrengthBitop::Identity(_))
        ));
        assert!(matches!(
            strength_reduce_bitops(&shr0, &env),
            Some(StrengthBitop::Identity(_))
        ));
        assert!(matches!(
            strength_reduce_bitops(&or_ones, &env),
            Some(StrengthBitop::Const(-1))
        ));
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
    fn strength_mul_to_shl_uses_const_env() {
        let mut env = HashMap::new();
        env.insert("K".into(), ConstValue::Int(16));
        let mul = (
            SimpleSpan::from(0..3),
            Box::new(Expression::Mul(
                id_expr("x"),
                (
                    SimpleSpan::from(0..1),
                    Box::new(Expression::Identifier("K")),
                ),
            )),
        );
        let (inner, shift) = strength_mul_to_shl(&mul, &env).expect("x*K");
        assert!(matches!(inner.1.as_ref(), Expression::Identifier("x")));
        assert_eq!(shift, 4);
    }

    #[test]
    fn strength_reduced_inner_add_zero_and_mul_one() {
        let add0 = (
            SimpleSpan::from(0..3),
            Box::new(Expression::Add(id_expr("x"), int_expr(0))),
        );
        let mul1 = (
            SimpleSpan::from(0..3),
            Box::new(Expression::Mul(int_expr(1), id_expr("x"))),
        );
        assert!(matches!(
            strength_reduced_inner(&add0).map(|e| e.1.as_ref()),
            Some(Expression::Identifier("x"))
        ));
        assert!(matches!(
            strength_reduced_inner(&mul1).map(|e| e.1.as_ref()),
            Some(Expression::Identifier("x"))
        ));
    }

    #[test]
    fn strength_pow_int_square_and_zero() {
        let env = HashMap::new();
        let sq = (
            SimpleSpan::from(0..3),
            Box::new(Expression::Pow(id_expr("x"), int_expr(2))),
        );
        let z = (
            SimpleSpan::from(0..3),
            Box::new(Expression::Pow(id_expr("x"), int_expr(0))),
        );
        let id = (
            SimpleSpan::from(0..3),
            Box::new(Expression::Pow(id_expr("x"), int_expr(1))),
        );
        let (base, kind) = strength_pow_int(&sq, &env).expect("**2");
        assert!(matches!(base.1.as_ref(), Expression::Identifier("x")));
        assert_eq!(kind, StrengthPow::Square);
        assert_eq!(
            strength_pow_int(&z, &env).map(|(_, k)| k),
            Some(StrengthPow::ConstOne)
        );
        // **1 is identity via strength_reduced_inner, not strength_pow_int.
        assert_eq!(strength_pow_int(&id, &env), None);
        assert!(matches!(
            strength_reduced_inner(&id).map(|e| e.1.as_ref()),
            Some(Expression::Identifier("x"))
        ));
    }

    #[test]
    fn range_trip_count_exclusive_and_inclusive() {
        assert_eq!(range_trip_count(&int_expr(0), &int_expr(3), false), Some(3));
        assert_eq!(range_trip_count(&int_expr(0), &int_expr(2), true), Some(3));
        assert_eq!(range_trip_count(&int_expr(0), &int_expr(9), false), None);
    }

    #[test]
    fn body_has_loop_control_detects_break() {
        let plain = (
            SimpleSpan::from(0..1),
            Box::new(Expression::Block(vec![int_expr(1)])),
        );
        let with_break = (
            SimpleSpan::from(0..1),
            Box::new(Expression::Block(vec![(
                SimpleSpan::from(0..1),
                Box::new(Expression::Break),
            )])),
        );
        let with_wrapped_break = (
            SimpleSpan::from(0..1),
            Box::new(Expression::Block(vec![(
                SimpleSpan::from(0..1),
                Box::new(Expression::ExprStatement((
                    SimpleSpan::from(0..1),
                    Box::new(Expression::Break),
                ))),
            )])),
        );
        assert!(!body_has_loop_control(&plain));
        assert!(body_has_loop_control(&with_break));
        assert!(body_has_loop_control(&with_wrapped_break));
    }

    #[test]
    fn string_add_folds_concatenation() {
        let env = HashMap::new();
        let lhs = (SimpleSpan::from(0..1), Box::new(Expression::String("he")));
        let rhs = (SimpleSpan::from(0..1), Box::new(Expression::String("llo")));
        assert_eq!(
            eval_string_add(&lhs, &rhs, &env),
            Some(ConstValue::Str("hello".into()))
        );
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
