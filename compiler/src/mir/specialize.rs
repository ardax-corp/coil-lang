//! Try to replace a numeric IL body with dense MIR bytecode.

use crate::il::IlOp;

use super::emit::emit_dense;
use super::infer::infer_numeric;
use super::lower::{LowerHints, try_lower_numeric};

/// If `ops` is a specialized numeric loop, return dense IL (Value ABI at edges).
pub fn try_specialize_body(
    ops: &[IlOp],
    name: &str,
    entry_sp: u32,
    pool: &mut Vec<u64>,
) -> Option<Vec<IlOp>> {
    let inferred = infer_numeric(ops, pool.len(), entry_sp).ok()?;
    if !inferred.has_fmul && !inferred.has_i32 {
        return None;
    }
    let mut hints = LowerHints::new(name);
    hints.slot_ty = inferred.slot_ty;
    hints.pool = pool.clone();
    hints.pool_ty = inferred.pool_ty;
    hints.param_count = entry_sp;
    let func = try_lower_numeric(ops, &hints).ok()?;
    let entry = ops.iter().find_map(|op| match op {
        IlOp::Label(l) | IlOp::JoinLabel(l) => Some(*l),
        _ => None,
    });
    emit_dense(&func, entry, pool).ok()
}
