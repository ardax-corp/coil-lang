    use super::*;
    use crate::il::opt::{OptimizeOptions, optimize_per_func};
    use crate::il::opt::cfg::{eliminate_dead_blocks, invert_branch_over_jump, jump_thread};
    use crate::il::opt::dce::{dead_store_at, stack_dce};
    use common::{Byte, Instruction};

    fn is_insn(op: &IlOp, i: Instruction) -> bool {
        op.as_encode_byte().is_some_and(|b| *b.bytecode() == i)
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
                kinds: 0,
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
                kinds: 0,
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
                kinds: 0,
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
                kinds: 0,
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
        dead_store_at(&mut ops, 0);
        assert!(
            ops.iter()
                .any(|op| matches!(op, IlOp::StorePop { slot: 9, .. }))
        );
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
        dead_store_at(&mut ops, 0);
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
        dead_store_at(&mut ops, 0);
        assert!(
            ops.iter()
                .any(|op| matches!(op, IlOp::StorePop { slot: 3, .. }))
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

    /// Opcode names for assertion messages (`IlOp` has no `Debug`).
    /// Symbolic control stays unlabeled: [`IlOp::as_encode_byte`] refuses it.
    fn insn_names(ops: &[IlOp]) -> Vec<String> {
        ops.iter()
            .map(|op| match op.as_encode_byte() {
                Some(b) => format!("{:?}", b.bytecode()),
                None if op.is_control() => "<control>".to_string(),
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
