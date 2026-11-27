//! MIR type lattice (COI-267 P0 numeric; COI-293 I1 heap / niche words).
//!
//! [`MirTy::Value`] is the boxed word and the
//! lattice top. I1 names shipped one-word heap/niche ABIs so later islands
//! can SSA them. Dense specialize uses [`MirTy::is_word_lane`] (numeric,
//! `HeapRef`, and niche Option/Result words). Two-slot CALL/RETURN may
//! native code (B3 / C1).

#[cfg(test)]
use crate::typechecking::infer::Checker;
#[cfg(test)]
use crate::typechecking::{Ty, ty as coil_ty, ty::is_option_ty};

use crate::layout::MirLayout;

/// Specialized numeric + heap/niche types plus lattice bounds.
///
/// ```text
///                         Value (⊤)
///          /        /       |        \
///     MaybeObj    F64     Bool   NicheOpt / NicheRes
///        |                            \   /
///       I64                          HeapRef
///                    \
///                  Bottom (⊥)
/// ```
///
/// [`MirTy::MaybeObj`] is an `i64` word that may hold an object: every
/// engine and pass treats it as an [`MirTy::I64`] word, and reference
/// counting counts it with a run-time address test.
///
/// Language `int` / `float` / `bool` map to [`MirTy::I64`] / [`MirTy::F64`].
/// Heap/niche variants are one `Value` word (COI-92), not two-slot.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum MirTy {
    Bottom,
    I64,
    F64,
    Bool,
    /// Aligned GC heap pointer (class / `string` / tuple / list / boxed enum).
    HeapRef,
    /// Heap `Option<T>` word: `None` = `0`, `Some` = aligned pointer.
    NicheOpt,
    /// Heap-heap `Result<T,E>` word: `Ok` = aligned pointer, `Err` = `ptr | 1`.
    NicheRes,
    /// Boxed runtime `Value`. Not specialized; P1+ keeps this ABI at edges.
    Value,
    /// An `i64` word that is an immediate or a heap pointer, decided at run
    /// time: the low word of a pair whose payload may be an object (a call's
    /// result, a parameter, a host call's), and an element a `Vec`
    /// expansion holds across its loop (`native_expand`). In registers,
    /// frames and every operand position it is an `i64` ([`Self::is_int`],
    /// [`Self::word_class`]); reference counting counts it, testing the
    /// address at run time, where a plain `i64` it counts only when it is
    /// read from memory or a call returns it (`rc_insert`).
    MaybeObj,
}

impl MirTy {
    #[cfg(test)]
    pub const TOP: Self = Self::Value;

    /// An `i64` word: [`Self::I64`] or [`Self::MaybeObj`].
    pub fn is_int(self) -> bool {
        matches!(self, Self::I64 | Self::MaybeObj)
    }

    /// The type an operand check compares: [`Self::MaybeObj`] is an `i64`
    /// word wherever one is expected.
    pub fn word_class(self) -> Self {
        match self {
            Self::MaybeObj => Self::I64,
            t => t,
        }
    }

    pub fn is_float(self) -> bool {
        matches!(self, Self::F64)
    }

    /// Dense / numeric SSA lane (`i64`/`f64`/`bool`).
    pub fn is_numeric(self) -> bool {
        matches!(self, Self::I64 | Self::MaybeObj | Self::F64 | Self::Bool)
    }

    /// One-word dense / CALL lane: numeric, heap pointer, or niche word (Q8).
    pub fn is_word_lane(self) -> bool {
        self.is_numeric() || self.is_heap_word()
    }

    /// One-word heap pointer or shipped niche Option/Result (COI-92).
    pub fn is_heap_word(self) -> bool {
        matches!(self, Self::HeapRef | Self::NicheOpt | Self::NicheRes)
    }

    /// Named SSA type (numeric or heap/niche). Not [`Self::Value`] / [`Self::Bottom`].
    pub fn is_specialized(self) -> bool {
        !matches!(self, Self::Bottom | Self::Value)
    }

    /// Call-edge layout for a single SSA word. Two-slot is not a `MirTy`.
    pub fn layout(self) -> MirLayout {
        match self {
            Self::NicheOpt | Self::NicheRes => MirLayout::HeapNiche,
            _ => MirLayout::Word,
        }
    }

    /// Least upper bound.
    pub fn join(self, other: Self) -> Self {
        use MirTy::*;
        if self == other {
            return self;
        }
        if self == Bottom {
            return other;
        }
        if other == Bottom {
            return self;
        }
        match (self, other) {
            (HeapRef, HeapRef) => HeapRef,
            (I64, MaybeObj) | (MaybeObj, I64) => MaybeObj,
            // An aligned heap pointer is a valid `Some` / `Ok` niche word.
            (NicheOpt, NicheOpt) | (HeapRef, NicheOpt) | (NicheOpt, HeapRef) => NicheOpt,
            (NicheRes, NicheRes) | (HeapRef, NicheRes) | (NicheRes, HeapRef) => NicheRes,
            _ => Value,
        }
    }

    /// Greatest lower bound.
    #[cfg(test)]
    pub fn meet(self, other: Self) -> Self {
        use MirTy::*;
        if self == other {
            return self;
        }
        if self == Value {
            return other;
        }
        if other == Value {
            return self;
        }
        if self == Bottom || other == Bottom {
            return Bottom;
        }
        match (self, other) {
            (HeapRef, NicheOpt | NicheRes) | (NicheOpt | NicheRes, HeapRef) => HeapRef,
            _ => Bottom,
        }
    }

    /// `self ⊑ other` in the lattice.
    pub fn le(self, other: Self) -> bool {
        self.join(other) == other
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Bottom => "bottom",
            Self::I64 => "i64",
            Self::F64 => "f64",
            Self::Bool => "bool",
            Self::HeapRef => "heapref",
            Self::NicheOpt => "niche_opt",
            Self::NicheRes => "niche_res",
            Self::Value => "value",
            Self::MaybeObj => "maybe_obj",
        }
    }

    #[cfg(test)]
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "bottom" => Self::Bottom,
            "i64" => Self::I64,
            "f64" => Self::F64,
            "bool" => Self::Bool,
            "heapref" => Self::HeapRef,
            "niche_opt" => Self::NicheOpt,
            "niche_res" => Self::NicheRes,
            "value" => Self::Value,
            "maybe_obj" => Self::MaybeObj,
            "int" => Self::I64,
            "float" => Self::F64,
            _ => return None,
        })
    }

    #[cfg(test)]
    /// Map a coil HM monotype onto the lattice.
    ///
    /// Immediate `int`/`float`/`bool`/`byte` are numeric lanes. Ground heap
    /// objects are [`Self::HeapRef`]. Shipped niche Option/Result words are
    /// [`Self::NicheOpt`] / [`Self::NicheRes`]. Two-slot pairs and boxed /
    /// unsure shapes stay [`Self::Value`] (not one SSA word).
    pub fn from_coil_ty(checker: &Checker, ty: &Ty) -> Self {
        match ty {
            Ty::Readonly(inner) => return Self::from_coil_ty(checker, inner),
            Ty::Con(n) if n == coil_ty::INT || n == coil_ty::BYTE => return Self::I64,
            Ty::Con(n) if n == coil_ty::FLOAT => return Self::F64,
            Ty::Con(n) if n == coil_ty::BOOL => return Self::Bool,
            Ty::Con(n) if n == coil_ty::UNIT => return Self::Value,
            _ => {}
        }
        match MirLayout::from_coil_ty(checker, ty) {
            MirLayout::HeapNiche if is_option_ty(ty) => Self::NicheOpt,
            MirLayout::HeapNiche => Self::NicheRes,
            MirLayout::TwoSlot => Self::Value,
            MirLayout::Word => {
                if crate::layout::niche_heap_only(checker, ty) {
                    Self::HeapRef
                } else {
                    match ty {
                        Ty::Con(n) => Self::parse(n).unwrap_or(Self::Value),
                        _ => Self::Value,
                    }
                }
            }
        }
    }
}

impl std::fmt::Display for MirTy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lattice_cross_family_is_value() {
        assert_eq!(MirTy::TOP, MirTy::Value);
        assert_eq!(MirTy::I64.join(MirTy::F64), MirTy::Value);
        assert_eq!(MirTy::I64.join(MirTy::Bool), MirTy::Value);
        assert_eq!(MirTy::I64.meet(MirTy::F64), MirTy::Bottom);
        assert!(MirTy::I64.le(MirTy::Value));
        assert!(MirTy::Bottom.le(MirTy::Bool));
    }

    #[test]
    fn maybe_obj_is_an_int_word() {
        assert!(MirTy::MaybeObj.is_int());
        assert!(MirTy::MaybeObj.is_numeric());
        assert!(!MirTy::MaybeObj.is_heap_word());
        assert!(MirTy::MaybeObj.is_specialized());
        assert_eq!(MirTy::MaybeObj.word_class(), MirTy::I64);
        assert_eq!(MirTy::I64.join(MirTy::MaybeObj), MirTy::MaybeObj);
        assert!(MirTy::I64.le(MirTy::MaybeObj));
        assert_eq!(MirTy::parse("maybe_obj"), Some(MirTy::MaybeObj));
    }

    #[test]
    fn language_types_map_to_wide_lanes() {
        assert_eq!(MirTy::from_coil_ty(&Checker::new(), &coil_ty::int()), MirTy::I64);
        assert_eq!(MirTy::from_coil_ty(&Checker::new(), &coil_ty::float()), MirTy::F64);
        assert_eq!(MirTy::from_coil_ty(&Checker::new(), &coil_ty::boolean()), MirTy::Bool);
        assert_eq!(MirTy::from_coil_ty(&Checker::new(), &Ty::Con("string".into())), MirTy::HeapRef);
        assert_eq!(MirTy::from_coil_ty(&Checker::new(), &coil_ty::byte()), MirTy::I64);
    }

    #[test]
    fn heap_and_niche_are_ssa_not_numeric() {
        assert!(MirTy::HeapRef.is_specialized());
        assert!(MirTy::NicheOpt.is_heap_word());
        assert!(MirTy::NicheRes.is_heap_word());
        assert!(!MirTy::HeapRef.is_numeric());
        assert!(!MirTy::NicheOpt.is_numeric());
        assert!(MirTy::I64.is_numeric());
        assert!(MirTy::NicheOpt.is_word_lane());
        assert!(MirTy::NicheRes.is_word_lane());
        assert_eq!(MirTy::HeapRef.join(MirTy::NicheOpt), MirTy::NicheOpt);
        assert_eq!(MirTy::HeapRef.join(MirTy::NicheRes), MirTy::NicheRes);
        assert_eq!(MirTy::NicheOpt.join(MirTy::NicheRes), MirTy::Value);
        assert_eq!(MirTy::HeapRef.meet(MirTy::NicheRes), MirTy::HeapRef);
        assert!(MirTy::HeapRef.le(MirTy::NicheRes));
        assert!(MirTy::HeapRef.le(MirTy::Value));
        assert_eq!(MirTy::HeapRef.layout(), MirLayout::Word);
        assert_eq!(MirTy::NicheOpt.layout(), MirLayout::HeapNiche);
        assert_eq!(MirTy::NicheRes.layout(), MirLayout::HeapNiche);
    }

    #[test]
    fn shipped_option_result_map_to_niche_words() {
        use crate::typechecking::ty::{option_ty, result_ty, string};
        assert_eq!(
            MirTy::from_coil_ty(&Checker::new(), &option_ty(string())),
            MirTy::NicheOpt
        );
        assert_eq!(
            MirTy::from_coil_ty(&Checker::new(), &result_ty(string(), string())),
            MirTy::NicheRes
        );
        assert_eq!(
            MirTy::from_coil_ty(&Checker::new(), &option_ty(coil_ty::int())),
            MirTy::Value
        );
        assert_eq!(
            MirTy::from_coil_ty(&Checker::new(), &option_ty(option_ty(coil_ty::int()))),
            MirTy::Value
        );
        assert_eq!(
            MirTy::from_coil_ty(&Checker::new(), &result_ty(string(), coil_ty::int())),
            MirTy::Value
        );
    }
}
