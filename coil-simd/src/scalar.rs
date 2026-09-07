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
