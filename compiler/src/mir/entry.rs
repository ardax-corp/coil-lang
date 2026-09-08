//! I8 — which leftover bodies enter MIR→LIR after dense specialize.
//!
//! Production is two-phase after stack-IL opts (`IlModule`):
//! 1. [`crate::mir::try_specialize_body`] — numeric loop / W3 dense.
//! 2. [`crate::mir::try_lower_abi_body_with`] — IL→MIR lift when
//!    [`lir_eligible`].
//!
//! Entry needs a **named reason** (not “infer succeeded”). Fuse-IL stays
//! default. There is no second AST walker.

use common::Instruction;

use crate::il::IlOp;

use super::gc::refuses_alloc;
use super::string_barrier::refuses_string_or_format;

/// Why a leftover body stays fuse-IL instead of MIR→LIR.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LirRefuse {
    /// I4: `FORMAT` / `STRING` / `STRINGIFY` / `PRINT`.
    String,
    /// I5: `MakeArray` / `MakeTuple` / `MakeEnum` / `InitTyped`.
    Alloc,
    /// I6: user `CALL` / `TailCall` / other `Entry`.
    Call,
    /// I6: HostInvoke (W4 is dense-only).
    Host,
    /// Heap index / pin.
    Index,
    /// Escaping / heap-backed field ops.
    HeapField,
    /// `BoxValue` / `UnboxValue`.
    Box,
    /// `JumpIfMatch` outside I2 (tag > 1 or arity ≠ 1).
    Match,
    /// `Unpack` arity > 1.
    Unpack,
    /// No I1–I3 / two-slot / compare-control reason.
    NoReason,
}

/// Production LIR entry after dense specialize misses.
///
/// Eligible when there is no hard refuse **and** a named reason:
/// two-slot `RETURN`, I2 match, I3 unboxed fields, I1 niche
/// `BITAND`/`BITOR`, or an inferable leftover (plain `if`/compare
/// diamonds, store-only loops, tiny lets). Hard refuse stays I4
/// string/FORMAT, I5 alloc, impure HostInvoke/`CALL`, heap index /
/// escaping fields, boxed overlap, I2-out-of-range match.
/// `IlModule` still replaces only when LIR cost ≤ opted fuse-IL.
pub fn lir_eligible(ops: &[IlOp], unboxed_fields: &[(u32, u32)]) -> bool {
    lir_refuse(ops, unboxed_fields).is_none()
}

pub fn lir_refuse(ops: &[IlOp], unboxed_fields: &[(u32, u32)]) -> Option<LirRefuse> {
    if let Some(r) = hard_refuse(ops) {
        return Some(r);
    }
    if lir_reason(ops, unboxed_fields) {
        None
    } else {
        Some(LirRefuse::NoReason)
    }
}

fn hard_refuse(ops: &[IlOp]) -> Option<LirRefuse> {
    for op in ops {
        match op {
            IlOp::Entry { .. } | IlOp::PrologueJmp { .. } => return Some(LirRefuse::Call),
            IlOp::HostInvoke { .. } => return Some(LirRefuse::Host),
            IlOp::GetField { .. } | IlOp::SetField { .. } | IlOp::LoadField { .. } => {
                return Some(LirRefuse::HeapField);
            }
            IlOp::BoxValue { .. } | IlOp::UnboxValue { .. } => return Some(LirRefuse::Box),
            IlOp::Index { .. }
            | IlOp::IndexUnchecked { .. }
            | IlOp::IndexPin { .. }
            | IlOp::IndexPinUnchecked { .. }
            | IlOp::StoreIndexPin { .. }
            | IlOp::StoreIndexPinUnchecked { .. }
            | IlOp::ArrayPin { .. } => return Some(LirRefuse::Index),
            op if refuses_string_or_format(op) => return Some(LirRefuse::String),
            op if refuses_alloc(op) => return Some(LirRefuse::Alloc),
            IlOp::Jump {
                kind: crate::il::IlJumpKind::JumpIfMatch { tag, arity },
                ..
            } => {
                // Arity 0 is boxed overlap (`JumpIfMatch` writes slots; tell
                // is peek-only). Reconstruct would drop the payload.
                if *tag > 1 || *arity != 1 {
                    return Some(LirRefuse::Match);
                }
            }
            IlOp::Byte { byte, .. } if *byte.bytecode() == Instruction::Unpack => {
                if byte.operand_u32() > 1 {
                    return Some(LirRefuse::Unpack);
                }
            }
            _ => {}
        }
    }
    None
}

/// I8 reasons — I1–I3 / two-slot plus inferable leftovers (compare/`if`,
/// store-only, tiny lets). Hard refuse still wins.
fn lir_reason(ops: &[IlOp], unboxed_fields: &[(u32, u32)]) -> bool {
    let mut ret2 = false;
    let mut match_shaped = false;
    let mut field_use = false;
    let mut niche_word = false;
    let mut leftover = false;
    for op in ops {
        match op {
            IlOp::Return { ret_words, .. } if *ret_words >= 2 => ret2 = true,
            IlOp::Jump {
                kind: crate::il::IlJumpKind::JumpIfMatch { .. },
                ..
            } => match_shaped = true,
            IlOp::Jump {
                kind: crate::il::IlJumpKind::JumpIfFalse
                    | crate::il::IlJumpKind::JumpIfTrue,
                ..
            } => leftover = true,
            IlOp::StorePop { slot, .. } => {
                leftover = true;
                if slot_in_unboxed_fields(*slot, unboxed_fields) {
                    field_use = true;
                }
            }
            IlOp::Byte { byte, .. } if *byte.bytecode() == Instruction::Unpack => {
                match_shaped = true;
            }
            IlOp::Load { slot, .. } if slot_in_unboxed_fields(*slot, unboxed_fields) => {
                field_use = true;
            }
            IlOp::Const { .. } => leftover = true,
            IlOp::Bin { op, .. } if is_compare(*op) => leftover = true,
            IlOp::Bin { op, .. } if matches!(*op, Instruction::BITAND | Instruction::BITOR) => {
                // I1 heap-niche bits: `ptr | 1` / `ptr & 1` (tag 0/1).
                if body_has_tag_imm(ops) {
                    niche_word = true;
                }
            }
            IlOp::BinSlotImm { op, imm, .. }
                if matches!(
                    Instruction::from(*op),
                    Instruction::BITAND | Instruction::BITOR
                ) && (*imm == 0 || *imm == 1) =>
            {
                niche_word = true;
            }
            IlOp::BinSlotImm { op, .. } if is_compare(Instruction::from(*op)) => leftover = true,
            IlOp::BinSlotSlot { op, .. }
                if matches!(
                    Instruction::from(*op),
                    Instruction::BITAND | Instruction::BITOR
                ) && body_has_tag_imm(ops) =>
            {
                niche_word = true;
            }
            IlOp::BinSlotSlot { op, .. } if is_compare(Instruction::from(*op)) => leftover = true,
            _ => {}
        }
    }
    ret2 || match_shaped || field_use || niche_word || leftover || adjacent_match_probe(ops)
}

fn is_compare(op: Instruction) -> bool {
    matches!(
        op,
        Instruction::EQ
            | Instruction::NEQ
            | Instruction::LE
            | Instruction::LEQ
            | Instruction::LEF
            | Instruction::LEQF
            | Instruction::GT
            | Instruction::GEQ
            | Instruction::GTF
            | Instruction::GEQF
    )
}

fn body_has_tag_imm(ops: &[IlOp]) -> bool {
    ops.iter().any(|op| matches!(op, IlOp::Const { imm: 0 | 1, .. }))
}

fn slot_in_unboxed_fields(slot: u32, fields: &[(u32, u32)]) -> bool {
    fields
        .iter()
        .any(|&(base, n)| slot >= base && slot < base + n)
}

/// Niche `DUP; LogNot; JMPx` or two-slot `DUP; CONST 0|1; EQ; JMPx`.
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
            && is_eq_or_bitand(w[2])
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

fn is_eq_or_bitand(op: &IlOp) -> bool {
    match op {
        IlOp::Bin { op, .. } => matches!(
            *op,
            Instruction::EQ | Instruction::NEQ | Instruction::BITAND
        ),
        IlOp::BinSlotImm { op, .. } | IlOp::BinSlotSlot { op, .. } => {
            matches!(
                Instruction::from(*op),
                Instruction::EQ | Instruction::NEQ | Instruction::BITAND
            )
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::il::{IlJumpKind, Label};
    use common::DebugLoc;

    fn loc() -> DebugLoc {
        DebugLoc::unknown()
    }

    #[test]
    fn i8_one_word_bitor_is_lir_eligible() {
        let loc = loc();
        let ops = [
            IlOp::Label(Label(0)),
            IlOp::Load { slot: 0, loc },
            IlOp::Const { imm: 1, loc },
            IlOp::Bin {
                op: Instruction::BITOR,
                loc,
            },
            IlOp::Return { loc, ret_words: 1 },
        ];
        assert_eq!(lir_refuse(&ops, &[]), None);
        assert!(lir_eligible(&ops, &[]));
    }

    #[test]
    fn i8_plain_if_diamond_is_lir_eligible() {
        let loc = loc();
        let ops = [
            IlOp::Label(Label(0)),
            IlOp::Load { slot: 0, loc },
            IlOp::Load { slot: 1, loc },
            IlOp::Bin {
                op: Instruction::LE,
                loc,
            },
            IlOp::Jump {
                kind: IlJumpKind::JumpIfFalse,
                target: Label(1),
                loc,
                hint: Default::default(),
            },
            IlOp::Load { slot: 0, loc },
            IlOp::Return { loc, ret_words: 1 },
            IlOp::Label(Label(1)),
            IlOp::Load { slot: 1, loc },
            IlOp::Return { loc, ret_words: 1 },
        ];
        assert_eq!(lir_refuse(&ops, &[]), None);
        assert!(lir_eligible(&ops, &[]));
    }

    #[test]
    fn i8_store_loop_is_lir_eligible() {
        let loc = loc();
        let ops = [
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(0),
                loc,
                hint: Default::default(),
            },
            IlOp::Label(Label(0)),
            IlOp::Const { imm: 1, loc },
            IlOp::StorePop { slot: 2, loc },
            IlOp::Jump {
                kind: IlJumpKind::Unconditional,
                target: Label(0),
                loc,
                hint: Default::default(),
            },
        ];
        assert_eq!(lir_refuse(&ops, &[]), None);
        assert!(lir_eligible(&ops, &[]));
    }

    #[test]
    fn i8_tiny_let_is_lir_eligible() {
        let loc = loc();
        let ops = [
            IlOp::Const { imm: 42, loc },
            IlOp::StorePop { slot: 0, loc },
            IlOp::Load { slot: 0, loc },
            IlOp::Return { loc, ret_words: 1 },
        ];
        assert_eq!(lir_refuse(&ops, &[]), None);
        assert!(lir_eligible(&ops, &[]));
    }

    #[test]
    fn i8_call_and_host_stay_fuse_il() {
        let loc = loc();
        let call = [IlOp::Entry {
            kind: crate::il::EntryKind::Call,
            arity: 1,
            target: Label(1),
            loc,
            ret_words: 1,
        }];
        assert_eq!(lir_refuse(&call, &[]), Some(LirRefuse::Call));
        let host = [IlOp::HostInvoke {
            arity: 0,
            layout: 0,
            loc,
        }];
        assert_eq!(lir_refuse(&host, &[]), Some(LirRefuse::Host));
    }
}
