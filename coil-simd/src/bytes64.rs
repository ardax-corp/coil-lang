//! 64 byte lanes in one 64-byte block: the compiler's byte-lane `V*` ops
//! (`docs/internals/byte-simd.md`).
//!
//! A lane mask holds `0xFF` (true) or `0x00` (false) per lane. Compares are
//! unsigned. Each op runs as four 16-byte steps on the target's baseline
//! (SSE2 on x86_64, NEON on aarch64), so there is no runtime dispatch; other
//! targets use the scalar loops, which are also the reference in tests.

/// Byte lanes per block.
pub const LANES: usize = 64;

/// One block of 64 byte lanes.
pub type Block = [u8; LANES];

/// Every lane `b`.
#[inline]
pub fn splat(b: u8) -> Block {
    [b; LANES]
}

/// `a == b` per lane.
#[inline]
pub fn eq(a: &Block, b: &Block) -> Block {
    #[cfg(target_arch = "x86_64")]
    {
        unsafe { sse2::eq(a, b) }
    }
    #[cfg(target_arch = "aarch64")]
    {
        unsafe { neon::eq(a, b) }
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        scalar::eq(a, b)
    }
}

/// `a < b` per lane, unsigned.
#[inline]
pub fn lt(a: &Block, b: &Block) -> Block {
    #[cfg(target_arch = "x86_64")]
    {
        unsafe { sse2::lt(a, b) }
    }
    #[cfg(target_arch = "aarch64")]
    {
        unsafe { neon::lt(a, b) }
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        scalar::lt(a, b)
    }
}

/// `a <= b` per lane, unsigned.
#[inline]
pub fn le(a: &Block, b: &Block) -> Block {
    not(&lt(b, a))
}

/// `a & b` per lane.
#[inline]
pub fn and(a: &Block, b: &Block) -> Block {
    zip(a, b, |x, y| x & y)
}

/// `a | b` per lane.
#[inline]
pub fn or(a: &Block, b: &Block) -> Block {
    zip(a, b, |x, y| x | y)
}

/// `a & !b` per lane.
#[inline]
pub fn andnot(a: &Block, b: &Block) -> Block {
    zip(a, b, |x, y| x & !y)
}

/// `!a` per lane.
#[inline]
pub fn not(a: &Block) -> Block {
    let mut o = [0u8; LANES];
    for (o, x) in o.iter_mut().zip(a) {
        *o = !x;
    }
    o
}

/// Bit `k` is the top bit of lane `k`.
#[inline]
pub fn movemask(a: &Block) -> u64 {
    #[cfg(target_arch = "x86_64")]
    {
        unsafe { sse2::movemask(a) }
    }
    #[cfg(target_arch = "aarch64")]
    {
        unsafe { neon::movemask(a) }
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        scalar::movemask(a)
    }
}

// Plain lane-wise loops: LLVM vectorises these on the baseline ISA.
#[inline]
fn zip(a: &Block, b: &Block, f: impl Fn(u8, u8) -> u8) -> Block {
    let mut o = [0u8; LANES];
    for i in 0..LANES {
        o[i] = f(a[i], b[i]);
    }
    o
}

/// Scalar reference implementations.
pub mod scalar {
    use super::{Block, LANES};

    pub fn eq(a: &Block, b: &Block) -> Block {
        let mut o = [0u8; LANES];
        for i in 0..LANES {
            o[i] = if a[i] == b[i] { 0xFF } else { 0 };
        }
        o
    }

    pub fn lt(a: &Block, b: &Block) -> Block {
        let mut o = [0u8; LANES];
        for i in 0..LANES {
            o[i] = if a[i] < b[i] { 0xFF } else { 0 };
        }
        o
    }

    pub fn movemask(a: &Block) -> u64 {
        let mut m = 0u64;
        for (i, &x) in a.iter().enumerate() {
            m |= u64::from(x >> 7) << i;
        }
        m
    }
}

#[cfg(target_arch = "x86_64")]
mod sse2 {
    use super::{Block, LANES};
    use std::arch::x86_64::*;

    #[inline]
    #[target_feature(enable = "sse2")]
    pub unsafe fn eq(a: &Block, b: &Block) -> Block {
        let mut o = [0u8; LANES];
        for k in (0..LANES).step_by(16) {
            let x = _mm_loadu_si128(a.as_ptr().add(k) as *const __m128i);
            let y = _mm_loadu_si128(b.as_ptr().add(k) as *const __m128i);
            _mm_storeu_si128(o.as_mut_ptr().add(k) as *mut __m128i, _mm_cmpeq_epi8(x, y));
        }
        o
    }

    /// Unsigned `a < b`: `a != max(a, b)`.
    #[inline]
    #[target_feature(enable = "sse2")]
    pub unsafe fn lt(a: &Block, b: &Block) -> Block {
        let mut o = [0u8; LANES];
        let ones = _mm_set1_epi8(-1);
        for k in (0..LANES).step_by(16) {
            let x = _mm_loadu_si128(a.as_ptr().add(k) as *const __m128i);
            let y = _mm_loadu_si128(b.as_ptr().add(k) as *const __m128i);
            let ge = _mm_cmpeq_epi8(_mm_max_epu8(x, y), x);
            _mm_storeu_si128(o.as_mut_ptr().add(k) as *mut __m128i, _mm_xor_si128(ge, ones));
        }
        o
    }

    #[inline]
    #[target_feature(enable = "sse2")]
    pub unsafe fn movemask(a: &Block) -> u64 {
        let mut m = 0u64;
        for k in (0..LANES).step_by(16) {
            let x = _mm_loadu_si128(a.as_ptr().add(k) as *const __m128i);
            m |= u64::from(_mm_movemask_epi8(x) as u16) << k;
        }
        m
    }
}

#[cfg(target_arch = "aarch64")]
mod neon {
    use super::{Block, LANES};
    use std::arch::aarch64::*;

    #[inline]
    #[target_feature(enable = "neon")]
    pub unsafe fn eq(a: &Block, b: &Block) -> Block {
        let mut o = [0u8; LANES];
        for k in (0..LANES).step_by(16) {
            let x = vld1q_u8(a.as_ptr().add(k));
            let y = vld1q_u8(b.as_ptr().add(k));
            vst1q_u8(o.as_mut_ptr().add(k), vceqq_u8(x, y));
        }
        o
    }

    #[inline]
    #[target_feature(enable = "neon")]
    pub unsafe fn lt(a: &Block, b: &Block) -> Block {
        let mut o = [0u8; LANES];
        for k in (0..LANES).step_by(16) {
            let x = vld1q_u8(a.as_ptr().add(k));
            let y = vld1q_u8(b.as_ptr().add(k));
            vst1q_u8(o.as_mut_ptr().add(k), vcltq_u8(x, y));
        }
        o
    }

    /// Top bits weighted `1, 2, 4, ..., 128` per half, then pairwise sums.
    #[inline]
    #[target_feature(enable = "neon")]
    pub unsafe fn movemask(a: &Block) -> u64 {
        const W: [u8; 16] = [1, 2, 4, 8, 16, 32, 64, 128, 1, 2, 4, 8, 16, 32, 64, 128];
        let w = vld1q_u8(W.as_ptr());
        let mut m = 0u64;
        for k in (0..LANES).step_by(16) {
            let x = vld1q_u8(a.as_ptr().add(k));
            let bits = vandq_u8(vshrq_n_u8(x, 7), vdupq_n_u8(1));
            let weighted = vmulq_u8(bits, w);
            let lo = vaddv_u8(vget_low_u8(weighted));
            let hi = vaddv_u8(vget_high_u8(weighted));
            m |= (u64::from(lo) | (u64::from(hi) << 8)) << k;
        }
        m
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(seed: u8) -> Block {
        let mut b = [0u8; LANES];
        let mut x = seed;
        for v in &mut b {
            x = x.wrapping_mul(31).wrapping_add(17);
            *v = x;
        }
        b[3] = 0;
        b[9] = 255;
        b[40] = 128;
        b
    }

    #[test]
    fn ops_match_scalar() {
        for s in 0..20u8 {
            let a = sample(s);
            let mut b = sample(s.wrapping_add(7));
            b[5] = a[5];
            b[63] = a[63];
            assert_eq!(eq(&a, &b), scalar::eq(&a, &b));
            assert_eq!(lt(&a, &b), scalar::lt(&a, &b));
            assert_eq!(le(&a, &b), not(&scalar::lt(&b, &a)));
            assert_eq!(movemask(&a), scalar::movemask(&a));
        }
    }

    #[test]
    fn movemask_bit_k_is_lane_k() {
        for k in 0..LANES {
            let mut a = [0u8; LANES];
            a[k] = 0xFF;
            assert_eq!(movemask(&a), 1u64 << k);
        }
        assert_eq!(movemask(&eq(&splat(7), &splat(7))), u64::MAX);
    }
}
