//! aarch64 NEON kernels (2-wide `f64` / `i64`).

#![allow(clippy::missing_safety_doc)]

#[cfg(target_arch = "aarch64")]
use std::arch::aarch64::*;

#[target_feature(enable = "neon")]
pub unsafe fn dot_f64(a: &[f64], b: &[f64]) -> f64 {
    let n = a.len().min(b.len());
    let mut i = 0;
    let mut acc = vdupq_n_f64(0.0);
    while i + 2 <= n {
        let va = vld1q_f64(a.as_ptr().add(i));
        let vb = vld1q_f64(b.as_ptr().add(i));
        acc = vfmaq_f64(acc, va, vb);
        i += 2;
    }
    let mut sum = vaddvq_f64(acc);
    while i < n {
        sum += *a.get_unchecked(i) * *b.get_unchecked(i);
        i += 1;
    }
    sum
}

#[target_feature(enable = "neon")]
pub unsafe fn zip_add_f64(a: &[f64], b: &[f64], out: &mut [f64]) {
    zip_binop_f64(a, b, out, |x, y| vaddq_f64(x, y), |x, y| x + y);
}

#[target_feature(enable = "neon")]
pub unsafe fn zip_sub_f64(a: &[f64], b: &[f64], out: &mut [f64]) {
    zip_binop_f64(a, b, out, |x, y| vsubq_f64(x, y), |x, y| x - y);
}

#[target_feature(enable = "neon")]
pub unsafe fn zip_mul_f64(a: &[f64], b: &[f64], out: &mut [f64]) {
    zip_binop_f64(a, b, out, |x, y| vmulq_f64(x, y), |x, y| x * y);
}

#[target_feature(enable = "neon")]
pub unsafe fn zip_div_f64(a: &[f64], b: &[f64], out: &mut [f64]) {
    zip_binop_f64(a, b, out, |x, y| vdivq_f64(x, y), |x, y| x / y);
}

#[target_feature(enable = "neon")]
pub unsafe fn scale_f64(a: &[f64], scalar: f64, out: &mut [f64]) {
    let n = a.len().min(out.len());
    let mut i = 0;
    let vs = vdupq_n_f64(scalar);
    while i + 2 <= n {
        let va = vld1q_f64(a.as_ptr().add(i));
        vst1q_f64(out.as_mut_ptr().add(i), vmulq_f64(va, vs));
        i += 2;
    }
    while i < n {
        *out.get_unchecked_mut(i) = *a.get_unchecked(i) * scalar;
        i += 1;
    }
}

#[target_feature(enable = "neon")]
pub unsafe fn zip_neg_f64(a: &[f64], out: &mut [f64]) {
    let n = a.len().min(out.len());
    let mut i = 0;
    while i + 2 <= n {
        let va = vld1q_f64(a.as_ptr().add(i));
        vst1q_f64(out.as_mut_ptr().add(i), vnegq_f64(va));
        i += 2;
    }
    while i < n {
        *out.get_unchecked_mut(i) = -*a.get_unchecked(i);
        i += 1;
    }
}

#[target_feature(enable = "neon")]
pub unsafe fn zip_add_i64(a: &[i64], b: &[i64], out: &mut [i64]) {
    zip_binop_i64(a, b, out, |x, y| vaddq_s64(x, y), i64::wrapping_add);
}

#[target_feature(enable = "neon")]
pub unsafe fn zip_sub_i64(a: &[i64], b: &[i64], out: &mut [i64]) {
    zip_binop_i64(a, b, out, |x, y| vsubq_s64(x, y), i64::wrapping_sub);
}

#[target_feature(enable = "neon")]
pub unsafe fn zip_neg_i64(a: &[i64], out: &mut [i64]) {
    let n = a.len().min(out.len());
    let mut i = 0;
    let zero = vdupq_n_s64(0);
    while i + 2 <= n {
        let va = vld1q_s64(a.as_ptr().add(i));
        vst1q_s64(out.as_mut_ptr().add(i), vsubq_s64(zero, va));
        i += 2;
    }
    while i < n {
        *out.get_unchecked_mut(i) = (*a.get_unchecked(i)).wrapping_neg();
        i += 1;
    }
}

#[target_feature(enable = "neon")]
pub unsafe fn matmul_f64(a: &[f64], b: &[f64], c: &mut [f64], m: usize, k: usize, n: usize) {
    c.fill(0.0);
    for i in 0..m {
        for t in 0..k {
            let a_it = *a.get_unchecked(i * k + t);
            let b_row = b.get_unchecked(t * n..t * n + n);
            let c_row = c.get_unchecked_mut(i * n..i * n + n);
            saxpy_f64(a_it, b_row, c_row);
        }
    }
}

#[target_feature(enable = "neon")]
pub unsafe fn matmul_i64(a: &[i64], b: &[i64], c: &mut [i64], m: usize, k: usize, n: usize) {
    c.fill(0);
    for i in 0..m {
        for t in 0..k {
            let a_it = *a.get_unchecked(i * k + t);
            let b_row = b.get_unchecked(t * n..t * n + n);
            let c_row = c.get_unchecked_mut(i * n..i * n + n);
            for j in 0..n {
                *c_row.get_unchecked_mut(j) = c_row
                    .get_unchecked(j)
                    .wrapping_add(a_it.wrapping_mul(*b_row.get_unchecked(j)));
            }
        }
    }
}

#[target_feature(enable = "neon")]
unsafe fn saxpy_f64(alpha: f64, x: &[f64], y: &mut [f64]) {
    let n = x.len().min(y.len());
    let mut i = 0;
    let va = vdupq_n_f64(alpha);
    while i + 2 <= n {
        let vx = vld1q_f64(x.as_ptr().add(i));
        let vy = vld1q_f64(y.as_ptr().add(i));
        vst1q_f64(y.as_mut_ptr().add(i), vfmaq_f64(vy, va, vx));
        i += 2;
    }
    while i < n {
        *y.get_unchecked_mut(i) += alpha * *x.get_unchecked(i);
        i += 1;
    }
}

#[target_feature(enable = "neon")]
#[inline]
unsafe fn zip_binop_f64(
    a: &[f64],
    b: &[f64],
    out: &mut [f64],
    simd: impl Fn(float64x2_t, float64x2_t) -> float64x2_t,
    scalar: impl Fn(f64, f64) -> f64,
) {
    let n = a.len().min(b.len()).min(out.len());
    let mut i = 0;
    while i + 2 <= n {
        let va = vld1q_f64(a.as_ptr().add(i));
        let vb = vld1q_f64(b.as_ptr().add(i));
        vst1q_f64(out.as_mut_ptr().add(i), simd(va, vb));
        i += 2;
    }
    while i < n {
        *out.get_unchecked_mut(i) = scalar(*a.get_unchecked(i), *b.get_unchecked(i));
        i += 1;
    }
}

#[target_feature(enable = "neon")]
#[inline]
unsafe fn zip_binop_i64(
    a: &[i64],
    b: &[i64],
    out: &mut [i64],
    simd: impl Fn(int64x2_t, int64x2_t) -> int64x2_t,
    scalar: impl Fn(i64, i64) -> i64,
) {
    let n = a.len().min(b.len()).min(out.len());
    let mut i = 0;
    while i + 2 <= n {
        let va = vld1q_s64(a.as_ptr().add(i));
        let vb = vld1q_s64(b.as_ptr().add(i));
        vst1q_s64(out.as_mut_ptr().add(i), simd(va, vb));
        i += 2;
    }
    while i < n {
        *out.get_unchecked_mut(i) = scalar(*a.get_unchecked(i), *b.get_unchecked(i));
        i += 1;
    }
}

/// 2-wide NEON `a*x` then left-fold `(s + ax) + y`.
#[target_feature(enable = "neon")]
pub unsafe fn axpy_reduce_f64(n: usize, a: f64, mut x: f64, dx: f64, y: f64) -> f64 {
    let mut s = 0.0;
    let mut i = 0;
    let va = vdupq_n_f64(a);
    while i + 2 <= n {
        let x0 = x;
        let x1 = x0 + dx;
        let xs = [x0, x1];
        let vx = vld1q_f64(xs.as_ptr());
        let mut ax = [0.0; 2];
        vst1q_f64(ax.as_mut_ptr(), vmulq_f64(va, vx));
        s = s + ax[0];
        s = s + y;
        s = s + ax[1];
        s = s + y;
        x = x1 + dx;
        i += 2;
    }
    while i < n {
        s = s + a * x;
        s = s + y;
        x = x + dx;
        i += 1;
    }
    s
}
