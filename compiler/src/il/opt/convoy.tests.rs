    use super::*;
    use crate::il::opt::{OptimizeOptions, optimize_at, optimize_per_func};
    use crate::il::opt::cfg::{eliminate_dead_blocks, invert_branch_over_jump, jump_thread};
    use crate::il::opt::dce::{copy_prop, dead_store, dead_store_at, mem_fwd, stack_dce};
    use common::{Byte, Instruction};

    fn is_insn(op: &IlOp, i: Instruction) -> bool {
        op.instruction() == Some(i)
    }

    #[test]
    fn mem_fwd_refuses_when_load_feeds_index() {
        let mut ops = vec![
            IlOp::StorePop {
                slot: 5,
                loc: common::DebugLoc::unknown(),
            },
            IlOp::Load {
                slot: 5,
                loc: common::DebugLoc::unknown(),
            },
            IlOp::Const {
                imm: 0,
                loc: common::DebugLoc::unknown(),
            },
            IlOp::Index {
                loc: common::DebugLoc::unknown(),
            },
            IlOp::Return {
                loc: common::DebugLoc::unknown(), ret_words: 1,},
        ];
        mem_fwd(&mut ops, 0);
        assert!(matches!(ops[0], IlOp::StorePop { slot: 5, .. }));
        assert!(matches!(ops[1], IlOp::Load { slot: 5, .. }));
    }

    #[test]
    fn mem_fwd_refuses_when_tos_aliases_store_slot() {
        let mut ops = vec![
            IlOp::Const {
                imm: 1,
                loc: common::DebugLoc::unknown(),
            },
            IlOp::Const {
                imm: 2,
                loc: common::DebugLoc::unknown(),
            },
            IlOp::MakeTuple {
                arity: 2,
                loc: common::DebugLoc::unknown(),
            },
            IlOp::StorePop {
                slot: 0,
                loc: common::DebugLoc::unknown(),
            },
            IlOp::Load {
                slot: 0,
                loc: common::DebugLoc::unknown(),
            },
            IlOp::Const {
                imm: 0,
                loc: common::DebugLoc::unknown(),
            },
            IlOp::Index {
                loc: common::DebugLoc::unknown(),
            },
            IlOp::Return {
                loc: common::DebugLoc::unknown(), ret_words: 1,},
        ];
        let before = ops.clone();
        mem_fwd(&mut ops, 0);
        assert!(matches!(ops[3], IlOp::StorePop { slot: 0, .. }));
        assert!(matches!(ops[4], IlOp::Load { slot: 0, .. }));
        assert_eq!(ops.len(), before.len());
    }

    /// After nested CALL return (`tell == frame_base + 1`), StorePop to a higher
    /// slot must not become Dup;Store — tell extension makes later operand pops
    /// consume the local (e.g. `let x = f(); if x == k`).
    #[test]
    fn mem_fwd_refuses_store_above_tos_after_call_return_height() {
        let loc = common::DebugLoc::unknown();
        // Model post-return height 1 (return value only) then store to slot 3.
        let mut ops = vec![
            IlOp::Const { imm: 4, loc },
            IlOp::StorePop { slot: 3, loc },
            IlOp::Load { slot: 3, loc },
            IlOp::Const { imm: 999999, loc },
            IlOp::Jump {
                kind: IlJumpKind::JumpIfFalse,
                target: Label(0),
                loc,
                hint: Default::default(),
            },
            IlOp::Label(Label(0)),
            IlOp::Return { loc, ret_words: 1},
        ];
        mem_fwd(&mut ops, 0);
        assert!(
            matches!(ops[1], IlOp::StorePop { slot: 3, .. }),
            "StorePop;Load at height 1 to slot 3 must not become Dup;StorePop"
        );
        assert!(matches!(ops[2], IlOp::Load { slot: 3, .. }));
    }

    /// Nested CALL resets height to 1; StorePop to a higher slot must not
    /// become Dup;Store (arithmetic `1 - arity` would overestimate height).
    #[test]
    fn mem_fwd_refuses_post_call_store_that_aliases_tos() {
        let loc = common::DebugLoc::unknown();
        let mut ops = vec![
            IlOp::Load { slot: 0, loc },
            IlOp::Load { slot: 1, loc },
            IlOp::Load { slot: 2, loc },
            IlOp::Entry {
                kind: crate::il::op::EntryKind::Call,
                arity: 0,
                target: Label(0),
                loc, ret_words: 1,},
            IlOp::StorePop { slot: 4, loc },
            IlOp::Load { slot: 4, loc },
            IlOp::Return { loc, ret_words: 1},
        ];
        // Deep frame; nullary CALL must not leave modeled height 6.
        mem_fwd(&mut ops, 5);
        assert!(
            matches!(ops[4], IlOp::StorePop { slot: 4, .. }),
            "must keep StorePop;Load when CALL resets height to 1"
        );
        assert!(matches!(ops[5], IlOp::Load { slot: 4, .. }));
    }

    #[test]
    fn jump_thread_collapses_goto_goto() {
        let mut ops = vec![
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(0),
                loc: common::DebugLoc::unknown(),
                hint: Default::default(),
            },
            IlOp::Byte {
                byte: Byte::new(Instruction::CONST).with_const_inline(1),
                loc: common::DebugLoc::unknown(),
            },
            IlOp::Label(Label(0)),
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(1),
                loc: common::DebugLoc::unknown(),
                hint: Default::default(),
            },
            IlOp::Label(Label(1)),
            IlOp::Byte {
                byte: Byte::new(Instruction::HALT),
                loc: common::DebugLoc::unknown(),
            },
        ];
        jump_thread(&mut ops);
        match &ops[0] {
            IlOp::Jump {
                target: Label(1), ..
            } => {}
            _ => panic!("expected JMP L1 after jump threading"),
        }
    }

    #[test]
    fn stack_dce_removes_dup_pop() {
        let mut ops = vec![
            IlOp::byte(Byte::new(Instruction::DUPLICATE)),
            IlOp::byte(Byte::new(Instruction::POP)),
            IlOp::byte(Byte::new(Instruction::HALT)),
        ];
        stack_dce(&mut ops);
        assert_eq!(ops.len(), 1);
    }

    #[test]
    fn stack_dce_removes_load_store_pop_same_slot() {
        let mut ops = vec![
            IlOp::byte(Byte::new(Instruction::LOAD).with_operand_u32(4)),
            IlOp::byte(Byte::new(Instruction::StorePop).with_operand_u32(4)),
            IlOp::byte(Byte::new(Instruction::HALT)),
        ];
        stack_dce(&mut ops);
        assert_eq!(ops.len(), 1);
        assert!(is_insn(&ops[0], Instruction::HALT));
    }

    #[test]
    fn stack_dce_keeps_load_store_pop_different_slots() {
        let mut ops = vec![
            IlOp::byte(Byte::new(Instruction::LOAD).with_operand_u32(1)),
            IlOp::byte(Byte::new(Instruction::StorePop).with_operand_u32(2)),
            IlOp::byte(Byte::new(Instruction::HALT)),
        ];
        let before = ops.clone();
        stack_dce(&mut ops);
        assert!(ops == before);
    }

    #[test]
    fn dead_block_drops_after_unconditional_jmp() {
        let mut ops = vec![
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(0),
                loc: common::DebugLoc::unknown(),
                hint: Default::default(),
            },
            IlOp::byte(Byte::new(Instruction::CONST).with_const_inline(1)),
            IlOp::Label(Label(0)),
            IlOp::byte(Byte::new(Instruction::HALT)),
        ];
        eliminate_dead_blocks(&mut ops);
        assert_eq!(ops.len(), 3);
        assert!(matches!(ops[1], IlOp::Label(Label(0))));
    }

    #[test]
    fn dead_block_drops_after_return_until_label() {
        let mut ops = vec![
            IlOp::byte(Byte::new(Instruction::RETURN)),
            IlOp::byte(Byte::new(Instruction::CONST).with_const_inline(1)),
            IlOp::Label(Label(0)),
            IlOp::byte(Byte::new(Instruction::HALT)),
        ];
        eliminate_dead_blocks(&mut ops);
        assert_eq!(ops.len(), 3);
        assert!(is_insn(&ops[0], Instruction::RETURN));
        assert!(matches!(ops[1], IlOp::Label(Label(0))));
    }

    #[test]
    fn dead_block_drops_after_fused_return_until_label() {
        let mut ops = vec![
            IlOp::byte(Byte::new(Instruction::ConstReturnImm).with_operand_u32(0)),
            IlOp::byte(Byte::new(Instruction::CONST).with_const_inline(99)),
            IlOp::Label(Label(0)),
            IlOp::byte(Byte::new(Instruction::HALT)),
        ];
        eliminate_dead_blocks(&mut ops);
        assert_eq!(ops.len(), 3);
        assert!(is_insn(&ops[0], Instruction::ConstReturnImm));
        assert!(matches!(ops[1], IlOp::Label(Label(0))));
    }

    #[test]
    fn return_convoy_fuses_agreeing_const_join() {
        let mut ops = vec![
            IlOp::byte(Byte::new(Instruction::CONST).with_const_inline(0)),
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(0),
                loc: common::DebugLoc::unknown(),
                hint: Default::default(),
            },
            IlOp::byte(Byte::new(Instruction::CONST).with_const_inline(0)),
            IlOp::Label(Label(0)),
            IlOp::byte(Byte::new(Instruction::RETURN)),
        ];
        return_convoy(&mut ops);
        assert!(
            ops.iter()
                .any(|op| is_insn(op, Instruction::ConstReturnImm)),
            "expected ConstReturnImm"
        );
        assert!(
            !ops.iter().any(|op| is_insn(op, Instruction::CONST)),
            "producers should be stripped"
        );
        assert!(ops.iter().any(|op| matches!(op, IlOp::Label(Label(0)))));
        assert!(ops.iter().any(|op| matches!(
            op,
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(0),
                ..
            }
        )));
    }

    #[test]
    fn return_convoy_fuses_agreeing_load_join() {
        let mut ops = vec![
            IlOp::byte(Byte::new(Instruction::LOAD).with_operand_u32(0)),
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(0),
                loc: common::DebugLoc::unknown(),
                hint: Default::default(),
            },
            IlOp::byte(Byte::new(Instruction::LOAD).with_operand_u32(0)),
            IlOp::Label(Label(0)),
            IlOp::byte(Byte::new(Instruction::RETURN)),
        ];
        return_convoy(&mut ops);
        assert!(ops.iter().any(|op| {
            matches!(op, IlOp::LoadReturnSlot { slot: 0, .. })
                || op.as_encode_byte().is_some_and(|b| {
                    *b.bytecode() == Instruction::LoadReturnSlot && b.operand_u32() == 0
                })
        }));
    }

    #[test]
    fn return_convoy_skips_disagreeing_consts() {
        let mut ops = vec![
            IlOp::byte(Byte::new(Instruction::CONST).with_const_inline(0)),
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(0),
                loc: common::DebugLoc::unknown(),
                hint: Default::default(),
            },
            IlOp::byte(Byte::new(Instruction::CONST).with_const_inline(1)),
            IlOp::Label(Label(0)),
            IlOp::byte(Byte::new(Instruction::RETURN)),
        ];
        let before = ops.clone();
        return_convoy(&mut ops);
        assert!(ops == before);
    }

    #[test]
    fn return_convoy_skips_jump_without_producer() {
        // JMP to join with a value already on the stack (no LOAD/CONST before JMP).
        let mut ops = vec![
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(0),
                loc: common::DebugLoc::unknown(),
                hint: Default::default(),
            },
            IlOp::byte(Byte::new(Instruction::CONST).with_const_inline(0)),
            IlOp::Label(Label(0)),
            IlOp::byte(Byte::new(Instruction::RETURN)),
        ];
        let before = ops.clone();
        return_convoy(&mut ops);
        assert!(ops == before);
    }

    #[test]
    fn clone_shared_return_fuses_const_arm_after_jump_only_clone() {
        // Unwrap-shaped: jump-only Some arm + CONST None arm into shared RETURN.
        let mut ops = vec![
            IlOp::Load {
                slot: 0,
                loc: common::DebugLoc::unknown(),
            },
            IlOp::Jump {
                kind: IlJumpKind::JumpIfMatch { tag: 0, arity: 0 },
                target: Label(1),
                loc: common::DebugLoc::unknown(),
                hint: Default::default(),
            },
            IlOp::byte(Byte::new(Instruction::Unpack).with_operand_u32(1)),
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(0),
                loc: common::DebugLoc::unknown(),
                hint: Default::default(),
            },
            IlOp::Label(Label(1)),
            IlOp::Const {
                imm: 0,
                loc: common::DebugLoc::unknown(),
            },
            IlOp::Label(Label(0)),
            IlOp::Return {
                loc: common::DebugLoc::unknown(), ret_words: 1,},
        ];
        clone_shared_return(&mut ops);
        assert!(
            ops.iter().any(|op| matches!(op, IlOp::Return { .. })),
            "Some arm should RETURN locally"
        );
        assert!(
            ops.iter().any(|op| {
                matches!(op, IlOp::ConstReturnImm { imm: 0, .. })
                    || op
                        .as_encode_byte()
                        .is_some_and(|b| *b.bytecode() == Instruction::ConstReturnImm)
            }),
            "None arm should fuse ConstReturnImm"
        );
        assert!(
            !ops.iter().any(|op| matches!(
                op,
                IlOp::Jump {
                    kind: IlJumpKind::Unconditional,
                    ..
                }
            )),
            "jump-only JMP to shared return should be gone"
        );
    }

    #[test]
    fn return_convoy_skips_conditional_jump_into_cluster() {
        // CONST immediately before JMPF is the condition, not a value-under-cond.
        let mut ops = vec![
            IlOp::byte(Byte::new(Instruction::CONST).with_const_inline(0)),
            IlOp::Jump {
                kind: IlJumpKind::JumpIfFalse,
                target: Label(0),
                loc: common::DebugLoc::unknown(),
                hint: Default::default(),
            },
            IlOp::byte(Byte::new(Instruction::CONST).with_const_inline(0)),
            IlOp::Label(Label(0)),
            IlOp::byte(Byte::new(Instruction::RETURN)),
        ];
        let before = ops.clone();
        return_convoy(&mut ops);
        assert!(ops == before);
    }

    #[test]
    fn return_convoy_fuses_agreeing_const_via_jmpf() {
        // Value under condition on both JMPF arms. POP between arms keeps join SP Known
        // (fall-through after JMPF would otherwise accumulate height).
        let mut ops = vec![
            IlOp::byte(Byte::new(Instruction::CONST).with_const_inline(7)),
            IlOp::byte(Byte::new(Instruction::CONST).with_const_inline(1)),
            IlOp::Jump {
                kind: IlJumpKind::JumpIfFalse,
                target: Label(0),
                loc: common::DebugLoc::unknown(),
                hint: Default::default(),
            },
            IlOp::Pop {
                loc: common::DebugLoc::unknown(),
            },
            IlOp::byte(Byte::new(Instruction::CONST).with_const_inline(7)),
            IlOp::byte(Byte::new(Instruction::CONST).with_const_inline(1)),
            IlOp::Jump {
                kind: IlJumpKind::JumpIfFalse,
                target: Label(0),
                loc: common::DebugLoc::unknown(),
                hint: Default::default(),
            },
            IlOp::Label(Label(0)),
            IlOp::byte(Byte::new(Instruction::RETURN)),
        ];
        return_convoy(&mut ops);
        assert!(ops.iter().any(|op| {
            matches!(op, IlOp::ConstReturnImm { imm: 7, .. })
                || (is_insn(op, Instruction::ConstReturnImm)
                    && op.as_encode_byte().map(|b| b.operand_u32()) == Some(7))
        }));
        assert_eq!(
            ops.iter()
                .filter(|op| {
                    matches!(
                        op,
                        IlOp::Jump {
                            kind: IlJumpKind::JumpIfFalse,
                            ..
                        }
                    )
                })
                .count(),
            2
        );
    }

    #[test]
    fn return_convoy_fuses_agreeing_const_via_jmpt() {
        let mut ops = vec![
            IlOp::byte(Byte::new(Instruction::CONST).with_const_inline(9)),
            IlOp::byte(Byte::new(Instruction::CONST).with_const_inline(1)),
            IlOp::Jump {
                kind: IlJumpKind::JumpIfTrue,
                target: Label(0),
                loc: common::DebugLoc::unknown(),
                hint: Default::default(),
            },
            IlOp::Pop {
                loc: common::DebugLoc::unknown(),
            },
            IlOp::byte(Byte::new(Instruction::CONST).with_const_inline(9)),
            IlOp::byte(Byte::new(Instruction::CONST).with_const_inline(1)),
            IlOp::Jump {
                kind: IlJumpKind::JumpIfTrue,
                target: Label(0),
                loc: common::DebugLoc::unknown(),
                hint: Default::default(),
            },
            IlOp::Label(Label(0)),
            IlOp::byte(Byte::new(Instruction::RETURN)),
        ];
        return_convoy(&mut ops);
        assert!(ops.iter().any(|op| {
            matches!(op, IlOp::ConstReturnImm { imm: 9, .. })
                || (is_insn(op, Instruction::ConstReturnImm)
                    && op.as_encode_byte().map(|b| b.operand_u32()) == Some(9))
        }));
        assert_eq!(
            ops.iter()
                .filter(|op| {
                    matches!(
                        op,
                        IlOp::Jump {
                            kind: IlJumpKind::JumpIfTrue,
                            ..
                        }
                    )
                })
                .count(),
            2
        );
    }

    #[test]
    fn return_convoy_skips_mixed_jmpf_and_jmp() {
        let mut ops = vec![
            IlOp::byte(Byte::new(Instruction::CONST).with_const_inline(0)),
            IlOp::byte(Byte::new(Instruction::CONST).with_const_inline(1)),
            IlOp::Jump {
                kind: IlJumpKind::JumpIfFalse,
                target: Label(0),
                loc: common::DebugLoc::unknown(),
                hint: Default::default(),
            },
            IlOp::byte(Byte::new(Instruction::CONST).with_const_inline(0)),
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(0),
                loc: common::DebugLoc::unknown(),
                hint: Default::default(),
            },
            IlOp::Label(Label(0)),
            IlOp::byte(Byte::new(Instruction::RETURN)),
        ];
        let before = ops.clone();
        return_convoy(&mut ops);
        assert!(ops == before);
    }

    #[test]
    fn return_convoy_fuses_agreeing_const_via_jump_if_match() {
        let mut ops = vec![
            IlOp::byte(Byte::new(Instruction::CONST).with_const_inline(0)),
            IlOp::Jump {
                kind: IlJumpKind::JumpIfMatch { tag: 0, arity: 0 },
                target: Label(0),
                loc: common::DebugLoc::unknown(),
                hint: Default::default(),
            },
            IlOp::byte(Byte::new(Instruction::CONST).with_const_inline(0)),
            IlOp::Jump {
                kind: IlJumpKind::JumpIfMatch { tag: 1, arity: 0 },
                target: Label(0),
                loc: common::DebugLoc::unknown(),
                hint: Default::default(),
            },
            IlOp::Label(Label(0)),
            IlOp::byte(Byte::new(Instruction::RETURN)),
        ];
        return_convoy(&mut ops);
        assert!(ops.iter().any(|op| {
            matches!(op, IlOp::ConstReturnImm { imm: 0, .. })
                || matches!(op.instruction(), Some(Instruction::ConstReturnImm))
        }));
        assert_eq!(
            ops.iter()
                .filter(|op| {
                    matches!(
                        op,
                        IlOp::Jump {
                            kind: IlJumpKind::JumpIfMatch { .. },
                            ..
                        }
                    )
                })
                .count(),
            2
        );
    }

    #[test]
    fn return_convoy_skips_mixed_jump_if_match_and_jmp() {
        let mut ops = vec![
            IlOp::byte(Byte::new(Instruction::CONST).with_const_inline(0)),
            IlOp::Jump {
                kind: IlJumpKind::JumpIfMatch { tag: 1, arity: 0 },
                target: Label(0),
                loc: common::DebugLoc::unknown(),
                hint: Default::default(),
            },
            IlOp::byte(Byte::new(Instruction::CONST).with_const_inline(0)),
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(0),
                loc: common::DebugLoc::unknown(),
                hint: Default::default(),
            },
            IlOp::Label(Label(0)),
            IlOp::byte(Byte::new(Instruction::RETURN)),
        ];
        let before = ops.clone();
        return_convoy(&mut ops);
        assert!(ops == before);
    }

    #[test]
    fn return_convoy_skips_jump_if_match_unknown_join_sp() {
        // FfiInvoke poisons SP; JumpIfMatch diamond must refuse.
        let mut ops = vec![
            IlOp::byte(Byte::new(Instruction::FfiInvoke)),
            IlOp::byte(Byte::new(Instruction::CONST).with_const_inline(0)),
            IlOp::Jump {
                kind: IlJumpKind::JumpIfMatch { tag: 0, arity: 0 },
                target: Label(0),
                loc: common::DebugLoc::unknown(),
                hint: Default::default(),
            },
            IlOp::byte(Byte::new(Instruction::CONST).with_const_inline(0)),
            IlOp::Jump {
                kind: IlJumpKind::JumpIfMatch { tag: 1, arity: 0 },
                target: Label(0),
                loc: common::DebugLoc::unknown(),
                hint: Default::default(),
            },
            IlOp::Label(Label(0)),
            IlOp::byte(Byte::new(Instruction::RETURN)),
        ];
        let before = ops.clone();
        return_convoy(&mut ops);
        assert!(ops == before);
    }

    #[test]
    fn return_convoy_skips_disagreeing_consts_in_label_cluster() {
        // Ord-shaped: Label(join); Label(ret); RETURN with mixed CONST 0/1.
        let mut ops = vec![
            IlOp::byte(Byte::new(Instruction::CONST).with_const_inline(0)),
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(54),
                loc: common::DebugLoc::unknown(),
                hint: Default::default(),
            },
            IlOp::byte(Byte::new(Instruction::CONST).with_const_inline(1)),
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(54),
                loc: common::DebugLoc::unknown(),
                hint: Default::default(),
            },
            IlOp::byte(Byte::new(Instruction::CONST).with_const_inline(0)),
            IlOp::Label(Label(54)),
            IlOp::Label(Label(48)),
            IlOp::byte(Byte::new(Instruction::RETURN)),
        ];
        let before = ops.clone();
        return_convoy(&mut ops);
        assert!(ops == before);
    }

    #[test]
    fn return_convoy_fuses_agreeing_const_through_label_cluster() {
        let mut ops = vec![
            IlOp::byte(Byte::new(Instruction::CONST).with_const_inline(0)),
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(54),
                loc: common::DebugLoc::unknown(),
                hint: Default::default(),
            },
            IlOp::byte(Byte::new(Instruction::CONST).with_const_inline(0)),
            IlOp::Label(Label(54)),
            IlOp::Label(Label(48)),
            IlOp::byte(Byte::new(Instruction::RETURN)),
        ];
        return_convoy(&mut ops);
        assert!(ops.iter().any(|op| {
            matches!(op.instruction(), Some(Instruction::ConstReturnImm))
                || matches!(op, IlOp::ConstReturnImm { .. })
        }));
        assert!(ops.iter().any(|op| matches!(op, IlOp::Label(Label(54)))));
        assert!(ops.iter().any(|op| matches!(op, IlOp::Label(Label(48)))));
        assert!(
            !ops.iter().any(|op| is_insn(op, Instruction::CONST)),
            "producers should be stripped"
        );
    }

    #[test]
    fn return_convoy_fuses_agreeing_load_through_label_cluster() {
        let mut ops = vec![
            IlOp::byte(Byte::new(Instruction::LOAD).with_operand_u32(2)),
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(1),
                loc: common::DebugLoc::unknown(),
                hint: Default::default(),
            },
            IlOp::byte(Byte::new(Instruction::LOAD).with_operand_u32(2)),
            IlOp::Label(Label(1)),
            IlOp::Label(Label(9)),
            IlOp::byte(Byte::new(Instruction::RETURN)),
        ];
        return_convoy(&mut ops);
        assert!(ops.iter().any(|op| {
            matches!(op, IlOp::LoadReturnSlot { slot: 2, .. })
                || op.as_encode_byte().is_some_and(|b| {
                    *b.bytecode() == Instruction::LoadReturnSlot && b.operand_u32() == 2
                })
        }));
    }

    #[test]
    fn bin_join_convoy_fuses_agreeing_binop_to_bin_return() {
        let mut ops = vec![
            IlOp::byte(Byte::new(Instruction::ADD)),
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(0),
                loc: common::DebugLoc::unknown(),
                hint: Default::default(),
            },
            IlOp::byte(Byte::new(Instruction::ADD)),
            IlOp::Label(Label(0)),
            IlOp::byte(Byte::new(Instruction::RETURN)),
        ];
        bin_join_convoy(&mut ops);
        assert!(ops.iter().any(|op| {
            matches!(
                op,
                IlOp::BinReturn {
                    op: Instruction::ADD,
                    ..
                }
            ) || op.as_encode_byte().is_some_and(|b| {
                *b.bytecode() == Instruction::BinReturn
                    && b.bin_return_op() == Instruction::ADD as u8
            })
        }));
        assert!(
            !ops.iter().any(|op| is_insn(op, Instruction::ADD)),
            "plain ADDs should be stripped"
        );
        assert!(
            !ops.iter().any(|op| is_insn(op, Instruction::RETURN)),
            "RETURN should be replaced by BinReturn"
        );
    }

    #[test]
    fn bin_join_convoy_sinks_identical_bin_slot_slot() {
        let slot =
            Byte::new(Instruction::BinSlotSlot).with_bin_slot_slot(Instruction::ADD as u8, 0, 1);
        let mut ops = vec![
            IlOp::byte(slot),
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(0),
                loc: common::DebugLoc::unknown(),
                hint: Default::default(),
            },
            IlOp::byte(slot),
            IlOp::Label(Label(0)),
            IlOp::byte(Byte::new(Instruction::RETURN)),
        ];
        bin_join_convoy(&mut ops);
        let slot_count = ops
            .iter()
            .filter(|op| is_insn(op, Instruction::BinSlotSlot))
            .count();
        assert_eq!(slot_count, 1, "exactly one BinSlotSlot before RETURN");
        assert!(ops.iter().any(|op| is_insn(op, Instruction::RETURN)));
    }

    #[test]
    fn bin_join_convoy_sinks_identical_bin_slot_imm() {
        let imm =
            Byte::new(Instruction::BinSlotImm).with_bin_slot_imm(Instruction::ADD as u8, 0, 1);
        let mut ops = vec![
            IlOp::byte(imm),
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(0),
                loc: common::DebugLoc::unknown(),
                hint: Default::default(),
            },
            IlOp::byte(imm),
            IlOp::Label(Label(0)),
            IlOp::byte(Byte::new(Instruction::RETURN)),
        ];
        bin_join_convoy(&mut ops);
        let imm_count = ops
            .iter()
            .filter(|op| is_insn(op, Instruction::BinSlotImm))
            .count();
        assert_eq!(imm_count, 1, "exactly one BinSlotImm before RETURN");
        assert!(
            !ops.iter().any(|op| is_insn(op, Instruction::BinReturn)),
            "BinSlotImm must stay as slot tail, not BinReturn"
        );
        assert!(ops.iter().any(|op| is_insn(op, Instruction::RETURN)));
    }

    #[test]
    fn bin_join_convoy_skips_disagreeing_binops() {
        // Ord-shaped: Lt arm ends in LE, Gt arm in GT — must not convoy.
        let mut ops = vec![
            IlOp::byte(Byte::new(Instruction::LE)),
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(54),
                loc: common::DebugLoc::unknown(),
                hint: Default::default(),
            },
            IlOp::byte(Byte::new(Instruction::GT)),
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(54),
                loc: common::DebugLoc::unknown(),
                hint: Default::default(),
            },
            IlOp::byte(Byte::new(Instruction::EQ)),
            IlOp::Label(Label(54)),
            IlOp::Label(Label(48)),
            IlOp::byte(Byte::new(Instruction::RETURN)),
        ];
        let before = ops.clone();
        bin_join_convoy(&mut ops);
        assert!(ops == before);
    }

    #[test]
    fn bin_join_convoy_skips_conditional_jump_into_cluster() {
        let mut ops = vec![
            IlOp::byte(Byte::new(Instruction::ADD)),
            IlOp::Jump {
                kind: IlJumpKind::JumpIfFalse,
                target: Label(0),
                loc: common::DebugLoc::unknown(),
                hint: Default::default(),
            },
            IlOp::byte(Byte::new(Instruction::ADD)),
            IlOp::Label(Label(0)),
            IlOp::byte(Byte::new(Instruction::RETURN)),
        ];
        let before = ops.clone();
        bin_join_convoy(&mut ops);
        assert!(ops == before);
    }

    #[test]
    fn bin_join_convoy_fuses_agreeing_binop_via_jmpf() {
        let mut ops = vec![
            IlOp::byte(Byte::new(Instruction::CONST).with_const_inline(1)),
            IlOp::byte(Byte::new(Instruction::CONST).with_const_inline(2)),
            IlOp::byte(Byte::new(Instruction::ADD)),
            IlOp::byte(Byte::new(Instruction::CONST).with_const_inline(1)),
            IlOp::Jump {
                kind: IlJumpKind::JumpIfFalse,
                target: Label(0),
                loc: common::DebugLoc::unknown(),
                hint: Default::default(),
            },
            IlOp::Pop {
                loc: common::DebugLoc::unknown(),
            },
            IlOp::byte(Byte::new(Instruction::CONST).with_const_inline(1)),
            IlOp::byte(Byte::new(Instruction::CONST).with_const_inline(2)),
            IlOp::byte(Byte::new(Instruction::ADD)),
            IlOp::byte(Byte::new(Instruction::CONST).with_const_inline(1)),
            IlOp::Jump {
                kind: IlJumpKind::JumpIfFalse,
                target: Label(0),
                loc: common::DebugLoc::unknown(),
                hint: Default::default(),
            },
            IlOp::Label(Label(0)),
            IlOp::byte(Byte::new(Instruction::RETURN)),
        ];
        bin_join_convoy(&mut ops);
        assert!(ops.iter().any(|op| {
            matches!(
                op,
                IlOp::BinReturn {
                    op: Instruction::ADD,
                    ..
                }
            ) || is_insn(op, Instruction::BinReturn)
        }));
    }

    #[test]
    fn bin_join_convoy_fuses_agreeing_binop_via_jump_if_match() {
        let imm =
            Byte::new(Instruction::BinSlotImm).with_bin_slot_imm(Instruction::ADD as u8, 0, 1);
        let mut ops = vec![
            IlOp::byte(imm),
            IlOp::Jump {
                kind: IlJumpKind::JumpIfMatch { tag: 0, arity: 0 },
                target: Label(0),
                loc: common::DebugLoc::unknown(),
                hint: Default::default(),
            },
            IlOp::byte(imm),
            IlOp::Jump {
                kind: IlJumpKind::JumpIfMatch { tag: 1, arity: 0 },
                target: Label(0),
                loc: common::DebugLoc::unknown(),
                hint: Default::default(),
            },
            IlOp::Label(Label(0)),
            IlOp::byte(Byte::new(Instruction::RETURN)),
        ];
        bin_join_convoy(&mut ops);
        let imm_count = ops
            .iter()
            .filter(|op| is_insn(op, Instruction::BinSlotImm))
            .count();
        assert_eq!(imm_count, 1, "identical BinSlotImm sunk once before RETURN");
    }

    #[test]
    fn bin_join_convoy_skips_mixed_jump_if_match_and_jmp() {
        let mut ops = vec![
            IlOp::byte(Byte::new(Instruction::ADD)),
            IlOp::Jump {
                kind: IlJumpKind::JumpIfMatch { tag: 1, arity: 0 },
                target: Label(0),
                loc: common::DebugLoc::unknown(),
                hint: Default::default(),
            },
            IlOp::byte(Byte::new(Instruction::ADD)),
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(0),
                loc: common::DebugLoc::unknown(),
                hint: Default::default(),
            },
            IlOp::Label(Label(0)),
            IlOp::byte(Byte::new(Instruction::RETURN)),
        ];
        let before = ops.clone();
        bin_join_convoy(&mut ops);
        assert!(ops == before);
    }

    #[test]
    fn bin_join_convoy_fuses_through_label_cluster() {
        let mut ops = vec![
            IlOp::byte(Byte::new(Instruction::SUB)),
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(1),
                loc: common::DebugLoc::unknown(),
                hint: Default::default(),
            },
            IlOp::byte(Byte::new(Instruction::SUB)),
            IlOp::Label(Label(1)),
            IlOp::Label(Label(9)),
            IlOp::byte(Byte::new(Instruction::RETURN)),
        ];
        bin_join_convoy(&mut ops);
        assert!(ops.iter().any(|op| {
            matches!(
                op,
                IlOp::BinReturn {
                    op: Instruction::SUB,
                    ..
                }
            ) || op.as_encode_byte().is_some_and(|b| {
                *b.bytecode() == Instruction::BinReturn
                    && b.bin_return_op() == Instruction::SUB as u8
            })
        }));
        assert!(ops.iter().any(|op| matches!(op, IlOp::Label(Label(1)))));
        assert!(ops.iter().any(|op| matches!(op, IlOp::Label(Label(9)))));
    }

    #[test]
    fn return_convoy_accepts_typed_const_ops() {
        let mut ops = vec![
            IlOp::Const {
                imm: 0,
                loc: common::DebugLoc::unknown(),
            },
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(0),
                loc: common::DebugLoc::unknown(),
                hint: Default::default(),
            },
            IlOp::Const {
                imm: 0,
                loc: common::DebugLoc::unknown(),
            },
            IlOp::Label(Label(0)),
            IlOp::Return {
                loc: common::DebugLoc::unknown(), ret_words: 1,},
        ];
        return_convoy(&mut ops);
        assert!(
            ops.iter()
                .any(|op| matches!(op, IlOp::ConstReturnImm { imm: 0, .. }))
        );
        assert!(!ops.iter().any(|op| matches!(op, IlOp::Const { .. })));
    }

    #[test]
    fn return_convoy_accepts_typed_load_ops() {
        let mut ops = vec![
            IlOp::Load {
                slot: 3,
                loc: common::DebugLoc::unknown(),
            },
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(0),
                loc: common::DebugLoc::unknown(),
                hint: Default::default(),
            },
            IlOp::Load {
                slot: 3,
                loc: common::DebugLoc::unknown(),
            },
            IlOp::Label(Label(0)),
            IlOp::Return {
                loc: common::DebugLoc::unknown(), ret_words: 1,},
        ];
        return_convoy(&mut ops);
        assert!(
            ops.iter()
                .any(|op| matches!(op, IlOp::LoadReturnSlot { slot: 3, .. }))
        );
        assert!(!ops.iter().any(|op| matches!(op, IlOp::Load { .. })));
    }

    #[test]
    fn bin_join_convoy_accepts_typed_bin_ops() {
        let mut ops = vec![
            IlOp::Bin {
                op: Instruction::MUL,
                loc: common::DebugLoc::unknown(),
            },
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(0),
                loc: common::DebugLoc::unknown(),
                hint: Default::default(),
            },
            IlOp::Bin {
                op: Instruction::MUL,
                loc: common::DebugLoc::unknown(),
            },
            IlOp::Label(Label(0)),
            IlOp::Return {
                loc: common::DebugLoc::unknown(), ret_words: 1,},
        ];
        bin_join_convoy(&mut ops);
        assert!(ops.iter().any(|op| matches!(
            op,
            IlOp::BinReturn {
                op: Instruction::MUL,
                ..
            }
        )));
        assert!(!ops.iter().any(|op| matches!(op, IlOp::Bin { .. })));
        assert!(!ops.iter().any(|op| matches!(op, IlOp::Return { .. })));
    }

    #[test]
    fn bin_join_convoy_sinks_typed_bin_slot_imm() {
        let imm = IlOp::BinSlotImm {
            op: Instruction::ADD as u8,
            slot: 0,
            imm: 1,
            loc: common::DebugLoc::unknown(),
        };
        let mut ops = vec![
            imm.clone(),
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(0),
                loc: common::DebugLoc::unknown(),
                hint: Default::default(),
            },
            imm,
            IlOp::Label(Label(0)),
            IlOp::Return {
                loc: common::DebugLoc::unknown(), ret_words: 1,},
        ];
        bin_join_convoy(&mut ops);
        let imm_count = ops
            .iter()
            .filter(|op| matches!(op, IlOp::BinSlotImm { .. }))
            .count();
        assert_eq!(imm_count, 1);
        assert!(ops.iter().any(|op| matches!(op, IlOp::Return { .. })));
    }

    #[test]
    fn stack_dce_removes_typed_dup_pop() {
        let mut ops = vec![
            IlOp::Dup {
                loc: common::DebugLoc::unknown(),
            },
            IlOp::Pop {
                loc: common::DebugLoc::unknown(),
            },
            IlOp::Halt {
                loc: common::DebugLoc::unknown(),
            },
        ];
        stack_dce(&mut ops);
        assert_eq!(ops.len(), 1);
        assert!(matches!(ops[0], IlOp::Halt { .. }));
    }

    #[test]
    fn stack_dce_drops_discarded_enum_construction() {
        let loc = common::DebugLoc::unknown();
        let mut ops = vec![
            IlOp::Const { imm: 1, loc },
            IlOp::Const { imm: 2, loc },
            IlOp::MakeEnum {
                tag: 3,
                arity: 2,
                loc,
            },
            IlOp::Pop { loc },
            IlOp::Return { loc, ret_words: 1},
        ];

        stack_dce(&mut ops);

        assert_eq!(ops.len(), 1);
        assert!(matches!(ops[0], IlOp::Return { .. }));
    }

    #[test]
    fn stack_dce_unwraps_unary_enum_without_heap_construction() {
        let loc = common::DebugLoc::unknown();
        let mut ops = vec![
            IlOp::Const { imm: 42, loc },
            IlOp::MakeEnum {
                tag: 3,
                arity: 1,
                loc,
            },
            IlOp::byte(Byte::new(Instruction::Unpack).with_operand_u32(1)),
            IlOp::Return { loc, ret_words: 1},
        ];

        stack_dce(&mut ops);

        assert_eq!(ops.len(), 2);
        assert!(matches!(ops[0], IlOp::Const { imm: 42, .. }));
        assert!(matches!(ops[1], IlOp::Return { .. }));
    }

    #[test]
    fn stack_dce_reads_unary_enum_field_without_heap_construction() {
        let loc = common::DebugLoc::unknown();
        let mut ops = vec![
            IlOp::Const { imm: 42, loc },
            IlOp::MakeEnum {
                tag: 3,
                arity: 1,
                loc,
            },
            IlOp::LoadField { index: 0, loc },
            IlOp::Return { loc, ret_words: 1},
        ];

        stack_dce(&mut ops);

        assert_eq!(ops.len(), 2);
        assert!(matches!(ops[0], IlOp::Const { imm: 42, .. }));
        assert!(matches!(ops[1], IlOp::Return { .. }));
    }

    #[test]
    fn stack_dce_threads_direct_constructor_match() {
        let loc = common::DebugLoc::unknown();
        let target = crate::il::op::Label(7);
        let mut ops = vec![
            IlOp::Const { imm: 42, loc },
            IlOp::MakeEnum {
                tag: 1,
                arity: 1,
                loc,
            },
            IlOp::Jump {
                kind: crate::il::op::IlJumpKind::JumpIfMatch { tag: 1, arity: 1 },
                target,
                loc,
                hint: Default::default(),
            },
            IlOp::Return { loc, ret_words: 1},
        ];

        stack_dce(&mut ops);

        assert_eq!(ops.len(), 3);
        assert!(matches!(ops[0], IlOp::Const { imm: 42, .. }));
        assert!(matches!(
            ops[1],
            IlOp::Jump {
                kind: crate::il::op::IlJumpKind::Unconditional,
                target: crate::il::op::Label(7),
                ..
            }
        ));
    }

    #[test]
    fn mem_fwd_store_pop_load_becomes_dup_store() {
        // Need height before StorePop > slot+1 (cursor-safe Dup;Store).
        let loc = common::DebugLoc::unknown();
        let mut ops = vec![
            IlOp::Const { imm: 7, loc },
            IlOp::StorePop { slot: 3, loc },
            IlOp::Load { slot: 3, loc },
            IlOp::Return { loc, ret_words: 1},
        ];
        mem_fwd(&mut ops, 5);
        assert!(matches!(ops[1], IlOp::Dup { .. }));
        assert!(matches!(ops[2], IlOp::StorePop { slot: 3, .. }));
    }

    #[test]
    fn copy_prop_replaces_load_and_cursor_safe_dead_store() {
        let loc = common::DebugLoc::unknown();
        let mut ops = vec![
            IlOp::Const { imm: 7, loc },
            IlOp::StorePop { slot: 1, loc },
            IlOp::Load { slot: 1, loc },
            IlOp::Return { loc, ret_words: 1},
        ];

        copy_prop(&mut ops, 3);
        dead_store_at(&mut ops, 3);

        assert_eq!(ops.len(), 2);
        assert!(matches!(ops[0], IlOp::Const { imm: 7, .. }));
        assert!(matches!(ops[1], IlOp::Return { .. }));
    }

    #[test]
    fn copy_prop_keeps_store_when_cursor_floor_is_needed() {
        let loc = common::DebugLoc::unknown();
        let mut ops = vec![
            IlOp::Const { imm: 7, loc },
            IlOp::StorePop { slot: 5, loc },
            IlOp::Load { slot: 5, loc },
            IlOp::Return { loc, ret_words: 1},
        ];

        copy_prop(&mut ops, 0);
        dead_store_at(&mut ops, 0);

        assert!(
            ops.iter()
                .any(|op| matches!(op, IlOp::StorePop { slot: 5, .. }))
        );
        assert!(matches!(ops[2], IlOp::Const { imm: 7, .. }));
    }

    #[test]
    fn copy_prop_invalidates_bindings_when_dependencies_are_stored() {
        let loc = common::DebugLoc::unknown();
        let mut ops = vec![
            IlOp::Load { slot: 0, loc },
            IlOp::StorePop { slot: 2, loc },
            IlOp::Const { imm: 9, loc },
            IlOp::StorePop { slot: 0, loc },
            IlOp::Load { slot: 2, loc },
            IlOp::Return { loc, ret_words: 1},
        ];

        copy_prop(&mut ops, 3);

        assert!(matches!(ops[4], IlOp::Load { slot: 2, .. }));
    }

    #[test]
    fn copy_prop_refuses_control_flow_boundaries() {
        let loc = common::DebugLoc::unknown();
        let mut ops = vec![
            IlOp::Const { imm: 7, loc },
            IlOp::StorePop { slot: 1, loc },
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(0),
                loc,
                hint: Default::default(),
            },
            IlOp::Label(Label(0)),
            IlOp::Load { slot: 1, loc },
            IlOp::Return { loc, ret_words: 1},
        ];

        copy_prop(&mut ops, 3);

        assert!(matches!(ops[4], IlOp::Load { slot: 1, .. }));
    }

    #[test]
    fn copy_prop_refuses_get_field_shape_sensitive_load() {
        let loc = common::DebugLoc::unknown();
        let mut ops = vec![
            IlOp::Const { imm: 7, loc },
            IlOp::StorePop { slot: 1, loc },
            IlOp::Load { slot: 1, loc },
            IlOp::GetField { loc },
            IlOp::Return { loc, ret_words: 1},
        ];

        copy_prop(&mut ops, 3);

        assert!(matches!(ops[2], IlOp::Load { slot: 1, .. }));
    }

    #[test]
    fn copy_prop_refuses_make_array_after_pure_load_chain() {
        let loc = common::DebugLoc::unknown();
        let mut ops = vec![
            IlOp::Const { imm: 7, loc },
            IlOp::StorePop { slot: 1, loc },
            IlOp::Load { slot: 1, loc },
            IlOp::Const { imm: 2, loc },
            IlOp::MakeArray { arity: 2, loc },
            IlOp::Return { loc, ret_words: 1},
        ];

        copy_prop(&mut ops, 3);

        assert!(matches!(ops[2], IlOp::Load { slot: 1, .. }));
    }

    #[test]
    fn copy_prop_forwards_bin_slot_imm_producer() {
        let loc = common::DebugLoc::unknown();
        let mut ops = vec![
            IlOp::BinSlotImm {
                op: Instruction::ADD as u8,
                slot: 0,
                imm: 1,
                loc,
            },
            IlOp::StorePop { slot: 2, loc },
            IlOp::Load { slot: 2, loc },
            IlOp::Return { loc, ret_words: 1},
        ];

        copy_prop(&mut ops, 3);

        assert!(matches!(
            ops[2],
            IlOp::BinSlotImm {
                slot: 0,
                imm: 1,
                ..
            }
        ));
    }

    #[test]
    fn copy_prop_forwards_bin_slot_slot_producer() {
        let loc = common::DebugLoc::unknown();
        let mut ops = vec![
            IlOp::BinSlotSlot {
                op: Instruction::ADD as u8,
                a: 0,
                b: 1,
                loc,
            },
            IlOp::StorePop { slot: 2, loc },
            IlOp::Load { slot: 2, loc },
            IlOp::Return { loc, ret_words: 1},
        ];

        copy_prop(&mut ops, 3);

        assert!(matches!(
            ops[2],
            IlOp::BinSlotSlot {
                a: 0,
                b: 1,
                ..
            }
        ));
    }

    #[test]
    fn copy_prop_forwards_string_producer_and_drops_copy_store() {
        let loc = common::DebugLoc::unknown();
        let mut ops = vec![
            IlOp::String { idx: 1, loc },
            IlOp::StorePop { slot: 1, loc },
            IlOp::Load { slot: 1, loc },
            IlOp::Return { loc, ret_words: 1},
        ];

        copy_prop(&mut ops, 2);
        dead_store_at(&mut ops, 2);

        assert_eq!(ops.len(), 2);
        assert!(matches!(ops[0], IlOp::String { idx: 1, .. }));
        assert!(matches!(ops[1], IlOp::Return { .. }));
    }

    #[test]
    fn dead_store_removes_unused_bin_slot_producer_when_cursor_allows() {
        let loc = common::DebugLoc::unknown();
        let mut ops = vec![
            IlOp::BinSlotImm {
                op: Instruction::ADD as u8,
                slot: 0,
                imm: 1,
                loc,
            },
            IlOp::StorePop { slot: 2, loc },
            IlOp::Return { loc, ret_words: 1},
        ];

        dead_store_at(&mut ops, 3);

        assert_eq!(ops.len(), 1);
        assert!(matches!(ops[0], IlOp::Return { .. }));
    }

    #[test]
    fn dead_store_removes_unused_bin_slot_slot_producer_when_cursor_allows() {
        let loc = common::DebugLoc::unknown();
        let mut ops = vec![
            IlOp::BinSlotSlot {
                op: Instruction::SUB as u8,
                a: 0,
                b: 1,
                loc,
            },
            IlOp::StorePop { slot: 2, loc },
            IlOp::Return { loc, ret_words: 1},
        ];

        dead_store_at(&mut ops, 3);

        assert_eq!(ops.len(), 1);
        assert!(matches!(ops[0], IlOp::Return { .. }));
    }

    #[test]
    fn copy_prop_skips_self_alias_store_binding() {
        let loc = common::DebugLoc::unknown();
        let mut ops = vec![
            IlOp::Load { slot: 1, loc },
            IlOp::StorePop { slot: 1, loc },
            IlOp::Load { slot: 1, loc },
            IlOp::Return { loc, ret_words: 1},
        ];

        copy_prop(&mut ops, 3);

        assert!(matches!(ops[2], IlOp::Load { slot: 1, .. }));
    }

    #[test]
    fn copy_prop_clears_bindings_across_host_invoke() {
        let loc = common::DebugLoc::unknown();
        let mut ops = vec![
            IlOp::Const { imm: 7, loc },
            IlOp::StorePop { slot: 1, loc },
            IlOp::HostInvoke { arity: 0, layout: 0, loc },
            IlOp::Load { slot: 1, loc },
            IlOp::Return { loc, ret_words: 1},
        ];

        copy_prop(&mut ops, 3);

        assert!(matches!(ops[3], IlOp::Load { slot: 1, .. }));
    }

    #[test]
    fn dead_store_keeps_store_before_opaque_byte_barrier() {
        let loc = common::DebugLoc::unknown();
        let mut ops = vec![
            IlOp::Const { imm: 1, loc },
            IlOp::StorePop { slot: 2, loc },
            IlOp::Byte {
                byte: Byte::new(Instruction::FfiInvoke).with_operand_u32(0),
                loc,
            },
            IlOp::Return { loc, ret_words: 1},
        ];

        dead_store_at(&mut ops, 4);

        assert!(
            ops.iter()
                .any(|op| matches!(op, IlOp::StorePop { slot: 2, .. }))
        );
    }

    #[test]
    fn dead_store_drops_dup_store_when_slot_unused() {
        let mut ops = vec![
            IlOp::Const {
                imm: 1,
                loc: common::DebugLoc::unknown(),
            },
            IlOp::Dup {
                loc: common::DebugLoc::unknown(),
            },
            IlOp::StorePop {
                slot: 9,
                loc: common::DebugLoc::unknown(),
            },
            IlOp::Return {
                loc: common::DebugLoc::unknown(), ret_words: 1,},
        ];
        dead_store_at(&mut ops, 10);
        assert!(!ops.iter().any(|op| matches!(op, IlOp::StorePop { .. })));
        assert!(!ops.iter().any(|op| matches!(op, IlOp::Dup { .. })));
        assert!(matches!(ops[0], IlOp::Const { imm: 1, .. }));
    }

    #[test]
    fn dead_store_keeps_store_when_slot_loaded() {
        let mut ops = vec![
            IlOp::Const {
                imm: 1,
                loc: common::DebugLoc::unknown(),
            },
            IlOp::StorePop {
                slot: 9,
                loc: common::DebugLoc::unknown(),
            },
            IlOp::Load {
                slot: 9,
                loc: common::DebugLoc::unknown(),
            },
            IlOp::Return {
                loc: common::DebugLoc::unknown(), ret_words: 1,},
        ];
        dead_store(&mut ops);
        assert!(
            ops.iter()
                .any(|op| matches!(op, IlOp::StorePop { slot: 9, .. }))
        );
    }

    #[test]
    fn mem_fwd_skips_mismatched_slots() {
        let mut ops = vec![
            IlOp::StorePop {
                slot: 1,
                loc: common::DebugLoc::unknown(),
            },
            IlOp::Load {
                slot: 2,
                loc: common::DebugLoc::unknown(),
            },
            IlOp::Return {
                loc: common::DebugLoc::unknown(), ret_words: 1,},
        ];
        let before = ops.clone();
        mem_fwd(&mut ops, 0);
        assert!(ops == before);
    }

    #[test]
    fn dead_store_drops_const_pool_store_when_unused() {
        let mut ops = vec![
            IlOp::ConstPool {
                idx: 4,
                loc: common::DebugLoc::unknown(),
            },
            IlOp::StorePop {
                slot: 8,
                loc: common::DebugLoc::unknown(),
            },
            IlOp::Return {
                loc: common::DebugLoc::unknown(), ret_words: 1,},
        ];
        dead_store_at(&mut ops, 9);
        assert!(!ops.iter().any(|op| matches!(op, IlOp::StorePop { .. })));
        assert!(!ops.iter().any(|op| matches!(op, IlOp::ConstPool { .. })));
        assert!(matches!(ops[0], IlOp::Return { .. }));
    }

    #[test]
    fn dead_store_keeps_loop_carried_store_before_jump() {
        let mut ops = vec![
            IlOp::Load {
                slot: 0,
                loc: common::DebugLoc::unknown(),
            },
            IlOp::Const {
                imm: 1,
                loc: common::DebugLoc::unknown(),
            },
            IlOp::Bin {
                op: Instruction::ADD,
                loc: common::DebugLoc::unknown(),
            },
            IlOp::StorePop {
                slot: 0,
                loc: common::DebugLoc::unknown(),
            },
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(1),
                loc: common::DebugLoc::unknown(),
                hint: Default::default(),
            },
            IlOp::Label(Label(1)),
            IlOp::Return {
                loc: common::DebugLoc::unknown(), ret_words: 1,},
        ];
        dead_store(&mut ops);
        assert!(
            ops.iter()
                .any(|op| matches!(op, IlOp::StorePop { slot: 0, .. }))
        );
    }

    #[test]
    fn dead_store_drops_assignment_only_local_across_jump() {
        // Slot 5 is stored then control jumps, but nothing ever loads it.
        let mut ops = vec![
            IlOp::Const {
                imm: 42,
                loc: common::DebugLoc::unknown(),
            },
            IlOp::StorePop {
                slot: 5,
                loc: common::DebugLoc::unknown(),
            },
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(1),
                loc: common::DebugLoc::unknown(),
                hint: Default::default(),
            },
            IlOp::Label(Label(1)),
            IlOp::Const {
                imm: 0,
                loc: common::DebugLoc::unknown(),
            },
            IlOp::Return {
                loc: common::DebugLoc::unknown(), ret_words: 1,},
        ];
        dead_store_at(&mut ops, 6);
        assert!(
            !ops.iter().any(|op| matches!(op, IlOp::StorePop { slot: 5, .. })),
            "assignment-only slot should die across Jump"
        );
        assert!(
            !ops.iter().any(|op| matches!(op, IlOp::Const { imm: 42, .. })),
            "dead producer should be removed with the store"
        );
    }

    #[test]
    fn dead_store_drops_assignment_only_local_across_label() {
        // Same unread-slot rule, but the next control edge is a Label join.
        let mut ops = vec![
            IlOp::Const {
                imm: 7,
                loc: common::DebugLoc::unknown(),
            },
            IlOp::StorePop {
                slot: 3,
                loc: common::DebugLoc::unknown(),
            },
            IlOp::Label(Label(2)),
            IlOp::Const {
                imm: 0,
                loc: common::DebugLoc::unknown(),
            },
            IlOp::Return {
                loc: common::DebugLoc::unknown(), ret_words: 1,},
        ];
        dead_store_at(&mut ops, 4);
        assert!(
            !ops.iter().any(|op| matches!(op, IlOp::StorePop { slot: 3, .. })),
            "assignment-only slot should die across Label"
        );
    }

    #[test]
    fn dead_store_keeps_store_when_load_follows_label() {
        // A later Load of the slot (after a Label) means Jump/Label must keep it.
        let mut ops = vec![
            IlOp::Const {
                imm: 9,
                loc: common::DebugLoc::unknown(),
            },
            IlOp::StorePop {
                slot: 4,
                loc: common::DebugLoc::unknown(),
            },
            IlOp::Label(Label(3)),
            IlOp::Load {
                slot: 4,
                loc: common::DebugLoc::unknown(),
            },
            IlOp::Return {
                loc: common::DebugLoc::unknown(), ret_words: 1,},
        ];
        dead_store_at(&mut ops, 5);
        assert!(
            ops.iter()
                .any(|op| matches!(op, IlOp::StorePop { slot: 4, .. })),
            "slot read after Label must keep the store"
        );
    }

    #[test]
    fn dead_store_keeps_store_when_bin_slot_imm_uses_slot() {
        let mut ops = vec![
            IlOp::Const {
                imm: 1,
                loc: common::DebugLoc::unknown(),
            },
            IlOp::StorePop {
                slot: 3,
                loc: common::DebugLoc::unknown(),
            },
            IlOp::BinSlotImm {
                op: Instruction::ADD as u8,
                slot: 3,
                imm: 1,
                loc: common::DebugLoc::unknown(),
            },
            IlOp::Return {
                loc: common::DebugLoc::unknown(), ret_words: 1,},
        ];
        dead_store(&mut ops);
        assert!(
            ops.iter()
                .any(|op| matches!(op, IlOp::StorePop { slot: 3, .. }))
        );
    }

    #[test]
    fn mem_fwd_then_dead_store_via_optimize() {
        // StorePop;Load same slot → Dup;StorePop (needs h > slot+1), then dead.
        let loc = common::DebugLoc::unknown();
        let mut ops = vec![
            IlOp::Const { imm: 5, loc },
            IlOp::StorePop { slot: 1, loc },
            IlOp::Load { slot: 1, loc },
            IlOp::Return { loc, ret_words: 1},
        ];
        optimize_at(
            &mut ops,
            &OptimizeOptions {
                jump_thread: false,
                dead_block: false,
                stack_dce: false,
                mem_fwd: true,
                copy_prop: true,
                slot_promote: false,
                tos_carry: false,
                canon: false,
                cast_spill: false,
                algebraic: false,
                instcombine: false,
                dom_check: false,
                licm: false,
                loop_bounds: false,
                return_convoy: false,
                clone_shared_return: false,
                bin_join_convoy: false,
                multi_op_join_convoy: false,
                invert_guard_branch: false,
                slot_promote_tell: false,
                seek_back_edge: false,
                loop_unroll: false,
                loop_unroll_factor: 8,
                invariant_store_elim: false,
                ssa_gvn: false,
                escape_analysis: false,
                branch_optimization: false,
                block_reordering: false,
                iterative_optimization: false,
                max_optimization_iterations: 10,
                collect_stats: false,
                pure_call_ctx: None,
            },
            3,
            &mut Vec::new(),
        );
        assert!(!ops.iter().any(|op| matches!(op, IlOp::StorePop { .. })));
        assert!(!ops.iter().any(|op| matches!(op, IlOp::Load { .. })));
        assert!(matches!(ops[0], IlOp::Const { imm: 5, .. }));
        assert!(matches!(ops[1], IlOp::Return { .. }));
    }

    fn load_const_add_suffix() -> Vec<IlOp> {
        vec![
            IlOp::Load {
                slot: 0,
                loc: common::DebugLoc::unknown(),
            },
            IlOp::Const {
                imm: 1,
                loc: common::DebugLoc::unknown(),
            },
            IlOp::Bin {
                op: Instruction::ADD,
                loc: common::DebugLoc::unknown(),
            },
        ]
    }

    #[test]
    fn multi_op_join_convoy_sinks_identical_suffix() {
        // Diamond: both arms end with Load;Const;ADD then join+RETURN.
        let suf = load_const_add_suffix();
        let mut ops = vec![
            IlOp::Const {
                imm: 0,
                loc: common::DebugLoc::unknown(),
            },
            IlOp::Jump {
                kind: IlJumpKind::JumpIfFalse,
                target: Label(1),
                loc: common::DebugLoc::unknown(),
                hint: Default::default(),
            },
        ];
        ops.extend(suf.clone());
        ops.push(IlOp::Jump {
            kind: IlJumpKind::Unconditional,
            target: Label(0),
            loc: common::DebugLoc::unknown(),
            hint: Default::default(),
        });
        ops.push(IlOp::Label(Label(1)));
        ops.extend(suf);
        ops.push(IlOp::Label(Label(0)));
        ops.push(IlOp::Return {
            loc: common::DebugLoc::unknown(), ret_words: 1,});

        multi_op_join_convoy(&mut ops);

        let load_count = ops
            .iter()
            .filter(|op| matches!(op, IlOp::Load { .. }))
            .count();
        let add_count = ops
            .iter()
            .filter(|op| {
                matches!(
                    op,
                    IlOp::Bin {
                        op: Instruction::ADD,
                        ..
                    }
                )
            })
            .count();
        assert_eq!(load_count, 1, "suffix should appear once after join");
        assert_eq!(add_count, 1);
        assert!(ops.iter().any(|op| matches!(op, IlOp::Label(Label(0)))));
        assert!(ops.iter().any(|op| matches!(op, IlOp::Return { .. })));
        assert!(ops.iter().any(|op| matches!(
            op,
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(0),
                ..
            }
        )));
    }

    #[test]
    fn multi_op_join_convoy_skips_disagreeing_suffixes() {
        // Ord-shaped: one arm LOAD;CONST 0;ADD, other LOAD;CONST 1;ADD.
        let mut ops = vec![
            IlOp::Load {
                slot: 0,
                loc: common::DebugLoc::unknown(),
            },
            IlOp::Const {
                imm: 0,
                loc: common::DebugLoc::unknown(),
            },
            IlOp::Bin {
                op: Instruction::ADD,
                loc: common::DebugLoc::unknown(),
            },
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(54),
                loc: common::DebugLoc::unknown(),
                hint: Default::default(),
            },
            IlOp::Load {
                slot: 0,
                loc: common::DebugLoc::unknown(),
            },
            IlOp::Const {
                imm: 1,
                loc: common::DebugLoc::unknown(),
            },
            IlOp::Bin {
                op: Instruction::ADD,
                loc: common::DebugLoc::unknown(),
            },
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(54),
                loc: common::DebugLoc::unknown(),
                hint: Default::default(),
            },
            IlOp::Load {
                slot: 0,
                loc: common::DebugLoc::unknown(),
            },
            IlOp::Const {
                imm: 0,
                loc: common::DebugLoc::unknown(),
            },
            IlOp::Bin {
                op: Instruction::ADD,
                loc: common::DebugLoc::unknown(),
            },
            IlOp::Label(Label(54)),
            IlOp::Label(Label(48)),
            IlOp::Return {
                loc: common::DebugLoc::unknown(), ret_words: 1,},
        ];
        let before = ops.clone();
        multi_op_join_convoy(&mut ops);
        assert!(ops == before);
    }

    #[test]
    fn multi_op_join_convoy_skips_jmpf_fallthrough_unknown_sp() {
        // JMPF + fall-through identical S: JMPF is −1 vs fall-through 0 → join
        // SP Unknown → refuse (fail closed).
        let suf = load_const_add_suffix();
        let mut ops = Vec::new();
        ops.extend(suf.clone());
        ops.push(IlOp::Jump {
            kind: IlJumpKind::JumpIfFalse,
            target: Label(0),
            loc: common::DebugLoc::unknown(),
            hint: Default::default(),
        });
        ops.extend(suf);
        ops.push(IlOp::Label(Label(0)));
        ops.push(IlOp::Return {
            loc: common::DebugLoc::unknown(), ret_words: 1,});
        let before = ops.clone();
        multi_op_join_convoy(&mut ops);
        assert!(ops == before);
    }

    #[test]
    fn multi_op_join_convoy_sinks_identical_suffix_via_jmpf() {
        // Two arms: S; cond; JMPF Ljoin — cond is an independent push (not fed by S).
        let suf = load_const_add_suffix();
        let cond = IlOp::Const {
            imm: 1,
            loc: common::DebugLoc::unknown(),
        };
        let mut ops = Vec::new();
        ops.extend(suf.clone());
        ops.push(cond.clone());
        ops.push(IlOp::Jump {
            kind: IlJumpKind::JumpIfFalse,
            target: Label(0),
            loc: common::DebugLoc::unknown(),
            hint: Default::default(),
        });
        // Pop value left under cond so second arm matches join SP (return_convoy shape).
        ops.push(IlOp::Pop {
            loc: common::DebugLoc::unknown(),
        });
        ops.extend(suf);
        ops.push(cond);
        ops.push(IlOp::Jump {
            kind: IlJumpKind::JumpIfFalse,
            target: Label(0),
            loc: common::DebugLoc::unknown(),
            hint: Default::default(),
        });
        ops.push(IlOp::Label(Label(0)));
        ops.push(IlOp::Return {
            loc: common::DebugLoc::unknown(), ret_words: 1,});

        multi_op_join_convoy(&mut ops);

        let load_count = ops
            .iter()
            .filter(|op| matches!(op, IlOp::Load { .. }))
            .count();
        let add_count = ops
            .iter()
            .filter(|op| {
                matches!(
                    op,
                    IlOp::Bin {
                        op: Instruction::ADD,
                        ..
                    }
                )
            })
            .count();
        assert_eq!(load_count, 1, "suffix should appear once after join");
        assert_eq!(add_count, 1);
        let jmpf_count = ops
            .iter()
            .filter(|op| {
                matches!(
                    op,
                    IlOp::Jump {
                        kind: IlJumpKind::JumpIfFalse,
                        ..
                    }
                )
            })
            .count();
        assert_eq!(jmpf_count, 2, "JMPF ops kept; only S stripped");
        assert!(ops.iter().any(|op| matches!(op, IlOp::Return { .. })));
    }

    /// `STORE; LOAD; CONST; EQ; JMPF` must not sink — EQ consumes the compare
    /// operands (parse_url `sep_at == 999999` hang).
    #[test]
    fn multi_op_join_convoy_refuses_eq_fed_jmpf_suffix() {
        let loc = common::DebugLoc::unknown();
        let mut ops = vec![
            IlOp::StorePop { slot: 3, loc },
            IlOp::Load { slot: 3, loc },
            IlOp::Const { imm: 999999, loc },
            IlOp::Bin {
                op: Instruction::EQ,
                loc,
            },
            IlOp::Jump {
                kind: IlJumpKind::JumpIfFalse,
                target: Label(0),
                loc,
                hint: Default::default(),
            },
            IlOp::Const { imm: 1, loc },
            IlOp::Return { loc, ret_words: 1},
            IlOp::Label(Label(0)),
            IlOp::Load { slot: 0, loc },
            IlOp::Return { loc, ret_words: 1},
        ];
        let before = ops.clone();
        multi_op_join_convoy(&mut ops);
        assert!(ops == before, "must not sink STORE/LOAD/CONST past EQ;JMPF");
    }

    #[test]
    fn multi_op_join_convoy_sinks_identical_suffix_via_jmpt() {
        let suf = load_const_add_suffix();
        let cond = IlOp::Const {
            imm: 1,
            loc: common::DebugLoc::unknown(),
        };
        let mut ops = Vec::new();
        ops.extend(suf.clone());
        ops.push(cond.clone());
        ops.push(IlOp::Jump {
            kind: IlJumpKind::JumpIfTrue,
            target: Label(0),
            loc: common::DebugLoc::unknown(),
            hint: Default::default(),
        });
        ops.push(IlOp::Pop {
            loc: common::DebugLoc::unknown(),
        });
        ops.extend(suf);
        ops.push(cond);
        ops.push(IlOp::Jump {
            kind: IlJumpKind::JumpIfTrue,
            target: Label(0),
            loc: common::DebugLoc::unknown(),
            hint: Default::default(),
        });
        ops.push(IlOp::Label(Label(0)));
        ops.push(IlOp::StorePop {
            slot: 2,
            loc: common::DebugLoc::unknown(),
        });

        multi_op_join_convoy(&mut ops);

        let load_count = ops
            .iter()
            .filter(|op| matches!(op, IlOp::Load { .. }))
            .count();
        assert_eq!(load_count, 1);
        let store_idx = ops
            .iter()
            .position(|op| matches!(op, IlOp::StorePop { slot: 2, .. }))
            .expect("StorePop kept");
        let add_idx = ops
            .iter()
            .position(|op| {
                matches!(
                    op,
                    IlOp::Bin {
                        op: Instruction::ADD,
                        ..
                    }
                )
            })
            .expect("ADD sunk");
        assert!(add_idx < store_idx);
    }

    #[test]
    fn multi_op_join_convoy_skips_disagreeing_jmpf_suffixes() {
        let mut ops = vec![
            IlOp::Load {
                slot: 0,
                loc: common::DebugLoc::unknown(),
            },
            IlOp::Const {
                imm: 0,
                loc: common::DebugLoc::unknown(),
            },
            IlOp::Bin {
                op: Instruction::ADD,
                loc: common::DebugLoc::unknown(),
            },
            IlOp::Jump {
                kind: IlJumpKind::JumpIfFalse,
                target: Label(0),
                loc: common::DebugLoc::unknown(),
                hint: Default::default(),
            },
            IlOp::Load {
                slot: 0,
                loc: common::DebugLoc::unknown(),
            },
            IlOp::Const {
                imm: 1,
                loc: common::DebugLoc::unknown(),
            },
            IlOp::Bin {
                op: Instruction::ADD,
                loc: common::DebugLoc::unknown(),
            },
            IlOp::Jump {
                kind: IlJumpKind::JumpIfFalse,
                target: Label(0),
                loc: common::DebugLoc::unknown(),
                hint: Default::default(),
            },
            IlOp::Label(Label(0)),
            IlOp::Return {
                loc: common::DebugLoc::unknown(), ret_words: 1,},
        ];
        let before = ops.clone();
        multi_op_join_convoy(&mut ops);
        assert!(ops == before);
    }

    #[test]
    fn multi_op_join_convoy_skips_mixed_jmpf_jmp_unknown_sp() {
        // Identical S on both arms, but JMPF is −1 vs JMP 0 at the join → Unknown.
        let suf = load_const_add_suffix();
        let mut ops = Vec::new();
        ops.extend(suf.clone());
        ops.push(IlOp::Jump {
            kind: IlJumpKind::JumpIfFalse,
            target: Label(0),
            loc: common::DebugLoc::unknown(),
            hint: Default::default(),
        });
        ops.extend(suf);
        ops.push(IlOp::Jump {
            kind: IlJumpKind::Unconditional,
            target: Label(0),
            loc: common::DebugLoc::unknown(),
            hint: Default::default(),
        });
        ops.push(IlOp::Label(Label(0)));
        ops.push(IlOp::Return {
            loc: common::DebugLoc::unknown(), ret_words: 1,});
        let before = ops.clone();
        multi_op_join_convoy(&mut ops);
        assert!(ops == before);
    }

    fn load_not_const_add_suffix() -> Vec<IlOp> {
        // Net SP +1 (needed for sequential JMPF diamonds to agree at the join).
        vec![
            IlOp::Load {
                slot: 0,
                loc: common::DebugLoc::unknown(),
            },
            IlOp::byte(Byte::new(Instruction::NOT)),
            IlOp::Const {
                imm: 1,
                loc: common::DebugLoc::unknown(),
            },
            IlOp::Bin {
                op: Instruction::ADD,
                loc: common::DebugLoc::unknown(),
            },
        ]
    }

    #[test]
    fn multi_op_join_convoy_prefers_longest_suffix_via_jmpf() {
        let suf = load_not_const_add_suffix();
        let cond = IlOp::Const {
            imm: 0,
            loc: common::DebugLoc::unknown(),
        };
        let mut ops = Vec::new();
        ops.extend(suf.clone());
        ops.push(cond.clone());
        ops.push(IlOp::Jump {
            kind: IlJumpKind::JumpIfFalse,
            target: Label(0),
            loc: common::DebugLoc::unknown(),
            hint: Default::default(),
        });
        ops.push(IlOp::Pop {
            loc: common::DebugLoc::unknown(),
        });
        ops.extend(suf);
        ops.push(cond);
        ops.push(IlOp::Jump {
            kind: IlJumpKind::JumpIfFalse,
            target: Label(0),
            loc: common::DebugLoc::unknown(),
            hint: Default::default(),
        });
        ops.push(IlOp::Label(Label(0)));
        ops.push(IlOp::Return {
            loc: common::DebugLoc::unknown(), ret_words: 1,});

        multi_op_join_convoy(&mut ops);

        let load_count = ops
            .iter()
            .filter(|op| matches!(op, IlOp::Load { .. }))
            .count();
        let not_count = ops
            .iter()
            .filter(|op| {
                matches!(
                    op.as_encode_byte().as_ref().map(|b| *b.bytecode()),
                    Some(Instruction::NOT)
                )
            })
            .count();
        let const1_count = ops
            .iter()
            .filter(|op| matches!(op, IlOp::Const { imm: 1, .. }))
            .count();
        assert_eq!(load_count, 1);
        assert_eq!(not_count, 1, "length-4 jump-pred template keeps NOT");
        assert_eq!(const1_count, 1);
        let jmpf_count = ops
            .iter()
            .filter(|op| {
                matches!(
                    op,
                    IlOp::Jump {
                        kind: IlJumpKind::JumpIfFalse,
                        ..
                    }
                )
            })
            .count();
        assert_eq!(
            jmpf_count, 2,
            "JMPFs must not be stripped by jump-pred rewrite"
        );
    }

    #[test]
    fn multi_op_join_convoy_jump_pred_template_keeps_pre_join_ops() {
        // All-jump diamond: ops between last pred and join are not the suffix.
        let suf = load_const_add_suffix();
        let cond = IlOp::Const {
            imm: 1,
            loc: common::DebugLoc::unknown(),
        };
        let mut ops = Vec::new();
        ops.extend(suf.clone());
        ops.push(cond.clone());
        ops.push(IlOp::Jump {
            kind: IlJumpKind::JumpIfFalse,
            target: Label(0),
            loc: common::DebugLoc::unknown(),
            hint: Default::default(),
        });
        ops.push(IlOp::Pop {
            loc: common::DebugLoc::unknown(),
        });
        ops.extend(suf);
        ops.push(cond);
        ops.push(IlOp::Jump {
            kind: IlJumpKind::JumpIfFalse,
            target: Label(0),
            loc: common::DebugLoc::unknown(),
            hint: Default::default(),
        });
        // Net-zero pre-join junk — must survive (Load+StorePop, not part of S).
        ops.push(IlOp::Load {
            slot: 9,
            loc: common::DebugLoc::unknown(),
        });
        ops.push(IlOp::StorePop {
            slot: 9,
            loc: common::DebugLoc::unknown(),
        });
        ops.push(IlOp::Label(Label(0)));
        ops.push(IlOp::Return {
            loc: common::DebugLoc::unknown(), ret_words: 1,});

        multi_op_join_convoy(&mut ops);

        let load9 = ops
            .iter()
            .position(|op| matches!(op, IlOp::Load { slot: 9, .. }))
            .expect("pre-join Load kept");
        let store9 = ops
            .iter()
            .position(|op| matches!(op, IlOp::StorePop { slot: 9, .. }))
            .expect("pre-join StorePop kept");
        let lab = ops
            .iter()
            .position(|op| matches!(op, IlOp::Label(Label(0))))
            .expect("join label");
        let add_idx = ops
            .iter()
            .position(|op| {
                matches!(
                    op,
                    IlOp::Bin {
                        op: Instruction::ADD,
                        ..
                    }
                )
            })
            .expect("suffix sunk after join");
        assert!(load9 < store9 && store9 < lab && lab < add_idx);
        let sunk_loads = ops
            .iter()
            .filter(|op| matches!(op, IlOp::Load { slot: 0, .. }))
            .count();
        assert_eq!(sunk_loads, 1);
    }

    #[test]
    fn multi_op_join_convoy_sinks_jmpf_through_label_cluster() {
        // Jump-pred template into a multi-label return cluster.
        let suf = load_const_add_suffix();
        let cond = IlOp::Const {
            imm: 1,
            loc: common::DebugLoc::unknown(),
        };
        let mut ops = Vec::new();
        ops.extend(suf.clone());
        ops.push(cond.clone());
        ops.push(IlOp::Jump {
            kind: IlJumpKind::JumpIfFalse,
            target: Label(54),
            loc: common::DebugLoc::unknown(),
            hint: Default::default(),
        });
        ops.push(IlOp::Pop {
            loc: common::DebugLoc::unknown(),
        });
        ops.extend(suf);
        ops.push(cond);
        ops.push(IlOp::Jump {
            kind: IlJumpKind::JumpIfFalse,
            target: Label(54),
            loc: common::DebugLoc::unknown(),
            hint: Default::default(),
        });
        ops.push(IlOp::Label(Label(54)));
        ops.push(IlOp::Label(Label(48)));
        ops.push(IlOp::Return {
            loc: common::DebugLoc::unknown(), ret_words: 1,});

        multi_op_join_convoy(&mut ops);

        let load_count = ops
            .iter()
            .filter(|op| matches!(op, IlOp::Load { .. }))
            .count();
        assert_eq!(load_count, 1);
        let lab54 = ops
            .iter()
            .position(|op| matches!(op, IlOp::Label(Label(54))))
            .expect("outer join");
        let lab48 = ops
            .iter()
            .position(|op| matches!(op, IlOp::Label(Label(48))))
            .expect("inner label");
        let add_idx = ops
            .iter()
            .position(|op| {
                matches!(
                    op,
                    IlOp::Bin {
                        op: Instruction::ADD,
                        ..
                    }
                )
            })
            .expect("ADD sunk");
        assert!(lab54 < lab48 && lab48 < add_idx);
    }

    #[test]
    fn multi_op_join_convoy_sinks_identical_suffix_via_jump_if_match() {
        // Two arms: S; scrutinee; JumpIfMatch — scrutinee is an independent Load.
        let suf = load_const_add_suffix();
        let scrut = IlOp::Load {
            slot: 7,
            loc: common::DebugLoc::unknown(),
        };
        let mut ops = Vec::new();
        ops.extend(suf.clone());
        ops.push(scrut.clone());
        ops.push(IlOp::Jump {
            kind: IlJumpKind::JumpIfMatch { tag: 0, arity: 1 },
            target: Label(0),
            loc: common::DebugLoc::unknown(),
            hint: Default::default(),
        });
        ops.push(IlOp::Pop {
            loc: common::DebugLoc::unknown(),
        });
        ops.extend(suf);
        ops.push(scrut);
        ops.push(IlOp::Jump {
            kind: IlJumpKind::JumpIfMatch { tag: 1, arity: 1 },
            target: Label(0),
            loc: common::DebugLoc::unknown(),
            hint: Default::default(),
        });
        ops.push(IlOp::Label(Label(0)));
        ops.push(IlOp::Return {
            loc: common::DebugLoc::unknown(), ret_words: 1,});

        multi_op_join_convoy(&mut ops);

        let load_count = ops
            .iter()
            .filter(|op| matches!(op, IlOp::Load { slot: 0, .. }))
            .count();
        let add_count = ops
            .iter()
            .filter(|op| {
                matches!(
                    op,
                    IlOp::Bin {
                        op: Instruction::ADD,
                        ..
                    }
                )
            })
            .count();
        assert_eq!(load_count, 1, "suffix should appear once after join");
        assert_eq!(add_count, 1);
        let jim_count = ops
            .iter()
            .filter(|op| {
                matches!(
                    op,
                    IlOp::Jump {
                        kind: IlJumpKind::JumpIfMatch { .. },
                        ..
                    }
                )
            })
            .count();
        assert_eq!(jim_count, 2, "JumpIfMatch ops kept; only S stripped");
        assert!(ops.iter().any(|op| matches!(op, IlOp::Return { .. })));
    }

    #[test]
    fn multi_op_join_convoy_skips_disagreeing_jump_if_match_suffixes() {
        let mut ops = vec![
            IlOp::Load {
                slot: 0,
                loc: common::DebugLoc::unknown(),
            },
            IlOp::Const {
                imm: 0,
                loc: common::DebugLoc::unknown(),
            },
            IlOp::Bin {
                op: Instruction::ADD,
                loc: common::DebugLoc::unknown(),
            },
            IlOp::Jump {
                kind: IlJumpKind::JumpIfMatch { tag: 0, arity: 1 },
                target: Label(0),
                loc: common::DebugLoc::unknown(),
                hint: Default::default(),
            },
            IlOp::Load {
                slot: 0,
                loc: common::DebugLoc::unknown(),
            },
            IlOp::Const {
                imm: 1,
                loc: common::DebugLoc::unknown(),
            },
            IlOp::Bin {
                op: Instruction::ADD,
                loc: common::DebugLoc::unknown(),
            },
            IlOp::Jump {
                kind: IlJumpKind::JumpIfMatch { tag: 1, arity: 1 },
                target: Label(0),
                loc: common::DebugLoc::unknown(),
                hint: Default::default(),
            },
            IlOp::Label(Label(0)),
            IlOp::Return {
                loc: common::DebugLoc::unknown(), ret_words: 1,},
        ];
        let before = ops.clone();
        multi_op_join_convoy(&mut ops);
        assert!(ops == before);
    }

    #[test]
    fn multi_op_join_convoy_prefers_longest_suffix_via_jump_if_match() {
        // Net-+1 len-4 suffix; independent scrutinee Load; POP balances fall-through.
        let suf = load_not_const_add_suffix();
        let scrut = IlOp::Load {
            slot: 7,
            loc: common::DebugLoc::unknown(),
        };
        let mut ops = Vec::new();
        ops.extend(suf.clone());
        ops.push(scrut.clone());
        ops.push(IlOp::Jump {
            kind: IlJumpKind::JumpIfMatch { tag: 0, arity: 1 },
            target: Label(0),
            loc: common::DebugLoc::unknown(),
            hint: Default::default(),
        });
        ops.push(IlOp::Pop {
            loc: common::DebugLoc::unknown(),
        });
        ops.extend(suf);
        ops.push(scrut);
        ops.push(IlOp::Jump {
            kind: IlJumpKind::JumpIfMatch { tag: 1, arity: 1 },
            target: Label(0),
            loc: common::DebugLoc::unknown(),
            hint: Default::default(),
        });
        ops.push(IlOp::Label(Label(0)));
        ops.push(IlOp::Return {
            loc: common::DebugLoc::unknown(), ret_words: 1,});

        multi_op_join_convoy(&mut ops);

        let suf_loads = ops
            .iter()
            .filter(|op| matches!(op, IlOp::Load { slot: 0, .. }))
            .count();
        let not_count = ops
            .iter()
            .filter(|op| {
                matches!(
                    op.as_encode_byte().as_ref().map(|b| *b.bytecode()),
                    Some(Instruction::NOT)
                )
            })
            .count();
        assert_eq!(suf_loads, 1);
        assert_eq!(not_count, 1, "length-4 JumpIfMatch template keeps NOT");
        let jim_count = ops
            .iter()
            .filter(|op| {
                matches!(
                    op,
                    IlOp::Jump {
                        kind: IlJumpKind::JumpIfMatch { .. },
                        ..
                    }
                )
            })
            .count();
        assert_eq!(jim_count, 2, "JumpIfMatch ops survive jump-pred rewrite");
    }

    #[test]
    fn multi_op_join_convoy_sinks_jump_if_match_through_label_cluster() {
        let suf = load_const_add_suffix();
        let scrut = IlOp::Load {
            slot: 7,
            loc: common::DebugLoc::unknown(),
        };
        let mut ops = Vec::new();
        ops.extend(suf.clone());
        ops.push(scrut.clone());
        ops.push(IlOp::Jump {
            kind: IlJumpKind::JumpIfMatch { tag: 0, arity: 1 },
            target: Label(54),
            loc: common::DebugLoc::unknown(),
            hint: Default::default(),
        });
        ops.push(IlOp::Pop {
            loc: common::DebugLoc::unknown(),
        });
        ops.extend(suf);
        ops.push(scrut);
        ops.push(IlOp::Jump {
            kind: IlJumpKind::JumpIfMatch { tag: 1, arity: 1 },
            target: Label(54),
            loc: common::DebugLoc::unknown(),
            hint: Default::default(),
        });
        ops.push(IlOp::Label(Label(54)));
        ops.push(IlOp::Label(Label(48)));
        ops.push(IlOp::Return {
            loc: common::DebugLoc::unknown(), ret_words: 1,});

        multi_op_join_convoy(&mut ops);

        let suf_loads = ops
            .iter()
            .filter(|op| matches!(op, IlOp::Load { slot: 0, .. }))
            .count();
        assert_eq!(suf_loads, 1);
        let lab54 = ops
            .iter()
            .position(|op| matches!(op, IlOp::Label(Label(54))))
            .expect("outer join");
        let lab48 = ops
            .iter()
            .position(|op| matches!(op, IlOp::Label(Label(48))))
            .expect("inner label");
        let add_idx = ops
            .iter()
            .position(|op| {
                matches!(
                    op,
                    IlOp::Bin {
                        op: Instruction::ADD,
                        ..
                    }
                )
            })
            .expect("ADD sunk");
        assert!(lab54 < lab48 && lab48 < add_idx);
    }

    #[test]
    fn multi_op_join_convoy_skips_mixed_jump_if_match_jmp_unknown_sp() {
        // JumpIfMatch (−1) + unconditional JMP (0) into same join → Unknown SP.
        let suf = load_const_add_suffix();
        let mut ops = Vec::new();
        ops.extend(suf.clone());
        ops.push(IlOp::Jump {
            kind: IlJumpKind::JumpIfMatch { tag: 0, arity: 1 },
            target: Label(0),
            loc: common::DebugLoc::unknown(),
            hint: Default::default(),
        });
        ops.extend(suf);
        ops.push(IlOp::Jump {
            kind: IlJumpKind::Unconditional,
            target: Label(0),
            loc: common::DebugLoc::unknown(),
            hint: Default::default(),
        });
        ops.push(IlOp::Label(Label(0)));
        ops.push(IlOp::Return {
            loc: common::DebugLoc::unknown(), ret_words: 1,});
        let before = ops.clone();
        multi_op_join_convoy(&mut ops);
        assert!(ops == before);
    }

    #[test]
    fn multi_op_join_convoy_skips_unknown_join_sp() {
        // Identical suffixes, but then-arm pushes an extra const first so join
        // heights disagree → SP Unknown → refuse sink.
        let suf = load_const_add_suffix();
        let mut ops = vec![
            IlOp::Const {
                imm: 0,
                loc: common::DebugLoc::unknown(),
            },
            IlOp::Jump {
                kind: IlJumpKind::JumpIfFalse,
                target: Label(1),
                loc: common::DebugLoc::unknown(),
                hint: Default::default(),
            },
            IlOp::Const {
                imm: 99,
                loc: common::DebugLoc::unknown(),
            },
        ];
        ops.extend(suf.clone());
        ops.push(IlOp::Jump {
            kind: IlJumpKind::Unconditional,
            target: Label(0),
            loc: common::DebugLoc::unknown(),
            hint: Default::default(),
        });
        ops.push(IlOp::Label(Label(1)));
        ops.extend(suf);
        ops.push(IlOp::Label(Label(0)));
        ops.push(IlOp::Return {
            loc: common::DebugLoc::unknown(), ret_words: 1,});
        let before = ops.clone();
        multi_op_join_convoy(&mut ops);
        assert!(ops == before);
    }

    fn load_two_const_add_suffix() -> Vec<IlOp> {
        vec![
            IlOp::Load {
                slot: 0,
                loc: common::DebugLoc::unknown(),
            },
            IlOp::Const {
                imm: 1,
                loc: common::DebugLoc::unknown(),
            },
            IlOp::Const {
                imm: 2,
                loc: common::DebugLoc::unknown(),
            },
            IlOp::Bin {
                op: Instruction::ADD,
                loc: common::DebugLoc::unknown(),
            },
        ]
    }

    #[test]
    fn multi_op_join_convoy_prefers_longest_suffix() {
        // Matching length-4 (and nested length-2/3) — sink the longest once.
        let suf = load_two_const_add_suffix();
        let mut ops = vec![
            IlOp::Const {
                imm: 0,
                loc: common::DebugLoc::unknown(),
            },
            IlOp::Jump {
                kind: IlJumpKind::JumpIfFalse,
                target: Label(1),
                loc: common::DebugLoc::unknown(),
                hint: Default::default(),
            },
        ];
        ops.extend(suf.clone());
        ops.push(IlOp::Jump {
            kind: IlJumpKind::Unconditional,
            target: Label(0),
            loc: common::DebugLoc::unknown(),
            hint: Default::default(),
        });
        ops.push(IlOp::Label(Label(1)));
        ops.extend(suf);
        ops.push(IlOp::Label(Label(0)));
        ops.push(IlOp::Return {
            loc: common::DebugLoc::unknown(), ret_words: 1,});

        multi_op_join_convoy(&mut ops);

        let load_count = ops
            .iter()
            .filter(|op| matches!(op, IlOp::Load { .. }))
            .count();
        let const_count = ops
            .iter()
            .filter(|op| matches!(op, IlOp::Const { imm: 1 | 2, .. }))
            .count();
        let add_count = ops
            .iter()
            .filter(|op| {
                matches!(
                    op,
                    IlOp::Bin {
                        op: Instruction::ADD,
                        ..
                    }
                )
            })
            .count();
        assert_eq!(load_count, 1);
        assert_eq!(const_count, 2, "both consts from the length-4 suffix");
        assert_eq!(add_count, 1);
    }

    #[test]
    fn multi_op_join_convoy_sinks_length_two() {
        let suf = vec![
            IlOp::Load {
                slot: 0,
                loc: common::DebugLoc::unknown(),
            },
            IlOp::Const {
                imm: 1,
                loc: common::DebugLoc::unknown(),
            },
        ];
        let mut ops = vec![
            IlOp::Const {
                imm: 0,
                loc: common::DebugLoc::unknown(),
            },
            IlOp::Jump {
                kind: IlJumpKind::JumpIfFalse,
                target: Label(1),
                loc: common::DebugLoc::unknown(),
                hint: Default::default(),
            },
        ];
        ops.extend(suf.clone());
        ops.push(IlOp::Jump {
            kind: IlJumpKind::Unconditional,
            target: Label(0),
            loc: common::DebugLoc::unknown(),
            hint: Default::default(),
        });
        ops.push(IlOp::Label(Label(1)));
        ops.extend(suf);
        ops.push(IlOp::Label(Label(0)));
        ops.push(IlOp::Return {
            loc: common::DebugLoc::unknown(), ret_words: 1,});

        multi_op_join_convoy(&mut ops);

        let load_count = ops
            .iter()
            .filter(|op| matches!(op, IlOp::Load { .. }))
            .count();
        assert_eq!(load_count, 1);
        assert!(ops.iter().any(|op| matches!(op, IlOp::Return { .. })));
    }

    #[test]
    fn multi_op_join_convoy_sinks_identical_suffix_non_return() {
        // Diamond into shared continuation (StorePop), not RETURN.
        let suf = load_const_add_suffix();
        let mut ops = vec![
            IlOp::Const {
                imm: 0,
                loc: common::DebugLoc::unknown(),
            },
            IlOp::Jump {
                kind: IlJumpKind::JumpIfFalse,
                target: Label(1),
                loc: common::DebugLoc::unknown(),
                hint: Default::default(),
            },
        ];
        ops.extend(suf.clone());
        ops.push(IlOp::Jump {
            kind: IlJumpKind::Unconditional,
            target: Label(0),
            loc: common::DebugLoc::unknown(),
            hint: Default::default(),
        });
        ops.push(IlOp::Label(Label(1)));
        ops.extend(suf);
        ops.push(IlOp::Label(Label(0)));
        ops.push(IlOp::StorePop {
            slot: 2,
            loc: common::DebugLoc::unknown(),
        });
        ops.push(IlOp::Halt {
            loc: common::DebugLoc::unknown(),
        });

        multi_op_join_convoy(&mut ops);

        let load_count = ops
            .iter()
            .filter(|op| matches!(op, IlOp::Load { .. }))
            .count();
        let add_count = ops
            .iter()
            .filter(|op| {
                matches!(
                    op,
                    IlOp::Bin {
                        op: Instruction::ADD,
                        ..
                    }
                )
            })
            .count();
        assert_eq!(load_count, 1, "suffix should appear once after join");
        assert_eq!(add_count, 1);
        // Suffix then StorePop: Load … ADD StorePop Halt
        let store_idx = ops
            .iter()
            .position(|op| matches!(op, IlOp::StorePop { slot: 2, .. }))
            .expect("StorePop kept");
        let add_idx = ops
            .iter()
            .position(|op| {
                matches!(
                    op,
                    IlOp::Bin {
                        op: Instruction::ADD,
                        ..
                    }
                )
            })
            .expect("ADD sunk");
        assert!(add_idx < store_idx, "suffix before shared continuation");
        assert!(ops.iter().any(|op| matches!(op, IlOp::Halt { .. })));
    }

    #[test]
    fn multi_op_join_convoy_skips_disagreeing_suffixes_non_return() {
        let mut ops = vec![
            IlOp::Load {
                slot: 0,
                loc: common::DebugLoc::unknown(),
            },
            IlOp::Const {
                imm: 0,
                loc: common::DebugLoc::unknown(),
            },
            IlOp::Bin {
                op: Instruction::ADD,
                loc: common::DebugLoc::unknown(),
            },
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(0),
                loc: common::DebugLoc::unknown(),
                hint: Default::default(),
            },
            IlOp::Load {
                slot: 0,
                loc: common::DebugLoc::unknown(),
            },
            IlOp::Const {
                imm: 1,
                loc: common::DebugLoc::unknown(),
            },
            IlOp::Bin {
                op: Instruction::ADD,
                loc: common::DebugLoc::unknown(),
            },
            IlOp::Label(Label(0)),
            IlOp::StorePop {
                slot: 2,
                loc: common::DebugLoc::unknown(),
            },
        ];
        let before = ops.clone();
        multi_op_join_convoy(&mut ops);
        assert!(ops == before);
    }

    #[test]
    fn multi_op_join_convoy_skips_jmpf_fallthrough_unknown_sp_non_return() {
        // JMPF + fall-through identical S: join SP Unknown → refuse (same as return).
        let suf = load_const_add_suffix();
        let mut ops = Vec::new();
        ops.extend(suf.clone());
        ops.push(IlOp::Jump {
            kind: IlJumpKind::JumpIfFalse,
            target: Label(0),
            loc: common::DebugLoc::unknown(),
            hint: Default::default(),
        });
        ops.extend(suf);
        ops.push(IlOp::Label(Label(0)));
        ops.push(IlOp::StorePop {
            slot: 2,
            loc: common::DebugLoc::unknown(),
        });
        let before = ops.clone();
        multi_op_join_convoy(&mut ops);
        assert!(ops == before);
    }

    #[test]
    fn multi_op_join_convoy_skips_jump_only_join() {
        // Labels followed only by unconditional JMP — no local work.
        let suf = load_const_add_suffix();
        let mut ops = Vec::new();
        ops.extend(suf.clone());
        ops.push(IlOp::Jump {
            kind: IlJumpKind::Unconditional,
            target: Label(0),
            loc: common::DebugLoc::unknown(),
            hint: Default::default(),
        });
        ops.extend(suf);
        ops.push(IlOp::Label(Label(0)));
        ops.push(IlOp::Jump {
            kind: IlJumpKind::Unconditional,
            target: Label(9),
            loc: common::DebugLoc::unknown(),
            hint: Default::default(),
        });
        ops.push(IlOp::Label(Label(9)));
        ops.push(IlOp::Halt {
            loc: common::DebugLoc::unknown(),
        });
        let before = ops.clone();
        multi_op_join_convoy(&mut ops);
        assert!(ops == before);
    }

    #[test]
    fn multi_op_join_convoy_skips_halt_join_consumer() {
        // HALT after labels is a terminator — leave to dead_block, not NonReturn sink.
        let suf = load_const_add_suffix();
        let mut ops = Vec::new();
        ops.extend(suf.clone());
        ops.push(IlOp::Jump {
            kind: IlJumpKind::Unconditional,
            target: Label(0),
            loc: common::DebugLoc::unknown(),
            hint: Default::default(),
        });
        ops.extend(suf);
        ops.push(IlOp::Label(Label(0)));
        ops.push(IlOp::Halt {
            loc: common::DebugLoc::unknown(),
        });
        let before = ops.clone();
        multi_op_join_convoy(&mut ops);
        assert!(ops == before);
    }

    #[test]
    fn multi_op_join_convoy_skips_fused_return_join_consumer() {
        // Fused *Return after labels must not be treated as a NonReturn continuation.
        let suf = load_const_add_suffix();
        let mut ops = Vec::new();
        ops.extend(suf.clone());
        ops.push(IlOp::Jump {
            kind: IlJumpKind::Unconditional,
            target: Label(0),
            loc: common::DebugLoc::unknown(),
            hint: Default::default(),
        });
        ops.extend(suf);
        ops.push(IlOp::Label(Label(0)));
        ops.push(IlOp::BinReturn {
            op: Instruction::ADD,
            loc: common::DebugLoc::unknown(),
        });
        let before = ops.clone();
        multi_op_join_convoy(&mut ops);
        assert!(ops == before);
    }

    #[test]
    fn multi_op_join_convoy_sinks_identical_suffix_via_jump_if_match_non_return() {
        let suf = load_const_add_suffix();
        let scrut = IlOp::Load {
            slot: 7,
            loc: common::DebugLoc::unknown(),
        };
        let mut ops = Vec::new();
        ops.extend(suf.clone());
        ops.push(scrut.clone());
        ops.push(IlOp::Jump {
            kind: IlJumpKind::JumpIfMatch { tag: 0, arity: 1 },
            target: Label(0),
            loc: common::DebugLoc::unknown(),
            hint: Default::default(),
        });
        ops.push(IlOp::Pop {
            loc: common::DebugLoc::unknown(),
        });
        ops.extend(suf);
        ops.push(scrut);
        ops.push(IlOp::Jump {
            kind: IlJumpKind::JumpIfMatch { tag: 1, arity: 1 },
            target: Label(0),
            loc: common::DebugLoc::unknown(),
            hint: Default::default(),
        });
        ops.push(IlOp::Label(Label(0)));
        ops.push(IlOp::StorePop {
            slot: 2,
            loc: common::DebugLoc::unknown(),
        });

        multi_op_join_convoy(&mut ops);

        let suf_loads = ops
            .iter()
            .filter(|op| matches!(op, IlOp::Load { slot: 0, .. }))
            .count();
        assert_eq!(suf_loads, 1);
        let store_idx = ops
            .iter()
            .position(|op| matches!(op, IlOp::StorePop { slot: 2, .. }))
            .expect("StorePop kept");
        let add_idx = ops
            .iter()
            .position(|op| {
                matches!(
                    op,
                    IlOp::Bin {
                        op: Instruction::ADD,
                        ..
                    }
                )
            })
            .expect("ADD sunk");
        assert!(add_idx < store_idx);
    }

    #[test]
    fn multi_op_join_convoy_skips_unknown_join_sp_non_return() {
        // Identical suffixes, mismatched arm heights → Unknown join SP → refuse.
        let suf = load_const_add_suffix();
        let mut ops = vec![
            IlOp::Const {
                imm: 0,
                loc: common::DebugLoc::unknown(),
            },
            IlOp::Jump {
                kind: IlJumpKind::JumpIfFalse,
                target: Label(1),
                loc: common::DebugLoc::unknown(),
                hint: Default::default(),
            },
            IlOp::Const {
                imm: 99,
                loc: common::DebugLoc::unknown(),
            },
        ];
        ops.extend(suf.clone());
        ops.push(IlOp::Jump {
            kind: IlJumpKind::Unconditional,
            target: Label(0),
            loc: common::DebugLoc::unknown(),
            hint: Default::default(),
        });
        ops.push(IlOp::Label(Label(1)));
        ops.extend(suf);
        ops.push(IlOp::Label(Label(0)));
        ops.push(IlOp::StorePop {
            slot: 2,
            loc: common::DebugLoc::unknown(),
        });
        let before = ops.clone();
        multi_op_join_convoy(&mut ops);
        assert!(ops == before);
    }

    #[test]
    fn multi_op_join_convoy_skips_without_jump_preds() {
        // Fall-through into a labeled continuation with no JMP preds — refuse.
        let suf = load_const_add_suffix();
        let mut ops = Vec::new();
        ops.extend(suf);
        ops.push(IlOp::Label(Label(0)));
        ops.push(IlOp::StorePop {
            slot: 2,
            loc: common::DebugLoc::unknown(),
        });
        let before = ops.clone();
        multi_op_join_convoy(&mut ops);
        assert!(ops == before);
    }

    #[test]
    fn multi_op_join_convoy_sinks_through_label_cluster_non_return() {
        // Multi-label join (JMPF diamond so both arms have known SP): sink after cluster.
        let suf = load_const_add_suffix();
        let mut ops = vec![
            IlOp::Const {
                imm: 0,
                loc: common::DebugLoc::unknown(),
            },
            IlOp::Jump {
                kind: IlJumpKind::JumpIfFalse,
                target: Label(1),
                loc: common::DebugLoc::unknown(),
                hint: Default::default(),
            },
        ];
        ops.extend(suf.clone());
        ops.push(IlOp::Jump {
            kind: IlJumpKind::Unconditional,
            target: Label(54),
            loc: common::DebugLoc::unknown(),
            hint: Default::default(),
        });
        ops.push(IlOp::Label(Label(1)));
        ops.extend(suf);
        ops.push(IlOp::Label(Label(54)));
        ops.push(IlOp::Label(Label(48)));
        ops.push(IlOp::StorePop {
            slot: 2,
            loc: common::DebugLoc::unknown(),
        });
        ops.push(IlOp::Halt {
            loc: common::DebugLoc::unknown(),
        });

        multi_op_join_convoy(&mut ops);

        let load_count = ops
            .iter()
            .filter(|op| matches!(op, IlOp::Load { .. }))
            .count();
        assert_eq!(load_count, 1, "suffix should appear once after cluster");
        assert!(ops.iter().any(|op| matches!(op, IlOp::Label(Label(54)))));
        assert!(ops.iter().any(|op| matches!(op, IlOp::Label(Label(48)))));
        let lab48 = ops
            .iter()
            .position(|op| matches!(op, IlOp::Label(Label(48))))
            .expect("inner label");
        let add_idx = ops
            .iter()
            .position(|op| {
                matches!(
                    op,
                    IlOp::Bin {
                        op: Instruction::ADD,
                        ..
                    }
                )
            })
            .expect("ADD sunk");
        let store_idx = ops
            .iter()
            .position(|op| matches!(op, IlOp::StorePop { slot: 2, .. }))
            .expect("StorePop");
        assert!(lab48 < add_idx && add_idx < store_idx);
    }

    #[test]
    fn multi_op_join_convoy_prefers_longest_suffix_non_return() {
        let suf = load_two_const_add_suffix();
        let mut ops = vec![
            IlOp::Const {
                imm: 0,
                loc: common::DebugLoc::unknown(),
            },
            IlOp::Jump {
                kind: IlJumpKind::JumpIfFalse,
                target: Label(1),
                loc: common::DebugLoc::unknown(),
                hint: Default::default(),
            },
        ];
        ops.extend(suf.clone());
        ops.push(IlOp::Jump {
            kind: IlJumpKind::Unconditional,
            target: Label(0),
            loc: common::DebugLoc::unknown(),
            hint: Default::default(),
        });
        ops.push(IlOp::Label(Label(1)));
        ops.extend(suf);
        ops.push(IlOp::Label(Label(0)));
        ops.push(IlOp::StorePop {
            slot: 2,
            loc: common::DebugLoc::unknown(),
        });

        multi_op_join_convoy(&mut ops);

        let load_count = ops
            .iter()
            .filter(|op| matches!(op, IlOp::Load { .. }))
            .count();
        let const_count = ops
            .iter()
            .filter(|op| matches!(op, IlOp::Const { imm: 1 | 2, .. }))
            .count();
        assert_eq!(load_count, 1);
        assert_eq!(const_count, 2, "length-4 suffix keeps both consts once");
        let add_idx = ops
            .iter()
            .position(|op| {
                matches!(
                    op,
                    IlOp::Bin {
                        op: Instruction::ADD,
                        ..
                    }
                )
            })
            .expect("ADD");
        let store_idx = ops
            .iter()
            .position(|op| matches!(op, IlOp::StorePop { slot: 2, .. }))
            .expect("StorePop");
        assert!(add_idx < store_idx);
    }

    #[test]
    fn multi_op_join_convoy_refuses_format_string_suffix() {
        // FORMAT (+ PRINT) must not count as sinkable compute — Known SP after
        // FORMAT would otherwise splice format runs across joins. Typed STRING
        // alone is eligible; keep FORMAT out of the allowlist.
        let loc = common::DebugLoc::unknown();
        let fmt = vec![
            IlOp::String { idx: 1, loc },
            IlOp::byte(Byte::new(Instruction::FORMAT).with_operand_u32(0)),
            IlOp::Print { loc },
        ];
        let mut ops = vec![
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(0),
                loc,
                hint: Default::default(),
            },
            IlOp::Label(Label(1)),
        ];
        ops.extend(fmt.clone());
        ops.push(IlOp::Jump {
            kind: IlJumpKind::Unconditional,
            target: Label(0),
            loc,
            hint: Default::default(),
        });
        ops.push(IlOp::Label(Label(2)));
        ops.extend(fmt.clone());
        ops.push(IlOp::Jump {
            kind: IlJumpKind::Unconditional,
            target: Label(0),
            loc,
            hint: Default::default(),
        });
        ops.push(IlOp::Label(Label(0)));
        ops.push(IlOp::Return { loc, ret_words: 1});

        let before = ops.len();
        multi_op_join_convoy(&mut ops);
        assert_eq!(ops.len(), before, "format suffixes must not sink");
        let strings = ops
            .iter()
            .filter(|op| matches!(op, IlOp::String { .. }))
            .count();
        assert_eq!(strings, 2, "each arm keeps its STRING");
    }

    #[test]
    fn multi_op_join_convoy_sinks_typed_string_suffix() {
        // Diamond: both arms end with String;Load then join+RETURN.
        let loc = common::DebugLoc::unknown();
        let suf = vec![IlOp::String { idx: 3, loc }, IlOp::Load { slot: 1, loc }];
        let mut ops = vec![
            IlOp::Const { imm: 0, loc },
            IlOp::Jump {
                kind: IlJumpKind::JumpIfFalse,
                target: Label(1),
                loc,
                hint: Default::default(),
            },
        ];
        ops.extend(suf.clone());
        ops.push(IlOp::Jump {
            kind: IlJumpKind::Unconditional,
            target: Label(0),
            loc,
            hint: Default::default(),
        });
        ops.push(IlOp::Label(Label(1)));
        ops.extend(suf);
        ops.push(IlOp::Label(Label(0)));
        ops.push(IlOp::Return { loc, ret_words: 1});

        multi_op_join_convoy(&mut ops);
        let strings = ops
            .iter()
            .filter(|op| matches!(op, IlOp::String { idx: 3, .. }))
            .count();
        assert_eq!(strings, 1, "identical STRING tails should sink once");
        assert_eq!(
            ops.iter()
                .filter(|op| matches!(op, IlOp::Load { slot: 1, .. }))
                .count(),
            1,
            "LOAD should remain once after sink"
        );
    }

    #[test]
    fn optimize_per_func_leaves_prologue_glue_untouched() {
        // Prologue: DUPLICATE; POP (would DCE on whole buffer).
        // Func body at emitting [2, 5): CONST 1; DUPLICATE; POP; RETURN
        // → only the func's DUP/POP pair is removed.
        let mut ops = vec![
            IlOp::Dup {
                loc: common::DebugLoc::unknown(),
            },
            IlOp::Pop {
                loc: common::DebugLoc::unknown(),
            },
            IlOp::Const {
                imm: 1,
                loc: common::DebugLoc::unknown(),
            },
            IlOp::Dup {
                loc: common::DebugLoc::unknown(),
            },
            IlOp::Pop {
                loc: common::DebugLoc::unknown(),
            },
            IlOp::Return {
                loc: common::DebugLoc::unknown(), ret_words: 1,},
            // Glue after the function: another DUP; POP that must survive.
            IlOp::Dup {
                loc: common::DebugLoc::unknown(),
            },
            IlOp::Pop {
                loc: common::DebugLoc::unknown(),
            },
        ];
        let funcs = vec![crate::il::IlFunc::new("f", None, 2, 6)];
        optimize_per_func(&mut ops, &funcs, &OptimizeOptions::default(), &mut Vec::new());

        assert!(
            matches!(ops[0], IlOp::Dup { .. }) && matches!(ops[1], IlOp::Pop { .. }),
            "prologue DUP/POP must survive"
        );
        assert!(
            matches!(ops.last(), Some(IlOp::Pop { .. })),
            "trailing glue DUP/POP must survive"
        );
        let body_dups = ops[2..ops.len() - 2]
            .iter()
            .filter(|op| matches!(op, IlOp::Dup { .. }))
            .count();
        assert_eq!(body_dups, 0, "func-body DUP/POP should DCE");
        assert!(ops.iter().any(|op| matches!(op, IlOp::Return { .. })));
    }

    /// Regression: jmp-pred-only bin tail at a shared return label must refuse when
    /// join SP is Unknown (`examples/tree.hy` sum_tree match arms).
    #[test]
    fn bin_join_convoy_refuses_unknown_sp_jump_pred_only_join() {
        let mut ops = vec![
            IlOp::Label(Label(0)),
            IlOp::Load {
                slot: 0,
                loc: common::DebugLoc::unknown(),
            },
            IlOp::Jump {
                kind: IlJumpKind::JumpIfMatch { tag: 0, arity: 0 },
                target: Label(1),
                loc: common::DebugLoc::unknown(),
                hint: Default::default(),
            },
            IlOp::Load {
                slot: 1,
                loc: common::DebugLoc::unknown(),
            },
            IlOp::Load {
                slot: 2,
                loc: common::DebugLoc::unknown(),
            },
            // Unknown stack effect poisons SP into the join (real match diamonds
            // with effectful ops must not convoy on jump-pred-only templates).
            IlOp::byte(common::Byte::new(Instruction::PRINT)),
            IlOp::Bin {
                op: Instruction::ADD,
                loc: common::DebugLoc::unknown(),
            },
            IlOp::Load {
                slot: 3,
                loc: common::DebugLoc::unknown(),
            },
            IlOp::byte(common::Byte::new(Instruction::PRINT)),
            IlOp::Bin {
                op: Instruction::ADD,
                loc: common::DebugLoc::unknown(),
            },
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(2),
                loc: common::DebugLoc::unknown(),
                hint: Default::default(),
            },
            IlOp::Label(Label(1)),
            IlOp::Const {
                imm: 0,
                loc: common::DebugLoc::unknown(),
            },
            IlOp::Dup {
                loc: common::DebugLoc::unknown(),
            },
            IlOp::Pop {
                loc: common::DebugLoc::unknown(),
            },
            IlOp::Label(Label(2)),
            IlOp::Return {
                loc: common::DebugLoc::unknown(), ret_words: 1,},
        ];
        let info = crate::il::sp::analyze(&ops);
        let lab2 = ops
            .iter()
            .position(|op| matches!(op, IlOp::Label(Label(2))))
            .unwrap();
        assert!(
            !info.sp_before(lab2).is_known(),
            "precondition: join SP must be Unknown"
        );
        let adds_before = ops
            .iter()
            .filter(|op| {
                matches!(
                    op,
                    IlOp::Bin {
                        op: Instruction::ADD,
                        ..
                    }
                )
            })
            .count();
        bin_join_convoy(&mut ops);
        let adds_after = ops
            .iter()
            .filter(|op| {
                matches!(
                    op,
                    IlOp::Bin {
                        op: Instruction::ADD,
                        ..
                    }
                )
            })
            .count();
        assert_eq!(adds_after, adds_before);
        assert!(!ops.iter().any(|op| matches!(op, IlOp::BinReturn { .. })));
    }

    /// Opcode names for assertion messages (`IlOp` has no `Debug`).
    fn insn_names(ops: &[IlOp]) -> Vec<String> {
        ops.iter()
            .map(|op| match op.instruction() {
                Some(i) => format!("{i:?}"),
                None => "<label/meta>".to_string(),
            })
            .collect()
    }

    #[test]
    fn stack_dce_drops_dead_const_pop() {
        let loc = common::DebugLoc::unknown();
        let mut ops = vec![
            IlOp::Const { imm: 0, loc },
            IlOp::Pop { loc },
            IlOp::Load { slot: 1, loc },
            IlOp::Return { loc, ret_words: 1},
        ];
        stack_dce(&mut ops);
        let names = insn_names(&ops);
        assert!(
            !ops.iter().any(|op| matches!(op, IlOp::Const { .. })),
            "statement-position CONST;POP should be removed; got {names:?}"
        );
        assert_eq!(ops.len(), 2, "only LOAD;RETURN should remain; got {names:?}");
    }

    #[test]
    fn stack_dce_drops_dead_string_and_const_pool_pop() {
        let loc = common::DebugLoc::unknown();
        let mut ops = vec![
            IlOp::String { idx: 0, loc },
            IlOp::Pop { loc },
            IlOp::ConstPool { idx: 3, loc },
            IlOp::Pop { loc },
            IlOp::Return { loc, ret_words: 1},
        ];
        stack_dce(&mut ops);
        let names = insn_names(&ops);
        assert_eq!(
            ops.len(),
            1,
            "String;Pop and ConstPool;Pop are pure discards; got {names:?}"
        );
        assert!(matches!(ops[0], IlOp::Return { .. }));
    }

    #[test]
    fn stack_dce_iterates_to_fixpoint() {
        // `Load; Const; Pop; Pop`: removing the inner pair exposes `Load; Pop`.
        let loc = common::DebugLoc::unknown();
        let mut ops = vec![
            IlOp::Load { slot: 2, loc },
            IlOp::Const { imm: 7, loc },
            IlOp::Pop { loc },
            IlOp::Pop { loc },
            IlOp::Return { loc, ret_words: 1},
        ];
        stack_dce(&mut ops);
        let names = insn_names(&ops);
        assert_eq!(ops.len(), 1, "both pairs should be removed; got {names:?}");
        assert!(matches!(ops[0], IlOp::Return { .. }));
    }

    #[test]
    fn inverts_guard_branch_over_unconditional_jump() {
        // `if flag { break }`: LOAD is not a *Jmpf-fusable condition.
        let loc = common::DebugLoc::unknown();
        let mut ops = vec![
            IlOp::Load { slot: 0, loc },
            IlOp::Jump {
                kind: IlJumpKind::JumpIfFalse,
                target: Label(1),
                loc,
                hint: Default::default(),
            },
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(2),
                loc,
                hint: Default::default(),
            },
            IlOp::Label(Label(1)),
            IlOp::Const { imm: 5, loc },
            IlOp::Label(Label(2)),
            IlOp::Return { loc, ret_words: 1},
        ];
        invert_branch_over_jump(&mut ops);
        let jumps: Vec<_> = ops
            .iter()
            .filter_map(|op| match op {
                IlOp::Jump { kind, target, .. } => Some((*kind, target.0)),
                _ => None,
            })
            .collect();
        assert_eq!(
            jumps,
            vec![(IlJumpKind::JumpIfTrue, 2)],
            "JMPF L1; JMP L2; L1: should collapse to JMPT L2"
        );
    }

    #[test]
    fn invert_guard_refuses_value_under_jmp_hint() {
        let loc = common::DebugLoc::unknown();
        let mut ops = vec![
            IlOp::Bin {
                op: Instruction::EQ,
                loc,
            },
            IlOp::jump_hinted(
                IlJumpKind::JumpIfFalse,
                Label(1),
                loc,
                crate::il::FuseHint::nofuse_value_under_jmp(),
            ),
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(2),
                loc,
                hint: Default::default(),
            },
            IlOp::Label(Label(1)),
            IlOp::Const { imm: 5, loc },
            IlOp::Label(Label(2)),
            IlOp::Return { loc, ret_words: 1},
        ];
        invert_branch_over_jump(&mut ops);
        let jumps: Vec<_> = ops
            .iter()
            .filter_map(|op| match op {
                IlOp::Jump { kind, target, .. } => Some((*kind, target.0)),
                _ => None,
            })
            .collect();
        assert_eq!(
            jumps,
            vec![
                (IlJumpKind::JumpIfFalse, 1),
                (IlJumpKind::Unconditional, 2)
            ],
            "ValueUnderJmp JMPF must not invert; got {jumps:?}"
        );
    }

    #[test]
    fn inverts_guard_when_condition_fuses_with_jmpf() {
        // `BinSlotSlot LE; JMPF; JMP` inverts to JMPT so fuse can emit BinSlotSlotJmpt.
        let loc = common::DebugLoc::unknown();
        let mut ops = vec![
            IlOp::BinSlotSlot {
                op: Instruction::LE as u8,
                a: 0,
                b: 1,
                loc,
            },
            IlOp::Jump {
                kind: IlJumpKind::JumpIfFalse,
                target: Label(1),
                loc,
                hint: Default::default(),
            },
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(2),
                loc,
                hint: Default::default(),
            },
            IlOp::Label(Label(1)),
            IlOp::Label(Label(2)),
            IlOp::Return { loc, ret_words: 1},
        ];
        invert_branch_over_jump(&mut ops);
        assert_eq!(ops.len(), 5, "trailing JMP should drop");
        assert!(matches!(
            ops[1],
            IlOp::Jump {
                kind: IlJumpKind::JumpIfTrue,
                ..
            }
        ));
    }

    #[test]
    fn refuses_guard_inversion_when_false_target_is_not_next() {
        // JMPF's target is bound after real code, so the JMP is reachable
        // independently and must not be dropped.
        let loc = common::DebugLoc::unknown();
        let mut ops = vec![
            IlOp::Load { slot: 0, loc },
            IlOp::Jump {
                kind: IlJumpKind::JumpIfFalse,
                target: Label(3),
                loc,
                hint: Default::default(),
            },
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(2),
                loc,
                hint: Default::default(),
            },
            IlOp::Const { imm: 1, loc },
            IlOp::Label(Label(3)),
            IlOp::Label(Label(2)),
            IlOp::Return { loc, ret_words: 1},
        ];
        let before = ops.len();
        invert_branch_over_jump(&mut ops);
        assert_eq!(ops.len(), before, "non-adjacent false target must refuse");
    }
