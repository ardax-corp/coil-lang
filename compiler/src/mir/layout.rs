//! Result/Option ABI layouts for MIR→LIR (COI-270) and I1 SSA word names.
//!
//! Matches the shipped checker/codegen contract in
//! [`crate::typechecking::return_layout`] and
//! `docs/internals/limitations.md` (COI-92). No new pair opcodes and no
//! nursery: two-slot is `[payload, tag]` (or `[a, b]`) on direct
//! `CALL`/`RETURN`; heap niches are one `Value` word.

use crate::hir::layout::Layout;
use crate::typechecking::infer::Checker;
use crate::typechecking::ty::Ty;

/// How a Result/Option (or arity-2 immediate product) crosses a call edge.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub enum MirLayout {
    /// One VM `Value` (boxed `ObjEnum`, scalar, or heap niche word).
    #[default]
    Word,
    /// Direct `CALL`/`RETURN` width 2. Enums are `[payload, tag]`; products
    /// are `[a, b]` (second on top).
    TwoSlot,
    /// Heap `Option<T>` (`None` = `0`) or heap-heap `Result<T,E>`
    /// (`Ok` = aligned pointer, `Err` = `pointer | 1`). Host packs at the
    /// HostInvoke edge; LIR uses `CONST 0` / `BITAND` / `BITOR` / `LogNot`.
    /// SSA names the word [`crate::mir::MirTy::NicheOpt`] vs
    /// [`crate::mir::MirTy::NicheRes`]; this variant is the shared ABI.
    HeapNiche,
}

impl MirLayout {
    pub fn words(self) -> u8 {
        match self {
            Self::TwoSlot => 2,
            Self::Word | Self::HeapNiche => 1,
        }
    }

    /// One-word heap/niche ABI (I1). Two-slot is a pair, not this.
    pub fn is_heap_word(self) -> bool {
        matches!(self, Self::HeapNiche)
    }

    /// The MIR view of [`crate::hir::layout::of_resolved`]. Codegen pins
    /// user payload enum pairs from the same query.
    pub fn from_coil_ty(checker: &Checker, ty: &Ty) -> Self {
        Self::from(&crate::hir::layout::of_resolved(checker, ty))
    }
}

impl From<&Layout> for MirLayout {
    fn from(layout: &Layout) -> Self {
        match layout {
            Layout::Word => Self::Word,
            Layout::Pair(_) => Self::TwoSlot,
            Layout::NicheOption | Layout::NicheUnitResult | Layout::NicheResult => Self::HeapNiche,
        }
    }
}

impl std::fmt::Display for MirLayout {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Word => "word",
            Self::TwoSlot => "twoslot",
            Self::HeapNiche => "heap_niche",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::typechecking::ty::{option_ty, result_ty, INT, STRING};

    fn layout(ty: &Ty) -> MirLayout {
        MirLayout::from_coil_ty(&Checker::new(), ty)
    }

    fn int() -> Ty {
        Ty::Con(INT.into())
    }
    fn string() -> Ty {
        Ty::Con(STRING.into())
    }

    #[test]
    fn option_int_is_two_slot() {
        assert_eq!(layout(&option_ty(int())), MirLayout::TwoSlot);
    }

    #[test]
    fn option_string_is_heap_niche() {
        assert_eq!(
            layout(&option_ty(string())),
            MirLayout::HeapNiche
        );
    }

    #[test]
    fn nested_option_stays_word() {
        let ty = option_ty(option_ty(int()));
        assert_eq!(layout(&ty), MirLayout::Word);
    }

    #[test]
    fn result_int_int_is_two_slot() {
        assert_eq!(
            layout(&result_ty(int(), int())),
            MirLayout::TwoSlot
        );
    }

    #[test]
    fn result_unit_int_is_two_slot_but_unit_string_stays_niche() {
        assert_eq!(
            layout(&result_ty(Ty::Tuple(vec![]), int())),
            MirLayout::TwoSlot
        );
        assert_eq!(
            layout(&result_ty(Ty::Tuple(vec![]), string())),
            MirLayout::HeapNiche
        );
    }

    #[test]
    fn result_int_string_is_two_slot() {
        assert_eq!(
            layout(&result_ty(int(), string())),
            MirLayout::TwoSlot
        );
    }

    #[test]
    fn result_string_string_is_heap_niche() {
        assert_eq!(
            layout(&result_ty(string(), string())),
            MirLayout::HeapNiche
        );
    }

    #[test]
    fn result_string_int_stays_boxed() {
        assert_eq!(
            layout(&result_ty(string(), int())),
            MirLayout::Word
        );
    }

    #[test]
    fn product_int_int_is_two_slot() {
        assert_eq!(
            layout(&Ty::Tuple(vec![int(), int()])),
            MirLayout::TwoSlot
        );
    }

    #[test]
    fn words_match_abi() {
        assert_eq!(MirLayout::Word.words(), 1);
        assert_eq!(MirLayout::HeapNiche.words(), 1);
        assert_eq!(MirLayout::TwoSlot.words(), 2);
        assert!(MirLayout::HeapNiche.is_heap_word());
        assert!(!MirLayout::Word.is_heap_word());
    }
}
