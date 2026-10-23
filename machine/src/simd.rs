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
#[inline(always)]
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
    ByteOp::new(kind, dest, a, b, scalar).run(regs);
}

/// `VScan` with one stream compared against one byte: from `p = from` in
/// steps of 64 while `p <= hi` and lanes `p + off ..` lie in `bytes`, the
/// first `p + k` where `bytes[p + off + k] pred b`, else where it stopped.
pub(crate) fn scan_byte(bytes: &[u8], from: i64, off: i64, hi: i128, pred: u8, b: u8) -> i64 {
    // The last `p` whose 64 lanes lie in `bytes`.
    let hi = hi.min(bytes.len() as i128 - bytes64::LANES as i128 - i128::from(off));
    if i128::from(from) + i128::from(off) < 0 {
        return from;
    }
    #[inline(always)]
    fn run(bytes: &[u8], from: i64, off: i64, hi: i128, f: impl Fn(&bytes64::Block) -> bytes64::Block) -> i64 {
        let mut p = from;
        while i128::from(p) <= hi {
            let i = (p + off) as usize;
            // SAFETY: `0 <= p + off` and `p + off + 64 <= bytes.len()` by `hi`.
            let block = unsafe { &*(bytes.as_ptr().add(i) as *const bytes64::Block) };
            let m = bytes64::movemask(&f(block));
            if m != 0 {
                return p + i64::from(m.trailing_zeros());
            }
            p += bytes64::LANES as i64;
        }
        p
    }
    let k = bytes64::splat(b);
    match pred {
        dense::CMP_EQ => run(bytes, from, off, hi, |x| bytes64::eq(x, &k)),
        dense::CMP_NE => run(bytes, from, off, hi, |x| bytes64::not(&bytes64::eq(x, &k))),
        _ => run(bytes, from, off, hi, |x| byte_cmp(pred, x, &k)),
    }
}

/// `VScan` for `bytes[p + oa + k] == a && bytes[p + ob + k] == b`, as
/// [`scan_byte`].
pub(crate) fn scan_pair(bytes: &[u8], from: i64, (oa, a): (i64, u8), (ob, b): (i64, u8), hi: i128) -> i64 {
    let lanes = bytes64::LANES as i128;
    let hi = hi.min(bytes.len() as i128 - lanes - i128::from(oa)).min(bytes.len() as i128 - lanes - i128::from(ob));
    if i128::from(from) + i128::from(oa.min(ob)) < 0 {
        return from;
    }
    let (ka, kb) = (bytes64::splat(a), bytes64::splat(b));
    let mut p = from;
    while i128::from(p) <= hi {
        // SAFETY: both blocks lie in `bytes` by `hi` and the check above.
        let x = unsafe { &*(bytes.as_ptr().add((p + oa) as usize) as *const bytes64::Block) };
        let y = unsafe { &*(bytes.as_ptr().add((p + ob) as usize) as *const bytes64::Block) };
        let m = bytes64::movemask(&bytes64::and(&bytes64::eq(x, &ka), &bytes64::eq(y, &kb)));
        if m != 0 {
            return p + i64::from(m.trailing_zeros());
        }
        p += bytes64::LANES as i64;
    }
    p
}

/// A byte-lane `VBin` with its scalar read: what a `VScan` body runs per
/// 64 bytes. Register indices are below `NREGS`.
#[derive(Clone, Copy)]
pub(crate) enum ByteOp {
    /// `M_*` kind on registers `a`, `b`.
    Logic { kind: u8, dest: usize, a: usize, b: usize },
    /// Lane `a pred b`.
    Cmp { pred: u8, dest: usize, a: usize, b: usize },
    /// Lane `b pred k`.
    CmpK { pred: u8, dest: usize, b: usize, k: u8 },
    /// Every lane `v` (a scalar outside a byte: the answer is the same in every lane).
    Fill { dest: usize, v: u8 },
}

impl ByteOp {
    pub(crate) fn new(kind: u8, dest: usize, a: usize, b: usize, scalar: Value) -> Self {
        match kind {
            simd::M_AND | simd::M_OR | simd::M_ANDN | simd::M_NOT => Self::Logic { kind, dest, a, b },
            k if (simd::B_CMP..simd::B_CMP + 6).contains(&k) => Self::Cmp { pred: k - simd::B_CMP, dest, a, b },
            k => {
                let pred = k - simd::BS_CMP;
                match u8::try_from(scalar.as_int()) {
                    Ok(k) => Self::CmpK { pred, dest, b, k },
                    Err(_) => Self::Fill { dest, v: byte_cmp_scalar(pred, &[0; bytes64::LANES], scalar.as_int())[0] },
                }
            }
        }
    }

    #[inline(always)]
    pub(crate) fn run(&self, regs: &mut [[u64; LANES]; simd::NREGS]) {
        let (dest, out) = match *self {
            Self::Logic { kind, dest, a, b } => {
                let (x, y) = (as_bytes(&regs[a]), as_bytes(&regs[b]));
                let out = match kind {
                    simd::M_AND => bytes64::and(x, y),
                    simd::M_OR => bytes64::or(x, y),
                    simd::M_ANDN => bytes64::andnot(x, y),
                    _ => bytes64::not(x),
                };
                (dest, out)
            }
            Self::Cmp { pred, dest, a, b } => (dest, byte_cmp(pred, as_bytes(&regs[a]), as_bytes(&regs[b]))),
            Self::CmpK { pred, dest, b, k } => (dest, byte_cmp(pred, as_bytes(&regs[b]), &bytes64::splat(k))),
            Self::Fill { dest, v } => (dest, bytes64::splat(v)),
        };
        *as_bytes_mut(&mut regs[dest]) = out;
    }
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
