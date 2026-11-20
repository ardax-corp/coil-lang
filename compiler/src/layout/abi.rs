//! How a value crosses a call edge: the two-slot direct `CALL`/`RETURN`
//! width, the MIR ABI class, and the typed call signature. Each is a
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
//! width as `[a, b]` (second word on top) without boxing a tuple object.
//! Niched heap `Option<T>` / heap-heap `Result<T, E>` already use a
//! strictly better one-word ABI and must stay there (never
//! double-classified). Unbounded `T`, mixed heap products, and wider
//! tuples keep the boxed ABI.
//!
//! ## MIR layout
//!
//! Result/Option ABI layouts for MIR lowering (COI-270) and I1 SSA word names.
//!
//! Matches [`two_word_kind`] and
//! `docs/internals/layout.md` (COI-92). No new pair opcodes and no
//! nursery: two-slot is `[payload, tag]` (or `[a, b]`) on direct
//! `CALL`/`RETURN`; heap niches are one `Value` word.
//!
//! ## Call ABI
//!
//! [`CallAbi`] is a MIR callee's typed signature (COI-291 M1): one
//! [`MirTy`] per parameter word and the returned word, plus `ret_hi`, the
//! tag or second product word of a two-slot return (B3 / C1). Niche
//! `Option` / `Result` and heap references are one-word lanes (Q8).
//!
//! SSA models at most dest + `dest_hi`. N>2 is the same `ret_words` field
//! but needs extra dests + archive encoding — not a self-vs-helper wall.


use super::rep::{Layout, PairKind, is_immediate, of_resolved};
use crate::typechecking::infer::Checker;
use crate::mir::MirTy;
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

/// Whether `split` holds for an open `Option` / `Result` (one whose
/// payload still has a type variable) that a parameter or result of
/// declared type `ty` holds below its top: an element of an array or
/// `Vec`, a tuple item, a type argument, a payload of another enum. A
/// shared generic body holds such a value as its enum object, and nothing
/// converts it at a call (only the top of a parameter or result is,
/// `docs/internals/layout.md`, "Open enums"); `split` says whether ground
/// code holds that one differently. The parameter and result of a function
/// type that is itself a top count as tops (a function argument's adapter
/// converts them), except a function among the parameters, which nothing
/// converts (`fn(fn(T) -> Option<T>) -> T`, #2591). A function below the
/// top (an element of `Vec<unit -> Option<T>>`) has no adapter: its
/// parameter and result are nested too (#2488).
///
/// A user class or enum over open arguments is read through its
/// declaration, its parameters bound to the arguments, for each nominal
/// instance `look` admits (its name and arguments; #2446, #2493, #2509,
/// #2547): a class's field is a top
/// (stored in its declared layout, `hir_field_rep`) unless it is a
/// function, an enum's payload never is. So `W<T>` with a field `xs:
/// Vec<Option<T>>`, `P<T>` with a payload `fn(T) -> Option<T>`, and a class
/// holding either, are nested open enums wherever they are named.
pub fn nested_open_enum(
    checker: &Checker,
    ty: &Ty,
    top: bool,
    look: &dyn Fn(&str, &[Ty]) -> bool,
    split: &mut dyn FnMut(&Ty) -> bool,
) -> bool {
    NestedOpen { checker, look, split, seen: Vec::new() }.ty(ty, top)
}

/// The nominal types [`nested_open_enum`] reads through: every user enum
/// and class, another module's too (#2547). A clone of another module's
/// method or function whose signature holds one is compiled from that
/// module's body, as one of this module's is (`fn_sig_split` in
/// `codegen`), so a `pub` field this module fills holds the layout the
/// class's methods read. A `Vec` is no user class.
pub fn nested_open_look(checker: &Checker) -> impl Fn(&str, &[Ty]) -> bool + '_ {
    move |name: &str, _: &[Ty]| name != common::BUILTIN_VEC_TYPE && (checker.enum_variants(name).is_some() || checker.is_class(name))
}

/// Whether `ty` is a nested open enum for any ground type, in the module
/// the checker is on ([`nested_open_enum`], [`nested_open_look`]). A bare
/// generic class name (a method body's `self`) stands for the class over
/// its own parameters.
pub fn nested_open_type(checker: &Checker, ty: &Ty) -> bool {
    let own;
    let ty = match crate::typechecking::ty::strip_readonly(ty) {
        Ty::Con(name) if checker.type_ctor_params(name).is_some_and(|p| !p.is_empty()) => {
            let n = checker.type_ctor_params(name).map_or(0, Vec::len);
            let vars = (0..n).map(|i| Ty::Var(crate::typechecking::ty::TyVarId(u32::MAX - i as u32))).collect();
            own = Ty::App(Box::new(Ty::Con(name.clone())), vars);
            &own
        }
        ty => ty,
    };
    nested_open_enum(checker, ty, true, &nested_open_look(checker), &mut |_| true)
}

struct NestedOpen<'a, 'b> {
    checker: &'a Checker,
    look: &'a dyn Fn(&str, &[Ty]) -> bool,
    split: &'b mut dyn FnMut(&Ty) -> bool,
    /// The nominal instances on the walk, against recursive declarations.
    seen: Vec<String>,
}

/// How many nominal declarations deep [`nested_open_enum`] reads: a
/// polymorphically recursive type (`Nest<T>` holding `Nest<Nest<T>>`)
/// meets new instances without end.
const NESTED_OPEN_DEPTH: usize = 16;

impl NestedOpen<'_, '_> {
    fn ty(&mut self, ty: &Ty, top: bool) -> bool {
        let ty = crate::typechecking::ty::strip_readonly(ty);
        if !ty.has_var() {
            return false;
        }
        let app = crate::typechecking::ty::builtin_enum_app(ty);
        let ty = app.as_ref().unwrap_or(ty);
        if !top && app.is_some() && (self.split)(ty) {
            return true;
        }
        // A top function's parameters and result are converted by its
        // adapter, but a function among its parameters is converted
        // nowhere: its own parts are below the top (#2591).
        if let Ty::Fun(param, ret) = ty {
            let param_top = top && !matches!(crate::typechecking::ty::strip_readonly(param), Ty::Fun(..));
            return self.ty(param, param_top) || self.ty(ret, top);
        }
        let mut found = false;
        ty.for_each_child(|child| found = found || self.ty(child, false));
        found || app.is_none() && self.declared(ty)
    }

    /// Whether user class or enum instance `ty` holds a nested open enum
    /// in a field or payload, its parameters bound to `ty`'s arguments.
    fn declared(&mut self, ty: &Ty) -> bool {
        let Ty::App(head, args) = ty else {
            return false;
        };
        let Ty::Con(name) = head.as_ref() else {
            return false;
        };
        let key = format!("{ty}");
        if self.seen.len() >= NESTED_OPEN_DEPTH || self.seen.contains(&key) || !(self.look)(name, args) {
            return false;
        }
        let Some(params) = self.checker.type_ctor_params(name).cloned() else {
            return false;
        };
        if params.len() != args.len() {
            return false;
        }
        // A class's field is a top unless it is a function; an enum's
        // payload is below the enum's top.
        let fields: Vec<(Ty, bool)> = if let Some(variants) = self.checker.enum_variants(name) {
            variants.into_iter().flat_map(|(_, _, payload)| payload).map(|t| (t, false)).collect()
        } else if let Some(fields) = self.checker.class_fields(name) {
            fields
                .into_iter()
                .map(|(_, t)| {
                    let top = !matches!(crate::typechecking::ty::strip_readonly(&t), Ty::Fun(..));
                    (t, top)
                })
                .collect()
        } else {
            return false;
        };
        self.seen.push(key);
        let found = fields.iter().any(|(field, top)| self.ty(&super::bind_params(field, &params, args), *top));
        self.seen.pop();
        found
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
    /// A trait instance method whose direct calls take its dictionary entry
    /// (`CodePtr`): not raw, and no definition site pinned its result's
    /// pair. A raw one's `$dict` adapter boxes the pair instead.
    pub instance_method: bool,
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
/// numeric range, an arity-2 immediate tuple, a two-field readonly class,
/// or a pair enum `enum_pair` admits. MIR passes the two words as two
/// parameters, so the caller boxes nothing.
pub fn convention_param(checker: &Checker, facts: &CalleeFacts, ty: &Ty, enum_pair: impl Fn(&PairKind) -> bool) -> Option<PairKind> {
    if facts.escaped || facts.instance_method {
        return None;
    }
    if let Some(kind) = two_word_range_kind(ty) {
        return Some(kind);
    }
    if facts.overloaded_or_generic || facts.coroutine {
        return None;
    }
    two_word_kind(checker, ty).filter(|kind| matches!(kind, PairKind::Product | PairKind::Class(_)) || enum_pair(kind))
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
    /// One runtime `Value` (boxed `ObjEnum`, scalar, or heap niche word).
    #[default]
    Word,
    /// Direct `CALL`/`RETURN` width 2. Enums are `[payload, tag]`; products
    /// are `[a, b]` (second on top).
    TwoSlot,
    /// Heap `Option<T>` (`None` = `0`) or heap-heap `Result<T,E>`
    /// (`Ok` = aligned pointer, `Err` = `pointer | 1`). Host packs at the
    /// HostInvoke edge; native code uses `CONST 0` / `BITAND` / `BITOR` / `LogNot`.
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

/// A MIR callee's signature: one MIR type per parameter word and the
/// returned word(s).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallAbi {
    pub params: Vec<MirTy>,
    pub ret: MirTy,
    /// Tag / second product word. `None` is the one-word ABI.
    pub ret_hi: Option<MirTy>,
}

/// Whether `unit` holds for the bare type variable a function below the
/// top of a parameter or result of declared type `ty` returns: an element
/// of `Vec<unit -> T>`, a tuple item, a type argument, a payload of an enum.
/// A ground `-> ()` function returns nothing a caller reads, while a shared
/// body keeps the word its call gives as a `T`, and a `()` stored or passed
/// is the empty tuple object. Only a function that is itself a top (a
/// function argument) gets an adapter that returns that object (#2498);
/// `unit` says whether ground code has `()` for the variable. The same for
/// a `Vec` or array of a type variable such a function returns
/// (`Vec<fn() -> Vec<T>>`): a ground function of a closed two-word element
/// returns a lane array, which only a function argument's adapter turns
/// into the one word per element a shared body reads (#2592); `unit` is
/// asked with the `Vec` and says whether ground code has a lane array.
pub fn nested_fn_var_result(ty: &Ty, top: bool, unit: &mut dyn FnMut(&Ty) -> bool) -> bool {
    let ty = crate::typechecking::ty::strip_readonly(ty);
    if !ty.has_var() {
        return false;
    }
    if let Ty::Fun(param, ret) = ty {
        if !top {
            let mut result = ret.as_ref();
            while let Ty::Fun(_, next) = result {
                result = next;
            }
            let result = crate::typechecking::ty::strip_readonly(result);
            let lanes = match result {
                Ty::Array { element, .. } => element.has_var(),
                t => crate::typechecking::ty::vec_element_ty(t).is_some_and(Ty::has_var),
            };
            if (matches!(result, Ty::Var(_)) || lanes) && unit(result) {
                return true;
            }
        }
        return nested_fn_var_result(param, top, unit) || nested_fn_var_result(ret, top, unit);
    }
    let mut found = false;
    ty.for_each_child(|child| found = found || nested_fn_var_result(child, false, unit));
    found
}

/// Whether result type `ty` holds a function over a type variable below its
/// top (`Vec<T -> int>`, `(T -> int, int)`, `[T -> string; 1]`; a returned
/// function's own parameters and result are tops too). The shared body
/// makes that function, and only a function that is itself a top gets an
/// adapter, so the call cannot rebuild the result in its ground layout:
/// the signature takes a clone for every ground call, as one whose such
/// function returns a bare type variable does ([`nested_fn_var_result`],
/// #2664). So does a declared result that is itself a function over a type
/// variable whose own signature is not plain words
/// ([`crate::hir::classify::plain_sig`]: `fn(T) -> Option<T>`,
/// `fn(Option<T>) -> T`): the shared body's closure takes and returns the
/// open enum's object where a ground caller passes and reads its niche word
/// (#2767).
pub fn nested_fn_in_result(checker: &Checker, ty: &Ty, top: bool) -> bool {
    let ty = crate::typechecking::ty::strip_readonly(ty);
    if !ty.has_var() {
        return false;
    }
    if let Ty::Fun(param, ret) = ty {
        if top {
            let mut params = vec![param.as_ref().clone()];
            let mut result = ret.as_ref();
            while let Ty::Fun(p, next) = crate::typechecking::ty::strip_readonly(result) {
                params.push(p.as_ref().clone());
                result = next;
            }
            if !crate::hir::classify::plain_sig(checker, &params, result) {
                return true;
            }
        }
        return !top || nested_fn_in_result(checker, param, top) || nested_fn_in_result(checker, ret, top);
    }
    let mut found = false;
    ty.for_each_child(|child| found = found || nested_fn_in_result(checker, child, false));
    found
}

/// Whether result type `ty` is a `coroutine<Y, S>` whose yield or receive
/// type is open in the signature's type variables. That generator's shared
/// body cannot tell a value nobody produced (an unsent binding `yield`, a
/// completion with no value) from a word of a bare `T`, nor make the type's
/// zero (#2738, `native.md`, "A value nobody produced"): every ground call
/// takes its per-type clone, which decides by the type.
pub fn open_coroutine(ty: &Ty) -> bool {
    crate::typechecking::infer::Checker::coroutine_parts(crate::typechecking::ty::strip_readonly(ty))
        .is_some_and(|(y, s)| y.has_var() || s.has_var())
}

/// Whether a parameter or result of declared type `ty` holds a function
/// over a type variable in a user class's field or a user enum's payload,
/// at any depth of the slot (`f: fn(T) -> T`, `fs: Vec<fn() -> T>`, a
/// tuple item, `Box<fn() -> T>`), the type's parameters bound to its
/// arguments (`Slot<T>` with `f: fn(T) -> T`, `G<T>` with `F(fn(T) ->
/// T)`), for each nominal instance `look` admits (#2511). Such a function
/// crosses a shared generic body inside the object: a ground function
/// takes a `float` or `bool` as its own MIR type, a two-word value as two
/// words and returns nothing for `()`, a function the shared body made or
/// adapted takes and returns their words, and only a function that is
/// itself an argument (or, for `()`, a slot's own function, filled through
/// `HirPacks::unit_fns`) gets an adapter. So the signature takes a clone
/// for every ground call, as a nested open enum does, and so do the
/// type's own methods (`self` is such a parameter).
pub fn nested_fn_field(checker: &Checker, ty: &Ty, look: &dyn Fn(&str, &[Ty]) -> bool) -> bool {
    fn open_fun(ty: &Ty) -> bool {
        match crate::typechecking::ty::strip_readonly(ty) {
            Ty::Fun(..) => ty.has_var(),
            t => t.any_child(open_fun),
        }
    }
    nominal_slot(checker, ty, look, &mut open_fun)
}

/// Whether a parameter or result of declared type `ty` holds, in a user
/// class's field or a user enum's payload (its parameters bound to its
/// arguments, for each instance `look` admits), a function below the
/// slot's top whose result is a bare type variable `unit` holds for
/// ([`nested_fn_var_result`]): `fs: Vec<fn() -> T>` at `T = ()`. No adapter
/// reaches it, so a call that takes a shared body there is refused (#2560).
pub fn nested_fn_unit_slot(checker: &Checker, ty: &Ty, look: &dyn Fn(&str, &[Ty]) -> bool, unit: &mut dyn FnMut(&Ty) -> bool) -> bool {
    nominal_slot(checker, ty, look, &mut |slot| nested_fn_var_result(slot, true, unit))
}

/// Whether `slot` holds for a field or payload type of a user class or
/// enum instance named in `ty` (transitively, through recursive
/// declarations, [`NESTED_OPEN_DEPTH`] deep), each slot's type with the
/// type's parameters bound to the instance's arguments.
fn nominal_slot(checker: &Checker, ty: &Ty, look: &dyn Fn(&str, &[Ty]) -> bool, slot: &mut dyn FnMut(&Ty) -> bool) -> bool {
    fn walk(
        checker: &Checker,
        ty: &Ty,
        look: &dyn Fn(&str, &[Ty]) -> bool,
        slot: &mut dyn FnMut(&Ty) -> bool,
        seen: &mut Vec<String>,
    ) -> bool {
        let ty = crate::typechecking::ty::strip_readonly(ty);
        if !ty.has_var() {
            return false;
        }
        let mut found = false;
        ty.for_each_child(|child| found = found || walk(checker, child, look, slot, seen));
        if found {
            return true;
        }
        let Ty::App(head, args) = ty else {
            return false;
        };
        let Ty::Con(name) = head.as_ref() else {
            return false;
        };
        let key = format!("{ty}");
        if seen.len() >= NESTED_OPEN_DEPTH || seen.contains(&key) || !look(name, args) {
            return false;
        }
        let Some(params) = checker.type_ctor_params(name).cloned() else {
            return false;
        };
        if params.len() != args.len() {
            return false;
        }
        let slots: Vec<Ty> = if let Some(variants) = checker.enum_variants(name) {
            variants.into_iter().flat_map(|(_, _, payload)| payload).collect()
        } else if let Some(fields) = checker.class_fields(name) {
            fields.into_iter().map(|(_, t)| t).collect()
        } else {
            return false;
        };
        seen.push(key);
        let found = slots.iter().any(|s| {
            let s = super::bind_params(s, &params, args);
            s.has_var() && slot(&s) || walk(checker, &s, look, slot, seen)
        });
        seen.pop();
        found
    }
    walk(checker, ty, look, slot, &mut Vec::new())
}
