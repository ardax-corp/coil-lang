//! Try to replace a numeric IL body with dense MIR bytecode, or lift
//! an eligible leftover body through MIR→LIR (I8).

use crate::il::IlOp;

use super::abi::{DenseAbi, DenseCallMap};
use super::emit::emit_dense;
use super::emit_lir::emit_lir;
use super::entry::lir_eligible;
use super::infer::{infer_lir, infer_numeric_with};
use super::lower::{try_lower_numeric, LowerHints};

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
    // Heap / CALL to a non-dense callee / multi-word RETURN stay refuse.
    // FORMAT / string ops stay fuse-IL (I4). Alloc / InitTyped stay fuse-IL
    // (I5: S2b maps attach; specialize across GC is S2c). Allowlisted HostInvoke (math / packed LA /
    // simd_axpy_reduce) is W4. Impure HostInvoke / CALL stay barriers (I6);
    // the W4 set is not grown for clocks / IO / FFI. Debugger-attached /
    // -Og skip this entry (I7; `OptimizeOptions::mir_specialize`).
    let inferred = infer_numeric_with(ops, pool.len(), entry_sp, calls).ok()?;
    if !inferred.has_float_arith && !inferred.has_i32 && !inferred.has_i64_arith {
        return None;
    }
    let mut hints = LowerHints::new(name);
    hints.slot_ty = inferred.slot_ty;
    hints.pool = pool.clone();
    hints.pool_ty = inferred.pool_ty;
    hints.calls = calls.clone();
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
    let abi = DenseAbi::from_func_and_live_ins(&func, ops, &hints.slot_ty)?;
    let entry = ops.iter().find_map(|op| match op {
        IlOp::Label(l) | IlOp::JoinLabel(l) => Some(*l),
        _ => None,
    });
    if let Some(packed) = super::pack::try_axpy_pack(&func, entry, pool) {
        return Some((packed, abi));
    }
    Some((emit_dense(&func, entry, pool).ok()?, abi))
}

/// IL→MIR→LIR for a leftover body after dense specialize misses (I8).
///
/// Dense stays off (`infer_numeric` still refuses `ret_words == 2` and
/// below-W3 / compare-only). Production `IlModule` replace uses this after
/// stack-IL opts; `emit_lir` keeps single-use return/cmp values on the
/// stack. Do not re-opt the reconstruct (`MOD` rematerializes). Call /
/// host / box / I4–I5 stay fuse-IL ([`super::entry::lir_eligible`]).
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
    if !lir_eligible(ops, unboxed_fields) {
        return None;
    }
    let inferred = infer_lir(ops, pool.len(), entry_sp).ok()?;
    let mut hints = LowerHints::new(name);
    hints.slot_ty = inferred.slot_ty;
    hints.pool = pool.clone();
    hints.pool_ty = inferred.pool_ty;
    hints.param_count = entry_sp;
    hints.allow_match = true;
    hints.unboxed_fields = unboxed_fields.to_vec();
    hints.allow_fields = !unboxed_fields.is_empty();
    let mut func = try_lower_numeric(ops, &hints).ok()?;
    crate::mir::cse(&mut func);
    let entry = ops.iter().find_map(|op| match op {
        IlOp::Label(l) | IlOp::JoinLabel(l) => Some(*l),
        _ => None,
    });
    emit_lir(&func, entry, pool).ok()
}
