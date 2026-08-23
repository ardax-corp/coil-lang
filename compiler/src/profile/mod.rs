//! Profile-guided optimization infrastructure (COI-132).

mod data;
mod instrument;
mod opt;

pub use data::{LoadError, ProfileData, PROFILE_VERSION};
pub use instrument::{instrument_for_pgo, instrument_for_pgo_mut, InstrumentMap};
pub use opt::{branch_profile, optimize_with_profile};

use std::cell::RefCell;

thread_local! {
    static CURRENT: RefCell<Option<ProfileData>> = const { RefCell::new(None) };
    static LAST_MAP: RefCell<Option<InstrumentMap>> = const { RefCell::new(None) };
}

/// Install a profile for this compile (or `None` to clear).
pub fn set_current_profile(profile: Option<ProfileData>) {
    CURRENT.with(|c| *c.borrow_mut() = profile);
}

pub fn current_profile() -> Option<ProfileData> {
    CURRENT.with(|c| c.borrow().clone())
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

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
