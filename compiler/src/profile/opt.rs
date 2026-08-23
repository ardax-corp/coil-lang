//! Apply a loaded [`super::ProfileData`] to IL layout and inlining.

use crate::il::IlOp;
use crate::il::opt::{optimize_branches, reorder_basic_blocks, BranchProfile};

use super::data::ProfileData;
use super::instrument::InstrumentMap;

/// Convert profile branch keys (instrument ids) into op-index counts.
pub fn branch_profile(ops: &[IlOp], profile: &ProfileData) -> BranchProfile {
    if super::fn_profile_ignored() {
        return BranchProfile::default();
    }
    let map = if let Some(map) = super::cached_instrument_map() {
        map
    } else {
        let name = super::pgo_function_names()
            .last()
            .cloned()
            .unwrap_or_else(|| "<module>".into());
        let map = super::heat::instrument_map_for(ops, &name);
        super::set_cached_instrument_map(Some(map.clone()));
        map
    };
    branch_profile_from_map(&map, profile)
}

pub fn branch_profile_from_map(map: &InstrumentMap, profile: &ProfileData) -> BranchProfile {
    let mut bp = BranchProfile::default();
    if super::fn_profile_ignored() {
        return bp;
    }
    for (id, &idx) in map.branches.iter().enumerate() {
        let key = super::current_fn_index().saturating_mul(super::SITE_STRIDE) + id as u32;
        if let Some(&(taken, not_taken)) = profile.branch_counts.get(&key) {
            bp.taken.insert(idx, taken.min(u32::MAX as u64) as u32);
            bp.not_taken
                .insert(idx, not_taken.min(u32::MAX as u64) as u32);
        }
    }
    bp
}

/// Layout using profile-guided branch polarity and block order.
pub fn optimize_with_profile(ops: &mut Vec<IlOp>, profile: &ProfileData) {
    let bp = branch_profile(ops, profile);
    optimize_branches(ops, Some(&bp));
    let _ = reorder_basic_blocks(ops, Some(&bp));
}
