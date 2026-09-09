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
}
