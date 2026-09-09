//! Fixed 8-lane numeric ops used by compiler-only `V*` opcodes.
//!
//! Width matches the public kernel profitability gate (`len >= 8`) so these
//! wrappers always take the runtime-dispatched SIMD path (or scalar fallback
//! inside the existing kernels). No second ISA stack.

use crate::kernels;

/// V0 closed width: eight `i64` / `f64` lanes.
pub const LANES: usize = 8;

#[inline]
pub fn add_f64(a: &[f64; LANES], b: &[f64; LANES], out: &mut [f64; LANES]) {
    kernels::zip_add_f64(a, b, out);
}

#[inline]
pub fn sub_f64(a: &[f64; LANES], b: &[f64; LANES], out: &mut [f64; LANES]) {
    kernels::zip_sub_f64(a, b, out);
}

#[inline]
pub fn mul_f64(a: &[f64; LANES], b: &[f64; LANES], out: &mut [f64; LANES]) {
    kernels::zip_mul_f64(a, b, out);
}

#[inline]
pub fn div_f64(a: &[f64; LANES], b: &[f64; LANES], out: &mut [f64; LANES]) {
    kernels::zip_div_f64(a, b, out);
}

#[inline]
pub fn neg_f64(a: &[f64; LANES], out: &mut [f64; LANES]) {
    kernels::zip_neg_f64(a, out);
}

#[inline]
pub fn add_i64(a: &[i64; LANES], b: &[i64; LANES], out: &mut [i64; LANES]) {
    kernels::zip_add_i64(a, b, out);
}

#[inline]
pub fn sub_i64(a: &[i64; LANES], b: &[i64; LANES], out: &mut [i64; LANES]) {
    kernels::zip_sub_i64(a, b, out);
}

#[inline]
pub fn mul_i64(a: &[i64; LANES], b: &[i64; LANES], out: &mut [i64; LANES]) {
    kernels::zip_mul_i64(a, b, out);
}

#[inline]
pub fn neg_i64(a: &[i64; LANES], out: &mut [i64; LANES]) {
    kernels::zip_neg_i64(a, out);
}

#[inline]
pub fn scale_f64(a: &[f64; LANES], scalar: f64, out: &mut [f64; LANES]) {
    kernels::scale_f64(a, scalar, out);
}

#[inline]
pub fn scale_i64(a: &[i64; LANES], scalar: i64, out: &mut [i64; LANES]) {
    kernels::scale_i64(a, scalar, out);
}

/// Left-fold `init + lanes[0] + … + lanes[7]`. Float order matches scalar
/// `s = s + a[i]` (P11 — no pairwise tree).
#[inline]
pub fn fold_add_f64(init: f64, lanes: &[f64; LANES]) -> f64 {
    let mut s = init;
    for x in lanes {
        s = s + *x;
    }
    s
}

/// Wrapping left-fold add (same result as a tree sum).
#[inline]
pub fn fold_add_i64(init: i64, lanes: &[i64; LANES]) -> i64 {
    let mut s = init;
    for x in lanes {
        s = s.wrapping_add(*x);
    }
    s
}

/// Conservative FMA: `out[i] = (a[i] * b[i]) + c[i]` (two IEEE roundings).
#[inline]
pub fn fmadd_f64(
    a: &[f64; LANES],
    b: &[f64; LANES],
    c: &[f64; LANES],
    out: &mut [f64; LANES],
) {
    let mut prod = [0.0f64; LANES];
    kernels::zip_mul_f64(a, b, &mut prod);
    kernels::zip_add_f64(&prod, c, out);
}

/// Wrapping `out[i] = a[i] * b[i] + c[i]`.
#[inline]
pub fn fmadd_i64(
    a: &[i64; LANES],
    b: &[i64; LANES],
    c: &[i64; LANES],
    out: &mut [i64; LANES],
) {
    let mut prod = [0i64; LANES];
    kernels::zip_mul_i64(a, b, &mut prod);
    kernels::zip_add_i64(&prod, c, out);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_f64_matches_scalar() {
        let a = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0];
        let b = [8.0, 7.0, 6.0, 5.0, 4.0, 3.0, 2.0, 1.0];
        let mut out = [0.0; LANES];
        add_f64(&a, &b, &mut out);
        assert_eq!(out, [9.0; LANES]);
    }

    #[test]
    fn add_i64_wrapping() {
        let a = [1i64; LANES];
        let b = [2i64; LANES];
        let mut out = [0i64; LANES];
        add_i64(&a, &b, &mut out);
        assert_eq!(out, [3i64; LANES]);
    }

    #[test]
    fn fold_add_f64_is_left_assoc() {
        let lanes = [1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0];
        let mut s = 10.0;
        for x in lanes {
            s = s + x;
        }
        assert_eq!(fold_add_f64(10.0, &lanes), s);
    }

    #[test]
    fn fmadd_f64_is_mul_then_add() {
        let a = [2.0; LANES];
        let b = [3.0; LANES];
        let c = [4.0; LANES];
        let mut out = [0.0; LANES];
        fmadd_f64(&a, &b, &c, &mut out);
        assert_eq!(out, [10.0; LANES]);
    }
}
