//! Byte-oriented SIMD helpers (equality / XOR).

use crate::level::{SimdLevel, detect};

/// Byte-slice equality with a SIMD fast path for longer buffers.
///
/// Returns `false` immediately when lengths differ. Not constant-time — do not
/// use for crypto tag compares (`subtle` remains the right tool there).
#[inline]
pub fn eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    if a.len() < 16 {
        return a == b;
    }
    match detect() {
        #[cfg(target_arch = "x86_64")]
        SimdLevel::Avx512 => unsafe { eq_avx512(a, b) },
        #[cfg(target_arch = "x86_64")]
        SimdLevel::Avx2 => unsafe { eq_avx2(a, b) },
        #[cfg(target_arch = "x86_64")]
        SimdLevel::Sse2 => unsafe { eq_sse2(a, b) },
        #[cfg(target_arch = "aarch64")]
        SimdLevel::Neon => unsafe { eq_neon(a, b) },
        _ => a == b,
    }
}

/// `out[i] = a[i] ^ b[i]` for `min(len)` bytes.
#[inline]
pub fn xor(a: &[u8], b: &[u8], out: &mut [u8]) {
    let n = a.len().min(b.len()).min(out.len());
    if n < 16 {
        return scalar_xor(a, b, out, n);
    }
    match detect() {
        #[cfg(target_arch = "x86_64")]
        SimdLevel::Avx512 => unsafe { xor_avx512(a, b, out, n) },
        #[cfg(target_arch = "x86_64")]
        SimdLevel::Avx2 => unsafe { xor_avx2(a, b, out, n) },
        #[cfg(target_arch = "x86_64")]
        SimdLevel::Sse2 => unsafe { xor_sse2(a, b, out, n) },
        #[cfg(target_arch = "aarch64")]
        SimdLevel::Neon => unsafe { xor_neon(a, b, out, n) },
        _ => scalar_xor(a, b, out, n),
    }
}

#[inline]
fn scalar_xor(a: &[u8], b: &[u8], out: &mut [u8], n: usize) {
    for i in 0..n {
        out[i] = a[i] ^ b[i];
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse2")]
unsafe fn eq_sse2(a: &[u8], b: &[u8]) -> bool {
    use std::arch::x86_64::*;
    let n = a.len();
    let mut i = 0;
    while i + 16 <= n {
        let va = _mm_loadu_si128(a.as_ptr().add(i) as *const __m128i);
        let vb = _mm_loadu_si128(b.as_ptr().add(i) as *const __m128i);
        let cmp = _mm_cmpeq_epi8(va, vb);
        if _mm_movemask_epi8(cmp) != 0xFFFF {
            return false;
        }
        i += 16;
    }
    a.get_unchecked(i..) == b.get_unchecked(i..)
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn eq_avx2(a: &[u8], b: &[u8]) -> bool {
    use std::arch::x86_64::*;
    let n = a.len();
    let mut i = 0;
    while i + 32 <= n {
        let va = _mm256_loadu_si256(a.as_ptr().add(i) as *const __m256i);
        let vb = _mm256_loadu_si256(b.as_ptr().add(i) as *const __m256i);
        let cmp = _mm256_cmpeq_epi8(va, vb);
        if _mm256_movemask_epi8(cmp) as u32 != 0xFFFF_FFFF {
            return false;
        }
        i += 32;
    }
    while i + 16 <= n {
        let va = _mm_loadu_si128(a.as_ptr().add(i) as *const __m128i);
        let vb = _mm_loadu_si128(b.as_ptr().add(i) as *const __m128i);
        let cmp = _mm_cmpeq_epi8(va, vb);
        if _mm_movemask_epi8(cmp) != 0xFFFF {
            return false;
        }
        i += 16;
    }
    a.get_unchecked(i..) == b.get_unchecked(i..)
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse2")]
unsafe fn xor_sse2(a: &[u8], b: &[u8], out: &mut [u8], n: usize) {
    use std::arch::x86_64::*;
    let mut i = 0;
    while i + 16 <= n {
        let va = _mm_loadu_si128(a.as_ptr().add(i) as *const __m128i);
        let vb = _mm_loadu_si128(b.as_ptr().add(i) as *const __m128i);
        _mm_storeu_si128(
            out.as_mut_ptr().add(i) as *mut __m128i,
            _mm_xor_si128(va, vb),
        );
        i += 16;
    }
    while i < n {
        *out.get_unchecked_mut(i) = *a.get_unchecked(i) ^ *b.get_unchecked(i);
        i += 1;
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn xor_avx2(a: &[u8], b: &[u8], out: &mut [u8], n: usize) {
    use std::arch::x86_64::*;
    let mut i = 0;
    while i + 32 <= n {
        let va = _mm256_loadu_si256(a.as_ptr().add(i) as *const __m256i);
        let vb = _mm256_loadu_si256(b.as_ptr().add(i) as *const __m256i);
        _mm256_storeu_si256(
            out.as_mut_ptr().add(i) as *mut __m256i,
            _mm256_xor_si256(va, vb),
        );
        i += 32;
    }
    while i < n {
        *out.get_unchecked_mut(i) = *a.get_unchecked(i) ^ *b.get_unchecked(i);
        i += 1;
    }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx512f", enable = "avx512bw")]
unsafe fn eq_avx512(a: &[u8], b: &[u8]) -> bool {
    use std::arch::x86_64::*;
    let n = a.len();
    let mut i = 0;
    while i + 64 <= n {
        let va = _mm512_loadu_si512(a.as_ptr().add(i) as *const __m512i);
        let vb = _mm512_loadu_si512(b.as_ptr().add(i) as *const __m512i);
        if _mm512_cmpeq_epi8_mask(va, vb) != !0u64 {
            return false;
        }
        i += 64;
    }
    a.get_unchecked(i..) == b.get_unchecked(i..)
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx512f")]
unsafe fn xor_avx512(a: &[u8], b: &[u8], out: &mut [u8], n: usize) {
    use std::arch::x86_64::*;
    let mut i = 0;
    while i + 64 <= n {
        let va = _mm512_loadu_si512(a.as_ptr().add(i) as *const __m512i);
        let vb = _mm512_loadu_si512(b.as_ptr().add(i) as *const __m512i);
        _mm512_storeu_si512(
            out.as_mut_ptr().add(i) as *mut __m512i,
            _mm512_xor_si512(va, vb),
        );
        i += 64;
    }
    while i < n {
        *out.get_unchecked_mut(i) = *a.get_unchecked(i) ^ *b.get_unchecked(i);
        i += 1;
    }
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn eq_neon(a: &[u8], b: &[u8]) -> bool {
    use std::arch::aarch64::*;
    let n = a.len();
    let mut i = 0;
    while i + 16 <= n {
        let va = vld1q_u8(a.as_ptr().add(i));
        let vb = vld1q_u8(b.as_ptr().add(i));
        let cmp = vceqq_u8(va, vb);
        if vminvq_u8(cmp) != 0xFF {
            return false;
        }
        i += 16;
    }
    a.get_unchecked(i..) == b.get_unchecked(i..)
}

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn xor_neon(a: &[u8], b: &[u8], out: &mut [u8], n: usize) {
    use std::arch::aarch64::*;
    let mut i = 0;
    while i + 16 <= n {
        let va = vld1q_u8(a.as_ptr().add(i));
        let vb = vld1q_u8(b.as_ptr().add(i));
        vst1q_u8(out.as_mut_ptr().add(i), veorq_u8(va, vb));
        i += 16;
    }
    while i < n {
        *out.get_unchecked_mut(i) = *a.get_unchecked(i) ^ *b.get_unchecked(i);
        i += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eq_matches_slice() {
        let a = vec![0u8; 64];
        let mut b = a.clone();
        assert!(eq(&a, &b));
        b[33] = 1;
        assert!(!eq(&a, &b));
        assert!(!eq(&a, &a[..63]));
    }

    #[test]
    fn xor_round_trip() {
        let a: Vec<u8> = (0..48).collect();
        let b: Vec<u8> = (0..48).map(|i| i ^ 0xA5).collect();
        let mut out = vec![0u8; 48];
        xor(&a, &b, &mut out);
        for i in 0..48 {
            assert_eq!(out[i], a[i] ^ b[i]);
        }
    }
}
