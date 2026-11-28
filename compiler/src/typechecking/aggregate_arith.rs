//! Aggregate (tuple / array) arithmetic shapes for the numeric tower.
//!
//! Homogeneous numeric tuples and fixed-length arrays support element-wise
//! and scalar-broadcast ops. Dynamic `[T] ⊕ [T]` is a hard type error.

use super::ty::{ArrayLength, Ty, vec_app_ty, vec_element_ty};

/// Which side of a broadcast is the scalar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScalarSide {
    Left,
    Right,
}

/// Arithmetic operator lowered element-wise on aggregates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AggregateOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Pow,
    Neg,
}

impl AggregateOp {
    pub fn from_str(op: &str) -> Option<Self> {
        match op {
            "+" => Some(Self::Add),
            "-" => Some(Self::Sub),
            "*" => Some(Self::Mul),
            "/" => Some(Self::Div),
            "%" => Some(Self::Mod),
            "**" => Some(Self::Pow),
            "neg" => Some(Self::Neg),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Add => "+",
            Self::Sub => "-",
            Self::Mul => "*",
            Self::Div => "/",
            Self::Mod => "%",
            Self::Pow => "**",
            Self::Neg => "-",
        }
    }
}

/// The container an element-wise operator runs over.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AggregateShape {
    Tuple(usize),
    /// `None` is the dynamic `[T]` / `Vec<T>` (broadcast and negation only:
    /// never zip).
    Array(Option<usize>),
}

impl AggregateShape {
    /// The element count, when it is static.
    pub fn static_len(self) -> Option<usize> {
        match self {
            Self::Tuple(n) => Some(n),
            Self::Array(n) => n,
        }
    }

    pub fn is_tuple(self) -> bool {
        matches!(self, Self::Tuple(_))
    }

    pub fn array(length: ArrayLength) -> Self {
        Self::Array(match length {
            ArrayLength::Static(n) => Some(n),
            ArrayLength::Dynamic => None,
        })
    }
}

/// How the operands of an element-wise operator combine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AggregateMode {
    /// Two aggregates of the same static shape, element by element.
    Zip,
    /// One aggregate and a scalar on the given side.
    Broadcast(ScalarSide),
    /// Unary `-`.
    Neg,
}

impl AggregateMode {
    pub fn scalar_on(self) -> Option<ScalarSide> {
        match self {
            Self::Broadcast(side) => Some(side),
            Self::Zip | Self::Neg => None,
        }
    }
}

/// Codegen recipe for aggregate arithmetic at a node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AggregateArithKind {
    pub shape: AggregateShape,
    pub mode: AggregateMode,
    pub elem_is_float: bool,
    /// Elements are `byte`: `+ - * **` and negation wrap modulo 256, as
    /// the scalar operators do.
    pub elem_is_byte: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AggregateArithInfo {
    pub kind: AggregateArithKind,
    pub op: AggregateOp,
}

/// Classification of one operand for arithmetic.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ArithShape {
    Scalar(Ty),
    Aggregate { elem: Ty, shape: AggregateShape },
}

pub fn is_numeric_elem(ty: &Ty) -> bool {
    matches!(
        ty,
        Ty::Con(n) if n == "int" || n == "float" || n == "byte"
    ) || matches!(ty, Ty::Var(_))
}

pub fn elem_is_float(ty: &Ty) -> bool {
    matches!(ty, Ty::Con(n) if n == "float")
}

pub fn elem_is_byte(ty: &Ty) -> bool {
    matches!(ty, Ty::Con(n) if n == "byte")
}

/// Classify a pruned type as an arithmetic shape.
///
/// Homogeneous tuples only; heterogeneous tuples return `None` from the
/// caller after a separate homogeneity check.
pub fn classify_arith(ty: &Ty) -> ArithShape {
    match ty {
        // Caller must verify homogeneity; use first element as elem.
        Ty::Tuple(elems) if !elems.is_empty() => ArithShape::Aggregate {
            elem: elems[0].clone(),
            shape: AggregateShape::Tuple(elems.len()),
        },
        Ty::Array { element, length } => ArithShape::Aggregate {
            elem: element.as_ref().clone(),
            shape: AggregateShape::array(*length),
        },
        // `Vec<T>` is the dynamic-length array (#890).
        other => match vec_element_ty(other) {
            Some(elem) => ArithShape::Aggregate {
                elem: elem.clone(),
                shape: AggregateShape::Array(None),
            },
            None => ArithShape::Scalar(other.clone()),
        },
    }
}

/// Traits that the numeric tower lifts from element to homogeneous aggregate.
pub fn is_liftable_arith_trait(class: &str) -> bool {
    matches!(class, "Add" | "Sub" | "Mul" | "Div" | "Neg" | "Rem" | "Pow" | "Num")
}

/// If `ty` is a homogeneous tuple or array of a single element type, return
/// that element type. Heterogeneous tuples yield `None`.
pub fn homogeneous_aggregate_elem(ty: &Ty) -> Option<Ty> {
    match ty {
        Ty::Tuple(elems) if !elems.is_empty() => {
            let first = &elems[0];
            if elems.iter().all(|e| e == first) {
                Some(first.clone())
            } else {
                None
            }
        }
        Ty::Array { element, .. } => Some(element.as_ref().clone()),
        _ => vec_element_ty(ty).cloned(),
    }
}

/// `Vec<T>` as the dynamic-length array it is at runtime; other types as is.
fn vec_as_dynamic_array(ty: &Ty) -> Ty {
    match vec_element_ty(ty) {
        Some(elem) => Ty::Array {
            element: Box::new(elem.clone()),
            length: ArrayLength::Dynamic,
        },
        None => ty.clone(),
    }
}

/// The type of an element-wise result: `shape` over `elem`.
pub fn result_ty_for(shape: AggregateShape, elem: Ty) -> Ty {
    match shape {
        AggregateShape::Tuple(arity) => Ty::Tuple(vec![elem; arity]),
        AggregateShape::Array(None) => vec_app_ty(elem),
        AggregateShape::Array(Some(n)) => Ty::Array {
            element: Box::new(elem),
            length: ArrayLength::Static(n),
        },
    }
}

/// Codegen recipe for named linear-algebra helpers (`dot` / `matmul` / `cross`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LinearAlgebraKind {
    /// Equal-length vectors → scalar.
    Dot {
        length: usize,
        left_is_tuple: bool,
        elem_is_float: bool,
        /// Elements are `byte`: the result wraps modulo 256.
        elem_is_byte: bool,
    },
    /// Length-3 vectors → length-3 vector (same container kind as left).
    Cross {
        left_is_tuple: bool,
        elem_is_float: bool,
        elem_is_byte: bool,
    },
    /// Nested static matrices (row-major): `(m×k) × (k×n) → (m×n)`.
    MatMul {
        m: usize,
        k: usize,
        n: usize,
        /// Outer container is a tuple-of-rows (vs array-of-rows).
        outer_is_tuple: bool,
        /// Each row is a tuple (vs array).
        row_is_tuple: bool,
        elem_is_float: bool,
        elem_is_byte: bool,
    },
    /// Element-wise op on equal-shaped `Matrix` values.
    ///
    /// `scalar_on` broadcasts one immediate across every cell (`m == 1`,
    /// `m << 2`). `Add` / `Sub` / `Intersect` / `Diff` stay matrix-matrix.
    MatrixZip {
        m: usize,
        n: usize,
        op: MatrixCellOp,
        outer_is_tuple: bool,
        row_is_tuple: bool,
        elem_is_float: bool,
        /// Cells are `byte`: `+`, `-` and bitwise results wrap to `0..=255`.
        elem_is_byte: bool,
        scalar_on: Option<ScalarSide>,
    },
    /// Element-wise unary `-` or bitwise `~` on a `Matrix`.
    MatrixNeg {
        m: usize,
        n: usize,
        outer_is_tuple: bool,
        row_is_tuple: bool,
        elem_is_float: bool,
        elem_is_byte: bool,
        /// `true` is bitwise `~`; `false` is arithmetic negation.
        bit_not: bool,
    },
}

/// Cell op lowered by `packed_matrix_zip`. Kind numbers are the packed meta
/// `zip_kind` byte (`Add` = 0 and `Sub` = 1 stay stable).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MatrixCellOp {
    Add,
    Sub,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
    Intersect,
    Diff,
}

impl MatrixCellOp {
    pub fn from_str(op: &str) -> Option<Self> {
        match op {
            "+" => Some(Self::Add),
            "-" => Some(Self::Sub),
            "==" => Some(Self::Eq),
            "!=" => Some(Self::Ne),
            "<" => Some(Self::Lt),
            "<=" => Some(Self::Le),
            ">" => Some(Self::Gt),
            ">=" => Some(Self::Ge),
            "&" => Some(Self::BitAnd),
            "|" => Some(Self::BitOr),
            "^" => Some(Self::BitXor),
            "<<" => Some(Self::Shl),
            ">>" => Some(Self::Shr),
            "intersect" => Some(Self::Intersect),
            "diff" => Some(Self::Diff),
            _ => None,
        }
    }

    pub fn zip_kind(self) -> u8 {
        match self {
            Self::Add => 0,
            Self::Sub => 1,
            Self::Eq => 2,
            Self::Ne => 3,
            Self::Lt => 4,
            Self::Le => 5,
            Self::Gt => 6,
            Self::Ge => 7,
            Self::BitAnd => 8,
            Self::BitOr => 9,
            Self::BitXor => 10,
            Self::Shl => 11,
            Self::Shr => 12,
            Self::Intersect => 13,
            Self::Diff => 14,
        }
    }

    /// Compare, intersect, and diff write a `byte` mask of `0` / `1`.
    pub fn is_mask(self) -> bool {
        matches!(
            self,
            Self::Eq
                | Self::Ne
                | Self::Lt
                | Self::Le
                | Self::Gt
                | Self::Ge
                | Self::Intersect
                | Self::Diff
        )
    }

    pub fn is_bitwise(self) -> bool {
        matches!(
            self,
            Self::BitAnd | Self::BitOr | Self::BitXor | Self::Shl | Self::Shr
        )
    }

    /// Scalar broadcast (`matrix OP scalar` and the reverse).
    pub fn allows_broadcast(self) -> bool {
        !matches!(self, Self::Add | Self::Sub | Self::Intersect | Self::Diff)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinearAlgebraInfo {
    pub kind: LinearAlgebraKind,
}

/// Classify a homogeneous numeric vector for `dot` / `cross`.
pub fn classify_vector(ty: &Ty) -> Option<(Ty /*elem*/, usize /*len*/, bool /*is_tuple*/)> {
    match ty {
        Ty::Tuple(elems) if !elems.is_empty() => {
            let elem = homogeneous_aggregate_elem(ty)?;
            Some((elem, elems.len(), true))
        }
        Ty::Array {
            element,
            length: ArrayLength::Static(n),
        } if *n > 0 => Some((element.as_ref().clone(), *n, false)),
        _ => None,
    }
}

/// Classify a nested static matrix (rows × cols) for `matmul`.
///
/// Accepts `[[T; N]; M]` or a homogeneous tuple of equal-arity row tuples/arrays.
pub fn classify_matrix(
    ty: &Ty,
) -> Option<(
    Ty,    /*elem*/
    usize, /*m*/
    usize, /*n*/
    bool,  /*outer_tuple*/
    bool,  /*row_tuple*/
)> {
    match ty {
        Ty::Array {
            element,
            length: ArrayLength::Static(m),
        } if *m > 0 => match element.as_ref() {
            Ty::Array {
                element: cell,
                length: ArrayLength::Static(n),
            } if *n > 0 => Some((cell.as_ref().clone(), *m, *n, false, false)),
            Ty::Tuple(row) if !row.is_empty() => {
                let elem = homogeneous_aggregate_elem(element)?;
                Some((elem, *m, row.len(), false, true))
            }
            _ => None,
        },
        Ty::Tuple(rows) if !rows.is_empty() => {
            let first = &rows[0];
            if !rows.iter().all(|r| r == first) {
                return None;
            }
            match first {
                Ty::Array {
                    element,
                    length: ArrayLength::Static(n),
                } if *n > 0 => Some((element.as_ref().clone(), rows.len(), *n, true, false)),
                Ty::Tuple(cols) if !cols.is_empty() => {
                    let elem = homogeneous_aggregate_elem(first)?;
                    Some((elem, rows.len(), cols.len(), true, true))
                }
                _ => None,
            }
        }
        _ => None,
    }
}

/// True when `ty` is `Matrix<Data>` (nominal wrapper).
pub fn is_matrix_ty(ty: &Ty) -> bool {
    matches!(
        ty,
        Ty::App(head, args)
            if args.len() == 1
                && matches!(
                    head.as_ref(),
                    Ty::Con(name) if name == common::BUILTIN_MATRIX_TYPE
                )
    )
}

/// `ty` with every `Matrix<Data>` in it replaced by its `Data`: the type
/// of the value at run time.
pub fn erase_matrix_ty(ty: &Ty) -> Ty {
    match unwrap_matrix_ty(ty) {
        Some(data) => erase_matrix_ty(data),
        None => ty.map_children(erase_matrix_ty),
    }
}

/// Peel `Matrix<Data>` → `Data`.
pub fn unwrap_matrix_ty(ty: &Ty) -> Option<&Ty> {
    match ty {
        Ty::App(head, args)
            if args.len() == 1
                && matches!(
                    head.as_ref(),
                    Ty::Con(name) if name == common::BUILTIN_MATRIX_TYPE
                ) =>
        {
            Some(&args[0])
        }
        _ => None,
    }
}

/// Wrap nested matrix data as `Matrix<Data>`.
pub fn wrap_matrix_ty(data: Ty) -> Ty {
    Ty::App(
        Box::new(Ty::Con(common::BUILTIN_MATRIX_TYPE.into())),
        vec![data],
    )
}

/// `Matrix` of `elem` with the same row/column containers as `shape`.
pub fn matrix_of_elem(
    elem: Ty,
    m: usize,
    n: usize,
    outer_is_tuple: bool,
    row_is_tuple: bool,
) -> Ty {
    let row_ty = if row_is_tuple {
        Ty::Tuple(vec![elem.clone(); n])
    } else {
        Ty::Array {
            element: Box::new(elem.clone()),
            length: ArrayLength::Static(n),
        }
    };
    let data = if outer_is_tuple {
        Ty::Tuple(vec![row_ty; m])
    } else {
        Ty::Array {
            element: Box::new(row_ty),
            length: ArrayLength::Static(m),
        }
    };
    wrap_matrix_ty(data)
}

/// The shape and element type of a homogeneous tuple or an array.
fn aggregate_shape(ty: &Ty) -> Option<(AggregateShape, Ty)> {
    let elem = homogeneous_aggregate_elem(ty)?;
    match ty {
        Ty::Tuple(elems) => Some((AggregateShape::Tuple(elems.len()), elem)),
        Ty::Array { length, .. } => Some((AggregateShape::array(*length), elem)),
        _ => None,
    }
}

/// The element-wise shape of `lty op rty` (`-lty` with no `rty`) from the
/// operand types alone, for a body the checker recorded none for (a mono
/// clone of a generic body).
pub fn recover(lty: &Ty, rty: Option<&Ty>, op: AggregateOp) -> Option<AggregateArithInfo> {
    use AggregateMode::{Broadcast, Neg, Zip};
    use AggregateShape::{Array, Tuple};
    let lty = &vec_as_dynamic_array(lty);
    let rty = rty.map(vec_as_dynamic_array);
    let left = aggregate_shape(lty);
    let (shape, mode, elem) = match (op, &rty) {
        (AggregateOp::Neg, None) => {
            let (shape, elem) = left?;
            (shape, Neg, elem)
        }
        (_, Some(rty)) => match (left, aggregate_shape(rty)) {
            (Some((Tuple(a), le)), Some((Tuple(b), re))) if a == b && le == re => (Tuple(a), Zip, le),
            (Some((Array(Some(n)), le)), Some((Array(Some(m)), _))) if n == m => (Array(Some(n)), Zip, le),
            (Some((shape, elem)), None) if is_numeric_elem(rty) => (shape, Broadcast(ScalarSide::Right), elem),
            (None, Some((shape, elem))) if is_numeric_elem(lty) => (shape, Broadcast(ScalarSide::Left), elem),
            _ => return None,
        },
        _ => return None,
    };
    Some(AggregateArithInfo {
        kind: AggregateArithKind {
            shape,
            mode,
            elem_is_float: elem_is_float(&elem),
            elem_is_byte: elem_is_byte(&elem),
        },
        op,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::typechecking::ty::{array, array_fixed, float, int};

    #[test]
    fn classify_tuple_and_array() {
        let t = classify_arith(&Ty::Tuple(vec![int(), int()]));
        assert!(matches!(
            t,
            ArithShape::Aggregate {
                shape: AggregateShape::Tuple(2),
                ..
            }
        ));
        let a = classify_arith(&array_fixed(int(), 3));
        assert!(matches!(
            a,
            ArithShape::Aggregate {
                shape: AggregateShape::Array(Some(3)),
                ..
            }
        ));
    }

    #[test]
    fn homogeneous_aggregate_elem_requires_uniform_tuple() {
        assert_eq!(
            homogeneous_aggregate_elem(&Ty::Tuple(vec![int(), int()])),
            Some(int())
        );
        assert!(
            homogeneous_aggregate_elem(&Ty::Tuple(vec![int(), Ty::Con("float".into())])).is_none()
        );
    }

    #[test]
    fn matrix_wrap_unwrap_round_trip() {
        let data = array_fixed(array_fixed(int(), 2), 2);
        let m = wrap_matrix_ty(data.clone());
        assert!(is_matrix_ty(&m));
        assert_eq!(unwrap_matrix_ty(&m), Some(&data));
    }

    #[test]
    fn classify_vector_distinguishes_tuple_and_array() {
        let tup = classify_vector(&Ty::Tuple(vec![int(), int(), int()])).unwrap();
        assert_eq!(tup.1, 3);
        assert!(tup.2, "tuple flag");
        let arr = classify_vector(&array_fixed(float(), 2)).unwrap();
        assert_eq!(arr.0, float());
        assert_eq!(arr.1, 2);
        assert!(!arr.2, "array flag");
        assert!(
            classify_vector(&array(int())).is_none(),
            "dynamic [T] is not a vector"
        );
    }

    #[test]
    fn classify_matrix_array_of_arrays_and_tuple_of_tuples() {
        let arr = classify_matrix(&array_fixed(array_fixed(int(), 3), 2)).unwrap();
        assert_eq!((arr.1, arr.2, arr.3, arr.4), (2, 3, false, false));
        let tup = classify_matrix(&Ty::Tuple(vec![
            Ty::Tuple(vec![int(), int()]),
            Ty::Tuple(vec![int(), int()]),
        ]))
        .unwrap();
        assert_eq!((tup.1, tup.2, tup.3, tup.4), (2, 2, true, true));
        // Heterogeneous outer rows must fail.
        assert!(
            classify_matrix(&Ty::Tuple(vec![
                Ty::Tuple(vec![int(), int()]),
                Ty::Tuple(vec![int(), int(), int()]),
            ]))
            .is_none()
        );
    }

    #[test]
    fn classify_matrix_mixed_row_containers() {
        // Outer array of row-tuples.
        let mixed = classify_matrix(&array_fixed(Ty::Tuple(vec![int(), int()]), 2)).unwrap();
        assert_eq!((mixed.1, mixed.2, mixed.3, mixed.4), (2, 2, false, true));
        // Outer tuple of row-arrays.
        let mixed2 = classify_matrix(&Ty::Tuple(vec![
            array_fixed(int(), 3),
            array_fixed(int(), 3),
        ]))
        .unwrap();
        assert_eq!(
            (mixed2.1, mixed2.2, mixed2.3, mixed2.4),
            (2, 3, true, false)
        );
    }
}
