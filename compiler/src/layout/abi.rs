//! How a value crosses a call edge: the two-slot direct `CALL`/`RETURN`
//! width, the MIR ABI class, and the typed dense call signature. Each is a
//! view of [`super::rep::of_resolved`].
//!
//! ## Two-slot pairs
//!
//! Two-slot CALL/RETURN width for known ≤2-word return layouts.
//!
//! `Result<int, int>`, `Result<int, heap-object>` (including unit-enum
//! errors), `Result<(), int>` (payload word `0`), `Option<int>`, and user
//! payload enums with arity ≤1 fit in
//! `[payload, tag]` without boxing an `ObjEnum`. A closed arity-2 tuple of
//! immediates (`(int, int)`, `(int, float)`, …) uses the same CALL/RETURN
//! width as `[a, b]` (second word on top) without boxing an `ObjTuple`.
//! Niched heap `Option<T>` / heap-heap `Result<T, E>` already use a
//! strictly better one-word ABI and must stay there (never
//! double-classified). Unbounded `T`, mixed heap products, and wider
//! tuples keep the boxed ABI.
//!
//! ## MIR layout
//!
//! Result/Option ABI layouts for MIR→LIR (COI-270) and I1 SSA word names.
//!
//! Matches [`two_word_kind`] and
//! `docs/internals/limitations.md` (COI-92). No new pair opcodes and no
//! nursery: two-slot is `[payload, tag]` (or `[a, b]`) on direct
//! `CALL`/`RETURN`; heap niches are one `Value` word.
//!
//! ## Dense call ABI
//!
//! Typed dense call/return ABI (COI-291 M1).
//!
//! Dense bodies keep `i64` / `f64` / `bool` in frame slots.
//! A dense→dense `CALL` uses the same one-word Value layout the VM already
//! uses at CALL/RETURN — bits are typed slots, not boxed heap objects.
//!
//! ### Layout
//!
//! | Edge | Words | Slots / stack |
//! |------|-------|----------------|
//! | Args | `arity` | callee slots `0..arity` (same bits as caller `LOAD`s) |
//! | Return | 1..=2 | TOS after `RETURN`; caller `STORE`s dests |
//! | Niche Option/Result | 1 | Q8 word lane (match reconstructs as `Br`) |
//! | Multi-word return | 2 | `[payload, tag]` / `[a, b]` (B3 / C1; cost gate vs fuse) |
//!
//! HostInvoke: LICM hoists scalar-pure math; S3 / Q9 R2 emit I6-typed hosts
//! including `from_bytes` / `to_bytes`. User `CALL` uses this map when the callee is already
//! dense, or an open one-word / two-slot ABI (S3 / B3). Q7 one-word
//! self-`CALL` / `TailCall` use the open one-word ABI. **B7** sibling /
//! mutual `TailCall` and **C1** self two-slot `CALL` / `RETURN` use the
//! same open ABI; keep/refuse is the cost gate. `CallIndirect` stays refuse.
//! HeapRef and niche words are one-word lanes (Q8).
//!
//! SSA models at most dest + `dest_hi`. N>2 is the same `ret_words` field
//! but needs extra dests + archive encoding — not a self-vs-helper wall.

use std::collections::HashMap;

use super::rep::{Layout, PairKind, is_immediate, of_resolved};
use crate::typechecking::infer::Checker;
use crate::mir::{MirFunc, MirTy};
use crate::typechecking::ty::{Ty, range_app};

/// Heap `Range` `InitTyped` id. High so user class ids (from 1) stay put.
pub const RANGE_HEAP_TYPE_ID: u32 = common::RANGE_HEAP_TYPE_ID;

/// Heap `RangeInclusive` `InitTyped` id. Inclusive lives in the type, not a slot.
pub const RANGE_INCLUSIVE_HEAP_TYPE_ID: u32 = common::RANGE_INCLUSIVE_HEAP_TYPE_ID;

/// Slotted heap object for an escaped numeric range (`start` = 0, `end` = 1).
pub fn range_heap_type_id(inclusive: bool) -> u32 {
    if inclusive {
        RANGE_INCLUSIVE_HEAP_TYPE_ID
    } else {
        RANGE_HEAP_TYPE_ID
    }
}

/// `Some(PairKind::Range { .. })` when `ty` is a numeric `Range` /
/// `RangeInclusive` two-slot pair.
pub fn two_word_range_kind(ty: &Ty) -> Option<PairKind> {
    let (elem, inclusive) = range_app(ty)?;
    is_immediate(elem).then_some(PairKind::Range { inclusive })
}

/// `Some(kind)` when direct `CALL`/`RETURN` of a function returning `ty`
/// can move two words instead of boxing: `[payload, tag]` of an enum,
/// `[a, b]` of an arity-2 immediate tuple, `[start, end]` of a numeric
/// range. The kind is used when the pair must be boxed at a boundary that
/// still needs one word (`CallIndirect`, FFI, coroutines).
/// The two-slot view of [`crate::layout::of_resolved`].
pub fn two_word_kind(checker: &Checker, ty: &Ty) -> Option<PairKind> {
    match of_resolved(checker, ty) {
        Layout::Pair(kind) => Some(kind),
        _ => None,
    }
}

/// What the code generator knows of a callee that decides its calling
/// convention. Two slots are only a calling convention: a direct `CALL` /
/// `RETURN` of a ground callee whose address never escapes; everywhere a
/// value must be one word (a field, a capture, a container, a shared
/// body, a dictionary or closure call) it is its box ([`two_word_kind`]'s
/// object, `box_pair_val` in MIR).
#[derive(Clone, Copy, Debug, Default)]
pub struct CalleeFacts {
    /// A function value, partial application, `spawn` or FFI takes its
    /// address: every caller uses the one-word ABI.
    pub escaped: bool,
    /// A `gen fn`: its call is `MakeCoro`, its result the handle word.
    pub coroutine: bool,
    /// A host native: one packed `HostInvoke` word.
    pub host_native: bool,
    /// A trait instance method: a dictionary entry (`CodePtr`), unless a
    /// definition site pinned its result's pair.
    pub instance_method: bool,
    /// An inherent method: its parameters stay one word.
    pub inherent_method: bool,
    /// An overloaded or generic function: its parameters stay one word
    /// (a numeric range's two words excepted).
    pub overloaded_or_generic: bool,
}

/// The two-slot form of a callee's result of type `ret`, if any.
pub fn convention_ret(checker: &Checker, facts: &CalleeFacts, ret: &Ty) -> Option<PairKind> {
    if facts.coroutine || facts.escaped || facts.instance_method || facts.host_native {
        return None;
    }
    two_word_kind(checker, ret)
}

/// The two-slot form of a callee's parameter of type `ty`, if any: a
/// numeric range, or a pair enum `enum_pair` admits. Products and
/// readonly classes stay one boxed word (a boxed tuple would split into
/// `[a, b]` through a temp slot above live operands).
pub fn convention_param(checker: &Checker, facts: &CalleeFacts, ty: &Ty, enum_pair: impl Fn(&PairKind) -> bool) -> Option<PairKind> {
    if facts.escaped || facts.instance_method || facts.inherent_method {
        return None;
    }
    if let Some(kind) = two_word_range_kind(ty) {
        return Some(kind);
    }
    if facts.overloaded_or_generic || facts.coroutine {
        return None;
    }
    two_word_kind(checker, ty).filter(|kind| !matches!(kind, PairKind::Product | PairKind::Class(_)) && (matches!(kind, PairKind::Range { .. }) || enum_pair(kind)))
}

#[cfg(test)]
mod pair_tests {
    use super::*;
    use crate::typechecking::ty::{FLOAT, INT, STRING, option_ty, range_inclusive_ty, range_ty, result_ty};

    fn checker() -> Checker {
        Checker::new()
    }

    #[test]
    fn option_int_is_two_word() {
        let c = checker();
        let ty = option_ty(Ty::Con(INT.into()));
        assert_eq!(
            two_word_kind(&c, &ty),
            Some(PairKind::Option)
        );
    }

    #[test]
    fn option_string_stays_one_word_niche() {
        let c = checker();
        let ty = option_ty(Ty::Con(STRING.into()));
        assert_eq!(two_word_kind(&c, &ty), None);
    }

    #[test]
    fn result_int_int_is_two_word() {
        let c = checker();
        let ty = result_ty(Ty::Con(INT.into()), Ty::Con(INT.into()));
        assert_eq!(
            two_word_kind(&c, &ty),
            Some(PairKind::Result)
        );
    }

    #[test]
    fn result_int_string_is_two_word() {
        let c = checker();
        let ty = result_ty(Ty::Con(INT.into()), Ty::Con(STRING.into()));
        assert_eq!(
            two_word_kind(&c, &ty),
            Some(PairKind::Result)
        );
    }

    #[test]
    fn result_unit_int_is_two_word() {
        let c = checker();
        for unit in [Ty::Con(crate::typechecking::ty::UNIT.into()), Ty::Tuple(vec![])] {
            let ty = result_ty(unit, Ty::Con(INT.into()));
            assert_eq!(
                two_word_kind(&c, &ty),
                Some(PairKind::Result)
            );
        }
    }

    #[test]
    fn result_unit_string_stays_one_word_niche() {
        let c = checker();
        let ty = result_ty(Ty::Tuple(vec![]), Ty::Con(STRING.into()));
        assert_eq!(two_word_kind(&c, &ty), None);
    }

    #[test]
    fn result_string_string_stays_one_word_niche() {
        let c = checker();
        let s = Ty::Con(STRING.into());
        let ty = result_ty(s.clone(), s);
        assert_eq!(two_word_kind(&c, &ty), None);
    }

    #[test]
    fn result_string_int_stays_boxed_ok_is_not_immediate() {
        let c = checker();
        let ty = result_ty(Ty::Con(STRING.into()), Ty::Con(INT.into()));
        assert_eq!(two_word_kind(&c, &ty), None);
    }

    #[test]
    fn unbounded_option_is_not_two_word() {
        let c = checker();
        let ty = option_ty(Ty::Var(crate::typechecking::ty::TyVarId(0)));
        assert_eq!(two_word_kind(&c, &ty), None);
    }

    #[test]
    fn plain_int_is_not_two_word() {
        let c = checker();
        assert_eq!(two_word_kind(&c, &Ty::Con(INT.into())), None);
    }

    fn checked(src: &str) -> Checker {
        let owned = Box::leak(src.to_string().into_boxed_str());
        let ast = parser::Pratt::default().parse(owned).expect("parse");
        let mut c = Checker::new();
        let _ = c.check_program(&ast);
        assert!(c.messages().is_empty(), "{:?}", c.messages());
        c
    }

    #[test]
    fn user_payload_enum_arity_one_is_two_word_regardless_of_variant_order() {
        let src = r#"
enum Cell {
    Num(int),
    Empty,
}
fn cell(int n) -> Cell {
    return Cell::Num(n);
}
"#;
        let c = checked(src);
        assert_eq!(
            two_word_kind(&c, &Ty::Con("Cell".into())),
            Some(PairKind::Enum("Cell".to_string()))
        );

        let src_reordered = r#"
enum Cell {
    Empty,
    Num(int),
}
fn cell(int n) -> Cell {
    return Cell::Num(n);
}
"#;
        let c2 = checked(src_reordered);
        assert_eq!(
            two_word_kind(&c2, &Ty::Con("Cell".into())),
            Some(PairKind::Enum("Cell".to_string()))
        );
    }

    #[test]
    fn user_enum_with_arity_two_variant_stays_boxed() {
        let src = r#"
enum Shape {
    Rect(int, int),
    Empty,
}
fn shape() -> Shape {
    return Shape::Empty;
}
"#;
        let c = checked(src);
        assert_eq!(two_word_kind(&c, &Ty::Con("Shape".into())), None);
    }

    #[test]
    fn int_int_product_is_two_word() {
        let c = checker();
        let ty = Ty::Tuple(vec![Ty::Con(INT.into()), Ty::Con(INT.into())]);
        assert_eq!(
            two_word_kind(&c, &ty),
            Some(PairKind::Product)
        );
    }

    #[test]
    fn mixed_immediate_product_is_two_word() {
        let c = checker();
        let ty = Ty::Tuple(vec![Ty::Con(INT.into()), Ty::Con(FLOAT.into())]);
        assert_eq!(
            two_word_kind(&c, &ty),
            Some(PairKind::Product)
        );
    }

    #[test]
    fn mixed_heap_product_stays_boxed() {
        let c = checker();
        let ty = Ty::Tuple(vec![Ty::Con(INT.into()), Ty::Con(STRING.into())]);
        assert_eq!(two_word_kind(&c, &ty), None);
    }

    #[test]
    fn arity_three_product_stays_boxed() {
        let c = checker();
        let i = Ty::Con(INT.into());
        let ty = Ty::Tuple(vec![i.clone(), i.clone(), i]);
        assert_eq!(two_word_kind(&c, &ty), None);
    }

    #[test]
    fn range_heap_type_ids_differ() {
        assert_ne!(range_heap_type_id(false), 0);
        assert_ne!(range_heap_type_id(true), 0);
        assert_ne!(range_heap_type_id(false), range_heap_type_id(true));
    }

    #[test]
    fn numeric_range_is_two_word() {
        let c = checker();
        assert_eq!(
            two_word_kind(&c, &range_ty(Ty::Con(INT.into()))),
            Some(PairKind::Range { inclusive: false })
        );
        assert_eq!(
            two_word_kind(&c, &range_inclusive_ty(Ty::Con(FLOAT.into()))),
            Some(PairKind::Range { inclusive: true })
        );
    }

    #[test]
    fn string_range_stays_boxed() {
        let c = checker();
        assert_eq!(
            two_word_kind(&c, &range_ty(Ty::Con(STRING.into()))),
            None
        );
    }
}

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
    #[cfg(test)]
    pub fn words(self) -> u8 {
        match self {
            Self::TwoSlot => 2,
            Self::Word | Self::HeapNiche => 1,
        }
    }

    #[cfg(test)]
    /// One-word heap/niche ABI (I1). Two-slot is a pair, not this.
    pub fn is_heap_word(self) -> bool {
        matches!(self, Self::HeapNiche)
    }

    #[cfg(test)]
    /// The MIR view of [`crate::layout::of_resolved`]. Codegen pins
    /// user payload enum pairs from the same query.
    pub fn from_coil_ty(checker: &Checker, ty: &Ty) -> Self {
        Self::from(&of_resolved(checker, ty))
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
mod mir_layout_tests {
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

/// Extra TOS words besides the first return (`ret_words >= 2`).
#[inline]
pub fn is_multi_word_ret(ret_words: u32) -> bool {
    ret_words >= 2
}

/// Width from a pair dest (`dest_hi` present → 2).
#[inline]
pub fn ret_words_from_hi(dest_hi: Option<impl Sized>) -> u32 {
    if dest_hi.is_some() { 2 } else { 1 }
}

/// One-word dense callee signature.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DenseAbi {
    pub params: Vec<MirTy>,
    pub ret: MirTy,
    /// Tag / second product word. `None` is the one-word ABI.
    pub ret_hi: Option<MirTy>,
}

/// Entry-label id → dense ABI. Built leaf-first during specialize.
/// Q7 self-`CALL` uses the open one-word ABI until this map records the body.
pub type DenseCallMap = HashMap<u32, DenseAbi>;

impl DenseAbi {
    /// Word-layout params + one- or two-slot specialized return.
    pub fn from_func(func: &MirFunc) -> Option<Self> {
        let ret = func.ret_ty?;
        if !ret.is_word_lane() {
            return None;
        }
        let ret_hi = match func.ret_layout {
            MirLayout::Word | MirLayout::HeapNiche => None,
            MirLayout::TwoSlot => {
                let hi = func.ret_hi_ty?;
                if !hi.is_word_lane() {
                    return None;
                }
                Some(hi)
            }
        };
        let params: Vec<MirTy> = func.params.iter().map(|p| func.ty(*p)).collect();
        if params.iter().any(|t| !t.is_word_lane()) {
            return None;
        }
        Some(Self {
            params,
            ret,
            ret_hi,
        })
    }

}

#[cfg(test)]
mod dense_tests {
    use super::*;
    use crate::mir::{MirBuilder, MirConst};

    #[test]
    fn from_func_records_word_abi() {
        let mut b = MirBuilder::new("leaf");
        let x = b.add_param(MirTy::F64).unwrap();
        let y = b.add_param(MirTy::I64).unwrap();
        let _ = y;
        b.set_ret_ty(MirTy::F64);
        b.ret(Some(x)).unwrap();
        let f = b.finish().unwrap();
        let abi = DenseAbi::from_func(&f).expect("word abi");
        assert_eq!(abi.params, vec![MirTy::F64, MirTy::I64]);
        assert_eq!(abi.ret, MirTy::F64);
    }

    #[test]
    fn from_func_accepts_heapref_word() {
        let mut b = MirBuilder::new("href");
        let p = b.add_param(MirTy::HeapRef).unwrap();
        b.set_ret_ty(MirTy::HeapRef);
        b.ret(Some(p)).unwrap();
        let f = b.finish().unwrap();
        let abi = DenseAbi::from_func(&f).expect("S3 HeapRef is a word lane");
        assert_eq!(abi.params, vec![MirTy::HeapRef]);
        assert_eq!(abi.ret, MirTy::HeapRef);
    }

    #[test]
    fn from_func_accepts_niche_word() {
        let mut b = MirBuilder::new("opt");
        let p = b.add_param(MirTy::NicheOpt).unwrap();
        b.set_ret_ty(MirTy::NicheOpt);
        b.ret(Some(p)).unwrap();
        let f = b.finish().unwrap();
        let abi = DenseAbi::from_func(&f).expect("Q8 niche is a word lane");
        assert_eq!(abi.params, vec![MirTy::NicheOpt]);
        assert_eq!(abi.ret, MirTy::NicheOpt);
    }

    #[test]
    fn from_func_accepts_two_slot() {
        let mut b = MirBuilder::new("pair");
        let lo = b.add_param(MirTy::I64).unwrap();
        let hi = b.ins_const(MirConst::I64(1)).unwrap();
        b.ret_pair(lo, hi).unwrap();
        let f = b.finish().unwrap();
        let abi = DenseAbi::from_func(&f).expect("B3 two-slot abi");
        assert_eq!(abi.ret, MirTy::I64);
        assert_eq!(abi.ret_hi, Some(MirTy::I64));
    }

    #[test]
    fn modeled_ret_width_is_one_or_two() {
        assert!(!is_multi_word_ret(1));
        assert!(is_multi_word_ret(2));
        assert!(is_multi_word_ret(3));
        assert_eq!(ret_words_from_hi(None::<u8>), 1);
        assert_eq!(ret_words_from_hi(Some(0u8)), 2);
    }
}
