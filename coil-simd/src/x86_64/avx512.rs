//! AVX-512F/DQ/BW kernels — 8-wide `f64` / `i64`, 64-byte byte ops.
//!
//! Only called after runtime detection of `avx512f` + `avx512dq` + `avx512bw`.

#![allow(clippy::missing_safety_doc)]

#[cfg(target_arch = "x86_64")]
use std::arch::x86_64::*;

#[target_feature(enable = "avx512f")]
pub unsafe fn dot_f64(a: &[f64], b: &[f64]) -> f64 {
    let n = a.len().min(b.len());
    let mut i = 0;
    let mut acc = _mm512_setzero_pd();
    while i + 8 <= n {
        let va = _mm512_loadu_pd(a.as_ptr().add(i));
        let vb = _mm512_loadu_pd(b.as_ptr().add(i));
        acc = _mm512_fmadd_pd(va, vb, acc);
        i += 8;
    }
    let mut sum = _mm512_reduce_add_pd(acc);
    while i < n {
        sum += *a.get_unchecked(i) * *b.get_unchecked(i);
        i += 1;
    }
    sum
}

#[target_feature(enable = "avx512f", enable = "avx512dq")]
pub unsafe fn dot_i64(a: &[i64], b: &[i64]) -> i64 {
    let n = a.len().min(b.len());
    let mut i = 0;
    let mut acc = _mm512_setzero_si512();
    while i + 8 <= n {
        let va = _mm512_loadu_si512(a.as_ptr().add(i) as *const __m512i);
        let vb = _mm512_loadu_si512(b.as_ptr().add(i) as *const __m512i);
        acc = _mm512_add_epi64(acc, _mm512_mullo_epi64(va, vb));
        i += 8;
    }
    let mut sum = _mm512_reduce_add_epi64(acc);
    while i < n {
        sum = sum.wrapping_add((*a.get_unchecked(i)).wrapping_mul(*b.get_unchecked(i)));
        i += 1;
    }
    sum
}

#[target_feature(enable = "avx512f")]
pub unsafe fn zip_add_f64(a: &[f64], b: &[f64], out: &mut [f64]) {
    zip_binop_f64(a, b, out, |x, y| _mm512_add_pd(x, y), |x, y| x + y);
}

#[target_feature(enable = "avx512f")]
pub unsafe fn zip_sub_f64(a: &[f64], b: &[f64], out: &mut [f64]) {
    zip_binop_f64(a, b, out, |x, y| _mm512_sub_pd(x, y), |x, y| x - y);
}

#[target_feature(enable = "avx512f")]
pub unsafe fn zip_mul_f64(a: &[f64], b: &[f64], out: &mut [f64]) {
    zip_binop_f64(a, b, out, |x, y| _mm512_mul_pd(x, y), |x, y| x * y);
}

#[target_feature(enable = "avx512f")]
pub unsafe fn zip_div_f64(a: &[f64], b: &[f64], out: &mut [f64]) {
    zip_binop_f64(a, b, out, |x, y| _mm512_div_pd(x, y), |x, y| x / y);
}

#[target_feature(enable = "avx512f")]
pub unsafe fn scale_f64(a: &[f64], scalar: f64, out: &mut [f64]) {
    let n = a.len().min(out.len());
    let mut i = 0;
    let vs = _mm512_set1_pd(scalar);
    while i + 8 <= n {
        let va = _mm512_loadu_pd(a.as_ptr().add(i));
        _mm512_storeu_pd(out.as_mut_ptr().add(i), _mm512_mul_pd(va, vs));
        i += 8;
    }
    while i < n {
        *out.get_unchecked_mut(i) = *a.get_unchecked(i) * scalar;
        i += 1;
    }
}

#[target_feature(enable = "avx512f")]
pub unsafe fn zip_neg_f64(a: &[f64], out: &mut [f64]) {
    let n = a.len().min(out.len());
    let mut i = 0;
    let sign = _mm512_set1_pd(-0.0);
    while i + 8 <= n {
        let va = _mm512_loadu_pd(a.as_ptr().add(i));
        _mm512_storeu_pd(out.as_mut_ptr().add(i), _mm512_xor_pd(va, sign));
        i += 8;
    }
    while i < n {
        *out.get_unchecked_mut(i) = -*a.get_unchecked(i);
        i += 1;
    }
}

#[target_feature(enable = "avx512f")]
pub unsafe fn zip_add_i64(a: &[i64], b: &[i64], out: &mut [i64]) {
    zip_binop_i64(a, b, out, |x, y| _mm512_add_epi64(x, y), i64::wrapping_add);
}

#[target_feature(enable = "avx512f")]
pub unsafe fn zip_sub_i64(a: &[i64], b: &[i64], out: &mut [i64]) {
    zip_binop_i64(a, b, out, |x, y| _mm512_sub_epi64(x, y), i64::wrapping_sub);
}

#[target_feature(enable = "avx512f", enable = "avx512dq")]
pub unsafe fn zip_mul_i64(a: &[i64], b: &[i64], out: &mut [i64]) {
    zip_binop_i64(
        a,
        b,
        out,
        |x, y| _mm512_mullo_epi64(x, y),
        i64::wrapping_mul,
    );
}

#[target_feature(enable = "avx512f", enable = "avx512dq")]
pub unsafe fn scale_i64(a: &[i64], scalar: i64, out: &mut [i64]) {
    let n = a.len().min(out.len());
    let mut i = 0;
    let vs = _mm512_set1_epi64(scalar);
    while i + 8 <= n {
        let va = _mm512_loadu_si512(a.as_ptr().add(i) as *const __m512i);
        _mm512_storeu_si512(
            out.as_mut_ptr().add(i) as *mut __m512i,
            _mm512_mullo_epi64(va, vs),
        );
        i += 8;
    }
    while i < n {
        *out.get_unchecked_mut(i) = (*a.get_unchecked(i)).wrapping_mul(scalar);
        i += 1;
    }
}

#[target_feature(enable = "avx512f")]
pub unsafe fn zip_neg_i64(a: &[i64], out: &mut [i64]) {
    let n = a.len().min(out.len());
    let mut i = 0;
    let zero = _mm512_setzero_si512();
    while i + 8 <= n {
        let va = _mm512_loadu_si512(a.as_ptr().add(i) as *const __m512i);
        _mm512_storeu_si512(
            out.as_mut_ptr().add(i) as *mut __m512i,
            _mm512_sub_epi64(zero, va),
        );
        i += 8;
    }
    while i < n {
        *out.get_unchecked_mut(i) = (*a.get_unchecked(i)).wrapping_neg();
        i += 1;
    }
}

#[target_feature(enable = "avx512f")]
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

#[target_feature(enable = "avx512f", enable = "avx512dq")]
pub unsafe fn matmul_i64(a: &[i64], b: &[i64], c: &mut [i64], m: usize, k: usize, n: usize) {
    c.fill(0);
    for i in 0..m {
        for t in 0..k {
            let a_it = *a.get_unchecked(i * k + t);
            let b_row = b.get_unchecked(t * n..t * n + n);
            let c_row = c.get_unchecked_mut(i * n..i * n + n);
            saxpy_i64(a_it, b_row, c_row);
        }
    }
}

#[target_feature(enable = "avx512f")]
unsafe fn saxpy_f64(alpha: f64, x: &[f64], y: &mut [f64]) {
    let n = x.len().min(y.len());
    let mut i = 0;
    let va = _mm512_set1_pd(alpha);
    while i + 8 <= n {
        let vx = _mm512_loadu_pd(x.as_ptr().add(i));
        let vy = _mm512_loadu_pd(y.as_ptr().add(i));
        _mm512_storeu_pd(y.as_mut_ptr().add(i), _mm512_fmadd_pd(va, vx, vy));
        i += 8;
    }
    while i < n {
        *y.get_unchecked_mut(i) += alpha * *x.get_unchecked(i);
        i += 1;
    }
}

#[target_feature(enable = "avx512f", enable = "avx512dq")]
unsafe fn saxpy_i64(alpha: i64, x: &[i64], y: &mut [i64]) {
    let n = x.len().min(y.len());
    let mut i = 0;
    let va = _mm512_set1_epi64(alpha);
    while i + 8 <= n {
        let vx = _mm512_loadu_si512(x.as_ptr().add(i) as *const __m512i);
        let vy = _mm512_loadu_si512(y.as_ptr().add(i) as *const __m512i);
        let prod = _mm512_mullo_epi64(va, vx);
        _mm512_storeu_si512(
            y.as_mut_ptr().add(i) as *mut __m512i,
            _mm512_add_epi64(vy, prod),
        );
        i += 8;
    }
    while i < n {
        *y.get_unchecked_mut(i) = y
            .get_unchecked(i)
            .wrapping_add(alpha.wrapping_mul(*x.get_unchecked(i)));
        i += 1;
    }
}

#[target_feature(enable = "avx512f")]
#[inline]
unsafe fn zip_binop_f64(
    a: &[f64],
    b: &[f64],
    out: &mut [f64],
    simd: impl Fn(__m512d, __m512d) -> __m512d,
    scalar: impl Fn(f64, f64) -> f64,
) {
    let n = a.len().min(b.len()).min(out.len());
    let mut i = 0;
    while i + 8 <= n {
        let va = _mm512_loadu_pd(a.as_ptr().add(i));
        let vb = _mm512_loadu_pd(b.as_ptr().add(i));
        _mm512_storeu_pd(out.as_mut_ptr().add(i), simd(va, vb));
        i += 8;
    }
    while i < n {
        *out.get_unchecked_mut(i) = scalar(*a.get_unchecked(i), *b.get_unchecked(i));
        i += 1;
    }
}

#[target_feature(enable = "avx512f")]
#[inline]
unsafe fn zip_binop_i64(
    a: &[i64],
    b: &[i64],
    out: &mut [i64],
    simd: impl Fn(__m512i, __m512i) -> __m512i,
    scalar: impl Fn(i64, i64) -> i64,
) {
    let n = a.len().min(b.len()).min(out.len());
    let mut i = 0;
    while i + 8 <= n {
        let va = _mm512_loadu_si512(a.as_ptr().add(i) as *const __m512i);
        let vb = _mm512_loadu_si512(b.as_ptr().add(i) as *const __m512i);
        _mm512_storeu_si512(out.as_mut_ptr().add(i) as *mut __m512i, simd(va, vb));
        i += 8;
    }
    while i < n {
        *out.get_unchecked_mut(i) = scalar(*a.get_unchecked(i), *b.get_unchecked(i));
        i += 1;
    }
}

/// 8-wide `a*x` then left-fold `(s + ax) + y` (no FMA).
#[target_feature(enable = "avx512f")]
pub unsafe fn axpy_reduce_f64(n: usize, a: f64, mut x: f64, dx: f64, y: f64) -> f64 {
    let mut s = 0.0;
    let mut i = 0;
    let va = _mm512_set1_pd(a);
    while i + 8 <= n {
        let mut xs = [0.0; 8];
        xs[0] = x;
        for k in 1..8 {
            xs[k] = xs[k - 1] + dx;
        }
        let vx = _mm512_loadu_pd(xs.as_ptr());
        let mut ax = [0.0; 8];
        _mm512_storeu_pd(ax.as_mut_ptr(), _mm512_mul_pd(va, vx));
        for k in 0..8 {
            s = s + ax[k];
            s = s + y;
        }
        x = xs[7] + dx;
        i += 8;
    }
    while i < n {
        s = s + a * x;
        s = s + y;
        x = x + dx;
        i += 1;
    }
    s
}
