use super::*;
use crate::il::op::{EntryKind, IlOp, Label};
use crate::il::opt::{OptimizeOptions, optimize};
use common::{Byte, DebugLoc, Instruction};

fn loc() -> DebugLoc {
    DebugLoc::unknown()
}

fn make_and_store(arity: u32, slot: u32) -> Vec<IlOp> {
    let mut ops = Vec::new();
    for i in 0..arity {
        ops.push(IlOp::Const {
            imm: (i + 1) as i32,
            loc: loc(),
        });
    }
    ops.push(IlOp::MakeArray { arity, loc: loc() });
    ops.push(IlOp::StorePop { slot, loc: loc() });
    ops
}

fn isolated() -> OptimizeOptions {
    OptimizeOptions {
        jump_thread: false,
        dead_block: false,
        stack_dce: false,
        mem_fwd: false,
        copy_prop: false,
        dest_prop: false,
        slot_promote: false,
        tos_carry: false,
        canon: false,
        cast_spill: false,
        algebraic: false,
        instcombine: false,
        local_cse: false,
        licm: false,
        loop_bounds: false,
        strength_reduce: false,
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
        escape_analysis: true,
                branch_optimization: false,
                block_reordering: false,
                iterative_optimization: false,
                max_optimization_iterations: 10,
                collect_stats: false,
                pure_call_ctx: None,
                mir_specialize: false,
    }
}

fn has_make_array(ops: &[IlOp]) -> bool {
    ops.iter().any(|op| matches!(op, IlOp::MakeArray { .. }))
}

#[test]
fn scalarizes_non_escaping_index() {
    let mut ops = make_and_store(2, 0);
    ops.extend([
        IlOp::Load {
            slot: 0,
            loc: loc(),
        },
        IlOp::Const { imm: 0, loc: loc() },
        IlOp::Index { loc: loc() },
        IlOp::Return { loc: loc(), ret_words: 1},
    ]);
    let info = analyze_escapes(&ops);
    assert!(is_stack_allocatable(&info.allocs[0]));
    allocate_on_stack(&mut ops, &info);
    assert!(!has_make_array(&ops));
    assert!(
        ops.iter()
            .any(|op| matches!(op, IlOp::Load { slot, .. } if *slot > 0)),
        "element should load from a scalarized slot"
    );
}

#[test]
fn boxes_at_return_edge() {
    let mut ops = make_and_store(2, 0);
    ops.extend([
        IlOp::Load {
            slot: 0,
            loc: loc(),
        },
        IlOp::Const { imm: 0, loc: loc() },
        IlOp::Index { loc: loc() },
        IlOp::Pop { loc: loc() },
        IlOp::Load {
            slot: 0,
            loc: loc(),
        },
        IlOp::Return { loc: loc(), ret_words: 1},
    ]);
    let info = analyze_escapes(&ops);
    assert!(info.allocs[0].box_at_escape);
    assert!(is_stack_allocatable(&info.allocs[0]));
    allocate_on_stack(&mut ops, &info);
    let makes = ops
        .iter()
        .filter(|op| matches!(op, IlOp::MakeArray { .. }))
        .count();
    assert_eq!(makes, 1, "one box at return");
    assert!(
        matches!(ops.last(), Some(IlOp::Return { .. })),
        "MakeArray stays on the return edge"
    );
}

#[test]
fn boxes_at_call_arg_edge() {
    let mut ops = make_and_store(2, 0);
    ops.extend([
        IlOp::Load {
            slot: 0,
            loc: loc(),
        },
        IlOp::Entry {
            kind: EntryKind::Call,
            arity: 1,
            target: Label(9),
            loc: loc(),
            ret_words: 1,
        },
        IlOp::Pop { loc: loc() },
        IlOp::Halt { loc: loc() },
    ]);
    let info = analyze_escapes(&ops);
    assert!(info.allocs[0].box_at_escape);
    assert!(is_stack_allocatable(&info.allocs[0]));
    allocate_on_stack(&mut ops, &info);
    let makes = ops
        .iter()
        .filter(|op| matches!(op, IlOp::MakeArray { .. }))
        .count();
    assert_eq!(makes, 1, "one box at call");
}

#[test]
fn boxes_at_field_store_edge() {
    let mut ops = make_and_store(2, 1);
    ops.extend([
        IlOp::Load {
            slot: 0,
            loc: loc(),
        },
        IlOp::String { idx: 0, loc: loc() },
        IlOp::Load {
            slot: 1,
            loc: loc(),
        },
        IlOp::SetField {
            loc: loc(),
            index: None,
        },
        IlOp::Pop { loc: loc() },
        IlOp::Halt { loc: loc() },
    ]);
    let info = analyze_escapes(&ops);
    assert!(info.allocs[0].box_at_escape);
    assert!(is_stack_allocatable(&info.allocs[0]));
    allocate_on_stack(&mut ops, &info);
    assert_eq!(
        ops.iter()
            .filter(|op| matches!(op, IlOp::MakeArray { .. }))
            .count(),
        1
    );
}

#[test]
fn boxes_at_host_edge() {
    let mut ops = make_and_store(1, 0);
    ops.extend([
        IlOp::Load {
            slot: 0,
            loc: loc(),
        },
        IlOp::HostInvoke {
            arity: 1,
            layout: 0,
            loc: loc(),
        },
        IlOp::Pop { loc: loc() },
        IlOp::Halt { loc: loc() },
    ]);
    let info = analyze_escapes(&ops);
    assert!(info.allocs[0].box_at_escape);
    assert!(is_stack_allocatable(&info.allocs[0]));
}

#[test]
fn boxes_array_push_value_not_dest() {
    let mut ops = make_and_store(2, 0);
    ops.extend([
        IlOp::Load {
            slot: 1,
            loc: loc(),
        },
        IlOp::Load {
            slot: 0,
            loc: loc(),
        },
        IlOp::byte(Byte::new(Instruction::ArrayPush)),
        IlOp::Pop { loc: loc() },
        IlOp::Halt { loc: loc() },
    ]);
    let info = analyze_escapes(&ops);
    assert!(info.allocs[0].box_at_escape);
    allocate_on_stack(&mut ops, &info);
    assert_eq!(
        ops.iter()
            .filter(|op| matches!(op, IlOp::MakeArray { .. }))
            .count(),
        1
    );
}

#[test]
fn refuses_array_push_grow_dest() {
    let mut ops = make_and_store(2, 0);
    ops.extend([
        IlOp::Load {
            slot: 0,
            loc: loc(),
        },
        IlOp::Const { imm: 9, loc: loc() },
        IlOp::byte(Byte::new(Instruction::ArrayPush)),
        IlOp::Pop { loc: loc() },
        IlOp::Halt { loc: loc() },
    ]);
    let info = analyze_escapes(&ops);
    assert!(!info.allocs[0].box_at_escape);
    assert!(!is_stack_allocatable(&info.allocs[0]));
}

#[test]
fn refuses_private_use_after_escape() {
    let mut ops = make_and_store(2, 0);
    ops.extend([
        IlOp::Load {
            slot: 0,
            loc: loc(),
        },
        IlOp::Entry {
            kind: EntryKind::Call,
            arity: 1,
            target: Label(9),
            loc: loc(),
            ret_words: 1,
        },
        IlOp::Pop { loc: loc() },
        IlOp::Load {
            slot: 0,
            loc: loc(),
        },
        IlOp::Const { imm: 0, loc: loc() },
        IlOp::Index { loc: loc() },
        IlOp::Return {
            loc: loc(),
            ret_words: 1,
        },
    ]);
    let info = analyze_escapes(&ops);
    assert!(!is_stack_allocatable(&info.allocs[0]));
}

#[test]
fn scalarizes_len_of_non_escaping_array() {
    let mut ops = make_and_store(3, 0);
    ops.extend([
        IlOp::Load {
            slot: 0,
            loc: loc(),
        },
        IlOp::byte(Byte::new(Instruction::ArrayLen)),
        IlOp::Return { loc: loc(), ret_words: 1},
    ]);
    escape_analysis(&mut ops);
    assert!(!has_make_array(&ops));
    assert!(
        ops.iter()
            .any(|op| matches!(op, IlOp::Const { imm: 3, .. }))
    );
}

#[test]
fn scalarizes_const_store_index() {
    let mut ops = make_and_store(2, 0);
    ops.extend([
        IlOp::Load {
            slot: 0,
            loc: loc(),
        },
        IlOp::Const { imm: 1, loc: loc() },
        IlOp::Const { imm: 9, loc: loc() },
        IlOp::byte(Byte::new(Instruction::StoreIndex)),
        IlOp::Pop { loc: loc() },
        IlOp::Load {
            slot: 0,
            loc: loc(),
        },
        IlOp::Const { imm: 1, loc: loc() },
        IlOp::Index { loc: loc() },
        IlOp::Return { loc: loc(), ret_words: 1},
    ]);
    escape_analysis(&mut ops);
    assert!(!has_make_array(&ops));
    assert!(!ops.iter().any(|op| {
        op.as_plain_byte()
            .is_some_and(|b| *b.bytecode() == Instruction::StoreIndex)
    }));
}

#[test]
fn isolated_optimize_flag_runs_pass() {
    let mut ops = make_and_store(2, 0);
    ops.extend([
        IlOp::Load {
            slot: 0,
            loc: loc(),
        },
        IlOp::Const { imm: 1, loc: loc() },
        IlOp::Index { loc: loc() },
        IlOp::Return { loc: loc(), ret_words: 1},
    ]);
    optimize(&mut ops, &isolated(), &mut Vec::new());
    assert!(!has_make_array(&ops));
}

#[test]
fn isolated_optimize_off_leaves_make_array() {
    let mut ops = make_and_store(2, 0);
    ops.extend([
        IlOp::Load {
            slot: 0,
            loc: loc(),
        },
        IlOp::Const { imm: 0, loc: loc() },
        IlOp::Index { loc: loc() },
        IlOp::Return { loc: loc(), ret_words: 1},
    ]);
    let mut opts = isolated();
    opts.escape_analysis = false;
    optimize(&mut ops, &opts, &mut Vec::new());
    assert!(has_make_array(&ops));
}

#[test]
fn keeps_heap_for_unproven_index() {
    // S2h: leftover MakeArray + `xs[k]` stays a heap object (checked Index).
    let mut ops = make_and_store(2, 0);
    ops.extend([
        IlOp::Load {
            slot: 0,
            loc: loc(),
        },
        IlOp::Load {
            slot: 1,
            loc: loc(),
        },
        IlOp::Index { loc: loc() },
        IlOp::Return {
            loc: loc(),
            ret_words: 1,
        },
    ]);
    assert!(!is_stack_allocatable(&analyze_escapes(&ops).allocs[0]));
    escape_analysis(&mut ops);
    assert!(has_make_array(&ops));
}

#[test]
fn scalarizes_private_computed_elems() {
    // S2i rule: non-escaping computed elems SROA into slots (not a vec_array refuse).
    let mut ops = vec![
        IlOp::Const { imm: 1, loc: loc() },
        IlOp::Const { imm: 3, loc: loc() },
        IlOp::Bin {
            op: Instruction::ADD,
            loc: loc(),
        },
        IlOp::Const { imm: 2, loc: loc() },
        IlOp::Const { imm: 4, loc: loc() },
        IlOp::Bin {
            op: Instruction::ADD,
            loc: loc(),
        },
        IlOp::MakeArray {
            arity: 2,
            loc: loc(),
        },
        IlOp::StorePop {
            slot: 0,
            loc: loc(),
        },
        IlOp::Load {
            slot: 0,
            loc: loc(),
        },
        IlOp::Const { imm: 0, loc: loc() },
        IlOp::Index { loc: loc() },
        IlOp::Return { loc: loc(), ret_words: 1},
    ];
    assert!(is_stack_allocatable(&analyze_escapes(&ops).allocs[0]));
    escape_analysis(&mut ops);
    assert!(!has_make_array(&ops));
}

#[test]
fn boxes_once_when_computed_elems_escape() {
    // S2i: observed zip boxes once at the escape (same Q1 rule as immediates).
    let mut ops = vec![
        IlOp::Const { imm: 1, loc: loc() },
        IlOp::Const { imm: 3, loc: loc() },
        IlOp::Bin {
            op: Instruction::ADD,
            loc: loc(),
        },
        IlOp::Const { imm: 2, loc: loc() },
        IlOp::Const { imm: 4, loc: loc() },
        IlOp::Bin {
            op: Instruction::ADD,
            loc: loc(),
        },
        IlOp::MakeArray {
            arity: 2,
            loc: loc(),
        },
        IlOp::StorePop {
            slot: 0,
            loc: loc(),
        },
        IlOp::Load {
            slot: 0,
            loc: loc(),
        },
        IlOp::Return {
            loc: loc(),
            ret_words: 1,
        },
    ];
    let info = analyze_escapes(&ops);
    assert!(is_stack_allocatable(&info.allocs[0]));
    assert!(info.allocs[0].box_at_escape);
    escape_analysis(&mut ops);
    let makes = ops
        .iter()
        .filter(|op| matches!(op, IlOp::MakeArray { .. }))
        .count();
    assert_eq!(makes, 1, "one box at return");
}

#[test]
fn boxes_once_across_two_escape_edges() {
    let mut ops = make_and_store(2, 0);
    ops.extend([
        IlOp::Load {
            slot: 0,
            loc: loc(),
        },
        IlOp::Entry {
            kind: EntryKind::Call,
            arity: 1,
            target: Label(9),
            loc: loc(),
            ret_words: 1,
        },
        IlOp::Pop { loc: loc() },
        IlOp::Load {
            slot: 0,
            loc: loc(),
        },
        IlOp::Return {
            loc: loc(),
            ret_words: 1,
        },
    ]);
    let info = analyze_escapes(&ops);
    assert!(info.allocs[0].box_at_escape);
    allocate_on_stack(&mut ops, &info);
    let makes = ops
        .iter()
        .filter(|op| matches!(op, IlOp::MakeArray { .. }))
        .count();
    assert_eq!(makes, 1, "Q1 box-once");
}
