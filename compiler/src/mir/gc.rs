//! I5 — alloc edges and GC barrier placeholders in MIR.
//!
//! `MakeArray` / `MakeTuple` / `MakeEnum` / `InitTyped` can lower to
//! [`crate::mir::MirInst::Alloc`] plus a [`crate::mir::MirInst::GcBarrier`]
//! safepoint. Dense specialize and MIR→LIR still refuse: there are no
//! precise stack maps, so crossing GC would invent rooted native/JIT
//! assumptions. Production bodies stay fuse-IL.
//!
//! Stack-map roadmap: `docs/internals/mir-stack-maps.md`.

use common::Instruction;

use crate::il::IlOp;

/// Residual object-init opcodes (cold `IlOp::Byte`).
pub fn is_alloc_inst(inst: Instruction) -> bool {
    matches!(inst, Instruction::InitTyped | Instruction::INIT)
}

/// IL that must not enter dense specialize or MIR→LIR (I5).
pub fn refuses_alloc(op: &IlOp) -> bool {
    refuse_reason(op).is_some()
}

/// Inventory label: `heap/aggregate` for Make*, `heap/alloc` for InitTyped.
pub fn refuse_reason(op: &IlOp) -> Option<&'static str> {
    match op {
        IlOp::MakeArray { .. } | IlOp::MakeTuple { .. } | IlOp::MakeEnum { .. } => {
            Some("heap/aggregate")
        }
        IlOp::Byte { byte, .. } if is_alloc_inst(*byte.bytecode()) => Some("heap/alloc"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::{Byte, DebugLoc};

    fn loc() -> DebugLoc {
        DebugLoc::unknown()
    }

    #[test]
    fn make_and_init_are_alloc_barriers() {
        let loc = loc();
        assert_eq!(
            refuse_reason(&IlOp::MakeArray { arity: 1, loc }),
            Some("heap/aggregate")
        );
        assert_eq!(
            refuse_reason(&IlOp::MakeTuple { arity: 2, loc }),
            Some("heap/aggregate")
        );
        assert_eq!(
            refuse_reason(&IlOp::MakeEnum {
                tag: 0,
                arity: 1,
                loc,
            }),
            Some("heap/aggregate")
        );
        assert_eq!(
            refuse_reason(&IlOp::Byte {
                byte: Byte::new(Instruction::InitTyped).with_operand_u32(1),
                loc,
            }),
            Some("heap/alloc")
        );
        assert!(!refuses_alloc(&IlOp::Const { imm: 1, loc }));
    }
}
