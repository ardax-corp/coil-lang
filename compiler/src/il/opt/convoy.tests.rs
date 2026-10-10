    use super::*;
    use crate::il::opt::{OptimizeOptions, optimize_per_func};
    use crate::il::opt::cfg::eliminate_dead_blocks;
    use common::{Byte, Instruction};

    fn is_insn(op: &IlOp, i: Instruction) -> bool {
        op.as_encode_byte().is_some_and(|b| *b.bytecode() == i)
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

