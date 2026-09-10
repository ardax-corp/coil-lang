//! I8 — which leftover bodies enter MIR→LIR after dense specialize.
//!
//! Production is two-phase after stack-IL opts (`IlModule`):
//! 1. [`crate::mir::try_specialize_body`] — numeric dense + cost gate.
//! 2. [`crate::mir::try_lower_abi_body_with`] — IL→MIR lift when there is
//!    no hard refuse; `IlModule` keeps the reconstruct only when cost ≤ fuse.
//!
//! Hard refuse is **LIR reconstruct** walls: unmapped alloc, `CALL` /
//! HostInvoke (emit cannot rebuild those), escaping fields, box,
//! multi-payload match. Q6 counted `for`, Q7 one-word dense `CALL`, and
//! Q8 niche / two-slot `Br` are not walls — they lift on the dense path;
//! keep/refuse is the cost gate. LIR reconstructs two-slot `CALL`
//! (B3); one-word `CALL` / `TailCall` stay dense-or-fuse. Heap index
//! is not a wall after A2. Fuse-IL stays the fallback.
//! There is no second AST walker.

use common::Instruction;

use crate::il::IlOp;

use super::gc::refuses_alloc;

/// Why a leftover body stays fuse-IL instead of MIR→LIR.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LirRefuse {
    /// I5: `MakeArray` / `MakeTuple` / `MakeEnum` / `InitTyped`.
    Alloc,
    /// I6: user `CALL` / `TailCall` / other `Entry` (LIR reconstruct wall;
    /// Q7 one-word self-`CALL` is dense + cost, not this refuse).
    Call,
    /// I6: HostInvoke (dense reconstructs; LIR emit does not).
    Host,
    /// Escaping / heap-backed field ops.
    HeapField,
    /// `BoxValue` / `UnboxValue`.
    Box,
    /// `JumpIfMatch` / `Unpack` the reconstruct cannot model.
    Match,
    /// `Unpack` arity > 1.
    Unpack,
}

/// Production LIR entry after dense specialize misses.
///
/// Eligible when there is no **LIR reconstruct** wall. Walls stay I5
/// alloc without maps, HostInvoke/`CALL` (LIR emit cannot rebuild those
/// — Q7 densifies one-word self-`CALL` instead), escaping fields, box,
/// multi-payload `Unpack` / `JumpIfMatch` arity > 1. Counted `for` (Q6)
/// and niche / two-slot match arity ≤ 1 (Q8) are not LIR walls. Q9 R1:
/// `STRING` / `PRINT` / `FORMAT` / `STRINGIFY` may lift. Q9 R3 maps
/// `FORMAT` / `STRINGIFY` (unmapped format is an alloc wall). Q9 R2 densifies
/// `from_bytes` / `to_bytes` HostInvoke (LIR still cannot reconstruct
/// HostInvoke). Heap index /
/// `ArrayLen` / `StoreIndex` may lift (A2).
/// `IlModule` still replaces only when LIR cost ≤ opted fuse-IL.
/// S2c: mapped alloc is not a hard refuse ([`lir_eligible_with`]).
pub fn lir_eligible(ops: &[IlOp], unboxed_fields: &[(u32, u32)]) -> bool {
    lir_eligible_with(ops, unboxed_fields, false)
}

/// Like [`lir_eligible`], with S2c maps: `maps_ok` lets alloc through.
pub fn lir_eligible_with(
    ops: &[IlOp],
    unboxed_fields: &[(u32, u32)],
    maps_ok: bool,
) -> bool {
    lir_refuse_with(ops, unboxed_fields, maps_ok).is_none()
}

pub fn lir_refuse(ops: &[IlOp], unboxed_fields: &[(u32, u32)]) -> Option<LirRefuse> {
    lir_refuse_with(ops, unboxed_fields, false)
}

pub fn lir_refuse_with(
    ops: &[IlOp],
    unboxed_fields: &[(u32, u32)],
    maps_ok: bool,
) -> Option<LirRefuse> {
    let _ = unboxed_fields;
    hard_refuse(ops, maps_ok)
}

fn hard_refuse(ops: &[IlOp], maps_ok: bool) -> Option<LirRefuse> {
    for op in ops {
        match op {
            IlOp::Entry {
                kind: crate::il::EntryKind::Call,
                ret_words,
                ..
            } if *ret_words == 2 => {}
            IlOp::Entry { .. } | IlOp::PrologueJmp { .. } => return Some(LirRefuse::Call),
            IlOp::HostInvoke { .. } => return Some(LirRefuse::Host),
            IlOp::GetField { .. } | IlOp::SetField { .. } | IlOp::LoadField { .. } => {
                return Some(LirRefuse::HeapField);
            }
            IlOp::BoxValue { .. } | IlOp::UnboxValue { .. } => return Some(LirRefuse::Box),
            op if refuses_alloc(op) && !maps_ok => return Some(LirRefuse::Alloc),
            IlOp::Jump {
                kind: crate::il::IlJumpKind::JumpIfMatch { arity, .. },
                ..
            } => {
                if *arity > 1 {
                    return Some(LirRefuse::Match);
                }
            }
            IlOp::Byte { byte, .. } if *byte.bytecode() == Instruction::Unpack => {
                // Multi-payload Unpack still needs per-index MatchPayload maps.
                if byte.operand_u32() > 1 {
                    return Some(LirRefuse::Unpack);
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::il::{IlJumpKind, IlOp, Label};
    use common::{Byte, DebugLoc, Instruction};

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
    fn i2_boxed_overlap_arity0_is_lir_eligible() {
        let loc = loc();
        let ops = [
            IlOp::Load { slot: 0, loc },
            IlOp::Jump {
                kind: IlJumpKind::JumpIfMatch { tag: 2, arity: 0 },
                target: Label(1),
                loc,
                hint: Default::default(),
            },
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

    #[test]
    fn b3_two_slot_call_is_not_a_lir_wall() {
        let loc = loc();
        let call = [IlOp::Entry {
            kind: crate::il::EntryKind::Call,
            arity: 1,
            target: Label(1),
            loc,
            ret_words: 2,
        }];
        assert_eq!(lir_refuse(&call, &[]), None);
        assert!(lir_eligible(&call, &[]));
        let tail = [IlOp::Entry {
            kind: crate::il::EntryKind::TailCall,
            arity: 1,
            target: Label(1),
            loc,
            ret_words: 2,
        }];
        assert_eq!(lir_refuse(&tail, &[]), Some(LirRefuse::Call));
    }

    #[test]
    fn s2c_mapped_alloc_is_lir_eligible() {
        let loc = loc();
        let ops = [
            IlOp::Label(Label(0)),
            IlOp::Load { slot: 0, loc },
            IlOp::Load { slot: 1, loc },
            IlOp::MakeArray { arity: 2, loc },
            IlOp::Return { loc, ret_words: 1 },
        ];
        assert_eq!(lir_refuse(&ops, &[]), Some(LirRefuse::Alloc));
        assert_eq!(lir_refuse_with(&ops, &[], true), None);
        assert!(lir_eligible_with(&ops, &[], true));
    }

    #[test]
    fn q9_r3_mapped_format_is_lir_eligible() {
        let loc = loc();
        let ops = [
            IlOp::Label(Label(0)),
            IlOp::String { idx: 0, loc },
            IlOp::Load { slot: 0, loc },
            IlOp::Byte {
                byte: Byte::new(Instruction::FORMAT).with_operand_u32(1),
                loc,
            },
            IlOp::Return { loc, ret_words: 1 },
        ];
        assert_eq!(lir_refuse(&ops, &[]), Some(LirRefuse::Alloc));
        assert_eq!(lir_refuse_with(&ops, &[], true), None);
    }

    #[test]
    fn b6_mapped_array_push_is_lir_eligible() {
        let loc = loc();
        let ops = [
            IlOp::Label(Label(0)),
            IlOp::Load { slot: 0, loc },
            IlOp::Const { imm: 1, loc },
            IlOp::byte(Byte::new(Instruction::ArrayPush)),
            IlOp::Return { loc, ret_words: 1 },
        ];
        assert_eq!(lir_refuse(&ops, &[]), Some(LirRefuse::Alloc));
        assert_eq!(lir_refuse_with(&ops, &[], true), None);
    }

    #[test]
    fn i8_heap_index_is_lir_eligible() {
        let loc = loc();
        let ops = [
            IlOp::Load { slot: 0, loc },
            IlOp::Load { slot: 1, loc },
            IlOp::Index { loc },
            IlOp::Return { loc, ret_words: 1 },
        ];
        assert_eq!(lir_refuse(&ops, &[]), None);
        assert!(lir_eligible(&ops, &[]));
    }

    #[test]
    fn q6_counted_index_loop_is_not_a_lir_wall() {
        let loc = loc();
        let ops = [
            IlOp::Label(Label(0)),
            IlOp::Load { slot: 0, loc },
            IlOp::Load { slot: 1, loc },
            IlOp::Bin {
                op: Instruction::ADD,
                loc,
            },
            IlOp::Index { loc },
            IlOp::Jump {
                kind: IlJumpKind::JumpIfFalse,
                target: Label(0),
                loc,
                hint: Default::default(),
            },
            IlOp::Return { loc, ret_words: 1 },
        ];
        assert_eq!(lir_refuse(&ops, &[]), None);
        assert!(lir_eligible(&ops, &[]));
    }

    #[test]
    fn q8_jump_if_match_arity1_is_not_a_lir_wall() {
        let loc = loc();
        let ops = [
            IlOp::Load { slot: 0, loc },
            IlOp::Jump {
                kind: IlJumpKind::JumpIfMatch { tag: 0, arity: 1 },
                target: Label(1),
                loc,
                hint: Default::default(),
            },
            IlOp::Return { loc, ret_words: 1 },
        ];
        assert_eq!(lir_refuse(&ops, &[]), None);
        assert!(lir_eligible(&ops, &[]));
    }
}
