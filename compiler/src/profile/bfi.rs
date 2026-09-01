//! Block-frequency estimates from entry / edge counts (BFI-lite).

use crate::il::{IlJumpKind, IlOp, Label};

use super::data::ProfileData;
use super::instrument::{InstrumentMap, SITE_STRIDE};

/// Per-function block frequencies keyed by local site id.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BlockFrequency {
    pub local: Vec<u64>,
}

impl BlockFrequency {
    pub fn heat_local(&self, local: u32) -> u64 {
        self.local.get(local as usize).copied().unwrap_or(0)
    }

    /// Layout uses [`crate::profile::block_heat_current`]; these predicates are test API.
    #[cfg(test)]
    pub fn max_heat(&self) -> u64 {
        self.local.iter().copied().max().unwrap_or(0)
    }

    /// Hot when at least half the hottest block and ≥ 8 hits (same spirit as fn hot).
    #[cfg(test)]
    pub fn is_hot_local(&self, local: u32) -> bool {
        let h = self.heat_local(local);
        let max = self.max_heat();
        h > 0 && h * 2 >= max && h >= 8
    }
}

fn site_key(fn_index: u32, local: u32) -> u32 {
    fn_index.saturating_mul(SITE_STRIDE).saturating_add(local)
}

fn block_local_at(map: &InstrumentMap, op_index: usize) -> Option<u32> {
    let mut best: Option<(usize, u32)> = None;
    for (local, &leader) in map.blocks.iter().enumerate() {
        if leader <= op_index {
            best = Some((leader, local as u32));
        }
    }
    best.map(|(_, local)| local)
}

/// Combine measured block hits with branch-edge weights into local frequencies.
pub fn compute_block_frequency(
    ops: &[IlOp],
    map: &InstrumentMap,
    profile: &ProfileData,
    fn_index: u32,
    fn_name: &str,
) -> BlockFrequency {
    let n = map.blocks.len();
    let mut local = vec![0u64; n];
    for i in 0..n {
        let key = site_key(fn_index, i as u32);
        local[i] = profile.block_counts.get(&key).copied().unwrap_or(0);
    }

    let fn_hits = profile.function_counts.get(fn_name).copied().unwrap_or(0);
    if n > 0 && local[0] == 0 && fn_hits > 0 {
        local[0] = fn_hits;
    }

    for (bi, &jmp_i) in map.branches.iter().enumerate() {
        let key = site_key(fn_index, bi as u32);
        let (taken, not_taken) = profile.branch_counts.get(&key).copied().unwrap_or((0, 0));
        if jmp_i + 1 < ops.len() {
            if let Some(ft) = block_local_at(map, jmp_i + 1) {
                let i = ft as usize;
                if i < local.len() {
                    local[i] = local[i].max(not_taken);
                }
            }
        }
        if let IlOp::Jump {
            kind: IlJumpKind::JumpIfFalse | IlJumpKind::JumpIfTrue,
            target: Label(id),
            ..
        } = ops[jmp_i]
        {
            if let Some(ti) = ops
                .iter()
                .position(|op| matches!(op, IlOp::Label(Label(l)) if *l == id))
            {
                if let Some(tk) = block_local_at(map, ti) {
                    let i = tk as usize;
                    if i < local.len() {
                        local[i] = local[i].max(taken);
                    }
                }
            }
        }
    }

    // One propagation pass: non-branch fall-through inherits predecessor heat.
    let mut next = local.clone();
    for (i, &leader) in map.blocks.iter().enumerate() {
        if i + 1 >= n {
            break;
        }
        let end = map.blocks[i + 1];
        let region = &ops[leader..end];
        let ends_with_term = region.last().is_some_and(|op| {
            matches!(
                op,
                IlOp::Jump { .. } | IlOp::Return { .. } | IlOp::Halt { .. }
            )
        });
        if !ends_with_term && local[i] > 0 {
            next[i + 1] = next[i + 1].max(local[i]);
        }
    }

    BlockFrequency { local: next }
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::DebugLoc;

    fn loc() -> DebugLoc {
        DebugLoc::unknown()
    }

    #[test]
    fn bfi_prefers_edge_weights_on_successors() {
        let ops = vec![
            IlOp::Const { imm: 1, loc: loc() },
            IlOp::Jump {
                kind: IlJumpKind::JumpIfFalse,
                target: Label(1),
                loc: loc(),
                hint: Default::default(),
            },
            IlOp::Const { imm: 0, loc: loc() },
            IlOp::Return { loc: loc() },
            IlOp::Label(Label(1)),
            IlOp::Const { imm: 2, loc: loc() },
            IlOp::Return { loc: loc() },
        ];
        let map = super::super::instrument::instrument_for_pgo_named(&ops, "f");
        let mut profile = ProfileData::new();
        profile.function_counts.insert("f".into(), 10);
        profile.branch_counts.insert(0, (80, 20));
        let bfi = compute_block_frequency(&ops, &map, &profile, 0, "f");
        assert!(bfi.local.len() >= 2);
        assert_eq!(bfi.local[0], 10);
        // Taken → label block; not-taken → fall-through.
        let taken_local = block_local_at(&map, 4).unwrap();
        let ft_local = block_local_at(&map, 2).unwrap();
        assert_eq!(bfi.heat_local(taken_local), 80);
        assert_eq!(bfi.heat_local(ft_local), 20);
        assert!(bfi.is_hot_local(taken_local));
    }
}
