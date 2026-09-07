//! Try to replace a numeric IL body with dense MIR bytecode, or a
//! two-slot / niche leaf with MIR→LIR.

use crate::il::IlOp;

use super::emit::emit_dense;
use super::emit_lir::emit_lir;
use super::infer::{infer_lir, infer_numeric};
use super::lower::{LowerHints, try_lower_numeric};

/// If `ops` is a specialized numeric loop, return dense IL (Value ABI at edges).
///
/// CSE → LICM → CSE → InstCombine (incl. P11 float peeps) → DestProp → SR → CSE → GVN/PRE,
/// then saxpy-reduce HostInvoke (P12) or dense emit.
pub fn try_specialize_body(
    ops: &[IlOp],
    name: &str,
    entry_sp: u32,
    pool: &mut Vec<u64>,
) -> Option<Vec<IlOp>> {
    // Nested / multi-header numeric loops are eligible (flagship mandelbrot).
    // Infer requires a back-edge plus float +/−/×/÷, counted i64 +/−/×/÷/%,
    // or i32. Heap / CALL / multi-word RETURN stay refuse (see specialize-refuse).
    let inferred = infer_numeric(ops, pool.len(), entry_sp).ok()?;
    if !inferred.has_float_arith && !inferred.has_i32 && !inferred.has_i64_arith {
        return None;
    }
    let mut hints = LowerHints::new(name);
    hints.slot_ty = inferred.slot_ty;
    hints.pool = pool.clone();
    hints.pool_ty = inferred.pool_ty;
    hints.param_count = entry_sp;
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
    let entry = ops.iter().find_map(|op| match op {
        IlOp::Label(l) | IlOp::JoinLabel(l) => Some(*l),
        _ => None,
    });
    if let Some(packed) = super::pack::try_axpy_pack(&func, entry, pool) {
        return Some(packed);
    }
    emit_dense(&func, entry, pool).ok()
}

/// Leaf two-slot helper: SSA then fuse-IL (shipped ABI).
///
/// Dense stays off (`infer_numeric` still refuses `ret_words == 2`).
/// Production `IlModule` replace uses this after stack-IL opts; `emit_lir`
/// keeps single-use return/cmp values on the stack. Do not re-opt the
/// reconstruct (`MOD` rematerializes). Callers that `CALL` / host / box
/// stay on fuse-IL.
pub fn try_lower_abi_body(
    ops: &[IlOp],
    name: &str,
    entry_sp: u32,
    pool: &mut Vec<u64>,
) -> Option<Vec<IlOp>> {
    if !abi_leaf(ops) {
        return None;
    }
    let inferred = infer_lir(ops, pool.len(), entry_sp).ok()?;
    let mut hints = LowerHints::new(name);
    hints.slot_ty = inferred.slot_ty;
    hints.pool = pool.clone();
    hints.pool_ty = inferred.pool_ty;
    hints.param_count = entry_sp;
    let mut func = try_lower_numeric(ops, &hints).ok()?;
    crate::mir::cse(&mut func);
    let entry = ops.iter().find_map(|op| match op {
        IlOp::Label(l) | IlOp::JoinLabel(l) => Some(*l),
        _ => None,
    });
    emit_lir(&func, entry, pool).ok()
}

fn abi_leaf(ops: &[IlOp]) -> bool {
    let mut ret2 = false;
    for op in ops {
        match op {
            IlOp::Return { ret_words, .. } if *ret_words >= 2 => ret2 = true,
            IlOp::Entry { .. }
            | IlOp::HostInvoke { .. }
            | IlOp::MakeEnum { .. }
            | IlOp::MakeTuple { .. }
            | IlOp::GetField { .. }
            | IlOp::SetField { .. }
            | IlOp::LoadField { .. }
            | IlOp::BoxValue { .. }
            | IlOp::UnboxValue { .. }
            | IlOp::Index { .. }
            | IlOp::String { .. }
            | IlOp::Print { .. }
            | IlOp::Jump {
                kind: crate::il::IlJumpKind::JumpIfMatch { .. },
                ..
            } => return false,
            _ => {}
        }
    }
    ret2
}
