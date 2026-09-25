//! One effect table for IL ops.
//!
//! CSE, LICM and the loop length proof each used to carry their own "is this a
//! barrier" list, and the lists drifted (GVN missed yields, pinned stores and
//! residual bytes; LICM missed indirect calls and coroutine resumes). Passes now ask [`effects`] and combine the bits they care
//! about. Slot stores are not an effect here: every pass already tracks slot
//! defs itself.

use common::Instruction;

use super::op::{EntryKind, IlJumpKind, IlOp};
use super::pure_call::PureCallCtx;

/// Effect bits of one IL op.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Effects(u16);

impl Effects {
    pub(crate) const NONE: Effects = Effects(0);
    /// Call into user code the purity sidecar cannot prove pure (or any
    /// non-call `Entry`: tail call, coroutine, code pointer, poly fn).
    pub(crate) const CALL: Effects = Effects(1 << 0);
    /// Host native, print, FFI.
    pub(crate) const HOST: Effects = Effects(1 << 1);
    /// `FORMAT` / `STRINGIFY` string-table ops (allocate, no user code).
    pub(crate) const FORMAT: Effects = Effects(1 << 2);
    /// Dynamic field get (`GetField`) — may run lookup code.
    pub(crate) const FIELD_READ: Effects = Effects(1 << 3);
    /// Field store into an object.
    pub(crate) const FIELD_WRITE: Effects = Effects(1 << 4);
    /// Element store into an array (length unchanged).
    pub(crate) const ELEM_WRITE: Effects = Effects(1 << 5);
    /// Array grow (`ArrayPush`): length changes.
    pub(crate) const GROW: Effects = Effects(1 << 6);
    /// Fresh heap object (aggregate builder, box, dict).
    pub(crate) const ALLOC: Effects = Effects(1 << 7);
    /// Coroutine suspension point.
    pub(crate) const YIELD: Effects = Effects(1 << 8);
    /// `JumpIfMatch` (unpacks / branches on an enum payload).
    pub(crate) const MATCH: Effects = Effects(1 << 9);
    /// Residual `Byte` this table does not model.
    pub(crate) const UNKNOWN: Effects = Effects(1 << 10);
    const ALL: Effects = Effects((1 << 11) - 1);

    pub(crate) const fn union(self, other: Effects) -> Effects {
        Effects(self.0 | other.0)
    }

    pub(crate) const fn any(self, mask: Effects) -> bool {
        self.0 & mask.0 != 0
    }
}

impl std::ops::Not for Effects {
    type Output = Effects;
    fn not(self) -> Effects {
        Effects(!self.0 & Effects::ALL.0)
    }
}

impl std::ops::BitOr for Effects {
    type Output = Effects;
    fn bitor(self, rhs: Effects) -> Effects {
        self.union(rhs)
    }
}

/// Effects of `op`. A `CALL` is effect-free only when `purity` proves the
/// callee pure and it returns one word.
pub(crate) fn effects(op: &IlOp, purity: Option<&PureCallCtx>) -> Effects {
    match op {
        IlOp::HostInvoke { .. } | IlOp::Print { .. } => Effects::HOST,
        IlOp::GetField { .. } => Effects::FIELD_READ,
        IlOp::SetField { .. } => Effects::FIELD_WRITE,
        IlOp::StoreIndexPin { .. } | IlOp::StoreIndexPinUnchecked { .. } => Effects::ELEM_WRITE,
        IlOp::MakeTuple { .. } | IlOp::MakeArray { .. } | IlOp::MakeEnum { .. } | IlOp::BoxValue { .. } => {
            Effects::ALLOC
        }
        IlOp::Entry {
            kind: EntryKind::Call,
            target,
            ret_words,
            ..
        } => {
            if *ret_words == 1 && purity.is_some_and(|c| c.call_is_pure(*target)) {
                Effects::NONE
            } else {
                Effects::CALL
            }
        }
        IlOp::Entry { .. } => Effects::CALL,
        IlOp::Jump {
            kind: IlJumpKind::JumpIfMatch { .. },
            ..
        } => Effects::MATCH,
        IlOp::Byte { byte, .. } => byte_effects(byte, purity),
        _ => Effects::NONE,
    }
}

fn byte_effects(byte: &common::Byte, purity: Option<&PureCallCtx>) -> Effects {
    use Instruction::*;
    match *byte.bytecode() {
        CALL => {
            let (_, target) = byte.call_parts();
            if byte.call_ret_words() == 1
                && purity.is_some_and(|c| c.call_offset_is_pure(target as u32))
            {
                Effects::NONE
            } else {
                Effects::CALL
            }
        }
        TailCall | MakeCoro | CallIndirect | ResumeCoro => Effects::CALL,
        HostInvoke | PRINT | FfiInvoke => Effects::HOST,
        FORMAT | STRINGIFY => Effects::FORMAT,
        GetField => Effects::FIELD_READ,
        SetField => Effects::FIELD_WRITE,
        StoreIndex | StoreIndexUnchecked | StoreIndexPin | StoreIndexPinUnchecked => {
            Effects::ELEM_WRITE
        }
        ArrayPush => Effects::GROW,
        MakeTuple | MakeArray | MakeEnum | BoxValue | MakeDict => Effects::ALLOC,
        YieldCoro | YieldFromCoro => Effects::YIELD,
        CONST | STRING | LOAD | DUPLICATE | POP | ADD | SUB | MUL | DIV | MOD | ADDF | SUBF
        | MULF | DIVF | MODF | BITAND | BITOR | XOR | SHL | SHR | EQ | NEQ | LE | LEQ | GT
        | GEQ | LEF | LEQF | GTF | GEQF | BinSlotImm | BinSlotSlot | Index | IndexUnchecked
        | LoadField | ArrayLen | CastIntToFloat => Effects::NONE,
        _ => Effects::UNKNOWN,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::{Byte, DebugLoc};

    fn byte(i: Instruction) -> IlOp {
        IlOp::byte(Byte::new(i))
    }

    #[test]
    fn yields_and_pinned_stores_are_effects() {
        assert!(effects(&byte(Instruction::YieldCoro), None).any(Effects::YIELD));
        let pin = IlOp::StoreIndexPin {
            slot: 0,
            loc: DebugLoc::unknown(),
        };
        assert!(effects(&pin, None).any(Effects::ELEM_WRITE));
    }

    #[test]
    fn unmodelled_bytes_are_unknown() {
        assert_eq!(effects(&byte(Instruction::Seek), None), Effects::UNKNOWN);
        assert_eq!(effects(&byte(Instruction::ArrayLen), None), Effects::NONE);
    }
}
