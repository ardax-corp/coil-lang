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

/// A vector register as 64 byte lanes (lane `k` is byte `k` in memory).
#[inline]
pub fn as_bytes(reg: &[u64; LANES]) -> &bytes64::Block {
    // SAFETY: `[u64; 8]` and `[u8; 64]` have the same size; `u8` has no
    // alignment requirement.
    unsafe { &*(reg as *const [u64; LANES] as *const bytes64::Block) }
}

/// A vector register as 64 mutable byte lanes.
#[inline]
pub fn as_bytes_mut(reg: &mut [u64; LANES]) -> &mut bytes64::Block {
    // SAFETY: as `as_bytes`.
    unsafe { &mut *(reg as *mut [u64; LANES] as *mut bytes64::Block) }
}

use coil_simd::bytes64;

/// Unsigned lane `a pred b` (`dense::CMP_*`).
#[inline]
fn byte_cmp(pred: u8, a: &bytes64::Block, b: &bytes64::Block) -> bytes64::Block {
    match pred {
        dense::CMP_LT => bytes64::lt(a, b),
        dense::CMP_LE => bytes64::le(a, b),
        dense::CMP_GT => bytes64::lt(b, a),
        dense::CMP_GE => bytes64::le(b, a),
        dense::CMP_EQ => bytes64::eq(a, b),
        _ => bytes64::not(&bytes64::eq(a, b)),
    }
}

/// Lane `pred` the `i64` `s`. A scalar outside `0..=255` gives the same
/// answer in every lane, as the integer compare does.
#[inline]
fn byte_cmp_scalar(pred: u8, a: &bytes64::Block, s: i64) -> bytes64::Block {
    if let Ok(b) = u8::try_from(s) {
        return byte_cmp(pred, a, &bytes64::splat(b));
    }
    // Every lane is in `0..=255`: below a negative `s`, above a large one.
    let lane_lt_s = s > 255;
    let all = match pred {
        dense::CMP_LT | dense::CMP_LE => lane_lt_s,
        dense::CMP_GT | dense::CMP_GE => !lane_lt_s,
        dense::CMP_EQ => false,
        _ => true,
    };
    bytes64::splat(if all { 0xFF } else { 0 })
}

/// A byte-lane `VBin` on the register file: `regs[dest] = regs[a] op
/// regs[b]`, or lane `regs[b]` against `scalar` for `BS_CMP` kinds.
#[inline]
pub fn vbin_bytes_in(regs: &mut [[u64; LANES]; simd::NREGS], kind: u8, dest: usize, a: usize, b: usize, scalar: Value) {
    let out = {
        let y = as_bytes(&regs[b]);
        match kind {
            simd::M_AND => bytes64::and(as_bytes(&regs[a]), y),
            simd::M_OR => bytes64::or(as_bytes(&regs[a]), y),
            simd::M_ANDN => bytes64::andnot(as_bytes(&regs[a]), y),
            simd::M_NOT => bytes64::not(as_bytes(&regs[a])),
            k if (simd::B_CMP..simd::B_CMP + 6).contains(&k) => byte_cmp(k - simd::B_CMP, as_bytes(&regs[a]), y),
            k => byte_cmp_scalar(k - simd::BS_CMP, y, scalar.as_int()),
        }
    };
    *as_bytes_mut(&mut regs[dest]) = out;
}

/// `VMask`: bit `k` is the top bit of byte lane `k`.
#[inline]
pub fn movemask(reg: &[u64; LANES]) -> i64 {
    bytes64::movemask(as_bytes(reg)) as i64
}

#[cfg(test)]
mod byte_tests {
    use super::*;

    /// `a op b` (or lane `b` against `scalar`) into a fresh register.
    fn eval_vbin_bytes(kind: u8, a: &[u64; LANES], b: &[u64; LANES], scalar: Value) -> [u64; LANES] {
        let mut regs = [[0u64; LANES]; simd::NREGS];
        regs[0] = *a;
        regs[1] = *b;
        vbin_bytes_in(&mut regs, kind, 2, 0, 1, scalar);
        regs[2]
    }

    fn reg(bytes: &[u8]) -> [u64; LANES] {
        let mut r = [0u64; LANES];
        as_bytes_mut(&mut r)[..bytes.len()].copy_from_slice(bytes);
        r
    }

    #[test]
    fn scalar_compare_out_of_byte_range() {
        let r = reg(b"a,b");
        let eq = |s: i64| movemask(&eval_vbin_bytes(simd::BS_CMP + dense::CMP_EQ, &[0; LANES], &r, Value::from(s)));
        assert_eq!(eq(44), 0b10);
        assert_eq!(eq(44 + 256), 0);
        assert_eq!(eq(-1), 0);
        let lt = |s: i64| movemask(&eval_vbin_bytes(simd::BS_CMP + dense::CMP_LT, &[0; LANES], &r, Value::from(s)));
        assert_eq!(lt(300), u64::MAX as i64);
        assert_eq!(lt(-5), 0);
        assert_eq!(lt(98) & 0b111, 0b011);
    }

    #[test]
    fn vector_compare_and_masks() {
        let a = reg(b"abcd");
        let b = reg(b"abzd");
        let ne = eval_vbin_bytes(simd::B_CMP + dense::CMP_NE, &a, &b, Value::from(0i64));
        assert_eq!(movemask(&ne), 0b100);
        let gt = eval_vbin_bytes(simd::B_CMP + dense::CMP_GT, &b, &a, Value::from(0i64));
        assert_eq!(movemask(&gt), 0b100);
        let not = eval_vbin_bytes(simd::M_NOT, &ne, &ne, Value::from(0i64));
        let both = eval_vbin_bytes(simd::M_AND, &not, &ne, Value::from(0i64));
        assert_eq!(movemask(&both), 0);
        let either = eval_vbin_bytes(simd::M_OR, &not, &ne, Value::from(0i64));
        assert_eq!(movemask(&either), -1);
        let andn = eval_vbin_bytes(simd::M_ANDN, &either, &ne, Value::from(0i64));
        assert_eq!(movemask(&andn), !0b100);
    }
}
