//! Seeded test order: shuffle files, then the cases inside each file.
//!
//! One seed reproduces the whole order (`--seed`). Each file's case order is
//! derived from the seed and the file's path under the test root, so a file
//! keeps its case order when run alone with the same seed.

use std::path::Path;

/// How to order files and cases.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Order {
    /// Sorted paths, cases in source order (`--no-shuffle`).
    Sorted,
    /// Shuffled by this seed.
    Shuffled(u64),
}

/// splitmix64: small, fast, and good enough to permute test lists.
pub struct SplitMix64(u64);

impl SplitMix64 {
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in `0..=bound` (Lemire-style rejection keeps it unbiased).
    fn below_or_eq(&mut self, bound: u64) -> u64 {
        if bound == u64::MAX {
            return self.next_u64();
        }
        let range = bound + 1;
        let zone = u64::MAX - (u64::MAX % range);
        loop {
            let v = self.next_u64();
            if v < zone {
                return v % range;
            }
        }
    }

    /// Fisher–Yates in place.
    pub fn shuffle<T>(&mut self, items: &mut [T]) {
        for i in (1..items.len()).rev() {
            let j = self.below_or_eq(i as u64) as usize;
            items.swap(i, j);
        }
    }
}

/// FNV-1a over the path relative to `root` (full path when outside it).
fn path_hash(root: &Path, path: &Path) -> u64 {
    let rel = path.strip_prefix(root).unwrap_or(path);
    let mut h: u64 = 0xCBF2_9CE4_8422_2325;
    for b in rel.to_string_lossy().bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01B3);
    }
    h
}

impl Order {
    /// Reorder the discovered files (already sorted by path).
    pub fn order_files<T>(self, files: &mut [T]) {
        if let Order::Shuffled(seed) = self {
            SplitMix64::new(seed).shuffle(files);
        }
    }

    /// Reorder one file's cases (source order on input).
    pub fn order_cases<T>(self, root: &Path, file: &Path, cases: &mut [T]) {
        if let Order::Shuffled(seed) = self {
            SplitMix64::new(seed ^ path_hash(root, file)).shuffle(cases);
        }
    }

    /// Header fragment, e.g. `seed 0x0000002a` or `sorted order`.
    pub fn describe(self) -> String {
        match self {
            Order::Sorted => "sorted order".to_string(),
            Order::Shuffled(seed) => format!("seed {}", format_seed(seed)),
        }
    }
}

/// `0x`-prefixed hex, the form `--seed` prints and accepts.
pub fn format_seed(seed: u64) -> String {
    format!("{seed:#x}")
}

/// Parse a `--seed` / `COIL_TEST_SEED` value: decimal or `0x` hex.
pub fn parse_seed(text: &str) -> Result<u64, String> {
    let t = text.trim();
    let parsed = match t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        Some(hex) => u64::from_str_radix(&hex.replace('_', ""), 16),
        None => t.replace('_', "").parse::<u64>(),
    };
    parsed.map_err(|_| format!("invalid seed `{text}` (expected a u64, decimal or 0x hex)"))
}

/// A fresh seed when none was given: wall-clock nanos mixed with the pid.
pub fn fresh_seed() -> u64 {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    SplitMix64::new(nanos ^ (u64::from(std::process::id()) << 32)).next_u64()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn splitmix_matches_reference_vector() {
        // Reference values for seed 1234567 (splitmix64.c, Vigna).
        let mut r = SplitMix64::new(1_234_567);
        assert_eq!(r.next_u64(), 6_457_827_717_110_365_317);
        assert_eq!(r.next_u64(), 3_203_168_211_198_807_973);
    }

    #[test]
    fn same_seed_same_order_and_every_item_kept() {
        let base: Vec<u32> = (0..50).collect();
        let (mut a, mut b) = (base.clone(), base.clone());
        Order::Shuffled(42).order_files(&mut a);
        Order::Shuffled(42).order_files(&mut b);
        assert_eq!(a, b);
        assert_ne!(a, base, "50 items should not stay sorted");
        let mut sorted = a.clone();
        sorted.sort();
        assert_eq!(sorted, base);

        let mut c = base.clone();
        Order::Shuffled(43).order_files(&mut c);
        assert_ne!(a, c, "different seeds should differ");

        let mut d = base.clone();
        Order::Sorted.order_files(&mut d);
        assert_eq!(d, base);
    }

    #[test]
    fn case_order_depends_on_path_under_root_not_the_root() {
        let cases: Vec<u32> = (0..20).collect();
        let mut a = cases.clone();
        let mut b = cases.clone();
        Order::Shuffled(7).order_cases(
            Path::new("tests"),
            Path::new("tests/positive/x.hy"),
            &mut a,
        );
        Order::Shuffled(7).order_cases(
            &PathBuf::from("/abs/tests"),
            &PathBuf::from("/abs/tests/positive/x.hy"),
            &mut b,
        );
        assert_eq!(a, b);
        let mut c = cases.clone();
        Order::Shuffled(7).order_cases(
            Path::new("tests"),
            Path::new("tests/positive/y.hy"),
            &mut c,
        );
        assert_ne!(a, c);
    }

    #[test]
    fn seeds_parse_decimal_and_hex_and_round_trip() {
        assert_eq!(parse_seed("42"), Ok(42));
        assert_eq!(parse_seed("0x2a"), Ok(42));
        assert_eq!(parse_seed("0X2A"), Ok(42));
        assert_eq!(parse_seed("1_000"), Ok(1000));
        assert!(parse_seed("-1").is_err());
        assert!(parse_seed("0xzz").is_err());
        assert!(parse_seed("").is_err());
        let s = 0xDEAD_BEEF_u64;
        assert_eq!(parse_seed(&format_seed(s)), Ok(s));
    }
}
