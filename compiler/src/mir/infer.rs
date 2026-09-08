//! Infer specialized slot types from pre-fuse IL (refuse heap / Value).
//!
//! Dense eligibility: float `+/−/×/÷`, i64 `+/−/×/÷/%` (or int `INC`/`DEC`),
//! or unused `has_i32`, plus either a back-edge **or** a straight-line body
//! that meets [`STRAIGHT_LINE_MIN_WORK_OPS`] (W3). Infer refuses user `CALL`
//! unless the target is already in the dense ABI map (COI-291), plus
//! non-allowlisted HostInvoke / heap index / class field / match / string /
//! alloc (`MakeArray` / `InitTyped`) /
//! multi-word `RETURN` / residual `Byte` / `Pow` / `AND`/`OR`. W4 accepts
//! allowlisted math / packed LA / `simd_axpy_reduce` HostInvokes. I6 types
//! other hosts as SSA edges under `allow_effects`; dense infer stays W4.
//! Compare-only stays fuse-IL.

use std::collections::HashMap;

use common::Instruction;

use crate::il::{EntryKind, IlOp, Label};

use super::abi::DenseCallMap;
use super::host_allow::host_spec;
use super::lower::LowerError;
use super::gc::refuse_reason as alloc_refuse_reason;
use super::string_barrier::{is_format_inst, refuse_reason};
use super::ty::MirTy;

/// Minimum numeric work ops for a no-back-edge body to take dense specialize.
///
/// Loops amortize dense `Seek` + Value ABI at CALL/RETURN over many trips.
/// A straight-line helper pays that tax once per CALL. Two-to-four-op
/// helpers (`i + j * 2`) stay cheaper as fuse-IL. Eight work ops is past
/// that handful and is the smallest integer that still leaves `eval_a`-sized
/// kernels optional after stack-IL opts (below → refuse).
pub const STRAIGHT_LINE_MIN_WORK_OPS: usize = 8;

/// Bin / slot-bin plus residual INC/DEC/NEG/NEGF/`CastIntToFloat`.
/// Load / Store / Const / control do not count.
pub fn numeric_work_ops(ops: &[IlOp]) -> usize {
    ops.iter().filter(|op| is_numeric_work_op(op)).count()
}

#[derive(Clone, Copy)]
enum Origin {
    Slot(u32),
    Pool(u32),
    Tmp,
}

#[derive(Clone, Copy)]
struct Cell {
    origin: Origin,
    ty: Option<MirTy>,
    /// Inline `CONST` bits — used to read HostInvoke native ids.
    imm: Option<i64>,
}

/// Result of a successful numeric type walk.
pub struct Inferred {
    pub slot_ty: HashMap<u32, MirTy>,
    pub pool_ty: Vec<Option<MirTy>>,
    pub has_i32: bool,
    /// Float `+` / `-` / `*` / `/` (or float `INC`/`DEC`). Compare-only is false.
    pub has_float_arith: bool,
    /// Integer `+` / `-` / `*` / `/` / `%` (or int `INC`/`DEC`). Compare-only is false.
    pub has_i64_arith: bool,
}

pub fn infer_numeric(
    ops: &[IlOp],
    pool_len: usize,
    param_count: u32,
) -> Result<Inferred, LowerError> {
    infer_numeric_with(ops, pool_len, param_count, &DenseCallMap::new())
}

/// Like [`infer_numeric`], but one-word `CALL` to a mapped dense callee is ok.
pub fn infer_numeric_with(
    ops: &[IlOp],
    pool_len: usize,
    param_count: u32,
    calls: &DenseCallMap,
) -> Result<Inferred, LowerError> {
    infer_walk(
        ops,
        pool_len,
        param_count,
        InferMode::Dense,
        calls,
        &HashMap::new(),
    )
}

/// Slot types for MIR→LIR (two-slot / niche leafs). No loop required.
pub fn infer_lir(ops: &[IlOp], pool_len: usize, param_count: u32) -> Result<Inferred, LowerError> {
    infer_lir_with_seed(ops, pool_len, param_count, &HashMap::new())
}

/// Like [`infer_lir`], with known heap/niche (or numeric) slot seeds (I1).
pub fn infer_lir_with_seed(
    ops: &[IlOp],
    pool_len: usize,
    param_count: u32,
    seed: &HashMap<u32, MirTy>,
) -> Result<Inferred, LowerError> {
    infer_walk(
        ops,
        pool_len,
        param_count,
        InferMode::Lir,
        &DenseCallMap::new(),
        seed,
    )
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum InferMode {
    Dense,
    Lir,
}

fn infer_walk(
    ops: &[IlOp],
    pool_len: usize,
    param_count: u32,
    mode: InferMode,
    calls: &DenseCallMap,
    seed: &HashMap<u32, MirTy>,
) -> Result<Inferred, LowerError> {
    if mode == InferMode::Dense && !has_back_edge(ops) {
        let work = numeric_work_ops(ops);
        if work < STRAIGHT_LINE_MIN_WORK_OPS {
            return Err(LowerError::Refused(format!(
                "straight-line work {work} < {STRAIGHT_LINE_MIN_WORK_OPS}"
            )));
        }
    }
    let mut slot_ty: HashMap<u32, MirTy> = seed.clone();
    let mut pool_ty = vec![None; pool_len];
    let mut stack: Vec<Cell> = Vec::new();
    let mut has_i32 = false;
    let mut has_float_arith = false;
    let mut has_i64_arith = false;
    let mut slot_imm: HashMap<u32, i64> = HashMap::new();

    for op in ops {
        match op {
            IlOp::Label(_) | IlOp::JoinLabel(_) => {}
            IlOp::Load { slot, .. } => {
                stack.push(Cell {
                    origin: Origin::Slot(*slot),
                    ty: slot_ty.get(slot).copied(),
                    imm: slot_imm.get(slot).copied(),
                });
            }
            IlOp::StorePop { slot, .. } => {
                let c = stack
                    .pop()
                    .ok_or_else(|| LowerError::Refused("store stack".into()))?;
                if let Some(imm) = c.imm {
                    slot_imm.insert(*slot, imm);
                } else {
                    slot_imm.remove(slot);
                }
                if let Some(ty) = c.ty {
                    set_slot(&mut slot_ty, *slot, ty)?;
                    paint(&mut slot_ty, &mut pool_ty, c, ty)?;
                }
            }
            IlOp::Const { imm, .. } => stack.push(Cell {
                origin: Origin::Tmp,
                ty: Some(MirTy::I64),
                imm: Some(i64::from(*imm)),
            }),
            IlOp::ConstPool { idx, .. } => stack.push(Cell {
                origin: Origin::Pool(*idx),
                ty: pool_ty.get(*idx as usize).copied().flatten(),
                imm: None,
            }),
            IlOp::Dup { .. } => {
                let c = *stack
                    .last()
                    .ok_or_else(|| LowerError::Refused("dup stack".into()))?;
                stack.push(c);
            }
            IlOp::Pop { .. } => {
                stack
                    .pop()
                    .ok_or_else(|| LowerError::Refused("pop stack".into()))?;
            }
            IlOp::LogNot { .. } => {
                let _ = stack
                    .pop()
                    .ok_or_else(|| LowerError::Refused("not stack".into()))?;
                stack.push(Cell {
                    origin: Origin::Tmp,
                    ty: Some(MirTy::Bool),
                    imm: None,
                });
            }
            IlOp::Bin { op: inst, .. } => {
                apply_bin(
                    &mut stack,
                    &mut slot_ty,
                    &mut pool_ty,
                    *inst,
                    &mut has_i32,
                    &mut has_float_arith,
                    &mut has_i64_arith,
                )?;
            }
            IlOp::BinSlotImm { op, slot, .. } => {
                let inst = Instruction::from(*op);
                let ty = operand_ty(inst);
                if ty == MirTy::I32 {
                    has_i32 = true;
                }
                if is_float_arith(inst) {
                    has_float_arith = true;
                }
                if is_int_arith(inst) {
                    has_i64_arith = true;
                }
                set_slot(&mut slot_ty, u32::from(*slot), ty)?;
                stack.push(Cell {
                    origin: Origin::Tmp,
                    ty: Some(if is_cmp(inst) { MirTy::Bool } else { ty }),
                    imm: None,
                });
            }
            IlOp::BinSlotSlot { op, a, b, .. } => {
                let inst = Instruction::from(*op);
                let ty = operand_ty(inst);
                if ty == MirTy::I32 {
                    has_i32 = true;
                }
                if is_float_arith(inst) {
                    has_float_arith = true;
                }
                if is_int_arith(inst) {
                    has_i64_arith = true;
                }
                set_slot(&mut slot_ty, u32::from(*a), ty)?;
                set_slot(&mut slot_ty, u32::from(*b), ty)?;
                stack.push(Cell {
                    origin: Origin::Tmp,
                    ty: Some(if is_cmp(inst) { MirTy::Bool } else { ty }),
                    imm: None,
                });
            }
            IlOp::Byte { byte, .. } => match *byte.bytecode() {
                Instruction::CastIntToFloat => {
                    let c = stack
                        .pop()
                        .ok_or_else(|| LowerError::Refused("cast stack".into()))?;
                    paint(&mut slot_ty, &mut pool_ty, c, MirTy::I64)?;
                    stack.push(Cell {
                        origin: Origin::Tmp,
                        ty: Some(MirTy::F64),
                        imm: None,
                    });
                }
                Instruction::NEGF => {
                    let c = stack
                        .pop()
                        .ok_or_else(|| LowerError::Refused("negf stack".into()))?;
                    paint(&mut slot_ty, &mut pool_ty, c, MirTy::F64)?;
                    stack.push(Cell {
                        origin: Origin::Tmp,
                        ty: Some(MirTy::F64),
                        imm: None,
                    });
                }
                Instruction::NEG => {
                    let c = stack
                        .pop()
                        .ok_or_else(|| LowerError::Refused("neg stack".into()))?;
                    let ty = c.ty.unwrap_or(MirTy::I64);
                    paint(&mut slot_ty, &mut pool_ty, c, ty)?;
                    stack.push(Cell {
                        origin: Origin::Tmp,
                        ty: Some(ty),
                        imm: None,
                    });
                }
                Instruction::NOT => {
                    stack
                        .pop()
                        .ok_or_else(|| LowerError::Refused("not stack".into()))?;
                    stack.push(Cell {
                        origin: Origin::Tmp,
                        ty: Some(MirTy::Bool),
                        imm: None,
                    });
                }
                Instruction::Seek if mode == InferMode::Lir => {}
                Instruction::Unpack if mode == InferMode::Lir => {
                    let arity = byte.operand_u32();
                    if arity > 1 {
                        return Err(LowerError::Refused("Unpack arity > 1 (I2)".into()));
                    }
                    let _ = stack
                        .pop()
                        .ok_or_else(|| LowerError::Refused("Unpack stack".into()))?;
                    if arity == 1 {
                        stack.push(Cell {
                            origin: Origin::Tmp,
                            ty: Some(MirTy::I64),
                            imm: None,
                        });
                    }
                }
                Instruction::INC | Instruction::DEC => {
                    let (slot, _, is_float) = byte.inc_dec_parts();
                    let ty = if is_float { MirTy::F64 } else { MirTy::I64 };
                    if is_float {
                        has_float_arith = true;
                    } else {
                        has_i64_arith = true;
                    }
                    set_slot(&mut slot_ty, slot as u32, ty)?;
                }
                other if is_format_inst(other) => {
                    return Err(LowerError::Refused("format".into()));
                }
                other => {
                    return Err(LowerError::Refused(format!(
                        "residual byte {}",
                        other.mnemonic()
                    )));
                }
            },
            IlOp::Jump {
                kind: crate::il::IlJumpKind::JumpIfMatch { tag, arity },
                ..
            } => {
                if mode != InferMode::Lir {
                    return Err(LowerError::Refused("match".into()));
                }
                if *arity > 1 {
                    return Err(LowerError::Refused(
                        "JumpIfMatch arity > 1 (keep fuse-IL)".into(),
                    ));
                }
                let _ = tag;
                if stack.is_empty() {
                    return Err(LowerError::Refused("JumpIfMatch stack".into()));
                }
                // Peek: miss fallthrough is the linear walk.
            }
            IlOp::Jump { .. } | IlOp::Return { ret_words: 1, .. } | IlOp::Halt { .. } => {}
            IlOp::Return { ret_words, .. } if *ret_words == 2 && mode == InferMode::Lir => {}
            IlOp::Return { ret_words, .. } if *ret_words != 1 => {
                return Err(LowerError::Refused("multi-word return".into()));
            }
            IlOp::HostInvoke { arity, layout, .. } => {
                apply_host(&mut stack, &mut slot_ty, &mut pool_ty, *arity, *layout)?;
            }
            IlOp::Entry {
                kind: EntryKind::Call,
                arity,
                target,
                ret_words,
                ..
            } if mode == InferMode::Dense => {
                apply_call(
                    &mut stack,
                    &mut slot_ty,
                    &mut pool_ty,
                    *arity,
                    *ret_words,
                    target.0,
                    calls,
                )?;
            }
            _ => {
                return Err(LowerError::Refused(format!(
                    "non-numeric IL ({})",
                    refuse_il_kind(op)
                )));
            }
        }
    }

    // Second pass: paint pool / unknown stores now that slots have types.
    let mut stack: Vec<Cell> = Vec::new();
    for op in ops {
        match op {
            IlOp::Load { slot, .. } => stack.push(Cell {
                origin: Origin::Slot(*slot),
                ty: slot_ty.get(slot).copied(),
                imm: None,
            }),
            IlOp::Const { .. } => stack.push(Cell {
                origin: Origin::Tmp,
                ty: Some(MirTy::I64),
                imm: None,
            }),
            IlOp::ConstPool { idx, .. } => stack.push(Cell {
                origin: Origin::Pool(*idx),
                ty: pool_ty.get(*idx as usize).copied().flatten(),
                imm: None,
            }),
            IlOp::Dup { .. } => {
                if let Some(c) = stack.last().copied() {
                    stack.push(c);
                }
            }
            IlOp::Pop { .. } | IlOp::LogNot { .. } | IlOp::Bin { .. } => {
                let _ = stack.pop();
                if matches!(op, IlOp::Bin { .. }) {
                    let _ = stack.pop();
                    stack.push(Cell {
                        origin: Origin::Tmp,
                        ty: None,
                        imm: None,
                    });
                } else if matches!(op, IlOp::LogNot { .. }) {
                    stack.push(Cell {
                        origin: Origin::Tmp,
                        ty: Some(MirTy::Bool),
                        imm: None,
                    });
                }
            }
            IlOp::StorePop { slot, .. } => {
                if let Some(c) = stack.pop() {
                    if let Some(ty) = slot_ty.get(slot).copied() {
                        let _ = paint(&mut slot_ty, &mut pool_ty, c, ty);
                    }
                }
            }
            IlOp::BinSlotImm { .. } | IlOp::BinSlotSlot { .. } => {
                stack.push(Cell {
                    origin: Origin::Tmp,
                    ty: None,
                    imm: None,
                });
            }
            IlOp::HostInvoke { arity, .. } => {
                for _ in 0..(*arity as usize + 1) {
                    let _ = stack.pop();
                }
                stack.push(Cell {
                    origin: Origin::Tmp,
                    ty: None,
                    imm: None,
                });
            }
            IlOp::Entry {
                kind: EntryKind::Call,
                arity,
                ..
            } => {
                for _ in 0..*arity {
                    let _ = stack.pop();
                }
                stack.push(Cell {
                    origin: Origin::Tmp,
                    ty: None,
                    imm: None,
                });
            }
            IlOp::Byte { byte, .. }
                if matches!(
                    *byte.bytecode(),
                    Instruction::CastIntToFloat
                        | Instruction::NEGF
                        | Instruction::NEG
                        | Instruction::NOT
                ) =>
            {
                let _ = stack.pop();
                stack.push(Cell {
                    origin: Origin::Tmp,
                    ty: None,
                    imm: None,
                });
            }
            _ => {}
        }
    }

    for i in 0..param_count {
        slot_ty.entry(i).or_insert(MirTy::I64);
    }
    if mode == InferMode::Dense && slot_ty.values().any(|t| t.is_heap_word()) {
        return Err(LowerError::Refused(
            "dense infer refuses heap/niche slots (I1 carry is LIR/SSA only)".into(),
        ));
    }
    if mode == InferMode::Dense && !has_float_arith && !has_i32 && !has_i64_arith {
        return Err(LowerError::Refused(
            "need float + - * / , i64 + - * / % , or i32 (compare-only stays on fuse-IL)".into(),
        ));
    }
    Ok(Inferred {
        slot_ty,
        pool_ty,
        has_i32,
        has_float_arith,
        has_i64_arith,
    })
}

fn is_numeric_work_op(op: &IlOp) -> bool {
    match op {
        IlOp::Bin { .. } | IlOp::BinSlotImm { .. } | IlOp::BinSlotSlot { .. } => true,
        IlOp::Byte { byte, .. } => matches!(
            *byte.bytecode(),
            Instruction::CastIntToFloat
                | Instruction::NEGF
                | Instruction::NEG
                | Instruction::INC
                | Instruction::DEC
        ),
        _ => false,
    }
}

fn has_back_edge(ops: &[IlOp]) -> bool {
    let mut seen = HashMap::new();
    for (i, op) in ops.iter().enumerate() {
        if let IlOp::Label(Label(id)) | IlOp::JoinLabel(Label(id)) = op {
            seen.entry(*id).or_insert(i);
        }
        if let IlOp::Jump {
            target: Label(id), ..
        } = op
        {
            if let Some(&at) = seen.get(id) {
                if at < i {
                    return true;
                }
            }
        }
    }
    false
}

fn operand_ty(inst: Instruction) -> MirTy {
    if is_float_inst(inst) {
        MirTy::F64
    } else {
        MirTy::I64
    }
}

fn is_float_inst(inst: Instruction) -> bool {
    matches!(
        inst,
        Instruction::ADDF
            | Instruction::SUBF
            | Instruction::MULF
            | Instruction::DIVF
            | Instruction::MODF
            | Instruction::LEF
            | Instruction::LEQF
            | Instruction::GTF
            | Instruction::GEQF
            | Instruction::PowF
            | Instruction::NEGF
    )
}

fn is_float_arith(inst: Instruction) -> bool {
    matches!(
        inst,
        Instruction::ADDF | Instruction::SUBF | Instruction::MULF | Instruction::DIVF
    )
}

fn is_int_arith(inst: Instruction) -> bool {
    matches!(
        inst,
        Instruction::ADD
            | Instruction::SUB
            | Instruction::MUL
            | Instruction::DIV
            | Instruction::MOD
    )
}

/// Coarse first-op kind for the infer catch-all (W0 inventory).
fn refuse_il_kind(op: &IlOp) -> &'static str {
    if let Some(reason) = refuse_reason(op) {
        return reason;
    }
    if let Some(reason) = alloc_refuse_reason(op) {
        return reason;
    }
    match op {
        IlOp::Entry { .. } | IlOp::PrologueJmp { .. } => "CALL",
        IlOp::HostInvoke { .. } => "HostInvoke",
        IlOp::Index { .. }
        | IlOp::IndexUnchecked { .. }
        | IlOp::IndexPin { .. }
        | IlOp::IndexPinUnchecked { .. }
        | IlOp::StoreIndexPin { .. }
        | IlOp::StoreIndexPinUnchecked { .. }
        | IlOp::ArrayPin { .. } => "heap/index",
        IlOp::GetField { .. } | IlOp::SetField { .. } | IlOp::LoadField { .. } => "class/field",
        IlOp::BoxValue { .. } | IlOp::UnboxValue { .. } => "box",
        IlOp::LoadReturnSlot { .. } | IlOp::ConstReturnImm { .. } | IlOp::BinReturn { .. } => {
            "fused-return"
        }
        IlOp::Jump {
            kind: crate::il::IlJumpKind::JumpIfMatch { .. },
            ..
        } => "match",
        _ => "other",
    }
}

fn is_cmp(inst: Instruction) -> bool {
    matches!(
        inst,
        Instruction::LE
            | Instruction::LEQ
            | Instruction::GT
            | Instruction::GEQ
            | Instruction::EQ
            | Instruction::NEQ
            | Instruction::LEF
            | Instruction::LEQF
            | Instruction::GTF
            | Instruction::GEQF
    )
}

fn apply_bin(
    stack: &mut Vec<Cell>,
    slot_ty: &mut HashMap<u32, MirTy>,
    pool_ty: &mut [Option<MirTy>],
    inst: Instruction,
    has_i32: &mut bool,
    has_float_arith: &mut bool,
    has_i64_arith: &mut bool,
) -> Result<(), LowerError> {
    let rhs = stack
        .pop()
        .ok_or_else(|| LowerError::Refused("bin stack".into()))?;
    let lhs = stack
        .pop()
        .ok_or_else(|| LowerError::Refused("bin stack".into()))?;
    if matches!(
        inst,
        Instruction::Pow | Instruction::PowF | Instruction::AND | Instruction::OR
    ) {
        return Err(LowerError::Refused(format!("binop {}", inst.mnemonic())));
    }
    if matches!(
        inst,
        Instruction::BITAND | Instruction::BITOR | Instruction::XOR
    ) {
        if let Some(result) = bitwise_heap_result(inst, lhs.ty, rhs.ty) {
            paint_keep_heap(slot_ty, pool_ty, lhs, result)?;
            paint_keep_heap(slot_ty, pool_ty, rhs, result)?;
            stack.push(Cell {
                origin: Origin::Tmp,
                ty: Some(result),
                imm: None,
            });
            return Ok(());
        }
    }
    let ty = operand_ty(inst);
    if ty == MirTy::I32 {
        *has_i32 = true;
    }
    if is_float_arith(inst) {
        *has_float_arith = true;
    }
    if is_int_arith(inst) {
        *has_i64_arith = true;
    }
    paint(slot_ty, pool_ty, lhs, ty)?;
    paint(slot_ty, pool_ty, rhs, ty)?;
    stack.push(Cell {
        origin: Origin::Tmp,
        ty: Some(if is_cmp(inst) { MirTy::Bool } else { ty }),
        imm: None,
    });
    Ok(())
}

fn bitwise_heap_result(inst: Instruction, lhs: Option<MirTy>, rhs: Option<MirTy>) -> Option<MirTy> {
    let heap = [lhs, rhs]
        .into_iter()
        .flatten()
        .find(|t| t.is_heap_word())?;
    Some(match inst {
        Instruction::BITOR => MirTy::NicheRes,
        Instruction::BITAND => MirTy::HeapRef,
        Instruction::XOR => heap,
        _ => heap,
    })
}

fn paint_keep_heap(
    slot_ty: &mut HashMap<u32, MirTy>,
    pool_ty: &mut [Option<MirTy>],
    cell: Cell,
    result: MirTy,
) -> Result<(), LowerError> {
    if cell.ty.is_some_and(|t| t.is_heap_word()) {
        if let Some(ty) = cell.ty {
            return paint(slot_ty, pool_ty, cell, ty);
        }
    }
    if cell.ty.is_some_and(MirTy::is_numeric) {
        return paint(slot_ty, pool_ty, cell, cell.ty.unwrap());
    }
    let _ = result;
    Ok(())
}

fn apply_call(
    stack: &mut Vec<Cell>,
    slot_ty: &mut HashMap<u32, MirTy>,
    pool_ty: &mut [Option<MirTy>],
    arity: u32,
    ret_words: u32,
    target: u32,
    calls: &DenseCallMap,
) -> Result<(), LowerError> {
    if ret_words != 1 {
        return Err(LowerError::Refused("dense CALL is one-word".into()));
    }
    let Some(abi) = calls.get(&target) else {
        return Err(LowerError::Refused("CALL".into()));
    };
    let n = arity as usize;
    if abi.params.len() != n {
        return Err(LowerError::Refused("CALL arity".into()));
    }
    if stack.len() < n {
        return Err(LowerError::Refused("CALL stack".into()));
    }
    let mut args = Vec::with_capacity(n);
    for _ in 0..n {
        args.push(stack.pop().expect("arity checked"));
    }
    args.reverse();
    for (cell, ty) in args.iter().zip(abi.params.iter()) {
        paint(slot_ty, pool_ty, *cell, *ty)?;
    }
    stack.push(Cell {
        origin: Origin::Tmp,
        ty: Some(abi.ret),
        imm: None,
    });
    Ok(())
}

fn apply_host(
    stack: &mut Vec<Cell>,
    slot_ty: &mut HashMap<u32, MirTy>,
    pool_ty: &mut [Option<MirTy>],
    arity: u32,
    layout: u8,
) -> Result<(), LowerError> {
    if layout != 0 {
        return Err(LowerError::Refused("HostInvoke".into()));
    }
    let n = arity as usize;
    if stack.len() < n + 1 {
        return Err(LowerError::Refused("HostInvoke stack".into()));
    }
    let fn_cell = stack[stack.len() - n - 1];
    let Some(id) = fn_cell.imm.and_then(|v| u16::try_from(v).ok()) else {
        return Err(LowerError::Refused("HostInvoke".into()));
    };
    let Some(spec) = host_spec(id) else {
        return Err(LowerError::Refused("HostInvoke".into()));
    };
    if spec.args.len() != n {
        return Err(LowerError::Refused(format!(
            "HostInvoke {} arity",
            spec.name
        )));
    }
    let mut args = Vec::with_capacity(n);
    for _ in 0..n {
        args.push(stack.pop().expect("arity checked"));
    }
    args.reverse();
    let _fn = stack.pop().expect("fn id checked");
    for (cell, ty) in args.iter().zip(spec.args.iter()) {
        paint(slot_ty, pool_ty, *cell, *ty)?;
    }
    stack.push(Cell {
        origin: Origin::Tmp,
        ty: Some(spec.ret),
        imm: None,
    });
    Ok(())
}

fn paint(
    slot_ty: &mut HashMap<u32, MirTy>,
    pool_ty: &mut [Option<MirTy>],
    cell: Cell,
    ty: MirTy,
) -> Result<(), LowerError> {
    match cell.origin {
        Origin::Slot(s) => set_slot(slot_ty, s, ty),
        Origin::Pool(i) => {
            let i = i as usize;
            if i >= pool_ty.len() {
                return Err(LowerError::Refused(format!("pool {i}")));
            }
            match pool_ty[i] {
                None => pool_ty[i] = Some(ty),
                Some(old) if old == ty || old.join(ty) == ty => pool_ty[i] = Some(old.join(ty)),
                Some(old) => {
                    return Err(LowerError::Refused(format!("pool {i} {old} vs {ty}")));
                }
            }
            Ok(())
        }
        Origin::Tmp => Ok(()),
    }
}

fn set_slot(map: &mut HashMap<u32, MirTy>, slot: u32, ty: MirTy) -> Result<(), LowerError> {
    match map.get(&slot).copied() {
        None => {
            map.insert(slot, ty);
            Ok(())
        }
        Some(old) if old == ty => Ok(()),
        Some(old) => {
            let j = old.join(ty);
            if j.is_specialized() {
                map.insert(slot, j);
                Ok(())
            } else {
                Err(LowerError::Refused(format!(
                    "slot {slot} joins {old} and {ty} to {j}"
                )))
            }
        }
    }
}
