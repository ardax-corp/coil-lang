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
        slot_promote: false,
        canon: false,
        cast_spill: false,
        algebraic: false,
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
        escape_analysis: true,
                branch_optimization: false,
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
        IlOp::Return { loc: loc() },
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
fn keeps_heap_when_array_is_returned() {
    let mut ops = make_and_store(2, 0);
    ops.extend([
        IlOp::Load {
            slot: 0,
            loc: loc(),
        },
        IlOp::Return { loc: loc() },
    ]);
    let info = analyze_escapes(&ops);
    assert!(!is_stack_allocatable(&info.allocs[0]));
    allocate_on_stack(&mut ops, &info);
    assert!(has_make_array(&ops));
}

#[test]
fn keeps_heap_when_passed_to_call() {
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
        },
        IlOp::Pop { loc: loc() },
        IlOp::Halt { loc: loc() },
    ]);
    let info = analyze_escapes(&ops);
    assert!(!is_stack_allocatable(&info.allocs[0]));
    assert!(has_make_array(&ops));
}

#[test]
fn keeps_heap_when_stored_to_field() {
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
        IlOp::SetField { loc: loc() },
        IlOp::Pop { loc: loc() },
        IlOp::Halt { loc: loc() },
    ]);
    let info = analyze_escapes(&ops);
    assert!(!is_stack_allocatable(&info.allocs[0]));
}

#[test]
fn keeps_heap_through_nested_call() {
    let mut ops = make_and_store(1, 0);
    ops.extend([
        IlOp::Load {
            slot: 0,
            loc: loc(),
        },
        IlOp::HostInvoke {
            arity: 1,
            loc: loc(),
        },
        IlOp::Pop { loc: loc() },
        IlOp::Halt { loc: loc() },
    ]);
    assert!(!is_stack_allocatable(&analyze_escapes(&ops).allocs[0]));
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
        IlOp::Return { loc: loc() },
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
        IlOp::Return { loc: loc() },
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
        IlOp::Return { loc: loc() },
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
        IlOp::Return { loc: loc() },
    ]);
    let mut opts = isolated();
    opts.escape_analysis = false;
    optimize(&mut ops, &opts, &mut Vec::new());
    assert!(has_make_array(&ops));
}

#[test]
fn keeps_heap_when_elements_are_computed() {
    // Zip/broadcast results are MakeArray of ADDs — fail-closed, stay heap.
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
        IlOp::Return { loc: loc() },
    ];
    assert!(!is_stack_allocatable(&analyze_escapes(&ops).allocs[0]));
    escape_analysis(&mut ops);
    assert!(has_make_array(&ops));
}
