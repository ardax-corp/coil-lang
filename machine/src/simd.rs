//! Thin VM glue for compiler-only `V*` opcodes. All packed math goes
//! through [`coil_simd::lanes`]; this file only copies bits.

use coil_simd::lanes::{self, LANES};
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

#[inline]
fn from_i64(lanes: &[i64; LANES]) -> [u64; LANES] {
    let mut out = [0u64; LANES];
    for i in 0..LANES {
        out[i] = lanes[i] as u64;
    }
    out
}

/// Evaluate `VBin`. `scalar` is the frame-slot word for splat kinds.
#[inline]
pub fn eval_vbin(kind: u8, lhs: &[u64; LANES], rhs: &[u64; LANES], scalar: Value) -> [u64; LANES] {
    match kind {
        simd::IADD64 => {
            let a = as_i64(lhs);
            let b = as_i64(rhs);
            let mut o = [0i64; LANES];
            lanes::add_i64(&a, &b, &mut o);
            from_i64(&o)
        }
        simd::ISUB64 => {
            let a = as_i64(lhs);
            let b = as_i64(rhs);
            let mut o = [0i64; LANES];
            lanes::sub_i64(&a, &b, &mut o);
            from_i64(&o)
        }
        simd::IMUL64 => {
            let a = as_i64(lhs);
            let b = as_i64(rhs);
            let mut o = [0i64; LANES];
            lanes::mul_i64(&a, &b, &mut o);
            from_i64(&o)
        }
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
        simd::INEG => {
            let a = as_i64(lhs);
            let mut o = [0i64; LANES];
            lanes::neg_i64(&a, &mut o);
            from_i64(&o)
        }
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
            for i in 0..LANES {
                o[i] = i as u64;
            }
            o
        }
        simd::IOTA_F64 => {
            let mut o = [0u64; LANES];
            for i in 0..LANES {
                o[i] = (i as f64).to_bits();
            }
            o
        }
        _ => [0u64; LANES],
    }
}

/// `acc ⊕ left-fold(lanes)` — float add is sequential (P11).
#[inline]
pub fn eval_vreduce(ty: u8, acc: Value, src: &[u64; LANES]) -> Value {
    match ty {
        dense::TY_I64 => {
            let lanes = as_i64(src);
            Value::from(lanes::fold_add_i64(acc.as_int(), &lanes))
        }
        dense::TY_F64 => {
            let lanes = as_f64(src);
            Value::from(lanes::fold_add_f64(acc.as_float(), &lanes))
        }
        _ => acc,
    }
}

/// Conservative `dest = a * b + dest` (mul then add).
#[inline]
pub fn eval_vfma(
    ty: u8,
    a: &[u64; LANES],
    b: &[u64; LANES],
    dest: &[u64; LANES],
) -> [u64; LANES] {
    match ty {
        dense::TY_I64 => {
            let aa = as_i64(a);
            let bb = as_i64(b);
            let cc = as_i64(dest);
            let mut o = [0i64; LANES];
            lanes::fmadd_i64(&aa, &bb, &cc, &mut o);
            from_i64(&o)
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
    }
}
