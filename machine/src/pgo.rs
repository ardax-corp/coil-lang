//! Runtime counters for `--pgo-instrument` HostInvoke hits.

use std::collections::BTreeMap;
use std::sync::Mutex;

/// Matches compiler `profile::SITE_STRIDE` packing.
pub const SITE_STRIDE: u32 = 1_000_000;

const KIND_FN: i64 = 0;
const KIND_BLOCK: i64 = 1;
const KIND_BR_TAKEN: i64 = 2;
const KIND_BR_NOT: i64 = 3;

#[derive(Default)]
struct Counts {
    functions: BTreeMap<u32, u64>,
    blocks: BTreeMap<u32, u64>,
    branches: BTreeMap<u32, (u64, u64)>,
}

static COUNTS: Mutex<Counts> = Mutex::new(Counts {
    functions: BTreeMap::new(),
    blocks: BTreeMap::new(),
    branches: BTreeMap::new(),
});

/// Snapshot of packed-key counters (function keys use `fn_index * SITE_STRIDE`).
#[derive(Clone, Debug, Default)]
pub struct PgoSnapshot {
    pub function_keys: BTreeMap<u32, u64>,
    pub block_counts: BTreeMap<u32, u64>,
    pub branch_counts: BTreeMap<u32, (u64, u64)>,
}

pub fn reset() {
    *COUNTS.lock().unwrap_or_else(|e| e.into_inner()) = Counts::default();
}

/// Apply one `pgo_hit` packed operand: `kind | (site_key << 2)`.
pub fn hit(packed: i64) {
    let kind = packed & 3;
    let key = (packed >> 2) as u32;
    let mut g = COUNTS.lock().unwrap_or_else(|e| e.into_inner());
    match kind {
        KIND_FN => *g.functions.entry(key).or_insert(0) += 1,
        KIND_BLOCK => *g.blocks.entry(key).or_insert(0) += 1,
        KIND_BR_TAKEN => g.branches.entry(key).or_insert((0, 0)).0 += 1,
        KIND_BR_NOT => g.branches.entry(key).or_insert((0, 0)).1 += 1,
        _ => {}
    }
}

pub fn snapshot() -> PgoSnapshot {
    let g = COUNTS.lock().unwrap_or_else(|e| e.into_inner());
    PgoSnapshot {
        function_keys: g.functions.clone(),
        block_counts: g.blocks.clone(),
        branch_counts: g.branches.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hit_accumulates_kinds() {
        reset();
        hit(KIND_FN | ((7u32 as i64) << 2));
        hit(KIND_BLOCK | ((7u32 as i64) << 2));
        hit(KIND_BR_TAKEN | ((3u32 as i64) << 2));
        hit(KIND_BR_NOT | ((3u32 as i64) << 2));
        hit(KIND_BR_TAKEN | ((3u32 as i64) << 2));
        let s = snapshot();
        assert_eq!(s.function_keys.get(&7), Some(&1));
        assert_eq!(s.block_counts.get(&7), Some(&1));
        assert_eq!(s.branch_counts.get(&3), Some(&(2, 1)));
        reset();
    }
}
