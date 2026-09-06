//! Typed dense numeric ops (COI-268). Operate on raw slot bits; no boxing.

use common::{Value, dense};

#[inline(always)]
pub fn eval_bin(kind: u8, a: Value, b: Value) -> Value {
    match kind {
        dense::IADD64 => Value::from(a.as_int().wrapping_add(b.as_int())),
        dense::ISUB64 => Value::from(a.as_int().wrapping_sub(b.as_int())),
        dense::IMUL64 => Value::from(a.as_int().wrapping_mul(b.as_int())),
        dense::IDIV64 => Value::from(a.as_int() / b.as_int()),
        dense::IREM64 => Value::from(a.as_int() % b.as_int()),
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
        dense::IADD32 => i32_bin(a, b, i32::wrapping_add),
        dense::ISUB32 => i32_bin(a, b, i32::wrapping_sub),
        dense::IMUL32 => i32_bin(a, b, i32::wrapping_mul),
        dense::IDIV32 => i32_bin(a, b, |x, y| x / y),
        dense::IREM32 => i32_bin(a, b, |x, y| x % y),
        dense::FADD32 => f32_bin(a, b, |x, y| x + y),
        dense::FSUB32 => f32_bin(a, b, |x, y| x - y),
        dense::FMUL32 => f32_bin(a, b, |x, y| x * y),
        dense::FDIV32 => f32_bin(a, b, |x, y| x / y),
        dense::FREM32 => f32_bin(a, b, |x, y| x % y),
        _ => Value::from(0i64),
    }
}

#[inline]
fn i32_bin(a: Value, b: Value, f: fn(i32, i32) -> i32) -> Value {
    Value::from(i64::from(f(a.as_int() as i32, b.as_int() as i32)))
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

#[inline]
pub fn eval_unary(kind: u8, src: Value) -> Value {
    match kind {
        dense::UNARY_NEG => Value::from(src.as_int().wrapping_neg()),
        dense::UNARY_FNEG => Value::from(-src.as_float()),
        dense::UNARY_NOT => Value::from(!src.as_bool()),
        _ => src,
    }
}

#[inline]
pub fn eval_cast(kind: u8, src: Value) -> Value {
    match kind {
        dense::CAST_I2F => Value::from(src.as_int() as f64),
        dense::CAST_SEXT => Value::from(i64::from(src.as_int() as i32)),
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
