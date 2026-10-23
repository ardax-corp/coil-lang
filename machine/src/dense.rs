//! Typed dense numeric ops (COI-268). Operate on raw slot bits; no boxing.

use common::int_arith::{self, IntTrap};
use common::{Value, dense};

use crate::arith::{self, Cmp, FloatOp, IntOp};

/// A dense binary op; an int op that overflows or divides by zero traps.
/// The 64-bit lanes are [`crate::arith`], as on every other VM path.
#[inline(always)]
pub fn eval_bin(kind: u8, a: Value, b: Value) -> Result<Value, IntTrap> {
    let i64_op = |op| arith::int(op, a.as_int(), b.as_int()).map(Value::from);
    let f64_op = |op| Value::from(arith::float(op, a.as_float(), b.as_float()));
    Ok(match kind {
        dense::IADD64 => return i64_op(IntOp::Add),
        dense::ISUB64 => return i64_op(IntOp::Sub),
        dense::IMUL64 => return i64_op(IntOp::Mul),
        dense::IDIV64 => return i64_op(IntOp::Div),
        dense::IREM64 => return i64_op(IntOp::Rem),
        dense::IAND64 => return i64_op(IntOp::BitAnd),
        dense::IOR64 => return i64_op(IntOp::BitOr),
        dense::IXOR64 => return i64_op(IntOp::Xor),
        dense::ISHL64 => return i64_op(IntOp::Shl),
        dense::ISHR64 => return i64_op(IntOp::Shr),
        dense::FADD64 => f64_op(FloatOp::Add),
        dense::FSUB64 => f64_op(FloatOp::Sub),
        dense::FMUL64 => f64_op(FloatOp::Mul),
        dense::FDIV64 => f64_op(FloatOp::Div),
        dense::FREM64 => f64_op(FloatOp::Rem),
        // The 32-bit lanes are wire ISA (minor 6); no compiler emits them.
        dense::IADD32 => return i32_bin(a, b, i32::checked_add),
        dense::ISUB32 => return i32_bin(a, b, i32::checked_sub),
        dense::IMUL32 => return i32_bin(a, b, i32::checked_mul),
        dense::IDIV32 => return i32_bin(a, b, i32::checked_div),
        dense::IREM32 => return i32_bin(a, b, |x, y| if y == 0 { None } else { Some(x.wrapping_rem(y)) }),
        dense::FADD32 => f32_bin(a, b, |x, y| x + y),
        dense::FSUB32 => f32_bin(a, b, |x, y| x - y),
        dense::FMUL32 => f32_bin(a, b, |x, y| x * y),
        dense::FDIV32 => f32_bin(a, b, |x, y| x / y),
        dense::FREM32 => f32_bin(a, b, |x, y| x % y),
        _ => Value::from(0i64),
    })
}

/// An i32 op; `None` with `b == 0` can only be a division by zero
/// (`x + 0`, `x - 0` and `x * 0` never overflow), else it is overflow.
#[inline]
fn i32_bin(a: Value, b: Value, f: fn(i32, i32) -> Option<i32>) -> Result<Value, IntTrap> {
    let (x, y) = (a.as_int() as i32, b.as_int() as i32);
    match f(x, y) {
        Some(r) => Ok(Value::from(i64::from(r))),
        None if y == 0 => Err(IntTrap::DivByZero),
        None => Err(IntTrap::Overflow),
    }
}


#[inline]
fn f32_bin(a: Value, b: Value, f: fn(f32, f32) -> f32) -> Value {
    let x = f32::from_bits(a.as_int() as u32);
    let y = f32::from_bits(b.as_int() as u32);
    Value::from(u64::from(f(x, y).to_bits()))
}

#[inline(always)]
pub fn eval_cmp(kind: u8, a: Value, b: Value) -> Value {
    let (lane, pred) = dense::unpack_cmp(kind);
    let Some(c) = cmp_pred(pred) else {
        return Value::from(false);
    };
    let flag = match lane {
        dense::CMP_I64 => arith::cmp(c, a.as_int(), b.as_int()),
        dense::CMP_F64 => arith::cmp(c, a.as_float(), b.as_float()),
        dense::CMP_I32 => arith::cmp(c, a.as_int() as i32, b.as_int() as i32),
        dense::CMP_F32 => {
            let x = f32::from_bits(a.as_int() as u32);
            let y = f32::from_bits(b.as_int() as u32);
            arith::cmp(c, x, y)
        }
        _ => false,
    };
    Value::from(flag)
}

#[inline(always)]
fn cmp_pred(pred: u8) -> Option<Cmp> {
    Some(match pred {
        dense::CMP_LT => Cmp::Lt,
        dense::CMP_LE => Cmp::Le,
        dense::CMP_GT => Cmp::Gt,
        dense::CMP_GE => Cmp::Ge,
        dense::CMP_EQ => Cmp::Eq,
        dense::CMP_NE => Cmp::Ne,
        _ => return None,
    })
}

/// A dense unary op; negating `int::MIN` traps.
#[inline]
pub fn eval_unary(kind: u8, src: Value) -> Result<Value, IntTrap> {
    Ok(match kind {
        dense::UNARY_NEG => return int_arith::neg(src.as_int()).map(Value::from),
        dense::UNARY_FNEG => Value::from(-src.as_float()),
        dense::UNARY_NOT => Value::from(!(src.as_int() != 0)),
        dense::UNARY_CTZ => Value::from(i64::from(src.as_int().trailing_zeros())),
        dense::UNARY_BLSR => {
            let x = src.as_int();
            Value::from(x & x.wrapping_sub(1))
        }
        _ => src,
    })
}

#[inline]
pub fn eval_cast(kind: u8, src: Value) -> Value {
    match kind {
        dense::CAST_I2F => Value::from(src.as_int() as f64),
        dense::CAST_SEXT => Value::from(i64::from(src.as_int() as i32)),
        dense::CAST_F2I => Value::from(src.as_float() as i64),
        _ => src,
    }
}

#[inline]
pub fn eval_const(ty: u8, raw: u64) -> Value {
    match ty {
        dense::TY_I32 => Value::from(i64::from(raw as i32)),
        dense::TY_BOOL => Value::from(raw != 0),
        dense::TY_F32 => Value::from(u64::from(raw as u32)),
        dense::TY_F64 | dense::TY_I64 => Value::from(raw),
        _ => Value::from(raw),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unary_not_matches_log_not_truthiness() {
        // VM LogNot is `!(as_int() != 0)`. Dense UNARY_NOT used to use
        // `as_bool` (`raw as u8 == 1`), so a heap pointer looked like None.
        let ptr = Value::from(0x1000i64);
        assert_eq!(eval_unary(dense::UNARY_NOT, Value::from(0i64)).unwrap().as_int(), 1);
        assert_eq!(eval_unary(dense::UNARY_NOT, Value::from(1i64)).unwrap().as_int(), 0);
        assert_eq!(eval_unary(dense::UNARY_NOT, Value::from(42i64)).unwrap().as_int(), 0);
        assert_eq!(eval_unary(dense::UNARY_NOT, ptr).unwrap().as_int(), 0);
        assert_eq!(eval_unary(dense::UNARY_NOT, Value::from(true)).unwrap().as_int(), 0);
        assert_eq!(eval_unary(dense::UNARY_NOT, Value::from(false)).unwrap().as_int(), 1);
    }
}
