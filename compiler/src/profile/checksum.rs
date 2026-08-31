//! Stable shape fingerprints for CFG matching between instrument and use-profile.

use crate::il::{IlJumpKind, IlOp};

use super::instrument::InstrumentMap;

/// Mix bits into a portable checksum (independent of Rust's hasher).
fn mix(acc: u64, x: u64) -> u64 {
    acc.wrapping_mul(0x9e3779b97f4a7c15).wrapping_add(x)
}

fn op_kind_tag(op: &IlOp) -> u64 {
    match op {
        IlOp::Label(_) => 1,
        IlOp::Jump {
            kind: IlJumpKind::JumpIfFalse,
            ..
        } => 2,
        IlOp::Jump {
            kind: IlJumpKind::JumpIfTrue,
            ..
        } => 3,
        IlOp::Jump {
            kind: IlJumpKind::Unconditional,
            ..
        } => 4,
        IlOp::Return { .. } => 5,
        IlOp::Halt { .. } => 6,
        IlOp::Const { .. } => 7,
        IlOp::Load { .. } => 8,
        IlOp::StorePop { .. } => 9,
        IlOp::Bin { .. } => 10,
        IlOp::HostInvoke { .. } => 11,
        _ => 12,
    }
}

/// Fingerprint of `name` + ordered block leaders and branch kinds (cleanup mid-IR).
pub fn fn_shape_checksum(ops: &[IlOp], map: &InstrumentMap, name: &str) -> u64 {
    let mut h = 0u64;
    for b in name.as_bytes() {
        h = mix(h, *b as u64);
    }
    h = mix(h, map.blocks.len() as u64);
    for &leader in &map.blocks {
        h = mix(h, leader as u64);
        if let Some(op) = ops.get(leader) {
            h = mix(h, op_kind_tag(op));
        }
    }
    h = mix(h, map.branches.len() as u64);
    for &bi in &map.branches {
        h = mix(h, bi as u64);
        if let Some(op) = ops.get(bi) {
            h = mix(h, op_kind_tag(op));
        }
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::il::Label;
    use common::DebugLoc;

    fn loc() -> DebugLoc {
        DebugLoc::unknown()
    }

    #[test]
    fn checksum_stable_for_same_shape() {
        let ops = vec![
            IlOp::Const { imm: 1, loc: loc() },
            IlOp::Jump {
                kind: IlJumpKind::JumpIfFalse,
                target: Label(1),
                loc: loc(),
                hint: Default::default(),
            },
            IlOp::Return { loc: loc() },
            IlOp::Label(Label(1)),
            IlOp::Return { loc: loc() },
        ];
        let map = super::super::instrument::instrument_for_pgo_named(&ops, "f");
        let a = fn_shape_checksum(&ops, &map, "f");
        let b = fn_shape_checksum(&ops, &map, "f");
        assert_eq!(a, b);
        assert_ne!(fn_shape_checksum(&ops, &map, "g"), a);
    }
}
