//! The runtime call an `Alloc` makes, which both MIR engines (Cranelift's
//! code and the interpreter) pass to `coil_native_alloc`.

use common::native_alloc::{
    ALLOC_ARRAY, ALLOC_CORO, ALLOC_DROP_ENUM, ALLOC_ENUM, ALLOC_FN, ALLOC_LANES, ALLOC_OBJECT, ALLOC_POLY_FN,
};

use super::MirAllocKind;
use crate::bodies::Label;

/// `coil_native_alloc`'s arguments for one `Alloc`: its kind code, the
/// word count, the `extra` word, and the word passed after the element
/// words (a function value's fill mask, a drop enum's type id).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AllocCall {
    pub code: u64,
    pub n: u64,
    pub extra: u64,
    pub last: Option<u64>,
}

/// The call an `Alloc { kind, kinds, .. }` of `words` element words makes.
/// `packed` gives the words' kinds packed from their types (asked only
/// when `kinds` records none and the kind needs them); `pc_of` gives a
/// body's entry id. `Err` is a target with no entry id.
pub fn alloc_call(
    kind: MirAllocKind,
    words: usize,
    kinds: u8,
    packed: impl FnOnce() -> u8,
    pc_of: impl FnOnce(Label) -> Option<u32>,
) -> Result<AllocCall, Label> {
    let n = words as u64;
    let kinds_or_packed = || u64::from(if kinds != 0 { kinds } else { packed() });
    let call = |code, n, extra, last| AllocCall { code, n, extra, last };
    Ok(match kind {
        MirAllocKind::Array => call(ALLOC_ARRAY, n, u64::from(kinds), None),
        MirAllocKind::Lanes { tag_mask } => call(ALLOC_LANES, n, tag_mask, None),
        MirAllocKind::Enum { tag } => call(ALLOC_ENUM, n, u64::from(tag) | (kinds_or_packed() << 32), None),
        MirAllocKind::Object { type_id, nfields } => call(ALLOC_OBJECT, u64::from(nfields), u64::from(type_id), None),
        MirAllocKind::DropEnum { tag, type_id } => {
            call(ALLOC_DROP_ENUM, n + 1, u64::from(tag) | (kinds_or_packed() << 32), Some(u64::from(type_id)))
        }
        MirAllocKind::Fn { target, operand, mask } => {
            let pc = pc_of(target).ok_or(target)?;
            call(ALLOC_FN, n + 1, u64::from(pc) | (u64::from(operand) << 32), Some(u64::from(mask)))
        }
        MirAllocKind::Coro { target } => call(ALLOC_CORO, n, u64::from(pc_of(target).ok_or(target)?), None),
        MirAllocKind::PolyFn { target } => call(ALLOC_POLY_FN, n, u64::from(pc_of(target).ok_or(target)?), None),
        MirAllocKind::PolyFnCapture { target, params } => {
            let pc = pc_of(target).ok_or(target)?;
            call(ALLOC_POLY_FN, n, u64::from(pc) | (u64::from(params) << 32), None)
        }
    })
}
