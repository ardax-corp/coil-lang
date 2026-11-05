//! Per-op operand-height deltas over IL ops.
//!
//! This is **not** the shared buffer cursor: [`super::tell`] tracks `tell`,
//! which `STORE` floors to `slot + 1` even when height is lower.
//! [`stack_delta`] only counts eval-stack values; it feeds [`super::tell`] and
//! `coil dissect`'s operand-depth column. Do not substitute tell here — a
//! STORE floor is not height (COI-81).



#[cfg(test)]
mod tests {
    use super::*;
    use crate::il::op::Label;
    use common::DebugLoc;

    fn loc() -> DebugLoc {
        DebugLoc::unknown()
    }

    #[test]
    fn format_and_stringify_stack_deltas() {
        use common::Byte;
        let format1 = IlOp::byte(Byte::new(Instruction::FORMAT).with_operand_u32(1));
        let format0 = IlOp::byte(Byte::new(Instruction::FORMAT).with_operand_u32(0));
        let stringify = IlOp::byte(Byte::new(Instruction::STRINGIFY));
        assert_eq!(stack_delta(&format1), Some(-1));
        assert_eq!(stack_delta(&format0), Some(0));
        assert_eq!(stack_delta(&stringify), Some(0));
    }

    #[test]
    fn array_len_and_store_index_stack_deltas() {
        use common::Byte;
        assert_eq!(
            stack_delta(&IlOp::byte(Byte::new(Instruction::ArrayLen))),
            Some(0)
        );
        assert_eq!(
            stack_delta(&IlOp::byte(Byte::new(Instruction::StoreIndex))),
            Some(-2)
        );
        assert_eq!(
            stack_delta(&IlOp::byte(Byte::new(Instruction::ArrayPush))),
            Some(-1)
        );
    }

    #[test]
    fn stack_delta_bin_slot_forms_push_one() {
        let imm = IlOp::BinSlotImm {
            op: Instruction::ADD as u8,
            slot: 0,
            imm: 1,
            loc: loc(),
        };
        let slot = IlOp::BinSlotSlot {
            op: Instruction::ADD as u8,
            a: 0,
            b: 1,
            loc: loc(),
        };
        assert_eq!(stack_delta(&imm), Some(1));
        assert_eq!(stack_delta(&slot), Some(1));
    }

    /// Packed LOAD/STORE and fused assign/jmpf forms must report accurate net deltas
    /// so convoy/LICM SP analysis does not poison on the new encodings.
    #[test]
    fn stack_delta_packed_load_store_and_fused_stores() {
        use common::Byte;
        let load3 = IlOp::byte(Byte::new(Instruction::LOAD).with_load_store_packed(3, 0, 1, 2));
        let store2 = IlOp::byte(Byte::new(Instruction::STORE).with_load_store_packed(2, 4, 5, 0));
        let imm_store = IlOp::byte(
            Byte::new(Instruction::BinSlotImmStore).with_bin_slot_imm_store(
                Instruction::ADD as u8,
                0,
                1,
            ),
        );
        let slot_store = IlOp::byte(
            Byte::new(Instruction::BinSlotSlotStore).with_bin_slot_slot_store(
                Instruction::BITAND as u8,
                0,
                1,
                2,
            ),
        );
        let slot_jmpf = IlOp::byte(
            Byte::new(Instruction::BinSlotSlotJmpf).with_bin_slot_slot_jmpf(
                Instruction::AND as u8,
                0,
                3,
            ),
        );
        assert_eq!(stack_delta(&load3), Some(3));
        assert_eq!(stack_delta(&store2), Some(-2));
        assert_eq!(stack_delta(&imm_store), Some(0));
        assert_eq!(stack_delta(&slot_store), Some(0));
        assert_eq!(stack_delta(&slot_jmpf), Some(0));
    }

    #[test]
    fn stack_delta_call_uses_arity_tail_call_unknown() {
        let call = IlOp::Entry {
            kind: EntryKind::Call,
            arity: 2,
            target: Label(0),
            loc: loc(), ret_words: 1,};
        let tail = IlOp::Entry {
            kind: EntryKind::TailCall,
            arity: 1,
            target: Label(0),
            loc: loc(), ret_words: 1,};
        assert_eq!(stack_delta(&call), Some(-1));
        assert_eq!(stack_delta(&tail), None);
    }

    /// `1 - arity`, not JumpIfMatch's `arity - 1`. Arity 0 is the discriminator
    /// (`+1` vs `-1`); arity 1 agrees under both formulas. Corpus gate: COI-80
    /// `tell_symbolic_il_entry_call_delta_is_not_jump_if_match_arity_minus_one`.
    #[test]
    fn stack_delta_call_uses_one_minus_arity_not_arity_minus_one() {
        let call0 = IlOp::Entry {
            kind: EntryKind::Call,
            arity: 0,
            target: Label(0),
            loc: loc(), ret_words: 1,};
        let coro0 = IlOp::Entry {
            kind: EntryKind::MakeCoro,
            arity: 0,
            target: Label(0),
            loc: loc(), ret_words: 1,};
        assert_eq!(stack_delta(&call0), Some(1));
        assert_eq!(stack_delta(&coro0), Some(1));
        assert_ne!(
            stack_delta(&call0),
            Some(-1),
            "must not use JumpIfMatch's arity-1"
        );
    }

    /// STORE pops; it does not raise operand height to `slot + 1`. After
    /// `CONST; STORE 5` height is 0, not 6.
    #[test]
    fn store_does_not_raise_operand_height() {
        let store = IlOp::StorePop {
            slot: 5,
            loc: loc(),
        };
        assert_eq!(stack_delta(&store), Some(-1));
    }

    #[test]
    fn stack_delta_jump_if_match_consumes_scrutinee() {
        let jmp = IlOp::Jump {
            kind: IlJumpKind::JumpIfMatch { tag: 1, arity: 2 },
            target: Label(0),
            loc: loc(),
            hint: Default::default(),
        };
        assert_eq!(stack_delta(&jmp), Some(-1));
    }

    #[test]
    fn stack_delta_typed_longtail_ops() {
        assert_eq!(stack_delta(&IlOp::Index { loc: loc() }), Some(-1));
        assert_eq!(
            stack_delta(&IlOp::MakeTuple {
                kinds: 0,
                arity: 3,
                loc: loc(),
            }),
            Some(-2)
        );
        assert_eq!(
            stack_delta(&IlOp::MakeArray { elem_kind: 0,
                arity: 2,
                loc: loc(),
            }),
            Some(-1)
        );
        assert_eq!(
            stack_delta(&IlOp::MakeEnum {
                kinds: 0,
                tag: 1,
                arity: 0,
                loc: loc(),
            }),
            Some(1)
        );
        assert_eq!(
            stack_delta(&IlOp::MakeEnum {
                kinds: 0,
                tag: 1,
                arity: 2,
                loc: loc(),
            }),
            Some(-1)
        );
        assert_eq!(stack_delta(&IlOp::BoxValue { tag: 0, loc: loc() }), Some(0));
        assert_eq!(
            stack_delta(&IlOp::UnboxValue { tag: 0, loc: loc() }),
            Some(0)
        );
        assert_eq!(
            stack_delta(&IlOp::LoadField {
                index: 1,
                loc: loc(),
            }),
            Some(0)
        );
        assert_eq!(stack_delta(&IlOp::GetField { loc: loc() }), Some(-1));
        assert_eq!(stack_delta(&IlOp::SetField { loc: loc(), index: None }), Some(-2));
        assert_eq!(
            stack_delta(&IlOp::SetField {
                loc: loc(),
                index: Some(1)
            }),
            Some(-1)
        );
        assert_eq!(
            stack_delta(&IlOp::HostInvoke {
                arity: 3,
                layout: 0,
                loc: loc(),
            }),
            Some(-3)
        );
        assert_eq!(stack_delta(&IlOp::Print { loc: loc() }), Some(-1));
        assert_eq!(
            stack_delta(&IlOp::ConstPool { idx: 2, loc: loc() }),
            Some(1)
        );
        assert_eq!(stack_delta(&IlOp::String { idx: 4, loc: loc() }), Some(1));
    }

    #[test]
    fn make_dict_and_data_stack_deltas() {
        use common::Byte;
        // MakeDict arity=2: pop 4 (k,v,k,v), push dict → −3.
        assert_eq!(
            stack_delta(&IlOp::byte(
                Byte::new(Instruction::MakeDict).with_operand_u32(2)
            )),
            Some(1 - 2 * 2)
        );
        assert_eq!(
            stack_delta(&IlOp::byte(
                Byte::new(Instruction::MakeDict).with_operand_u32(0)
            )),
            Some(1)
        );
        assert_eq!(
            stack_delta(&IlOp::byte(
                Byte::new(Instruction::STRING).with_operand_u32(1)
            )),
            Some(1)
        );
    }

    #[test]
    fn set_field_byte_matches_typed_delta() {
        use common::Byte;
        assert_eq!(
            stack_delta(&IlOp::byte(Byte::new(Instruction::SetField))),
            Some(-2)
        );
        assert_eq!(
            stack_delta(&IlOp::byte(Byte::new(Instruction::GetField))),
            Some(-1)
        );
    }
}
