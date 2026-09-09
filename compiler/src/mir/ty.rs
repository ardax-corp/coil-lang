//! MIR type lattice (COI-267 P0 numeric; COI-293 I1 heap / niche words).
//!
//! [`MirTy::Value`] is the boxed VM word — the interpreter path — and the
//! lattice top. I1 names shipped one-word heap/niche ABIs so later islands
//! can SSA them. Dense specialize uses [`MirTy::is_word_lane`] (numeric
//! plus `HeapRef`); niche stays LIR.

use crate::typechecking::{Ty, ty as coil_ty, ty::is_option_ty};

use super::layout::MirLayout;

/// Specialized numeric + heap/niche types plus lattice bounds.
///
/// ```text
///                         Value (⊤)
///          /        /       |        \         \
///        I64      F64     Bool    HeapRef   NicheOpt / NicheRes
///         |        |
///        I32      F32
///                    \
///                  Bottom (⊥)
/// ```
///
/// Language `int` / `float` / `bool` map to [`MirTy::I64`] / [`MirTy::F64`].
/// Heap/niche variants are one `Value` word (COI-92), not two-slot.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum MirTy {
    Bottom,
    I32,
    I64,
    F32,
    F64,
    Bool,
    /// Aligned GC heap pointer (class / `string` / tuple / list / boxed enum).
    HeapRef,
    /// Heap `Option<T>` word: `None` = `0`, `Some` = aligned pointer.
    NicheOpt,
    /// Heap-heap `Result<T,E>` word: `Ok` = aligned pointer, `Err` = `ptr | 1`.
    NicheRes,
    /// Boxed VM `Value`. Not specialized; P1+ keeps this ABI at edges.
    Value,
}

impl MirTy {
    pub const TOP: Self = Self::Value;

    pub fn is_int(self) -> bool {
        matches!(self, Self::I32 | Self::I64)
    }

    pub fn is_float(self) -> bool {
        matches!(self, Self::F32 | Self::F64)
    }

    /// Dense / numeric SSA lane (`i32`/`i64`/`f32`/`f64`/`bool`).
    pub fn is_numeric(self) -> bool {
        matches!(self, Self::I32 | Self::I64 | Self::F32 | Self::F64 | Self::Bool)
    }

    /// One-word dense / CALL lane: numeric or a plain heap pointer (S3).
    /// Niche Option/Result stays LIR.
    pub fn is_word_lane(self) -> bool {
        self.is_numeric() || self == Self::HeapRef
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
            (I32, I64) | (I64, I32) => I64,
            (F32, F64) | (F64, F32) => F64,
            (HeapRef, HeapRef) => HeapRef,
            (NicheOpt, NicheOpt) => NicheOpt,
            (NicheRes, NicheRes) => NicheRes,
            _ => Value,
        }
    }

    /// Greatest lower bound.
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
            (I32, I64) | (I64, I32) => I32,
            (F32, F64) | (F64, F32) => F32,
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
            Self::I32 => "i32",
            Self::I64 => "i64",
            Self::F32 => "f32",
            Self::F64 => "f64",
            Self::Bool => "bool",
            Self::HeapRef => "heapref",
            Self::NicheOpt => "niche_opt",
            Self::NicheRes => "niche_res",
            Self::Value => "value",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "bottom" => Self::Bottom,
            "i32" => Self::I32,
            "i64" => Self::I64,
            "f32" => Self::F32,
            "f64" => Self::F64,
            "bool" => Self::Bool,
            "heapref" => Self::HeapRef,
            "niche_opt" => Self::NicheOpt,
            "niche_res" => Self::NicheRes,
            "value" => Self::Value,
            "int" => Self::I64,
            "float" => Self::F64,
            _ => return None,
        })
    }

    /// Map a coil HM monotype onto the lattice.
    ///
    /// Immediate `int`/`float`/`bool`/`byte` are numeric lanes. Ground heap
    /// objects are [`Self::HeapRef`]. Shipped niche Option/Result words are
    /// [`Self::NicheOpt`] / [`Self::NicheRes`]. Two-slot pairs and boxed /
    /// unsure shapes stay [`Self::Value`] (not one SSA word).
    pub fn from_coil_ty(ty: &Ty) -> Self {
        match ty {
            Ty::Readonly(inner) => return Self::from_coil_ty(inner),
            Ty::Con(n) if n == coil_ty::INT || n == coil_ty::BYTE => return Self::I64,
            Ty::Con(n) if n == coil_ty::FLOAT => return Self::F64,
            Ty::Con(n) if n == coil_ty::BOOL => return Self::Bool,
            Ty::Con(n) if n == coil_ty::UNIT => return Self::Value,
            _ => {}
        }
        match MirLayout::from_coil_ty(ty) {
            MirLayout::HeapNiche if is_option_ty(ty) => Self::NicheOpt,
            MirLayout::HeapNiche => Self::NicheRes,
            MirLayout::TwoSlot => Self::Value,
            MirLayout::Word => {
                if super::layout::is_ground_heap(ty) {
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
    fn lattice_int_widens() {
        assert_eq!(MirTy::I32.join(MirTy::I64), MirTy::I64);
        assert_eq!(MirTy::I64.meet(MirTy::I32), MirTy::I32);
        assert!(MirTy::I32.le(MirTy::I64));
        assert!(!MirTy::I64.le(MirTy::I32));
    }

    #[test]
    fn lattice_float_widens() {
        assert_eq!(MirTy::F32.join(MirTy::F64), MirTy::F64);
        assert_eq!(MirTy::F64.meet(MirTy::F32), MirTy::F32);
        assert!(MirTy::F32.le(MirTy::F64));
    }

    #[test]
    fn lattice_cross_family_is_value() {
        assert_eq!(MirTy::TOP, MirTy::Value);
        assert_eq!(MirTy::I64.join(MirTy::F64), MirTy::Value);
        assert_eq!(MirTy::I32.join(MirTy::Bool), MirTy::Value);
        assert_eq!(MirTy::I64.meet(MirTy::F64), MirTy::Bottom);
        assert!(MirTy::I64.le(MirTy::Value));
        assert!(MirTy::Bottom.le(MirTy::Bool));
    }

    #[test]
    fn language_types_map_to_wide_lanes() {
        assert_eq!(MirTy::from_coil_ty(&coil_ty::int()), MirTy::I64);
        assert_eq!(MirTy::from_coil_ty(&coil_ty::float()), MirTy::F64);
        assert_eq!(MirTy::from_coil_ty(&coil_ty::boolean()), MirTy::Bool);
        assert_eq!(MirTy::from_coil_ty(&Ty::Con("string".into())), MirTy::HeapRef);
        assert_eq!(MirTy::from_coil_ty(&coil_ty::byte()), MirTy::I64);
    }

    #[test]
    fn heap_and_niche_are_ssa_not_numeric() {
        assert!(MirTy::HeapRef.is_specialized());
        assert!(MirTy::NicheOpt.is_heap_word());
        assert!(MirTy::NicheRes.is_heap_word());
        assert!(!MirTy::HeapRef.is_numeric());
        assert!(!MirTy::NicheOpt.is_numeric());
        assert!(MirTy::I64.is_numeric());
        assert_eq!(MirTy::HeapRef.join(MirTy::NicheOpt), MirTy::Value);
        assert_eq!(MirTy::HeapRef.meet(MirTy::NicheRes), MirTy::Bottom);
        assert!(MirTy::HeapRef.le(MirTy::Value));
        assert_eq!(MirTy::HeapRef.layout(), MirLayout::Word);
        assert_eq!(MirTy::NicheOpt.layout(), MirLayout::HeapNiche);
        assert_eq!(MirTy::NicheRes.layout(), MirLayout::HeapNiche);
    }

    #[test]
    fn shipped_option_result_map_to_niche_words() {
        use crate::typechecking::ty::{option_ty, result_ty, string};
        assert_eq!(
            MirTy::from_coil_ty(&option_ty(string())),
            MirTy::NicheOpt
        );
        assert_eq!(
            MirTy::from_coil_ty(&result_ty(string(), string())),
            MirTy::NicheRes
        );
        assert_eq!(
            MirTy::from_coil_ty(&option_ty(coil_ty::int())),
            MirTy::Value
        );
        assert_eq!(
            MirTy::from_coil_ty(&option_ty(option_ty(coil_ty::int()))),
            MirTy::Value
        );
        assert_eq!(
            MirTy::from_coil_ty(&result_ty(string(), coil_ty::int())),
            MirTy::Value
        );
    }
}
