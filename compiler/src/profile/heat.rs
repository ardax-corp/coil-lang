//! Block heat lookups using packed `SITE_STRIDE` profile keys.

use crate::il::IlOp;

use super::data::ProfileData;
use super::instrument::{InstrumentMap, SITE_STRIDE};

/// Heat for the basic block that contains `op_index` (0 if unknown / no profile).
pub fn block_heat(ops: &[IlOp], profile: &ProfileData, op_index: usize) -> u64 {
    if super::fn_profile_ignored() {
        return 0;
    }
    let map = current_or_build_map(ops);
    block_heat_from_map(&map, profile, op_index, super::current_fn_index())
}

/// Same as [`block_heat`] against [`super::current_profile`].
pub fn block_heat_current(ops: &[IlOp], op_index: usize) -> u64 {
    if super::fn_profile_ignored() {
        return 0;
    }
    match super::current_profile() {
        Some(p) => block_heat(ops, &p, op_index),
        None => 0,
    }
}

pub fn block_heat_from_map(
    map: &InstrumentMap,
    profile: &ProfileData,
    op_index: usize,
    fn_index: u32,
) -> u64 {
    let Some(local) = block_local_for_op(map, op_index) else {
        return 0;
    };
    if let Some(bfi) = super::cached_bfi() {
        return bfi.heat_local(local);
    }
    let key = fn_index.saturating_mul(SITE_STRIDE).saturating_add(local);
    profile.block_counts.get(&key).copied().unwrap_or(0)
}

/// Local block site id whose leader is the greatest leader ≤ `op_index`.
fn block_local_for_op(map: &InstrumentMap, op_index: usize) -> Option<u32> {
    let mut best: Option<(usize, u32)> = None;
    for (local, &leader) in map.blocks.iter().enumerate() {
        if leader <= op_index {
            best = Some((leader, local as u32));
        }
    }
    best.map(|(_, local)| local)
}

fn current_or_build_map(ops: &[IlOp]) -> InstrumentMap {
    if let Some(map) = super::cached_instrument_map() {
        return map;
    }
    let name = super::pgo_function_names()
        .last()
        .cloned()
        .unwrap_or_else(|| "<module>".into());
    let map = instrument_map_for(ops, &name);
    super::set_cached_instrument_map(Some(map.clone()));
    map
}

/// Site assignment without recording [`super::LAST_MAP`].
pub(crate) fn instrument_map_for(ops: &[IlOp], fn_name: &str) -> InstrumentMap {
    use crate::il::IlJumpKind;
    let mut map = InstrumentMap::default();
    if ops.is_empty() {
        return map;
    }
    map.functions.push((fn_name.into(), 0));
    map.blocks = super::instrument::block_leaders_pub(ops);
    for (i, op) in ops.iter().enumerate() {
        if let IlOp::Jump {
            kind: IlJumpKind::JumpIfFalse | IlJumpKind::JumpIfTrue,
            ..
        } = op
        {
            map.branches.push(i);
        }
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::il::{IlJumpKind, IlOp, Label};
    use common::DebugLoc;

    fn loc() -> DebugLoc {
        DebugLoc::unknown()
    }

    #[test]
    fn block_heat_uses_stride_local_not_raw_index() {
        let ops = vec![
            IlOp::Const { imm: 1, loc: loc() },
            IlOp::Jump {
                kind: IlJumpKind::JumpIfFalse,
                target: Label(1),
                loc: loc(),
                hint: Default::default(),
            },
            IlOp::Const { imm: 0, loc: loc() },
            IlOp::Return { loc: loc(), ret_words: 1},
            IlOp::Label(Label(1)),
            IlOp::Const { imm: 2, loc: loc() },
            IlOp::Return { loc: loc(), ret_words: 1},
        ];
        let map = instrument_map_for(&ops, "f");
        assert!(map.blocks.len() >= 2);
        let join = map.blocks[1];
        let mut profile = ProfileData::new();
        // Wrong: raw op index would look like heat if we used join as key.
        profile.block_counts.insert(join as u32, 99);
        // Right: local site 1 under fn 0.
        profile.block_counts.insert(1, 42);
        assert_eq!(block_heat_from_map(&map, &profile, join, 0), 42);
        assert_eq!(block_heat_from_map(&map, &profile, join + 1, 0), 42);
    }
}
