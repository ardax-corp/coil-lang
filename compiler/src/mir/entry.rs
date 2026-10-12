//! I8 — which leftover bodies enter MIR→LIR after dense specialize.
//!
//! Both MIR tiers lower from HIR (M4: there is no stack-IL lift). Dense
//! goes first ([`crate::mir::try_specialize_body_side`]); a body it misses
//! tries LIR ([`crate::mir::try_lower_abi_body_side`]), which `IlModule`
//! keeps only when cost ≤ fuse. The walls below are what the stack IL the
//! LIR replaces must not hold: one-word `CALL`, unmapped alloc, box.
//! Fuse-IL stays the fallback.

use common::Instruction;

use crate::il::IlOp;

use super::gc::refuses_alloc;

/// Why a leftover body stays fuse-IL instead of MIR→LIR.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LirRefuse {
    /// I5: `MakeArray` / `MakeTuple` / `MakeEnum` / `InitTyped`.
    Alloc,
    /// I6: user `CALL` / `TailCall` / other `Entry` (LIR reconstruct wall;
    /// Q7 one-word self-`CALL` is dense + cost, not this refuse).
    Call,
    /// `BoxValue` / `UnboxValue`.
    Box,
}

/// The walls for a body whose LIR comes from MIR lowered from HIR: that
/// MIR reads and writes class fields itself and maps its own allocations
/// (`maps_ok` from [`super::stackmap::mir_maps_ok`]), so neither is a wall
/// on the stack IL it replaces.
pub fn lir_refuse_direct(ops: &[IlOp], maps_ok: bool) -> Option<LirRefuse> {
    for op in ops {
        match op {
            IlOp::Entry {
                kind: crate::il::EntryKind::Call,
                ret_words,
                ..
            } if super::abi::is_multi_word_ret(*ret_words)
                && super::abi::ret_words_ok(*ret_words) => {}
            IlOp::Entry {
                kind: crate::il::EntryKind::MakeCoro,
                ..
            } if maps_ok => {}
            IlOp::Entry {
                kind: crate::il::EntryKind::MakeCoro,
                ..
            } => return Some(LirRefuse::Alloc),
            IlOp::Entry { .. } | IlOp::PrologueJmp { .. } => return Some(LirRefuse::Call),
            IlOp::BoxValue { .. } | IlOp::UnboxValue { .. } => return Some(LirRefuse::Box),
            op if refuses_alloc(op) && !maps_ok => return Some(LirRefuse::Alloc),
            IlOp::Jump {
                kind: crate::il::IlJumpKind::JumpIfMatch { .. },
                ..
            } => {}
            IlOp::Byte { byte, .. } if *byte.bytecode() == Instruction::Unpack => {}
            // Packed Dense*/fused ops are not a LIR entry tax (COI-385 S8).
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::il::{IlJumpKind, IlOp, Label};
    use common::{Byte, DebugLoc, Instruction};

    fn loc() -> DebugLoc {
        DebugLoc::unknown()
    }

    #[test]
    fn i8_one_word_bitor_is_lir_eligible() {
        let loc = loc();
        let ops = [
            IlOp::Label(Label(0)),
            IlOp::Load { slot: 0, loc },
            IlOp::Const { imm: 1, loc },
            IlOp::Bin {
                op: Instruction::BITOR,
                loc,
            },
            IlOp::Return { loc, ret_words: 1 },
        ];
        assert_eq!(lir_refuse_direct(&ops, false), None);
    }

    #[test]
    fn i8_plain_if_diamond_is_lir_eligible() {
        let loc = loc();
        let ops = [
            IlOp::Label(Label(0)),
            IlOp::Load { slot: 0, loc },
            IlOp::Load { slot: 1, loc },
            IlOp::Bin {
                op: Instruction::LE,
                loc,
            },
            IlOp::Jump {
                kind: IlJumpKind::JumpIfFalse,
                target: Label(1),
                loc,
                hint: Default::default(),
            },
            IlOp::Load { slot: 0, loc },
            IlOp::Return { loc, ret_words: 1 },
            IlOp::Label(Label(1)),
            IlOp::Load { slot: 1, loc },
            IlOp::Return { loc, ret_words: 1 },
        ];
        assert_eq!(lir_refuse_direct(&ops, false), None);
    }

    #[test]
    fn i8_store_loop_is_lir_eligible() {
        let loc = loc();
        let ops = [
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(0),
                loc,
                hint: Default::default(),
            },
            IlOp::Label(Label(0)),
            IlOp::Const { imm: 1, loc },
            IlOp::StorePop { slot: 2, loc },
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(0),
                loc,
                hint: Default::default(),
            },
        ];
        assert_eq!(lir_refuse_direct(&ops, false), None);
    }

    #[test]
    fn i8_tiny_let_is_lir_eligible() {
        let loc = loc();
        let ops = [
            IlOp::Const { imm: 42, loc },
            IlOp::StorePop { slot: 0, loc },
            IlOp::Load { slot: 0, loc },
            IlOp::Return { loc, ret_words: 1 },
        ];
        assert_eq!(lir_refuse_direct(&ops, false), None);
    }

    #[test]
    fn i2_boxed_overlap_arity0_is_lir_eligible() {
        let loc = loc();
        let ops = [
            IlOp::Load { slot: 0, loc },
            IlOp::Jump {
                kind: IlJumpKind::JumpIfMatch { tag: 2, arity: 0 },
                target: Label(1),
                loc,
                hint: Default::default(),
            },
            IlOp::Return { loc, ret_words: 1 },
        ];
        assert_eq!(lir_refuse_direct(&ops, false), None);
    }

    #[test]
    fn i8_one_word_call_is_a_wall_but_host_lifts() {
        let loc = loc();
        let call = [IlOp::Entry {
            kind: crate::il::EntryKind::Call,
            arity: 1,
            target: Label(1),
            loc,
            ret_words: 1,
        }];
        assert_eq!(lir_refuse_direct(&call, false), Some(LirRefuse::Call));
        let host = [IlOp::HostInvoke {
            arity: 0,
            layout: 0,
            loc,
        }];
        // HostInvoke lifts; the MIR builder refuses untyped native ids.
        assert_eq!(lir_refuse_direct(&host, false), None);
    }

    #[test]
    fn b3_two_slot_call_is_not_a_lir_wall() {
        let loc = loc();
        let call = [IlOp::Entry {
            kind: crate::il::EntryKind::Call,
            arity: 1,
            target: Label(1),
            loc,
            ret_words: 2,
        }];
        assert_eq!(lir_refuse_direct(&call, false), None);
        let tail = [IlOp::Entry {
            kind: crate::il::EntryKind::TailCall,
            arity: 1,
            target: Label(1),
            loc,
            ret_words: 2,
        }];
        assert_eq!(lir_refuse_direct(&tail, false), Some(LirRefuse::Call));
        let wide = [IlOp::Entry {
            kind: crate::il::EntryKind::Call,
            arity: 1,
            target: Label(1),
            loc,
            ret_words: 3,
        }];
        assert_eq!(
            lir_refuse_direct(&wide, false),
            Some(LirRefuse::Call),
            "N>MAX_MODELED_RET_WORDS stays a LIR reconstruct wall"
        );
    }

    #[test]
    fn s2c_mapped_alloc_is_lir_eligible() {
        let loc = loc();
        let ops = [
            IlOp::Label(Label(0)),
            IlOp::Load { slot: 0, loc },
            IlOp::Load { slot: 1, loc },
            IlOp::MakeArray { elem_kind: 0, arity: 2, loc },
            IlOp::Return { loc, ret_words: 1 },
        ];
        assert_eq!(lir_refuse_direct(&ops, false), Some(LirRefuse::Alloc));
        assert_eq!(lir_refuse_direct(&ops, true), None);
    }

    #[test]
    fn q9_r3_mapped_format_is_lir_eligible() {
        let loc = loc();
        let ops = [
            IlOp::Label(Label(0)),
            IlOp::String { idx: 0, loc },
            IlOp::Load { slot: 0, loc },
            IlOp::Byte {
                byte: Byte::new(Instruction::FORMAT).with_operand_u32(1),
                loc,
            },
            IlOp::Return { loc, ret_words: 1 },
        ];
        assert_eq!(lir_refuse_direct(&ops, false), Some(LirRefuse::Alloc));
        assert_eq!(lir_refuse_direct(&ops, true), None);
    }

    #[test]
    fn b6_mapped_array_push_is_lir_eligible() {
        let loc = loc();
        let ops = [
            IlOp::Label(Label(0)),
            IlOp::Load { slot: 0, loc },
            IlOp::Const { imm: 1, loc },
            IlOp::byte(Byte::new(Instruction::ArrayPush)),
            IlOp::Return { loc, ret_words: 1 },
        ];
        assert_eq!(lir_refuse_direct(&ops, false), Some(LirRefuse::Alloc));
        assert_eq!(lir_refuse_direct(&ops, true), None);
    }

    #[test]
    fn c2b_mapped_make_coro_is_lir_eligible() {
        let loc = loc();
        let ops = [
            IlOp::Label(Label(0)),
            IlOp::Entry {
                kind: crate::il::EntryKind::MakeCoro,
                arity: 0,
                target: Label(1),
                loc,
                ret_words: 1,
            },
            IlOp::Return { loc, ret_words: 1 },
        ];
        assert_eq!(lir_refuse_direct(&ops, false), Some(LirRefuse::Alloc));
        assert_eq!(lir_refuse_direct(&ops, true), None);
    }

    #[test]
    fn c2b_resume_coro_is_lir_eligible() {
        let loc = loc();
        let ops = [
            IlOp::Label(Label(0)),
            IlOp::Load { slot: 0, loc },
            IlOp::byte(Byte::new(Instruction::ResumeCoro)),
            IlOp::byte(Byte::new(Instruction::DoneCoro)),
            IlOp::Return { loc, ret_words: 1 },
        ];
        assert_eq!(lir_refuse_direct(&ops, false), None);
    }

    #[test]
    fn i8_heap_index_is_lir_eligible() {
        let loc = loc();
        let ops = [
            IlOp::Load { slot: 0, loc },
            IlOp::Load { slot: 1, loc },
            IlOp::Index { loc },
            IlOp::Return { loc, ret_words: 1 },
        ];
        assert_eq!(lir_refuse_direct(&ops, false), None);
    }

    #[test]
    fn q6_counted_index_loop_is_not_a_lir_wall() {
        let loc = loc();
        let ops = [
            IlOp::Label(Label(0)),
            IlOp::Load { slot: 0, loc },
            IlOp::Load { slot: 1, loc },
            IlOp::Bin {
                op: Instruction::ADD,
                loc,
            },
            IlOp::Index { loc },
            IlOp::Jump {
                kind: IlJumpKind::JumpIfFalse,
                target: Label(0),
                loc,
                hint: Default::default(),
            },
            IlOp::Return { loc, ret_words: 1 },
        ];
        assert_eq!(lir_refuse_direct(&ops, false), None);
    }

    #[test]
    fn q8_jump_if_match_arity1_is_not_a_lir_wall() {
        let loc = loc();
        let ops = [
            IlOp::Load { slot: 0, loc },
            IlOp::Jump {
                kind: IlJumpKind::JumpIfMatch { tag: 0, arity: 1 },
                target: Label(1),
                loc,
                hint: Default::default(),
            },
            IlOp::Return { loc, ret_words: 1 },
        ];
        assert_eq!(lir_refuse_direct(&ops, false), None);
    }

    #[test]
    fn d3_unpack_arity2_is_not_a_lir_wall() {
        let loc = loc();
        let ops = [
            IlOp::Load { slot: 0, loc },
            IlOp::Jump {
                kind: IlJumpKind::JumpIfMatch { tag: 0, arity: 0 },
                target: Label(1),
                loc,
                hint: Default::default(),
            },
            IlOp::Byte {
                byte: Byte::new(Instruction::Unpack).with_operand_u32(2),
                loc,
            },
            IlOp::Return { loc, ret_words: 1 },
        ];
        assert_eq!(lir_refuse_direct(&ops, false), None);
        let jim2 = [IlOp::Jump {
            kind: IlJumpKind::JumpIfMatch { tag: 1, arity: 2 },
            target: Label(1),
            loc,
            hint: Default::default(),
        }];
        assert_eq!(lir_refuse_direct(&jim2, false), None);
    }

    /// COI-385: packed dense ops are not a `hard_refuse` / `lir_eligible` tax.
    #[test]
    fn dense_packed_ops_are_lir_eligible() {
        let loc = loc();
        let ops = [
            IlOp::byte(Byte::new(Instruction::DenseBin).with_dense_abc(0, 2, 0, 1)),
            IlOp::byte(Byte::new(Instruction::DenseCast).with_dense_abc(0, 3, 2, 0)),
            IlOp::byte(Byte::new(Instruction::DenseIndex).with_dense_abc(0, 4, 0, 1)),
            IlOp::byte(Byte::new(Instruction::DenseStoreIndex).with_dense_abc(0, 0, 1, 2)),
            IlOp::byte(Byte::new(Instruction::DenseMove).with_dense_abc(0, 1, 0, 0)),
            IlOp::Return { loc, ret_words: 1 },
        ];
        assert_eq!(lir_refuse_direct(&ops, false), None);
    }
}
