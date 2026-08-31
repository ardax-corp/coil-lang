//! Fast hasher for integer-keyed VM tables (heap addresses, enum tags).
//!
//! `Heap::live` is probed on every `Index` / `GetField` / `ArrayPush`, and
//! the default SipHash accounted for ~30% of retired instructions on the nsieve
//! benchmark. Keys are allocator-chosen addresses and compiler-assigned tags,
//! never attacker-controlled, so HashDoS resistance buys nothing here.

use std::hash::{BuildHasherDefault, Hasher};

/// `BuildHasher` for [`AddrHasher`]; use with `HashMap::default()`.
pub type AddrHashBuilder = BuildHasherDefault<AddrHasher>;

/// murmur3 `fmix64` finalizer.
///
/// Heap addresses are aligned, so their low bits are always zero. hashbrown
/// picks a bucket with the *low* bits of the hash, which rules out plain
/// multiplicative hashes (their low bits ignore the input's high bits) — this
/// avalanches in both directions for ~6 ALU ops.
#[inline(always)]
const fn fmix64(mut x: u64) -> u64 {
    x ^= x >> 33;
    x = x.wrapping_mul(0xff51_afd7_ed55_8ccd);
    x ^= x >> 33;
    x = x.wrapping_mul(0xc4ce_b9fe_1a85_ec53);
    x ^= x >> 33;
    x
}

/// Single-value hasher for `u32` / `u64` / `usize` keys.
#[derive(Default, Clone, Copy)]
pub struct AddrHasher(u64);

impl Hasher for AddrHasher {
    #[inline]
    fn finish(&self) -> u64 {
        self.0
    }

    #[inline]
    fn write_u64(&mut self, n: u64) {
        self.0 = fmix64(n);
    }

    #[inline]
    fn write_usize(&mut self, n: usize) {
        self.write_u64(n as u64);
    }

    #[inline]
    fn write_u32(&mut self, n: u32) {
        self.write_u64(n as u64);
    }

    /// Byte-slice keys are not the intended use; fold them so the hasher stays
    /// a correct (if unremarkable) `Hasher` for any `Hash` impl.
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = fmix64(self.0 ^ b as u64);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn hash_of(v: u64) -> u64 {
        let mut h = AddrHasher::default();
        h.write_u64(v);
        h.finish()
    }

    #[test]
    fn distinct_keys_hash_distinctly() {
        assert_ne!(hash_of(0), hash_of(1));
        assert_ne!(hash_of(16), hash_of(32));
    }

    #[test]
    fn aligned_addresses_spread_across_low_bits() {
        // 16-byte-aligned addresses must not collide in the bucket index bits;
        // a multiply-only hash would map them all to a few buckets.
        let buckets: std::collections::HashSet<u64> =
            (0..64u64).map(|i| hash_of(i * 16) & 0x3F).collect();
        assert!(
            buckets.len() >= 24,
            "aligned keys clustered into {} of 64 low-bit buckets",
            buckets.len()
        );
    }

    #[test]
    fn works_as_a_hashmap_hasher() {
        let mut m: HashMap<u64, u32, AddrHashBuilder> = HashMap::default();
        for i in 0..1000u64 {
            m.insert(i * 32, i as u32);
        }
        assert_eq!(m.len(), 1000);
        for i in 0..1000u64 {
            assert_eq!(m.get(&(i * 32)), Some(&(i as u32)));
        }
        assert_eq!(m.get(&7), None);
    }
}
