//! Pipeline round behaviour and opt stats.

use super::*;
use crate::il::op::{IlJumpKind, IlOp, Label};
use common::DebugLoc;

fn loc() -> DebugLoc {
    DebugLoc::unknown()
}

fn jmp(id: u32) -> IlOp {
    IlOp::Jump {
        kind: IlJumpKind::Unconditional,
        target: Label(id),
        loc: loc(),
        hint: Default::default(),
    }
}

fn label(id: u32) -> IlOp {
    IlOp::Label(Label(id))
}

fn ret() -> IlOp {
    IlOp::Return { loc: loc(), ret_words: 1}
}

fn c(n: i32) -> IlOp {
    IlOp::Const {
        imm: n,
        loc: loc(),
    }
}

fn entry_target(ops: &[IlOp]) -> Option<u32> {
    match ops.first() {
        Some(IlOp::Jump {
            kind: IlJumpKind::Unconditional,
            target: Label(id),
            ..
        }) => Some(*id),
        _ => None,
    }
}

/// Jump threading only. One hop per jump per round, so a 3-edge chain
/// still has work after a single pipeline pass.
fn jump_thread_opts() -> OptimizeOptions {
    let mut o = super::OptLevel::None.options();
    o.algebraic = false;
    o.jump_thread = true;
    o
}

/// JMP L1; L1: JMP L2; L2: JMP L3; L3: RET
fn jmp_chain() -> Vec<IlOp> {
    vec![
        jmp(1),
        label(1),
        jmp(2),
        label(2),
        jmp(3),
        label(3),
        ret(),
    ]
}

/// Production runs the pipeline once; re-running it is not a supported mode
/// (LICM is not idempotent), so a 3-edge chain threads exactly one hop.
#[test]
fn one_round_threads_one_hop() {
    let mut ops = jmp_chain();
    optimize(&mut ops, &jump_thread_opts(), &mut Vec::new());
    assert_eq!(entry_target(&ops), Some(2));
}

#[test]
fn stats_collect_stack_dce_and_format() {
    begin_opt_stats();
    let mut ops = vec![
        c(1),
        IlOp::Dup { loc: loc() },
        IlOp::Pop { loc: loc() },
        ret(),
    ];
    let mut opts = OptLevel::None.options();
    opts.stack_dce = true;
    opts.collect_stats = true;
    optimize(&mut ops, &opts, &mut Vec::new());
    let stats = last_opt_stats();
    assert_eq!(stats.iterations, 1);
    assert!(stats.ops_eliminated >= 2);
    assert!(
        stats.passes.iter().any(|p| p.name == "stack_dce" && p.applied >= 1),
        "{:?}",
        stats.passes
    );
    let text = stats.format_text();
    assert!(text.contains("ops eliminated"));
    assert!(text.contains("stack_dce"));
    let json = stats.format_json();
    assert!(json.contains("\"ops_eliminated\""));
    assert!(json.contains("stack_dce"));
}

#[test]
fn fact_mul_keeps_call_result_across_opts() {
    use crate::il::{EntryKind, IlFunc, IlModule};

    let ops = vec![
        label(1),
        IlOp::Load { slot: 0, loc: loc() },
        c(1),
        IlOp::Bin {
            op: common::Instruction::LEQ,
            loc: loc(),
        },
        IlOp::Jump {
            kind: IlJumpKind::JumpIfFalse,
            target: Label(2),
            loc: loc(),
            hint: Default::default(),
        },
        IlOp::Load { slot: 0, loc: loc() },
        ret(),
        label(2),
        IlOp::Load { slot: 0, loc: loc() },
        IlOp::StorePop { slot: 1, loc: loc() },
        IlOp::Load { slot: 0, loc: loc() },
        c(1),
        IlOp::Bin {
            op: common::Instruction::SUB,
            loc: loc(),
        },
        IlOp::Entry {
            kind: EntryKind::Call,
            arity: 1,
            target: Label(1),
            loc: loc(),
            ret_words: 1,
        },
        IlOp::StorePop { slot: 2, loc: loc() },
        IlOp::Load { slot: 1, loc: loc() },
        IlOp::Load { slot: 2, loc: loc() },
        IlOp::Bin {
            op: common::Instruction::MUL,
            loc: loc(),
        },
        ret(),
    ];
    let emitting = ops.iter().filter(|op| op.emits_code()).count();
    let funcs = vec![IlFunc::with_entry_sp(
        "fact",
        Some(Label(1)),
        0,
        emitting,
        1,
    )];
    let mut module = IlModule::from_flat(&ops, &funcs);
    let (optimized, _, _) =
        module.optimize_and_flatten(&OptimizeOptions::default(), &mut Vec::new());
    let rendered = optimized
        .iter()
        .map(crate::dissect::format_il_op)
        .collect::<Vec<_>>()
        .join("\n");
    let squared = optimized.windows(3).any(|w| {
        matches!(
            (&w[0], &w[1], &w[2]),
            (
                IlOp::Load { slot: 0, .. },
                IlOp::Load { slot: 0, .. },
                IlOp::Bin {
                    op: common::Instruction::MUL,
                    ..
                }
            )
        )
    }) || optimized.iter().any(|op| {
        matches!(
            op,
            IlOp::BinSlotSlot { op, a: 0, b: 0, .. } if *op == common::Instruction::MUL as u8
        )
    });
    assert!(!squared, "fact multiply collapsed into slot0 * slot0\n{rendered}");
    assert!(
        rendered.contains("MUL"),
        "fact multiply disappeared\n{rendered}"
    );
}

#[test]
fn stats_off_does_not_record() {
    begin_opt_stats();
    let mut ops = vec![
        c(1),
        IlOp::Dup { loc: loc() },
        IlOp::Pop { loc: loc() },
        ret(),
    ];
    let mut opts = OptLevel::None.options();
    opts.stack_dce = true;
    opts.collect_stats = false;
    optimize(&mut ops, &opts, &mut Vec::new());
    let stats = last_opt_stats();
    assert_eq!(stats, OptStats::default());
}

#[test]
fn emitting_range_does_not_steal_prefix_jump_end_label() {
    let loc = loc();
    let ops = vec![
        label(1),
        IlOp::Jump {
            kind: IlJumpKind::JumpIfFalse,
            target: Label(8),
            loc,
            hint: Default::default(),
        },
        ret(),
        label(8),
        label(2),
        c(0),
        ret(),
    ];
    // emitting: JMPF, RET | CONST, RET
    let (s0, e0) = emitting_range_to_raw(&ops, 0, 2);
    let (s1, e1) = emitting_range_to_raw(&ops, 2, 4);
    assert!(
        ops[s0..e0]
            .iter()
            .any(|op| matches!(op, IlOp::Label(Label(8))))
            || s1 > s0 && ops[e0..s1].iter().any(|op| matches!(op, IlOp::Label(Label(8)))),
        "label 8 must stay with pred or the gap, not hot"
    );
    assert!(
        !ops[s1..e1]
            .iter()
            .any(|op| matches!(op, IlOp::Label(Label(8)))),
        "hot span must not include pred's trailing if-end"
    );
}
