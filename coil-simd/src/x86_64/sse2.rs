//! SSE2 kernels (2-wide `f64` / `i64`). Always available on x86_64.

#![allow(clippy::missing_safety_doc)]

#[cfg(target_arch = "x86_64")]
use std::arch::x86_64::*;

#[target_feature(enable = "sse2")]
pub unsafe fn dot_f64(a: &[f64], b: &[f64]) -> f64 {
    let n = a.len().min(b.len());
    let mut i = 0;
    let mut acc = _mm_setzero_pd();
    while i + 2 <= n {
        let va = _mm_loadu_pd(a.as_ptr().add(i));
        let vb = _mm_loadu_pd(b.as_ptr().add(i));
        acc = _mm_add_pd(acc, _mm_mul_pd(va, vb));
        i += 2;
    }
    let mut sum = hsum_pd(acc);
    while i < n {
        sum += *a.get_unchecked(i) * *b.get_unchecked(i);
        i += 1;
    }
    sum
}

#[target_feature(enable = "sse2")]
pub unsafe fn zip_add_f64(a: &[f64], b: &[f64], out: &mut [f64]) {
    zip_binop_f64(a, b, out, |x, y| _mm_add_pd(x, y), |x, y| x + y);
}

#[target_feature(enable = "sse2")]
pub unsafe fn zip_sub_f64(a: &[f64], b: &[f64], out: &mut [f64]) {
    zip_binop_f64(a, b, out, |x, y| _mm_sub_pd(x, y), |x, y| x - y);
}

#[target_feature(enable = "sse2")]
pub unsafe fn zip_mul_f64(a: &[f64], b: &[f64], out: &mut [f64]) {
    zip_binop_f64(a, b, out, |x, y| _mm_mul_pd(x, y), |x, y| x * y);
}

#[target_feature(enable = "sse2")]
pub unsafe fn zip_div_f64(a: &[f64], b: &[f64], out: &mut [f64]) {
    zip_binop_f64(a, b, out, |x, y| _mm_div_pd(x, y), |x, y| x / y);
}

#[target_feature(enable = "sse2")]
pub unsafe fn scale_f64(a: &[f64], scalar: f64, out: &mut [f64]) {
    let n = a.len().min(out.len());
    let mut i = 0;
    let vs = _mm_set1_pd(scalar);
    while i + 2 <= n {
        let va = _mm_loadu_pd(a.as_ptr().add(i));
        _mm_storeu_pd(out.as_mut_ptr().add(i), _mm_mul_pd(va, vs));
        i += 2;
    }
    while i < n {
        *out.get_unchecked_mut(i) = *a.get_unchecked(i) * scalar;
        i += 1;
    }
}

#[target_feature(enable = "sse2")]
pub unsafe fn zip_neg_f64(a: &[f64], out: &mut [f64]) {
    let n = a.len().min(out.len());
    let mut i = 0;
    let sign = _mm_set1_pd(-0.0);
    while i + 2 <= n {
        let va = _mm_loadu_pd(a.as_ptr().add(i));
        _mm_storeu_pd(out.as_mut_ptr().add(i), _mm_xor_pd(va, sign));
        i += 2;
    }
    while i < n {
        *out.get_unchecked_mut(i) = -*a.get_unchecked(i);
        i += 1;
    }
}

#[target_feature(enable = "sse2")]
pub unsafe fn zip_add_i64(a: &[i64], b: &[i64], out: &mut [i64]) {
    zip_binop_i64(a, b, out, |x, y| _mm_add_epi64(x, y), i64::wrapping_add);
}

#[target_feature(enable = "sse2")]
pub unsafe fn zip_sub_i64(a: &[i64], b: &[i64], out: &mut [i64]) {
    zip_binop_i64(a, b, out, |x, y| _mm_sub_epi64(x, y), i64::wrapping_sub);
}

#[target_feature(enable = "sse2")]
pub unsafe fn zip_neg_i64(a: &[i64], out: &mut [i64]) {
    let n = a.len().min(out.len());
    let mut i = 0;
    let zero = _mm_setzero_si128();
    while i + 2 <= n {
        let va = _mm_loadu_si128(a.as_ptr().add(i) as *const __m128i);
        _mm_storeu_si128(
            out.as_mut_ptr().add(i) as *mut __m128i,
            _mm_sub_epi64(zero, va),
        );
        i += 2;
    }
    while i < n {
        *out.get_unchecked_mut(i) = (*a.get_unchecked(i)).wrapping_neg();
        i += 1;
    }
}

/// GEMM with contiguous B/C rows: for each `a_it`, `c_row += a_it * b_row`.
#[target_feature(enable = "sse2")]
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

#[target_feature(enable = "sse2")]
pub unsafe fn matmul_i64(a: &[i64], b: &[i64], c: &mut [i64], m: usize, k: usize, n: usize) {
    // No cheap 64-bit integer multiply in SSE2 — keep scalar product, SIMD adds via saxpy fallback.
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

#[target_feature(enable = "sse2")]
unsafe fn saxpy_f64(alpha: f64, x: &[f64], y: &mut [f64]) {
    let n = x.len().min(y.len());
    let mut i = 0;
    let va = _mm_set1_pd(alpha);
    while i + 2 <= n {
        let vx = _mm_loadu_pd(x.as_ptr().add(i));
        let vy = _mm_loadu_pd(y.as_ptr().add(i));
        _mm_storeu_pd(y.as_mut_ptr().add(i), _mm_add_pd(vy, _mm_mul_pd(va, vx)));
        i += 2;
    }
    while i < n {
        *y.get_unchecked_mut(i) += alpha * *x.get_unchecked(i);
        i += 1;
    }
}

#[target_feature(enable = "sse2")]
#[inline]
unsafe fn zip_binop_f64(
    a: &[f64],
    b: &[f64],
    out: &mut [f64],
    simd: impl Fn(__m128d, __m128d) -> __m128d,
    scalar: impl Fn(f64, f64) -> f64,
) {
    let n = a.len().min(b.len()).min(out.len());
    let mut i = 0;
    while i + 2 <= n {
        let va = _mm_loadu_pd(a.as_ptr().add(i));
        let vb = _mm_loadu_pd(b.as_ptr().add(i));
        _mm_storeu_pd(out.as_mut_ptr().add(i), simd(va, vb));
        i += 2;
    }
    while i < n {
        *out.get_unchecked_mut(i) = scalar(*a.get_unchecked(i), *b.get_unchecked(i));
        i += 1;
    }
}

#[target_feature(enable = "sse2")]
#[inline]
unsafe fn zip_binop_i64(
    a: &[i64],
    b: &[i64],
    out: &mut [i64],
    simd: impl Fn(__m128i, __m128i) -> __m128i,
    scalar: impl Fn(i64, i64) -> i64,
) {
    let n = a.len().min(b.len()).min(out.len());
    let mut i = 0;
    while i + 2 <= n {
        let va = _mm_loadu_si128(a.as_ptr().add(i) as *const __m128i);
        let vb = _mm_loadu_si128(b.as_ptr().add(i) as *const __m128i);
        _mm_storeu_si128(out.as_mut_ptr().add(i) as *mut __m128i, simd(va, vb));
        i += 2;
    }
    while i < n {
        *out.get_unchecked_mut(i) = scalar(*a.get_unchecked(i), *b.get_unchecked(i));
        i += 1;
    }
}

#[target_feature(enable = "sse2")]
#[inline]
unsafe fn hsum_pd(v: __m128d) -> f64 {
    let hi = _mm_unpackhi_pd(v, v);
    let sum = _mm_add_sd(v, hi);
    _mm_cvtsd_f64(sum)
}

/// 2-wide `a*x` then left-fold `(s + ax) + y`.
#[target_feature(enable = "sse2")]
pub unsafe fn axpy_reduce_f64(n: usize, a: f64, mut x: f64, dx: f64, y: f64) -> f64 {
    let mut s = 0.0;
    let mut i = 0;
    let va = _mm_set1_pd(a);
    while i + 2 <= n {
        let x0 = x;
        let x1 = x0 + dx;
        let vx = _mm_set_pd(x1, x0);
        let mut ax = [0.0; 2];
        _mm_storeu_pd(ax.as_mut_ptr(), _mm_mul_pd(va, vx));
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
