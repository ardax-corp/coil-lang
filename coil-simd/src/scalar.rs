//! Scalar reference implementations (also used for tails / unsupported ISAs).

#[inline]
pub fn dot_f64(a: &[f64], b: &[f64]) -> f64 {
    let n = a.len().min(b.len());
    let mut sum = 0.0;
    for i in 0..n {
        sum += a[i] * b[i];
    }
    sum
}

#[inline]
pub fn dot_i64(a: &[i64], b: &[i64]) -> i64 {
    let n = a.len().min(b.len());
    let mut sum = 0_i64;
    for i in 0..n {
        sum = sum.wrapping_add(a[i].wrapping_mul(b[i]));
    }
    sum
}

#[inline]
pub fn zip_add_f64(a: &[f64], b: &[f64], out: &mut [f64]) {
    let n = a.len().min(b.len()).min(out.len());
    for i in 0..n {
        out[i] = a[i] + b[i];
    }
}

#[inline]
pub fn zip_sub_f64(a: &[f64], b: &[f64], out: &mut [f64]) {
    let n = a.len().min(b.len()).min(out.len());
    for i in 0..n {
        out[i] = a[i] - b[i];
    }
}

#[inline]
pub fn zip_neg_f64(a: &[f64], out: &mut [f64]) {
    let n = a.len().min(out.len());
    for i in 0..n {
        out[i] = -a[i];
    }
}

#[inline]
pub fn zip_add_i64(a: &[i64], b: &[i64], out: &mut [i64]) {
    let n = a.len().min(b.len()).min(out.len());
    for i in 0..n {
        out[i] = a[i].wrapping_add(b[i]);
    }
}

#[inline]
pub fn zip_sub_i64(a: &[i64], b: &[i64], out: &mut [i64]) {
    let n = a.len().min(b.len()).min(out.len());
    for i in 0..n {
        out[i] = a[i].wrapping_sub(b[i]);
    }
}

#[inline]
pub fn zip_neg_i64(a: &[i64], out: &mut [i64]) {
    let n = a.len().min(out.len());
    for i in 0..n {
        out[i] = a[i].wrapping_neg();
    }
}

#[inline]
pub fn zip_mul_f64(a: &[f64], b: &[f64], out: &mut [f64]) {
    let n = a.len().min(b.len()).min(out.len());
    for i in 0..n {
        out[i] = a[i] * b[i];
    }
}

#[inline]
pub fn zip_mul_i64(a: &[i64], b: &[i64], out: &mut [i64]) {
    let n = a.len().min(b.len()).min(out.len());
    for i in 0..n {
        out[i] = a[i].wrapping_mul(b[i]);
    }
}

#[inline]
pub fn zip_div_f64(a: &[f64], b: &[f64], out: &mut [f64]) {
    let n = a.len().min(b.len()).min(out.len());
    for i in 0..n {
        out[i] = a[i] / b[i];
    }
}

/// Sequential `s = (s + a*x) + y; x = x + dx` for `n` trips. Matches Coil
/// `s = s + a * x + y` (left-assoc, mul then add, no FMA).
#[inline]
pub fn axpy_reduce_f64(n: usize, a: f64, mut x: f64, dx: f64, y: f64) -> f64 {
    let mut s = 0.0;
    for _ in 0..n {
        s = s + a * x;
        s = s + y;
        x = x + dx;
    }
    s
}

/// `out[i] = a[i] * scalar` (broadcast multiply).
#[inline]
pub fn scale_f64(a: &[f64], scalar: f64, out: &mut [f64]) {
    let n = a.len().min(out.len());
    for i in 0..n {
        out[i] = a[i] * scalar;
    }
}

/// Wrapping `out[i] = a[i] * scalar`.
#[inline]
pub fn scale_i64(a: &[i64], scalar: i64, out: &mut [i64]) {
    let n = a.len().min(out.len());
    for i in 0..n {
        out[i] = a[i].wrapping_mul(scalar);
    }
}

/// Row-major C = A(m×k) * B(k×n). Accumulates with wrapping for `i64`.
#[inline]
pub fn matmul_f64(a: &[f64], b: &[f64], c: &mut [f64], m: usize, k: usize, n: usize) {
    debug_assert_eq!(a.len(), m.saturating_mul(k));
    debug_assert_eq!(b.len(), k.saturating_mul(n));
    debug_assert_eq!(c.len(), m.saturating_mul(n));
    c.fill(0.0);
    for i in 0..m {
        for t in 0..k {
            let a_it = a[i * k + t];
            let b_row = &b[t * n..t * n + n];
            let c_row = &mut c[i * n..i * n + n];
            for j in 0..n {
                c_row[j] += a_it * b_row[j];
            }
        }
    }
}

#[inline]
pub fn matmul_i64(a: &[i64], b: &[i64], c: &mut [i64], m: usize, k: usize, n: usize) {
    debug_assert_eq!(a.len(), m.saturating_mul(k));
    debug_assert_eq!(b.len(), k.saturating_mul(n));
    debug_assert_eq!(c.len(), m.saturating_mul(n));
    c.fill(0);
    for i in 0..m {
        for t in 0..k {
            let a_it = a[i * k + t];
            let b_row = &b[t * n..t * n + n];
            let c_row = &mut c[i * n..i * n + n];
            for j in 0..n {
                c_row[j] = c_row[j].wrapping_add(a_it.wrapping_mul(b_row[j]));
            }
        }
    }
}

/// Element-wise integer cell op. `kind` matches `MatrixCellOp::zip_kind`
/// (2 eq … 14 diff). Shift counts use the low 6 bits. When `byte_width` is
/// set, bitwise results are masked to `0..=255`.
#[inline]
pub fn zip_i64_op(kind: u8, a: &[i64], b: &[i64], out: &mut [i64], byte_width: bool) {
    let n = a.len().min(b.len()).min(out.len());
    for i in 0..n {
        let (x, y) = (a[i], b[i]);
        let mut v = match kind {
            2 => i64::from(x == y),
            3 => i64::from(x != y),
            4 => i64::from(x < y),
            5 => i64::from(x <= y),
            6 => i64::from(x > y),
            7 => i64::from(x >= y),
            8 => x & y,
            9 => x | y,
            10 => x ^ y,
            11 => x.wrapping_shl((y as u32) & 63),
            12 => x >> ((y as u32) & 63),
            13 => i64::from(x != 0 && y != 0),
            14 => i64::from(x != 0 && y == 0),
            _ => 0,
        };
        if byte_width && matches!(kind, 8 | 9 | 10 | 11 | 12) {
            v &= 0xFF;
        }
        out[i] = v;
    }
}

/// Bitwise `~`. `byte_width` keeps the low 8 bits.
#[inline]
pub fn zip_i64_not(a: &[i64], out: &mut [i64], byte_width: bool) {
    let n = a.len().min(out.len());
    for i in 0..n {
        let mut v = !a[i];
        if byte_width {
            v &= 0xFF;
        }
        out[i] = v;
    }
}

/// Float compares and presence masks. `out` is `0`/`1`. `kind` 2..=7 are
/// IEEE compares; 13 is intersect (both non-zero); 14 is diff (left non-zero,
/// right zero). `NaN` counts as present and compares unequal.
#[inline]
pub fn zip_f64_mask(kind: u8, a: &[f64], b: &[f64], out: &mut [i64]) {
    let n = a.len().min(b.len()).min(out.len());
    for i in 0..n {
        let (x, y) = (a[i], b[i]);
        out[i] = match kind {
            2 => i64::from(x == y),
            3 => i64::from(x != y),
            4 => i64::from(x < y),
            5 => i64::from(x <= y),
            6 => i64::from(x > y),
            7 => i64::from(x >= y),
            13 => i64::from(x != 0.0 && y != 0.0),
            14 => i64::from(x != 0.0 && y == 0.0),
            _ => 0,
        };
    }
}

#[cfg(test)]
mod mask_tests {
    use super::{zip_f64_mask, zip_i64_not, zip_i64_op};

    #[test]
    fn integer_masks_and_byte_not() {
        let a = [1, 2, 3, 4, 5, 6, 7, 8];
        let b = [1, 0, 3, 9, 5, 1, 0, 8];
        let mut eq = [0; 8];
        zip_i64_op(2, &a, &b, &mut eq, false);
        assert_eq!(eq, [1, 0, 1, 0, 1, 0, 0, 1]);
        let mut both = [0; 8];
        zip_i64_op(13, &a, &b, &mut both, false);
        assert_eq!(both, [1, 0, 1, 1, 1, 1, 0, 1]);
        let mut only = [0; 8];
        zip_i64_op(14, &a, &b, &mut only, false);
        assert_eq!(only, [0, 1, 0, 0, 0, 0, 1, 0]);
        let mut bits = [0; 8];
        zip_i64_op(8, &a, &b, &mut bits, false);
        assert_eq!(bits, [1, 0, 3, 0, 5, 0, 0, 8]);
        let mut shifted = [0; 8];
        let ones = [1; 8];
        zip_i64_op(11, &a, &ones, &mut shifted, false);
        assert_eq!(shifted, [2, 4, 6, 8, 10, 12, 14, 16]);
        let mut neg = [0; 1];
        zip_i64_not(&[34], &mut neg, true);
        assert_eq!(neg[0], 221);
    }

    #[test]
    fn float_compare_treats_nan_as_unequal_and_present() {
        let a = [1.0, f64::NAN];
        let b = [1.0, 1.0];
        let mut eq = [0; 2];
        zip_f64_mask(2, &a, &b, &mut eq);
        assert_eq!(eq[0], 1);
        assert_eq!(eq[1], 0);
        let mut both = [0; 2];
        zip_f64_mask(13, &a, &b, &mut both);
        assert_eq!(both, [1, 1]);
    }
}
