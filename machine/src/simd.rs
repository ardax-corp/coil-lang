//! Thin VM glue for compiler-only `V*` opcodes. Float math goes through
//! [`coil_simd::lanes`]; int lanes use [`common::int_arith`], which traps
//! on overflow like the scalar ops.

use coil_simd::lanes::{self, LANES};
use common::int_arith::{self, IntResult, IntTrap};
use common::{dense, simd, Value};

#[inline]
fn as_f64(bits: &[u64; LANES]) -> [f64; LANES] {
    let mut out = [0.0f64; LANES];
    for i in 0..LANES {
        out[i] = f64::from_bits(bits[i]);
    }
    out
}

#[inline]
fn as_i64(bits: &[u64; LANES]) -> [i64; LANES] {
    let mut out = [0i64; LANES];
    for i in 0..LANES {
        out[i] = bits[i] as i64;
    }
    out
}

#[inline]
fn from_f64(lanes: &[f64; LANES]) -> [u64; LANES] {
    let mut out = [0u64; LANES];
    for i in 0..LANES {
        out[i] = lanes[i].to_bits();
    }
    out
}

/// Each lane's exact int result, or the first lane's trap.
#[inline]
fn int_lanes(lhs: &[u64; LANES], rhs: &[u64; LANES], f: fn(i64, i64) -> IntResult) -> Result<[u64; LANES], IntTrap> {
    let mut o = [0u64; LANES];
    for i in 0..LANES {
        o[i] = f(lhs[i] as i64, rhs[i] as i64)? as u64;
    }
    Ok(o)
}

/// Evaluate `VBin`. `scalar` is the frame-slot word for splat kinds. Int
/// lanes trap like the scalar ops (overflow, division by zero).
#[inline]
pub fn eval_vbin(kind: u8, lhs: &[u64; LANES], rhs: &[u64; LANES], scalar: Value) -> Result<[u64; LANES], IntTrap> {
    Ok(match kind {
        simd::IADD64 => return int_lanes(lhs, rhs, int_arith::add),
        simd::ISUB64 => return int_lanes(lhs, rhs, int_arith::sub),
        simd::IMUL64 => return int_lanes(lhs, rhs, int_arith::mul),
        simd::IDIV64 => return int_lanes(lhs, rhs, int_arith::div),
        simd::FADD64 => {
            let a = as_f64(lhs);
            let b = as_f64(rhs);
            let mut o = [0.0f64; LANES];
            lanes::add_f64(&a, &b, &mut o);
            from_f64(&o)
        }
        simd::FSUB64 => {
            let a = as_f64(lhs);
            let b = as_f64(rhs);
            let mut o = [0.0f64; LANES];
            lanes::sub_f64(&a, &b, &mut o);
            from_f64(&o)
        }
        simd::FMUL64 => {
            let a = as_f64(lhs);
            let b = as_f64(rhs);
            let mut o = [0.0f64; LANES];
            lanes::mul_f64(&a, &b, &mut o);
            from_f64(&o)
        }
        simd::FDIV64 => {
            let a = as_f64(lhs);
            let b = as_f64(rhs);
            let mut o = [0.0f64; LANES];
            lanes::div_f64(&a, &b, &mut o);
            from_f64(&o)
        }
        simd::INEG => return int_lanes(lhs, lhs, |a, _| int_arith::neg(a)),
        simd::FNEG => {
            let a = as_f64(lhs);
            let mut o = [0.0f64; LANES];
            lanes::neg_f64(&a, &mut o);
            from_f64(&o)
        }
        simd::SPLAT_I64 => [scalar.as_int() as u64; LANES],
        simd::SPLAT_F64 => [scalar.as_float().to_bits(); LANES],
        simd::IOTA_I64 => {
            let mut o = [0u64; LANES];
            for (i, lane) in o.iter_mut().enumerate() {
                *lane = i as u64;
            }
            o
        }
        simd::IOTA_F64 => {
            let mut o = [0u64; LANES];
            for (i, lane) in o.iter_mut().enumerate() {
                *lane = (i as f64).to_bits();
            }
            o
        }
        _ => [0u64; LANES],
    })
}

/// `acc ⊕ left-fold(lanes)` — float add is sequential (P11), and so is an
/// int fold, which traps on the first partial result that overflows.
#[inline]
pub fn eval_vreduce(ty: u8, acc: Value, src: &[u64; LANES], fold: u8) -> Result<Value, IntTrap> {
    Ok(match (ty, fold) {
        (dense::TY_I64, simd::REDUCE_MUL) => {
            let lanes = as_i64(src);
            Value::from(lanes.iter().try_fold(acc.as_int(), |a, &x| int_arith::mul(a, x))?)
        }
        (dense::TY_F64, simd::REDUCE_MUL) => {
            let lanes = as_f64(src);
            Value::from(lanes::fold_mul_f64(acc.as_float(), &lanes))
        }
        (dense::TY_I64, _) => {
            let lanes = as_i64(src);
            Value::from(lanes.iter().try_fold(acc.as_int(), |a, &x| int_arith::add(a, x))?)
        }
        (dense::TY_F64, _) => {
            let lanes = as_f64(src);
            Value::from(lanes::fold_add_f64(acc.as_float(), &lanes))
        }
        _ => acc,
    })
}

/// Conservative `dest = a * b + dest` (mul then add; int lanes trap).
#[inline]
pub fn eval_vfma(
    ty: u8,
    a: &[u64; LANES],
    b: &[u64; LANES],
    dest: &[u64; LANES],
) -> Result<[u64; LANES], IntTrap> {
    Ok(match ty {
        dense::TY_I64 => {
            let prod = int_lanes(a, b, int_arith::mul)?;
            return int_lanes(&prod, dest, int_arith::add);
        }
        dense::TY_F64 => {
            let aa = as_f64(a);
            let bb = as_f64(b);
            let cc = as_f64(dest);
            let mut o = [0.0f64; LANES];
            lanes::fmadd_f64(&aa, &bb, &cc, &mut o);
            from_f64(&o)
        }
        _ => *dest,
    })
}
