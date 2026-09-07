//! Infer specialized slot types from pre-fuse IL (refuse heap / Value).

use std::collections::HashMap;

use common::Instruction;

use crate::il::{IlOp, Label};

use super::lower::LowerError;
use super::ty::MirTy;

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
}

/// Result of a successful numeric type walk.
pub struct Inferred {
    pub slot_ty: HashMap<u32, MirTy>,
    pub pool_ty: Vec<Option<MirTy>>,
    pub has_i32: bool,
    /// Float `+` / `-` / `*` / `/` (or float `INC`/`DEC`). Compare-only is false.
    pub has_float_arith: bool,
}

pub fn infer_numeric(
    ops: &[IlOp],
    pool_len: usize,
    param_count: u32,
) -> Result<Inferred, LowerError> {
    infer_walk(ops, pool_len, param_count, InferMode::Dense)
}

/// Slot types for MIR→LIR (two-slot / niche leafs). No loop required.
pub fn infer_lir(
    ops: &[IlOp],
    pool_len: usize,
    param_count: u32,
) -> Result<Inferred, LowerError> {
    infer_walk(ops, pool_len, param_count, InferMode::Lir)
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
) -> Result<Inferred, LowerError> {
    if mode == InferMode::Dense && !has_back_edge(ops) {
        return Err(LowerError::Refused("no loop (dense path is for hot numeric)".into()));
    }
    let mut slot_ty: HashMap<u32, MirTy> = HashMap::new();
    let mut pool_ty = vec![None; pool_len];
    let mut stack: Vec<Cell> = Vec::new();
    let mut has_i32 = false;
    let mut has_float_arith = false;

    for op in ops {
        match op {
            IlOp::Label(_) | IlOp::JoinLabel(_) => {}
            IlOp::Load { slot, .. } => {
                stack.push(Cell {
                    origin: Origin::Slot(*slot),
                    ty: slot_ty.get(slot).copied(),
                });
            }
            IlOp::StorePop { slot, .. } => {
                let c = stack
                    .pop()
                    .ok_or_else(|| LowerError::Refused("store stack".into()))?;
                if let Some(ty) = c.ty {
                    set_slot(&mut slot_ty, *slot, ty)?;
                    paint(&mut slot_ty, &mut pool_ty, c, ty)?;
                }
            }
            IlOp::Const { .. } => stack.push(Cell {
                origin: Origin::Tmp,
                ty: Some(MirTy::I64),
            }),
            IlOp::ConstPool { idx, .. } => stack.push(Cell {
                origin: Origin::Pool(*idx),
                ty: pool_ty.get(*idx as usize).copied().flatten(),
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
                set_slot(&mut slot_ty, u32::from(*slot), ty)?;
                stack.push(Cell {
                    origin: Origin::Tmp,
                    ty: Some(if is_cmp(inst) { MirTy::Bool } else { ty }),
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
                set_slot(&mut slot_ty, u32::from(*a), ty)?;
                set_slot(&mut slot_ty, u32::from(*b), ty)?;
                stack.push(Cell {
                    origin: Origin::Tmp,
                    ty: Some(if is_cmp(inst) { MirTy::Bool } else { ty }),
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
                    });
                }
                Instruction::NOT => {
                    stack
                        .pop()
                        .ok_or_else(|| LowerError::Refused("not stack".into()))?;
                    stack.push(Cell {
                        origin: Origin::Tmp,
                        ty: Some(MirTy::Bool),
                    });
                }
                Instruction::INC | Instruction::DEC => {
                    let (slot, _, is_float) = byte.inc_dec_parts();
                    let ty = if is_float { MirTy::F64 } else { MirTy::I64 };
                    if is_float {
                        has_float_arith = true;
                    }
                    set_slot(&mut slot_ty, slot as u32, ty)?;
                }
                other => {
                    return Err(LowerError::Refused(format!(
                        "residual byte {}",
                        other.mnemonic()
                    )));
                }
            },
            IlOp::Jump { .. } | IlOp::Return { ret_words: 1, .. } | IlOp::Halt { .. } => {}
            IlOp::Return { ret_words, .. } if *ret_words == 2 && mode == InferMode::Lir => {}
            IlOp::Return { ret_words, .. } if *ret_words != 1 => {
                return Err(LowerError::Refused("multi-word return".into()));
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
            }),
            IlOp::Const { .. } => stack.push(Cell {
                origin: Origin::Tmp,
                ty: Some(MirTy::I64),
            }),
            IlOp::ConstPool { idx, .. } => stack.push(Cell {
                origin: Origin::Pool(*idx),
                ty: pool_ty.get(*idx as usize).copied().flatten(),
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
                    });
                } else if matches!(op, IlOp::LogNot { .. }) {
                    stack.push(Cell {
                        origin: Origin::Tmp,
                        ty: Some(MirTy::Bool),
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
                });
            }
            _ => {}
        }
    }

    for i in 0..param_count {
        slot_ty.entry(i).or_insert(MirTy::I64);
    }
    if mode == InferMode::Dense && !has_float_arith && !has_i32 {
        return Err(LowerError::Refused(
            "need float + - * / or i32 (i64-only stays on fuse-IL)".into(),
        ));
    }
    Ok(Inferred {
        slot_ty,
        pool_ty,
        has_i32,
        has_float_arith,
    })
}

fn has_back_edge(ops: &[IlOp]) -> bool {
    let mut seen = HashMap::new();
    for (i, op) in ops.iter().enumerate() {
        if let IlOp::Label(Label(id)) | IlOp::JoinLabel(Label(id)) = op {
            seen.entry(*id).or_insert(i);
        }
        if let IlOp::Jump { target: Label(id), .. } = op {
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

/// Coarse first-op kind for the infer catch-all (W0 inventory).
fn refuse_il_kind(op: &IlOp) -> &'static str {
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
        IlOp::MakeEnum { .. } | IlOp::MakeTuple { .. } | IlOp::MakeArray { .. } => "heap/aggregate",
        IlOp::BoxValue { .. } | IlOp::UnboxValue { .. } => "box",
        IlOp::String { .. } | IlOp::Print { .. } => "string/io",
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
) -> Result<(), LowerError> {
    let rhs = stack
        .pop()
        .ok_or_else(|| LowerError::Refused("bin stack".into()))?;
    let lhs = stack
        .pop()
        .ok_or_else(|| LowerError::Refused("bin stack".into()))?;
    if matches!(inst, Instruction::Pow | Instruction::PowF | Instruction::AND | Instruction::OR)
    {
        return Err(LowerError::Refused(format!("binop {}", inst.mnemonic())));
    }
    let ty = operand_ty(inst);
    if ty == MirTy::I32 {
        *has_i32 = true;
    }
    if is_float_arith(inst) {
        *has_float_arith = true;
    }
    paint(slot_ty, pool_ty, lhs, ty)?;
    paint(slot_ty, pool_ty, rhs, ty)?;
    stack.push(Cell {
        origin: Origin::Tmp,
        ty: Some(if is_cmp(inst) { MirTy::Bool } else { ty }),
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
