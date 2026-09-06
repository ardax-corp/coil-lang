//! Numeric MIR type lattice (COI-267 P0).
//!
//! Ptr / GC refs are later. [`MirTy::Value`] is the boxed VM word — the
//! existing interpreter path — and the lattice top.

use crate::typechecking::{Ty, ty as coil_ty};

/// Specialized numeric types plus lattice bounds.
///
/// ```text
///                    Value (⊤)
///               /    |     |    \
///             I64   F64   Bool   …
///              |     |
///             I32   F32
///                  |
///               Bottom (⊥)
/// ```
///
/// Language `int` / `float` map to [`MirTy::I64`] / [`MirTy::F64`]. `i32` and
/// `f32` exist for later dense / SIMD cuts; they are not language spellings.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum MirTy {
    Bottom,
    I32,
    I64,
    F32,
    F64,
    Bool,
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

    pub fn is_specialized(self) -> bool {
        !matches!(self, Self::Bottom | Self::Value)
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
            "value" => Self::Value,
            "int" => Self::I64,
            "float" => Self::F64,
            _ => return None,
        })
    }

    /// Map a coil HM monotype onto the lattice. Classes / heap stay [`MirTy::Value`].
    pub fn from_coil_ty(ty: &Ty) -> Self {
        match ty {
            Ty::Readonly(inner) => Self::from_coil_ty(inner),
            Ty::Con(n) if n == coil_ty::INT => Self::I64,
            Ty::Con(n) if n == coil_ty::FLOAT => Self::F64,
            Ty::Con(n) if n == coil_ty::BOOL => Self::Bool,
            Ty::Con(n) => Self::parse(n).unwrap_or(Self::Value),
            _ => Self::Value,
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
        assert_eq!(MirTy::from_coil_ty(&Ty::Con("string".into())), MirTy::Value);
    }
}
