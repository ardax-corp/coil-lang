//! Typed dense numeric ops (COI-268). Operate on raw slot bits; no boxing.

use common::int_arith::{self, IntTrap};
use common::{Value, dense};

/// A dense binary op; an int op that overflows or divides by zero traps.
#[inline(always)]
pub fn eval_bin(kind: u8, a: Value, b: Value) -> Result<Value, IntTrap> {
    let i64_op = |f: fn(i64, i64) -> int_arith::IntResult| f(a.as_int(), b.as_int()).map(Value::from);
    Ok(match kind {
        dense::IADD64 => return i64_op(int_arith::add),
        dense::ISUB64 => return i64_op(int_arith::sub),
        dense::IMUL64 => return i64_op(int_arith::mul),
        dense::IDIV64 => return i64_op(int_arith::div),
        dense::IREM64 => return i64_op(int_arith::rem),
        dense::IAND64 => Value::from(a.as_int() & b.as_int()),
        dense::IOR64 => Value::from(a.as_int() | b.as_int()),
        dense::IXOR64 => Value::from(a.as_int() ^ b.as_int()),
        dense::ISHL64 => Value::from(a.as_int() << (b.as_int() & 63)),
        dense::ISHR64 => Value::from(a.as_int() >> (b.as_int() & 63)),
        dense::FADD64 => Value::from(a.as_float() + b.as_float()),
        dense::FSUB64 => Value::from(a.as_float() - b.as_float()),
        dense::FMUL64 => Value::from(a.as_float() * b.as_float()),
        dense::FDIV64 => Value::from(a.as_float() / b.as_float()),
        dense::FREM64 => Value::from(a.as_float() % b.as_float()),
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
    let flag = match lane {
        dense::CMP_I64 => icmp(a.as_int(), b.as_int(), pred),
        dense::CMP_F64 => fcmp(a.as_float(), b.as_float(), pred),
        dense::CMP_I32 => icmp(i64::from(a.as_int() as i32), i64::from(b.as_int() as i32), pred),
        dense::CMP_F32 => {
            let x = f32::from_bits(a.as_int() as u32);
            let y = f32::from_bits(b.as_int() as u32);
            fcmp(x as f64, y as f64, pred)
        }
        _ => false,
    };
    Value::from(flag)
}

#[inline]
fn icmp(a: i64, b: i64, pred: u8) -> bool {
    match pred {
        dense::CMP_LT => a < b,
        dense::CMP_LE => a <= b,
        dense::CMP_GT => a > b,
        dense::CMP_GE => a >= b,
        dense::CMP_EQ => a == b,
        dense::CMP_NE => a != b,
        _ => false,
    }
}

#[inline]
fn fcmp(a: f64, b: f64, pred: u8) -> bool {
    match pred {
        dense::CMP_LT => a < b,
        dense::CMP_LE => a <= b,
        dense::CMP_GE => a >= b,
        dense::CMP_GT => a > b,
        dense::CMP_EQ => a == b,
        dense::CMP_NE => a != b,
        _ => false,
    }
}

/// A dense unary op; negating `int::MIN` traps.
#[inline]
pub fn eval_unary(kind: u8, src: Value) -> Result<Value, IntTrap> {
    Ok(match kind {
        dense::UNARY_NEG => return int_arith::neg(src.as_int()).map(Value::from),
        dense::UNARY_FNEG => Value::from(-src.as_float()),
        dense::UNARY_NOT => Value::from(!(src.as_int() != 0)),
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
