//! Assign counter ids to function entries, blocks, and branch edges.

use crate::il::{IlJumpKind, IlOp};

/// Sites that a profile file's integer keys refer to.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InstrumentMap {
    /// `(name, op_index)` for each function entry (first op when names are
    /// unknown: a single `"<module>"` site at 0).
    pub functions: Vec<(String, usize)>,
    /// Block leader op indices; the profile `block_counts` key is the index
    /// into this vec.
    pub blocks: Vec<usize>,
    /// Conditional-jump op indices; the profile `branch_counts` key is the
    /// index into this vec.
    pub branches: Vec<usize>,
}

/// Record counter sites. Does not insert runtime ops: counters are the map
/// entries themselves (id → IL index), so instrumentation has no hot-path
/// overhead until a profile is applied.
pub fn instrument_for_pgo(ops: &[IlOp]) -> InstrumentMap {
    let mut map = InstrumentMap::default();
    if ops.is_empty() {
        return map;
    }
    map.functions.push(("<module>".into(), 0));
    map.blocks = block_leaders(ops);
    for (i, op) in ops.iter().enumerate() {
        if let IlOp::Jump {
            kind: IlJumpKind::JumpIfFalse | IlJumpKind::JumpIfTrue,
            ..
        } = op
        {
            map.branches.push(i);
        }
    }
    super::record_instrument_map(map.clone());
    map
}

/// Same as [`instrument_for_pgo`], matching the ticket's `instrument_for_pgo(ops)`
/// `&mut` shape.
pub fn instrument_for_pgo_mut(ops: &mut [IlOp]) -> InstrumentMap {
    instrument_for_pgo(ops)
}

fn block_leaders(ops: &[IlOp]) -> Vec<usize> {
    let n = ops.len();
    let mut leaders = vec![false; n];
    leaders[0] = true;
    for (i, op) in ops.iter().enumerate() {
        if matches!(op, IlOp::Label(_)) {
            leaders[i] = true;
        }
        if ends_block(op) && i + 1 < n {
            leaders[i + 1] = true;
        }
    }
    (0..n).filter(|&i| leaders[i]).collect()
}

fn ends_block(op: &IlOp) -> bool {
    matches!(
        op,
        IlOp::Jump { .. } | IlOp::Return { .. } | IlOp::Halt { .. }
    )
}
