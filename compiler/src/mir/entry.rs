//! I8 — which bodies enter MIR (dense specialize vs IL→MIR→LIR).
//!
//! Production is two-phase after stack-IL opts (`IlModule`):
//! 1. [`crate::mir::try_specialize_body`] — numeric loop / W3 dense.
//! 2. [`crate::mir::try_lower_abi_body_with`] — IL→MIR lift + LIR reconstruct
//!    when [`lir_eligible`].
//!
//! Entry is **infer + lower success**, not a specialize-from-IL shape
//! accident (two-slot / `JumpIfMatch` / unboxed fields only). Fuse-IL
//! stays the default for refused shapes. There is no second AST walker.

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
    /// I6: user `CALL` / `TailCall` / other `Entry` (dense already consumed
    /// leaf-first COI-291 callees).
    Call,
    /// I6: HostInvoke (W4 is dense-only; clocks / IO stay fuse-IL).
    Host,
    /// Heap index / pin (I5 maps; dense refuse).
    Index,
    /// Escaping / heap-backed `GetField` / `SetField` / `LoadField`.
    HeapField,
    /// `BoxValue` / `UnboxValue`.
    Box,
    /// `JumpIfMatch` outside I2 (tag > 1 or arity ≠ 1).
    Match,
    /// `Unpack` arity > 1.
    Unpack,
}

/// Production LIR entry after dense specialize misses.
///
/// Eligible when the body has no hard refuse. I1 niche words, compare-only
/// diamonds, and below-W3 numeric helpers may lift; I2 match and I3
/// unboxed fields still do. `IlModule` still replaces only when LIR cost
/// ≤ opted fuse-IL.
pub fn lir_eligible(ops: &[IlOp], unboxed_fields: &[(u32, u32)]) -> bool {
    lir_refuse(ops, unboxed_fields).is_none()
}

/// First hard refuse, if any. `unboxed_fields` unused: I3 ranges are
/// hints for lower, not a gate (I8).
pub fn lir_refuse(ops: &[IlOp], _unboxed_fields: &[(u32, u32)]) -> Option<LirRefuse> {
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
    fn i8_compare_diamond_is_lir_eligible() {
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
