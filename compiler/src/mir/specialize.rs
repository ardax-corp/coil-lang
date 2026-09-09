//! Try to replace a numeric IL body with dense MIR bytecode, or lift
//! an eligible leftover body through MIR→LIR (I8).

use common::Instruction;

use crate::il::{IlOp, Label};

use super::abi::{DenseAbi, DenseCallMap};
use super::emit::emit_dense;
use super::emit_lir::emit_lir;
use super::entry::lir_eligible_with;
use super::gc::refuses_alloc;
use super::infer::{infer_lir, infer_lir_across_alloc, infer_numeric_across_alloc, infer_numeric_with};
use super::lower::{try_lower_numeric, LowerHints};
use super::stackmap::has_real_maps;

/// `official_entry` is `IlFunc.meta.entry` (CALL target). New labels must
/// not reuse that id or concat lands calls on a loop header.
///
/// If `ops` is a specialized numeric body, return dense IL plus its ABI.
///
/// CSE → LICM → CSE → InstCombine (incl. P11 float peeps) → DestProp → SR → CSE → GVN/PRE,
/// then saxpy-reduce HostInvoke (P12) or dense emit (W4 allowlisted
/// HostInvoke edges box → call → unbox; COI-291 dense→dense `CALL` when
/// `calls` lists the callee).
pub fn try_specialize_body(
    ops: &[IlOp],
    name: &str,
    entry_sp: u32,
    pool: &mut Vec<u64>,
    calls: &DenseCallMap,
    official_entry: Option<Label>,
) -> Option<(Vec<IlOp>, DenseAbi)> {
    // Nested / multi-header numeric loops are eligible (flagship mandelbrot).
    // Infer requires float +/−/×/÷, counted i64 +/−/×/÷/%, or i32, plus a
    // back-edge or a straight-line body at/above STRAIGHT_LINE_MIN_WORK_OPS.
    // S3/S3b: one-word CALL (dense map or open), I6 HostInvoke except I4
    // string bytes, heap index / ArrayLen / StoreIndex (dense residuals
    // after V*). FORMAT / string ops stay fuse-IL (I4). Match stays LIR.
    // Alloc / InitTyped take dense only when S2b maps exist (S2c).
    // S2d: mapped *preheader* Make* + index loop may take dense.
    // S2e: residuals no longer Seek-restore, but in-loop Make* is still
    // ~13–17% slower than fuse-IL (LOAD/STORE boxing). Default refuse.
    // `COIL_S2D_DENSE_INLOOP=1` re-enables dense for A/B.
    // Post-loop-only `return [x]` stays fuse-IL (COI-87 invert+fuse).
    // Debugger-attached / -Og skip this entry (I7).
    // S2k: slot-select diamonds may take dense when Seek fits the 64-slot
    // prove frame and last-arm writes survive reconstruct.
    let select_cfg = has_sroa_select_cfg(ops);
    let has_alloc = ops.iter().any(refuses_alloc);
    let force_dense_inloop = std::env::var_os("COIL_S2D_DENSE_INLOOP")
        .is_some_and(|v| v != "0");
    if (!force_dense_inloop && super::infer::has_alloc_inside_loop(ops))
        || (has_alloc && super::infer::has_alloc_only_after_loops(ops))
    {
        return None;
    }
    if has_alloc && !has_real_maps(ops, name, entry_sp, pool, &[]) {
        return None;
    }
    let inferred = if has_alloc {
        infer_numeric_across_alloc(ops, pool.len(), entry_sp, calls).ok()?
    } else {
        infer_numeric_with(ops, pool.len(), entry_sp, calls).ok()?
    };
    if !inferred.has_float_arith && !inferred.has_i32 && !inferred.has_i64_arith {
        return None;
    }
    let mut hints = LowerHints::new(name);
    hints.slot_ty = inferred.slot_ty;
    hints.pool = pool.clone();
    hints.pool_ty = inferred.pool_ty;
    hints.calls = calls.clone();
    hints.allow_alloc = has_alloc;
    hints.allow_index = true;
    hints.allow_effects = true;
    let _heap_index = super::infer::has_heap_index(ops);
    let live_params = super::abi::live_in_params(ops, &hints.slot_ty);
    hints.param_count = live_params
        .as_ref()
        .map(|p| p.len() as u32)
        .unwrap_or(entry_sp)
        .max(entry_sp);
    let mut func = try_lower_numeric(ops, &hints).ok()?;
    // Stack-IL CSE refuses DIVF; number it on SSA before dense emit.
    crate::mir::cse(&mut func);
    crate::mir::licm(&mut func);
    crate::mir::cse(&mut func);
    crate::mir::instcombine(&mut func);
    crate::mir::destprop(&mut func);
    crate::mir::strength_reduce(&mut func);
    crate::mir::cse(&mut func);
    crate::mir::gvn(&mut func);
    // S2f: reuse the mutated array after StoreIndex (drop rematerialized Alloc).
    crate::mir::sroa(&mut func);
    paint_index_dest_from_uses(&mut func);
    let stores_ssa = func
        .blocks
        .iter()
        .flat_map(|b| b.insts.iter())
        .filter(|i| matches!(i, crate::mir::MirInst::StoreIndex { .. }))
        .count();
    if count_store_index(ops) > 0 && stores_ssa == 0 {
        return None;
    }
    let abi = DenseAbi::from_func_and_live_ins(&func, ops, &hints.slot_ty)?;
    let entry = official_entry.or_else(|| {
        ops.iter().find_map(|op| match op {
            IlOp::Label(l) | IlOp::JoinLabel(l) => Some(*l),
            _ => None,
        })
    });
    let label_hi = crate::il::opt::max_code_label(ops)
        .max(official_entry.map(|Label(id)| id).unwrap_or(0));
    if let Some(packed) = super::pack::try_axpy_pack(&func, entry, pool) {
        return Some((packed, abi));
    }
    if let Some(vecd) = super::vectorize::try_vectorize(&func, entry, pool, label_hi) {
        return Some((vecd, abi));
    }
    let out = emit_dense(&func, entry, pool, has_alloc).ok()?;
    // Heap writes have no SSA users; refuse if reconstruct dropped one.
    if count_store_index(&out) < count_store_index(ops) {
        return None;
    }
    // Const-fold must not erase every Index (for-in / invert+fuse leftover).
    if count_index(ops) > 0 && count_index(&out) == 0 {
        return None;
    }
    if select_cfg && !select_reconstruct_ok(ops, &out) {
        return None;
    }
    Some((out, abi))
}

/// Index / open CALL dests default to i64 when the next IL is StorePop.
/// Repaint from float uses so DenseBin / RETURN keep the Value bits.
fn paint_index_dest_from_uses(func: &mut crate::mir::func::MirFunc) {
    use crate::mir::inst::{MirInst, ValueId};
    let mut paint = Vec::new();
    for b in &func.blocks {
        for inst in &b.insts {
            match inst {
                MirInst::Index { dest, .. } | MirInst::Call { dest, .. } => {
                    paint.push(*dest);
                }
                _ => {}
            }
        }
    }
    if paint.is_empty() {
        return;
    }
    let mut ty_of = vec![None; func.types.len()];
    for b in &func.blocks {
        for inst in &b.insts {
            match inst {
                MirInst::Bin { ty, lhs, rhs, .. } if ty.is_float() => {
                    ty_of[lhs.index()] = Some(*ty);
                    ty_of[rhs.index()] = Some(*ty);
                }
                MirInst::StoreIndex { value, .. } => {
                    let vt = func.ty(*value);
                    if vt.is_float() {
                        ty_of[value.index()] = Some(vt);
                    }
                }
                _ => {}
            }
        }
    }
    for ValueId(id) in paint {
        if let Some(ty) = ty_of.get(id as usize).copied().flatten() {
            func.types[id as usize] = ty;
        }
    }
}

fn count_store_index(ops: &[IlOp]) -> usize {
    ops.iter()
        .filter(|op| match op {
            IlOp::StoreIndexPin { .. } | IlOp::StoreIndexPinUnchecked { .. } => true,
            IlOp::Byte { byte, .. } => matches!(
                *byte.bytecode(),
                Instruction::StoreIndex | Instruction::StoreIndexUnchecked
            ),
            _ => false,
        })
        .count()
}

fn count_index(ops: &[IlOp]) -> usize {
    ops.iter()
        .filter(|op| match op {
            IlOp::Index { .. }
            | IlOp::IndexUnchecked { .. }
            | IlOp::IndexPin { .. }
            | IlOp::IndexPinUnchecked { .. } => true,
            IlOp::Byte { byte, .. } => matches!(
                *byte.bytecode(),
                Instruction::Index | Instruction::IndexUnchecked
            ),
            _ => false,
        })
        .count()
}

/// IL→MIR→LIR for a leftover body after dense specialize misses (I8).
///
/// Dense stays off (`infer_numeric` still refuses `ret_words == 2` and
/// below-W3 / compare-only). Production `IlModule` replace uses this after
/// stack-IL opts; `emit_lir` keeps single-use return/cmp values on the
/// stack. Do not re-opt the reconstruct (`MOD` rematerializes). Call /
/// host / box / I4 stay fuse-IL. I5 alloc needs S2b maps ([`lir_eligible_with`]).
pub fn try_lower_abi_body(
    ops: &[IlOp],
    name: &str,
    entry_sp: u32,
    pool: &mut Vec<u64>,
) -> Option<Vec<IlOp>> {
    try_lower_abi_body_with(ops, name, entry_sp, pool, &[])
}

/// Like [`try_lower_abi_body`], with unboxed class field ranges from
/// codegen (`local_escape` → consecutive slots).
pub fn try_lower_abi_body_with(
    ops: &[IlOp],
    name: &str,
    entry_sp: u32,
    pool: &mut Vec<u64>,
    unboxed_fields: &[(u32, u32)],
) -> Option<Vec<IlOp>> {
    // I8: any inferable unfused body, not only two-slot / match / field accidents.
    // S2c: allocating leftovers need a real S2b draft; else fuse-IL.
    // S2d: mapped in-loop / preheader Make* may reconstruct; post-loop-only
    // `return [x]` stays fuse-IL so invert+fuse (COI-87) remains.
    let has_alloc = ops.iter().any(refuses_alloc);
    if has_alloc && super::infer::has_alloc_only_after_loops(ops) {
        return None;
    }
    let maps_ok =
        has_alloc && has_real_maps(ops, name, entry_sp, pool, unboxed_fields);
    if !lir_eligible_with(ops, unboxed_fields, maps_ok) {
        return None;
    }
    let inferred = if has_alloc {
        infer_lir_across_alloc(ops, pool.len(), entry_sp).ok()?
    } else {
        infer_lir(ops, pool.len(), entry_sp).ok()?
    };
    let mut hints = LowerHints::new(name);
    hints.slot_ty = inferred.slot_ty;
    hints.pool = pool.clone();
    hints.pool_ty = inferred.pool_ty;
    hints.param_count = entry_sp;
    hints.allow_match = true;
    hints.unboxed_fields = unboxed_fields.to_vec();
    hints.allow_fields = !unboxed_fields.is_empty();
    hints.allow_alloc = has_alloc;
    hints.allow_index = true;
    let mut func = try_lower_numeric(ops, &hints).ok()?;
    if has_alloc {
        crate::mir::sroa(&mut func);
    } else {
        crate::mir::cse(&mut func);
    }
    let entry = ops.iter().find_map(|op| match op {
        IlOp::Label(l) | IlOp::JoinLabel(l) => Some(*l),
        _ => None,
    });
    let out = emit_lir(&func, entry, pool, has_alloc).ok()?;
    if has_alloc {
        let before = ops.iter().filter(|o| refuses_alloc(o)).count();
        let after = out.iter().filter(|o| refuses_alloc(o)).count();
        if after < before {
            return None;
        }
    }
    if has_sroa_select_cfg(ops) && !select_reconstruct_ok(ops, &out) {
        return None;
    }
    Some(out)
}

/// Prove-frame cap used by `Machine::<64>` MIR tests and S2k accept.
const SELECT_OPERAND_STACK_CAP: u32 = 64;

/// Seek must fit the 64-slot prove frame. Last-arm writes must remain as
/// `StorePop` (LIR) or `DenseMove` (dense phi copies).
fn select_reconstruct_ok(src: &[IlOp], out: &[IlOp]) -> bool {
    if max_seek(out) > SELECT_OPERAND_STACK_CAP {
        return false;
    }
    last_arm_writes(out) >= last_arm_writes(src)
}

fn max_seek(ops: &[IlOp]) -> u32 {
    ops.iter()
        .filter_map(|op| match op {
            IlOp::Byte { byte, .. } if *byte.bytecode() == Instruction::Seek => {
                Some(byte.operand_u32())
            }
            _ => None,
        })
        .max()
        .unwrap_or(0)
}

fn last_arm_writes(ops: &[IlOp]) -> usize {
    let mut n = 0usize;
    for (i, op) in ops.iter().enumerate() {
        if !is_select_write(op) {
            continue;
        }
        if is_last_arm_write(ops, i) {
            n += 1;
        }
    }
    n
}

fn is_select_write(op: &IlOp) -> bool {
    matches!(op, IlOp::StorePop { .. })
        || matches!(
            op,
            IlOp::Byte { byte, .. }
                if matches!(
                    *byte.bytecode(),
                    Instruction::STORE | Instruction::StorePop | Instruction::DenseMove
                )
        )
}

/// A last arm falls into a join: write, then only labels until a join bind
/// (no intervening jump).
fn is_last_arm_write(ops: &[IlOp], write_i: usize) -> bool {
    let mut saw_join = false;
    for op in &ops[write_i + 1..] {
        match op {
            IlOp::Label(_) | IlOp::JoinLabel(_) => saw_join = true,
            IlOp::Jump { .. } | IlOp::Return { .. } | IlOp::Halt { .. } => return false,
            _ if saw_join => return true,
            _ => {}
        }
    }
    saw_join
}

/// S2f computed-index slot-select: several EQ/JMPF arms into one join.
fn has_sroa_select_cfg(ops: &[IlOp]) -> bool {
    use crate::il::IlJumpKind;
    let mut n = 0usize;
    for (i, op) in ops.iter().enumerate() {
        match op {
            IlOp::Jump {
                kind: IlJumpKind::JumpIfFalse | IlJumpKind::JumpIfTrue,
                ..
            } if eq_immediately_before(ops, i) => n += 1,
            IlOp::Byte { byte, .. } if fused_eq_jmp(byte) => n += 1,
            _ => {}
        }
    }
    n >= 2
}

fn eq_immediately_before(ops: &[IlOp], jump_i: usize) -> bool {
    let mut i = jump_i;
    while i > 0 {
        i -= 1;
        match &ops[i] {
            IlOp::Label(_) | IlOp::JoinLabel(_) => continue,
            other => return is_eq_byte(other),
        }
    }
    false
}

fn is_eq_byte(op: &IlOp) -> bool {
    matches!(
        op,
        IlOp::Byte { byte, .. } if *byte.bytecode() == Instruction::EQ
    ) || matches!(op, IlOp::Bin { op: Instruction::EQ, .. })
}

fn fused_eq_jmp(byte: &common::Byte) -> bool {
    match *byte.bytecode() {
        Instruction::BinSlotImmJmpf | Instruction::BinSlotImmJmpt => {
            byte.bin_slot_imm_jmpf_parts().0 == Instruction::EQ as u8
        }
        Instruction::BinSlotSlotJmpf | Instruction::BinSlotSlotJmpt => {
            byte.bin_slot_slot_jmpf_parts().0 == Instruction::EQ as u8
        }
        _ => false,
    }
}
