//! Scalar binary-op semantics, defined once for every VM path: the plain
//! stack handlers (`exec_rest`), the fused slot ops ([`crate::fused`]) and
//! the dense kernels ([`crate::dense`]).
//!
//! Each path keeps its own operand decode and its own op-code match (the
//! packed `Instruction` discriminant, or a dense kind byte); every arm names
//! an op here by a constant. These are `#[inline(always)]`, so the inner
//! match folds away and each path still dispatches through one jump table.
//! `int` ops trap through [`common::int_arith`].

use common::Value;
use common::int_arith::{self, IntResult};

/// A binary op on two `int` words.
#[derive(Clone, Copy)]
pub(crate) enum IntOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Pow,
    BitAnd,
    BitOr,
    Xor,
    Shl,
    Shr,
}

/// A binary op on two `float` words.
#[derive(Clone, Copy)]
pub(crate) enum FloatOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Pow,
}

/// An ordering or equality predicate.
#[derive(Clone, Copy)]
pub(crate) enum Cmp {
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
    Ne,
}

/// The exact `int` result, or the trap (overflow, division by zero, a
/// negative exponent, a shift count outside `0..=63`).
#[inline(always)]
pub(crate) fn int(op: IntOp, a: i64, b: i64) -> IntResult {
    match op {
        IntOp::Add => int_arith::add(a, b),
        IntOp::Sub => int_arith::sub(a, b),
        IntOp::Mul => int_arith::mul(a, b),
        IntOp::Div => int_arith::div(a, b),
        IntOp::Rem => int_arith::rem(a, b),
        IntOp::Pow => int_arith::pow(a, b),
        IntOp::BitAnd => Ok(a & b),
        IntOp::BitOr => Ok(a | b),
        IntOp::Xor => Ok(a ^ b),
        IntOp::Shl => int_arith::shl(a, b),
        IntOp::Shr => int_arith::shr(a, b),
    }
}

/// IEEE 754 binary64; `Rem` is `fmod` (the sign of `a`).
#[inline(always)]
pub(crate) fn float(op: FloatOp, a: f64, b: f64) -> f64 {
    match op {
        FloatOp::Add => a + b,
        FloatOp::Sub => a - b,
        FloatOp::Mul => a * b,
        FloatOp::Div => a / b,
        FloatOp::Rem => a % b,
        FloatOp::Pow => a.powf(b),
    }
}

/// `PartialOrd` order: on floats every compare with a NaN is false except
/// `Ne`, and `-0.0 == 0.0`.
#[inline(always)]
pub(crate) fn cmp<T: PartialOrd>(op: Cmp, a: T, b: T) -> bool {
    match op {
        Cmp::Lt => a < b,
        Cmp::Le => a <= b,
        Cmp::Gt => a > b,
        Cmp::Ge => a >= b,
        Cmp::Eq => a == b,
        Cmp::Ne => a != b,
    }
}

/// `bool` `&&` on two words (`true` is the word `1`).
#[inline(always)]
pub(crate) fn and(a: Value, b: Value) -> bool {
    a.as_bool() && b.as_bool()
}

/// `bool` `||` on two words (`true` is the word `1`).
#[inline(always)]
pub(crate) fn or(a: Value, b: Value) -> bool {
    a.as_bool() || b.as_bool()
}
