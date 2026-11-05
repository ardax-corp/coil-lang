//! Stack-convoy reconstruct for dense `CALL` / `TailCall` (COI-340 B2).
//!
//! Tight recursive leafs run slower than their LIR form when every SSA
//! value gets a unique slot, a prologue `Seek`, `DensePush`, and a `STORE`
//! after each `CALL`. Values that only feed a same-block call / return stay on the
//! operand stack — the fuse-IL convoy — so Seek is skipped when only
//! param slots are live.

use crate::il::Label;
use super::func::MirFunc;
use super::inst::{BlockId, MirAllocKind, MirConst, MirInst, Terminator, ValueId};

pub(super) struct ConvoyPlan {
    pub need_slot: Vec<bool>,
    pub def: Vec<Option<(BlockId, usize)>>,
    pub self_entry: Option<Label>,
}

impl ConvoyPlan {

}

/// `inst` is a planned tail call ([`MirFunc::tail_targets`]) whose result
/// `block` returns: a `TailCall`. Any other call keeps the caller's frame,
/// which a failed `requires` in the callee blames and a backtrace names.
pub fn is_tail_call_inst(func: &MirFunc, block: &super::func::MirBlock, inst: &MirInst) -> bool {
    let MirInst::Call { dest, dest_hi, target, .. } = inst else {
        return false;
    };
    if !func.tail_targets.contains(target) {
        return false;
    }
    match block.term {
        Some(Terminator::Return { lo: Some(v), hi }) if v == *dest && hi == *dest_hi => {
            matches!(
                block.insts.last(),
                Some(MirInst::Call { dest: d, dest_hi: h, .. }) if d == dest && h == dest_hi
            )
        }
        _ => false,
    }
}
