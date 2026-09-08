//! Try to replace a numeric IL body with dense MIR bytecode, or a
//! two-slot / niche leaf with MIR→LIR.

use common::Instruction;

use crate::il::IlOp;

use super::abi::{DenseAbi, DenseCallMap};
use super::emit::emit_dense;
use super::emit_lir::emit_lir;
use super::infer::{infer_lir, infer_numeric_with};
use super::lower::{LowerHints, try_lower_numeric};

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
    // Allowlisted HostInvoke (math / packed LA / simd_axpy_reduce) is W4.
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

/// Leaf two-slot helper: SSA then fuse-IL (shipped ABI).
///
/// Dense stays off (`infer_numeric` still refuses `ret_words == 2`).
/// Production `IlModule` replace uses this after stack-IL opts; `emit_lir`
/// keeps single-use return/cmp values on the stack. Do not re-opt the
/// reconstruct (`MOD` rematerializes). Callers that `CALL` / host / box
/// stay on fuse-IL unless the callee is dense (COI-291).
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
    hints.allow_match = true;
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
    let mut match_shaped = false;
    let mut jim = false;
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
            | IlOp::Print { .. } => return false,
            IlOp::Jump {
                kind: crate::il::IlJumpKind::JumpIfMatch { tag, arity },
                ..
            } => {
                if *tag > 1 || *arity > 1 {
                    return false;
                }
                jim = true;
                match_shaped = true;
            }
            IlOp::Byte { byte, .. } if *byte.bytecode() == Instruction::Unpack => {
                if byte.operand_u32() > 1 {
                    return false;
                }
                match_shaped = true;
            }
            _ => {}
        }
    }
    ret2 || jim || match_shaped || adjacent_match_probe(ops)
}

/// Niche `DUP; LogNot; JMPx` or two-slot `DUP; CONST 0|1; EQ; JMPx`.
/// Whole-body `LogNot` + `DUP` is too wide (`if !flag { break }`).
fn adjacent_match_probe(ops: &[IlOp]) -> bool {
    let solid: Vec<&IlOp> = ops
        .iter()
        .filter(|op| !matches!(op, IlOp::Label(_) | IlOp::JoinLabel(_)))
        .collect();
    for w in solid.windows(3) {
        if matches!(w[0], IlOp::Dup { .. })
            && matches!(w[1], IlOp::LogNot { .. })
            && is_cond_jump(w[2])
        {
            return true;
        }
    }
    for w in solid.windows(4) {
        if matches!(w[0], IlOp::Dup { .. })
            && is_tag_imm(w[1])
            && is_eq_bin(w[2])
            && is_cond_jump(w[3])
        {
            return true;
        }
        if matches!(w[0], IlOp::Dup { .. })
            && is_tag_imm(w[1])
            && is_bitand(w[2])
            && is_cond_jump(w[3])
        {
            return true;
        }
    }
    false
}

fn is_cond_jump(op: &IlOp) -> bool {
    matches!(
        op,
        IlOp::Jump {
            kind: crate::il::IlJumpKind::JumpIfFalse | crate::il::IlJumpKind::JumpIfTrue,
            ..
        }
    )
}

fn is_tag_imm(op: &IlOp) -> bool {
    matches!(op, IlOp::Const { imm: 0 | 1, .. })
}

fn is_bitand(op: &IlOp) -> bool {
    match op {
        IlOp::Bin { op, .. } => *op == Instruction::BITAND,
        IlOp::BinSlotImm { op, .. } | IlOp::BinSlotSlot { op, .. } => {
            Instruction::from(*op) == Instruction::BITAND
        }
        _ => false,
    }
}

fn is_eq_bin(op: &IlOp) -> bool {
    match op {
        IlOp::Bin { op, .. } => matches!(*op, Instruction::EQ | Instruction::NEQ),
        IlOp::BinSlotImm { op, .. } | IlOp::BinSlotSlot { op, .. } => {
            matches!(Instruction::from(*op), Instruction::EQ | Instruction::NEQ)
        }
        _ => false,
    }
}
