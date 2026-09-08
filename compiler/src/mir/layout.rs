//! Result/Option ABI layouts for MIR→LIR (COI-270) and I1 SSA word names.
//!
//! Matches the shipped checker/codegen contract in
//! [`crate::typechecking::return_layout`] and
//! `docs/internals/limitations.md` (COI-92). No new pair opcodes and no
//! nursery: two-slot is `[payload, tag]` (or `[a, b]`) on direct
//! `CALL`/`RETURN`; heap niches are one `Value` word.

use crate::typechecking::ty::{
    is_option_ty, is_result_ty, option_inner, result_ok_err, strip_readonly, Ty, BOOL, BYTE, FLOAT,
    INT,
};

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

    /// Builtin Option/Result / arity-2 immediate product only. User payload
    /// enums stay [`MirLayout::Word`] here — codegen still classifies them
    /// via [`crate::typechecking::return_layout::two_word_return_enum`].
    pub fn from_coil_ty(ty: &Ty) -> Self {
        let ty = strip_readonly(ty);
        if let Ty::Tuple(items) = ty {
            if items.len() == 2 && items.iter().all(is_immediate) {
                return Self::TwoSlot;
            }
            return Self::Word;
        }
        if is_option_ty(ty) {
            let Some(inner) = option_inner(ty) else {
                return Self::Word;
            };
            if is_immediate(&inner) {
                return Self::TwoSlot;
            }
            if is_ground_heap(&inner) {
                return Self::HeapNiche;
            }
            return Self::Word;
        }
        if is_result_ty(ty) {
            let Some((ok, err)) = result_ok_err(ty) else {
                return Self::Word;
            };
            if is_immediate(&ok) {
                return Self::TwoSlot;
            }
            if is_ground_heap(&ok) && is_ground_heap(&err) {
                return Self::HeapNiche;
            }
            return Self::Word;
        }
        Self::Word
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

fn is_immediate(ty: &Ty) -> bool {
    matches!(
        strip_readonly(ty),
        Ty::Con(n) if n == INT || n == FLOAT || n == BOOL || n == BYTE
    )
}

/// Ground heap object, not a nested Option/Result (those stay boxed).
pub(crate) fn is_ground_heap(ty: &Ty) -> bool {
    let ty = strip_readonly(ty);
    if is_immediate(ty) || is_option_ty(ty) || is_result_ty(ty) {
        return false;
    }
    match ty {
        Ty::Var(_) | Ty::Fun(_, _) | Ty::Existential { .. } | Ty::Forall { .. } | Ty::Never => {
            false
        }
        Ty::Con(_)
        | Ty::List(_)
        | Ty::Tuple(_)
        | Ty::Record { .. }
        | Ty::Array { .. }
        | Ty::Sum { .. }
        | Ty::App(_, _)
        | Ty::Constructor { .. } => true,
        Ty::Readonly(_) => unreachable!("stripped"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::typechecking::ty::{option_ty, result_ty, STRING};

    fn int() -> Ty {
        Ty::Con(INT.into())
    }
    fn string() -> Ty {
        Ty::Con(STRING.into())
    }

    #[test]
    fn option_int_is_two_slot() {
        assert_eq!(MirLayout::from_coil_ty(&option_ty(int())), MirLayout::TwoSlot);
    }

    #[test]
    fn option_string_is_heap_niche() {
        assert_eq!(
            MirLayout::from_coil_ty(&option_ty(string())),
            MirLayout::HeapNiche
        );
    }

    #[test]
    fn nested_option_stays_word() {
        let ty = option_ty(option_ty(int()));
        assert_eq!(MirLayout::from_coil_ty(&ty), MirLayout::Word);
    }

    #[test]
    fn result_int_int_is_two_slot() {
        assert_eq!(
            MirLayout::from_coil_ty(&result_ty(int(), int())),
            MirLayout::TwoSlot
        );
    }

    #[test]
    fn result_int_string_is_two_slot() {
        assert_eq!(
            MirLayout::from_coil_ty(&result_ty(int(), string())),
            MirLayout::TwoSlot
        );
    }

    #[test]
    fn result_string_string_is_heap_niche() {
        assert_eq!(
            MirLayout::from_coil_ty(&result_ty(string(), string())),
            MirLayout::HeapNiche
        );
    }

    #[test]
    fn result_string_int_stays_boxed() {
        assert_eq!(
            MirLayout::from_coil_ty(&result_ty(string(), int())),
            MirLayout::Word
        );
    }

    #[test]
    fn product_int_int_is_two_slot() {
        assert_eq!(
            MirLayout::from_coil_ty(&Ty::Tuple(vec![int(), int()])),
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
