//! Assign counter ids and insert HostInvoke hits at function, block, and branch sites.

use crate::il::{IlJumpKind, IlOp, Label};
use common::DebugLoc;

/// Per-function site ids are `fn_index * SITE_STRIDE + local`.
pub const SITE_STRIDE: u32 = 1_000_000;

const KIND_FN: i32 = 0;
const KIND_BLOCK: i32 = 1;
const KIND_BR_TAKEN: i32 = 2;
const KIND_BR_NOT: i32 = 3;

/// Sites that a profile file's integer keys refer to.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InstrumentMap {
    /// `(name, op_index)` for each function entry.
    pub functions: Vec<(String, usize)>,
    /// Block leader op indices; the profile `block_counts` key is the local
    /// index plus [`SITE_STRIDE`] × function index.
    pub blocks: Vec<usize>,
    /// Conditional-jump op indices; the profile `branch_counts` key is the
    /// local index plus stride × function index.
    pub branches: Vec<usize>,
}

/// Record counter sites without inserting runtime ops.
pub fn instrument_for_pgo(ops: &[IlOp]) -> InstrumentMap {
    instrument_for_pgo_named(ops, "<module>")
}

pub fn instrument_for_pgo_named(ops: &[IlOp], fn_name: &str) -> InstrumentMap {
    let mut map = InstrumentMap::default();
    if ops.is_empty() {
        return map;
    }
    map.functions.push((fn_name.into(), 0));
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
    let cs = super::fn_shape_checksum(ops, &map, fn_name);
    super::record_fn_checksum(fn_name, cs);
    super::record_instrument_map(map.clone());
    map
}

/// Assign sites and insert `pgo_hit` HostInvoke sequences (stack-neutral).
pub fn instrument_for_pgo_mut(ops: &mut Vec<IlOp>) -> InstrumentMap {
    instrument_for_pgo_named_mut(ops, "<module>")
}

pub fn instrument_for_pgo_named_mut(ops: &mut Vec<IlOp>, fn_name: &str) -> InstrumentMap {
    let map = instrument_for_pgo_named(ops, fn_name);
    let native = super::pgo_native_id().unwrap_or(0);
    insert_pgo_counters(ops, native, &map);
    map
}

pub fn pack_hit(kind: i32, site_key: u32) -> i32 {
    ((site_key as i32) << 2) | (kind & 3)
}

fn site_key(local: u32) -> u32 {
    super::current_fn_index().saturating_mul(SITE_STRIDE) + local
}

/// Insert stack-neutral `CONST native; CONST packed; HostInvoke 1; Pop`.
pub fn insert_pgo_counters(ops: &mut Vec<IlOp>, native_id: i32, map: &InstrumentMap) {
    if ops.is_empty() {
        return;
    }
    let loc = DebugLoc::unknown();
    let mut at: Vec<(usize, i32)> = Vec::new();
    at.push((map.functions.first().map(|(_, i)| *i).unwrap_or(0), pack_hit(KIND_FN, site_key(0))));
    for (local, &idx) in map.blocks.iter().enumerate() {
        at.push((idx, pack_hit(KIND_BLOCK, site_key(local as u32))));
    }
    for (local, &jmp_i) in map.branches.iter().enumerate() {
        let key = site_key(local as u32);
        if jmp_i + 1 < ops.len() {
            at.push((jmp_i + 1, pack_hit(KIND_BR_NOT, key)));
        }
        if let IlOp::Jump { target: Label(id), .. } = ops[jmp_i] {
            if let Some(ti) = ops.iter().position(|op| matches!(op, IlOp::Label(Label(l)) if *l == id))
            {
                at.push((ti, pack_hit(KIND_BR_TAKEN, key)));
            }
        }
    }
    // High index first; at the same index, higher packed kinds first so fn/block land first.
    at.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)));
    for (pos, packed) in at {
        let pos = pos.min(ops.len());
        let seq = hit_ops(native_id, packed, loc);
        ops.splice(pos..pos, seq);
    }
}

fn hit_ops(native_id: i32, packed: i32, loc: DebugLoc) -> [IlOp; 4] {
    [
        IlOp::Const {
            imm: native_id,
            loc,
        },
        IlOp::Const { imm: packed, loc },
        IlOp::HostInvoke { arity: 1, loc },
        IlOp::Pop { loc },
    ]
}

pub(crate) fn block_leaders_pub(ops: &[IlOp]) -> Vec<usize> {
    block_leaders(ops)
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
