//! Profile-guided optimization infrastructure (COI-132).

mod bfi;
mod checksum;
mod data;
mod heat;
mod instrument;
mod opt;

pub use bfi::{compute_block_frequency, BlockFrequency};
pub use checksum::fn_shape_checksum;
pub use data::{LoadError, ProfileData, PROFILE_VERSION};
pub use heat::block_heat_current;
pub use instrument::{
    instrument_for_pgo, instrument_for_pgo_mut, instrument_for_pgo_named_mut, InstrumentMap,
    SITE_STRIDE,
};
pub use opt::{branch_profile, optimize_with_profile};

use std::cell::RefCell;
use std::collections::BTreeMap;

use crate::il::IlOp;

thread_local! {
    static CURRENT: RefCell<Option<ProfileData>> = const { RefCell::new(None) };
    static LAST_MAP: RefCell<Option<InstrumentMap>> = const { RefCell::new(None) };
    /// Per-function map reused by layout / unroll / LICM during one optimize.
    static CACHED_MAP: RefCell<Option<InstrumentMap>> = const { RefCell::new(None) };
    static CACHED_BFI: RefCell<Option<BlockFrequency>> = const { RefCell::new(None) };
    static FN_PROFILE_IGNORED: RefCell<bool> = const { RefCell::new(false) };
    static FN_CHECKSUMS: RefCell<BTreeMap<String, u64>> = const { RefCell::new(BTreeMap::new()) };
    static CHECKSUM_WARNED: RefCell<BTreeMap<String, bool>> = const { RefCell::new(BTreeMap::new()) };
    static INSTRUMENT: RefCell<bool> = const { RefCell::new(false) };
    static NATIVE_ID: RefCell<Option<i32>> = const { RefCell::new(None) };
    static FN_INDEX: RefCell<u32> = const { RefCell::new(0) };
    static FN_NAMES: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

/// Install a profile for this compile (or `None` to clear).
pub fn set_current_profile(profile: Option<ProfileData>) {
    CURRENT.with(|c| *c.borrow_mut() = profile);
}

pub fn current_profile() -> Option<ProfileData> {
    CURRENT.with(|c| c.borrow().clone())
}

pub fn set_pgo_instrument(on: bool) {
    INSTRUMENT.with(|c| *c.borrow_mut() = on);
}

pub fn pgo_instrumenting() -> bool {
    INSTRUMENT.with(|c| *c.borrow())
}

pub fn set_pgo_native_id(id: Option<i32>) {
    NATIVE_ID.with(|c| *c.borrow_mut() = id);
}

pub(crate) fn pgo_native_id() -> Option<i32> {
    NATIVE_ID.with(|c| *c.borrow())
}

pub fn begin_pgo_module() {
    FN_INDEX.with(|c| *c.borrow_mut() = 0);
    FN_NAMES.with(|c| c.borrow_mut().clear());
    FN_CHECKSUMS.with(|c| c.borrow_mut().clear());
    CHECKSUM_WARNED.with(|c| c.borrow_mut().clear());
    clear_function_profile_cache();
}

pub fn next_pgo_function(name: &str) {
    FN_NAMES.with(|c| c.borrow_mut().push(name.to_string()));
    let n = FN_NAMES.with(|c| c.borrow().len() as u32);
    FN_INDEX.with(|c| *c.borrow_mut() = n.saturating_sub(1));
    clear_function_profile_cache();
}

fn clear_function_profile_cache() {
    set_cached_instrument_map(None);
    set_cached_bfi(None);
    set_fn_profile_ignored(false);
}

pub(crate) fn set_cached_instrument_map(map: Option<InstrumentMap>) {
    CACHED_MAP.with(|c| *c.borrow_mut() = map);
}

pub(crate) fn cached_instrument_map() -> Option<InstrumentMap> {
    CACHED_MAP.with(|c| c.borrow().clone())
}

pub(crate) fn set_cached_bfi(bfi: Option<BlockFrequency>) {
    CACHED_BFI.with(|c| *c.borrow_mut() = bfi);
}

pub(crate) fn cached_bfi() -> Option<BlockFrequency> {
    CACHED_BFI.with(|c| c.borrow().clone())
}

pub(crate) fn set_fn_profile_ignored(ignored: bool) {
    FN_PROFILE_IGNORED.with(|c| *c.borrow_mut() = ignored);
}

pub(crate) fn fn_profile_ignored() -> bool {
    FN_PROFILE_IGNORED.with(|c| *c.borrow())
}

pub(crate) fn current_fn_index() -> u32 {
    FN_INDEX.with(|c| *c.borrow())
}

pub fn pgo_function_names() -> Vec<String> {
    FN_NAMES.with(|c| c.borrow().clone())
}

pub(crate) fn record_instrument_map(map: InstrumentMap) {
    LAST_MAP.with(|c| *c.borrow_mut() = Some(map));
}

pub fn last_instrument_map() -> Option<InstrumentMap> {
    LAST_MAP.with(|c| c.borrow().clone())
}

pub(crate) fn record_fn_checksum(name: &str, checksum: u64) {
    FN_CHECKSUMS.with(|c| {
        c.borrow_mut().insert(name.to_string(), checksum);
    });
}

pub fn collected_fn_checksums() -> BTreeMap<String, u64> {
    FN_CHECKSUMS.with(|c| c.borrow().clone())
}

/// Build / cache [`InstrumentMap`] + BFI for the current function; ignore on checksum mismatch.
pub fn prepare_function_profile(ops: &[IlOp]) {
    clear_function_profile_cache();
    let Some(profile) = current_profile() else {
        return;
    };
    let name = pgo_function_names()
        .last()
        .cloned()
        .unwrap_or_else(|| "<module>".into());
    let map = heat::instrument_map_for(ops, &name);
    if let Some(&expected) = profile.fn_checksums.get(&name) {
        let got = fn_shape_checksum(ops, &map, &name);
        if got != expected {
            warn_checksum_mismatch(&name);
            set_fn_profile_ignored(true);
            return;
        }
    }
    let bfi = compute_block_frequency(ops, &map, &profile, current_fn_index(), &name);
    set_cached_instrument_map(Some(map));
    set_cached_bfi(Some(bfi));
}

fn warn_checksum_mismatch(name: &str) {
    let already = CHECKSUM_WARNED.with(|c| c.borrow().get(name).copied().unwrap_or(false));
    if already {
        return;
    }
    CHECKSUM_WARNED.with(|c| {
        c.borrow_mut().insert(name.to_string(), true);
    });
    eprintln!(
        "warning: PGO profile checksum mismatch for `{name}`; ignoring that function's counts"
    );
}

/// True when `name` is hot in the current profile (unknown → not hot).
pub fn current_function_is_hot(name: &str) -> bool {
    CURRENT.with(|c| {
        c.borrow()
            .as_ref()
            .map(|p| p.function_is_hot(name))
            .unwrap_or(false)
    })
}

/// True when the profile has data and `name` never ran.
pub fn current_function_is_cold(name: &str) -> bool {
    CURRENT.with(|c| {
        c.borrow()
            .as_ref()
            .map(|p| p.function_is_cold(name))
            .unwrap_or(false)
    })
}

/// Fold VM `pgo_hit` counters into a [`ProfileData`] using this compile's fn names.
pub fn profile_from_runtime(
    function_keys: &BTreeMap<u32, u64>,
    block_counts: BTreeMap<u32, u64>,
    branch_counts: BTreeMap<u32, (u64, u64)>,
) -> ProfileData {
    let mut data = ProfileData::new();
    let names = pgo_function_names();
    for (i, name) in names.iter().enumerate() {
        let key = (i as u32).saturating_mul(SITE_STRIDE);
        if let Some(&n) = function_keys.get(&key) {
            data.function_counts.insert(name.clone(), n);
        }
    }
    data.block_counts = block_counts;
    data.branch_counts = branch_counts;
    data.fn_checksums = collected_fn_checksums();
    data
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
