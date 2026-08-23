//! Profile-guided optimization infrastructure (COI-132).

mod data;
mod instrument;
mod opt;

pub use data::{LoadError, ProfileData, PROFILE_VERSION};
pub use instrument::{
    instrument_for_pgo, instrument_for_pgo_mut, instrument_for_pgo_named_mut, InstrumentMap,
    SITE_STRIDE,
};
pub use opt::{branch_profile, optimize_with_profile};

use std::cell::RefCell;

thread_local! {
    static CURRENT: RefCell<Option<ProfileData>> = const { RefCell::new(None) };
    static LAST_MAP: RefCell<Option<InstrumentMap>> = const { RefCell::new(None) };
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
}

pub fn next_pgo_function(name: &str) {
    FN_NAMES.with(|c| c.borrow_mut().push(name.to_string()));
    let n = FN_NAMES.with(|c| c.borrow().len() as u32);
    FN_INDEX.with(|c| *c.borrow_mut() = n.saturating_sub(1));
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
    function_keys: &std::collections::BTreeMap<u32, u64>,
    block_counts: std::collections::BTreeMap<u32, u64>,
    branch_counts: std::collections::BTreeMap<u32, (u64, u64)>,
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
    data
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
