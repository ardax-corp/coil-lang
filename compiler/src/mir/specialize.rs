//! Try to replace a numeric IL body with dense MIR bytecode, or lift
//! an eligible leftover body through MIR→LIR (I8).

use common::Instruction;

use crate::il::IlOp;

use super::abi::{DenseAbi, DenseCallMap};
use super::emit::emit_dense;
use super::emit_lir::emit_lir;
use super::entry::lir_eligible_with;
use super::gc::refuses_alloc;
use super::infer::{infer_lir, infer_lir_across_alloc, infer_numeric_across_alloc, infer_numeric_with};
use super::lower::{try_lower_numeric, LowerHints};
use super::stackmap::has_real_maps;

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
) -> Option<(Vec<IlOp>, DenseAbi)> {
    // Nested / multi-header numeric loops are eligible (flagship mandelbrot).
    // Infer requires float +/−/×/÷, counted i64 +/−/×/÷/%, or i32, plus a
    // back-edge or a straight-line body at/above STRAIGHT_LINE_MIN_WORK_OPS.
    // S3: one-word CALL (dense map or open), I6 HostInvoke except I4
    // string bytes, heap index / ArrayLen / StoreIndex. FORMAT / string
    // ops stay fuse-IL (I4). Match stays LIR (dense+match is unsafe).
    // Alloc / InitTyped take dense only when S2b maps exist (S2c).
    // Debugger-attached / -Og skip this entry (I7).
    let has_alloc = ops.iter().any(refuses_alloc);
    // Heap-index + DenseBin residuals stay unsound on `Vec`. V0 may still
    // take a closed `V*` rewrite (no dense+Index mix). Anything else stays
    // fuse-IL + invert+fuse (COI-87).
    let heap_index = super::infer::has_heap_index(ops);
    if super::infer::has_alloc_inside_loop(ops)
        || (has_alloc && super::infer::has_back_edge(ops))
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
    let entry = ops.iter().find_map(|op| match op {
        IlOp::Label(l) | IlOp::JoinLabel(l) => Some(*l),
        _ => None,
    });
    if let Some(packed) = super::pack::try_axpy_pack(&func, entry, pool) {
        return Some((packed, abi));
    }
    if let Some(vecd) = super::vectorize::try_vectorize(&func, entry, pool) {
        return Some((vecd, abi));
    }
    if heap_index {
        return None;
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
    Some((out, abi))
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
    // In-loop Make* stays fuse-IL so invert+fuse (COI-87) remains;
    // preheader alloc + leftover body may reconstruct when mapped.
    let has_alloc = ops.iter().any(refuses_alloc);
    if has_alloc && super::infer::has_back_edge(ops) {
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
    if !has_alloc {
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
    Some(out)
}
