//! What MIR lowering may do with a value of a type ([`ValueClass`]), and
//! the type queries it rests on: words, enums, ranges, closed and open
//! types.

use super::{HirBody, HirId, HirKind, LocalId, MakeKind};
use crate::typechecking::infer::Checker;
use crate::typechecking::subst::apply_ty_prune;
use crate::typechecking::ty::{self as coil_ty, Ty, strip_readonly};

/// What the lowering may do with a value of some type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueClass {
    /// `int`, `float` or `bool`: operators apply.
    Scalar,
    /// `()`: only as a call or function result.
    Unit,
    /// `string`: moved and passed, never operated on.
    Opaque,
    /// `Option`, `Result` or a closed, non-generic user enum.
    Enum,
    /// An instance of a non-generic user class: one heap pointer word.
    Object,
    /// A heap tuple, array or `Vec<T>` with closed element types: one
    /// pointer word, indexed with `Index` / `StoreIndex`.
    Aggregate,
}

/// The classes a local, argument, field or payload word can hold.
pub fn is_word(class: ValueClass) -> bool {
    !matches!(class, ValueClass::Unit)
}

/// The class of `ty`, or `None` when the lowering does not handle it.
pub fn classify(checker: &Checker, ty: &Ty) -> Option<ValueClass> {
    let ty = apply_ty_prune(checker.subst(), ty);
    // Each level of an aggregate asks whether its items are closed, a
    // walk of the whole item: kept per node of `ty` while it is classified,
    // so a value `d` levels deep is classified in `O(d)`, not `O(d^2)`
    // (#2657).
    // Only an aggregate asks it level by level.
    let aggregate = matches!(strip_readonly(&ty), Ty::Tuple(_) | Ty::Array { .. } | Ty::Record { .. }) || coil_ty::vec_element_ty(&ty).is_some();
    if !aggregate {
        return classify_in(checker, &ty, &mut Vec::new());
    }
    // The node table is made the first time `params_closed` is asked.
    CLOSED.with(|c| c.borrow_mut().push(ClosedMemo { root: &ty as *const Ty, nodes: None, closed: std::collections::HashMap::new() }));
    let was = MEMO_ON.with(|m| m.replace(true));
    let class = classify_in(checker, &ty, &mut Vec::new());
    MEMO_ON.with(|m| m.set(was));
    CLOSED.with(|c| c.borrow_mut().pop());
    class
}

/// Types of more nodes than this keep [`params_closed`] per node.
const MEMO_NODES: usize = 64;

/// `ty` has more than `n` nodes (counted no further than that).
fn larger_than(ty: &Ty, n: usize) -> bool {
    fn count(ty: &Ty, left: &mut usize) -> bool {
        if *left == 0 {
            return true;
        }
        *left -= 1;
        ty.any_child(|c| count(c, left))
    }
    let mut left = n;
    count(ty, &mut left)
}

thread_local! {
    /// For each [`classify`] in progress: the addresses of the nodes of the
    /// type it classifies, and [`params_closed`] of those asked so far.
    /// Only those nodes are kept: they live as long as the call, while a
    /// type a helper builds may be freed and its address reused.
    static CLOSED: std::cell::RefCell<Vec<ClosedMemo>> = const { std::cell::RefCell::new(Vec::new()) };
    /// `CLOSED` is not empty: checked first, as most calls are outside one.
    static MEMO_ON: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// One [`classify`]'s table: its type, that type's node addresses (made
/// when first needed) and [`params_closed`] of those asked so far.
struct ClosedMemo {
    root: *const Ty,
    nodes: Option<std::collections::HashSet<usize>>,
    closed: std::collections::HashMap<usize, bool>,
}

fn collect_nodes(ty: &Ty, nodes: &mut std::collections::HashSet<usize>) {
    common::deep_stack(|| {
        nodes.insert(ty as *const Ty as usize);
        ty.for_each_child(|c| collect_nodes(c, nodes));
    })
}

fn classify_in(checker: &Checker, ty: &Ty, seen: &mut Vec<String>) -> Option<ValueClass> {
    let ty = strip_readonly(ty);
    if is_scalar(ty) {
        return Some(ValueClass::Scalar);
    }
    if crate::layout::is_unit(ty) {
        return Some(ValueClass::Unit);
    }
    match ty {
        Ty::Con(n) if n == coil_ty::STRING => Some(ValueClass::Opaque),
        // A foreign pointer: one word, only moved and passed to `extern`s.
        Ty::Con(n) if n == "ptr" && !checker.is_class(n) && checker.enum_variants(n).is_none() => Some(ValueClass::Opaque),
        // Host handles (`io` streams, threads, channels, locks): one word.
        Ty::Con(n) if n == coil_ty::STREAM && !checker.is_class(n) && checker.enum_variants(n).is_none() => {
            Some(ValueClass::Opaque)
        }
        Ty::Constructor { owner, .. } => classify_in(checker, owner, seen),
        // A `()` item is stored as the empty tuple object (#1427).
        Ty::Tuple(items) if !items.is_empty() => aggregate(checker, items.iter(), true, seen),
        // A record: one dict word, its fields read and written by name. A
        // `()` field is stored as the empty tuple object, as a tuple's.
        Ty::Record { fields } if !fields.is_empty() => fields
            .iter()
            .all(|(_, f)| classify_in(checker, f, seen).is_some_and(|class| is_word(class) || class == ValueClass::Unit))
            .then_some(ValueClass::Opaque),
        Ty::Array { element, .. } => aggregate(checker, std::iter::once(element.as_ref()), true, seen),
        // A bare-class existential (`Show x`): one `[value, dictionary]`
        // tuple word, only moved and called through its dictionary.
        Ty::Existential { .. } => Some(ValueClass::Opaque),
        // `Matrix<D>` is its data at run time.
        Ty::App(..) if crate::typechecking::aggregate_arith::unwrap_matrix_ty(ty).is_some() => {
            classify_in(checker, crate::typechecking::aggregate_arith::unwrap_matrix_ty(ty)?, seen)
        }
        Ty::App(..) if coil_ty::vec_element_ty(ty).is_some() => {
            aggregate(checker, coil_ty::vec_element_ty(ty).into_iter(), true, seen)
        }
        // A higher-kinded parameter's instance (`F<A>` in a shared body):
        // the instance's one word, only moved and passed to its dictionary.
        Ty::App(head, _) if matches!(head.as_ref(), Ty::Var(_)) => Some(ValueClass::Opaque),
        Ty::App(head, args) => {
            let Ty::Con(name) = head.as_ref() else {
                return None;
            };
            // A coroutine handle: one heap word, only moved, resumed and
            // tested with `done`.
            if name == "coroutine" && args.len() == 2 {
                return Some(ValueClass::Opaque);
            }
            // A GC `Root<T>` / `Weak<T>` handle: one host word, only moved
            // and passed to the `gc` natives.
            if matches!(name.as_str(), coil_ty::ROOT | coil_ty::WEAK) && args.len() == 1 && !checker.is_class(name) {
                return Some(ValueClass::Opaque);
            }
            // A `thread` handle (`Thread<T>`, `Sender<T>`, …): one host word.
            if coil_ty::opaque_handle_name(ty).is_some() && !checker.is_class(name) {
                return Some(ValueClass::Opaque);
            }
            // A generic class instance: one object word, its methods shared
            // across instances (fields are only read inside them).
            // (A generic enum with an `impl` has a class key too; it is
            // still an enum.)
            if is_generic_class(checker, name) && checker.enum_variants(name).is_none() {
                return Some(ValueClass::Opaque);
            }
            let option = common::is_builtin_option_enum(name);
            let result = common::is_builtin_result_enum(name);
            if !(option || result) {
                return generic_user_enum(checker, name, args, seen);
            }
            if args.len() != if option { 1 } else { 2 } {
                return None;
            }
            // A `()` payload is the empty tuple object (#1449).
            for arg in args {
                classify_in(checker, arg, seen)?;
            }
            args.iter().all(|a| payload_closed(checker, a, seen)).then_some(ValueClass::Enum)
        }
        Ty::Sum { name, variants } => {
            if common::is_builtin_option_enum(name) || common::is_builtin_result_enum(name) {
                for (_, payload) in variants {
                    for field in payload.field_types() {
                        classify_in(checker, field, seen)?;
                    }
                }
                let closed = variants
                    .iter()
                    .all(|(_, p)| p.field_types().into_iter().all(|f| payload_closed(checker, f, seen)));
                return closed.then_some(ValueClass::Enum);
            }
            if checker.is_scalar_enum(name) {
                return Some(ValueClass::Opaque);
            }
            user_enum(checker, name, seen)
        }
        // A monomorphic function value (closure, partial, `fn` object):
        // one word, only moved and called through `CallIndirect`.
        Ty::Fun(..) if fun_words(checker, ty, seen) => Some(ValueClass::Opaque),
        // A polymorphic function value (`let f = id`, a `forall` parameter):
        // one `PolyFn` word, called with boxed arguments.
        Ty::Fun(..) | Ty::Forall { .. } if poly_fun(ty) => Some(ValueClass::Opaque),
        // `self` inside a generic class's shared method body.
        Ty::Con(name) if is_generic_class(checker, name) => Some(ValueClass::Opaque),
        // A scalar-backed enum is its backing word, only moved and matched.
        Ty::Con(name) if checker.is_scalar_enum(name) => Some(ValueClass::Opaque),
        Ty::Con(name) if checker.is_class(name) && checker.enum_variants(name).is_none() => object_class(checker, name, seen),
        Ty::Con(name) => user_enum(checker, name, seen),
        // A type parameter inside a generic class's shared method body: one
        // boxed word, only moved.
        Ty::Var(_) => Some(ValueClass::Opaque),
        _ => None,
    }
}

/// A function type still open in a type parameter, or a rank-n `forall`:
/// a `PolyFn` value, whose calls box their arguments.
pub fn poly_fun(ty: &Ty) -> bool {
    match strip_readonly(ty) {
        Ty::Forall { .. } => true,
        t @ Ty::Fun(..) => t.has_free_var(),
        _ => false,
    }
}

/// A function type whose parameters and result are plain words: no enum
/// (its layout may be niche or a pair), no unit; a bare type variable is
/// one word.
/// The result may also be a closed enum: a call through the function
/// returns a one-word enum as that word, any other boxed.
fn fun_words(checker: &Checker, ty: &Ty, seen: &mut Vec<String>) -> bool {
    fun_words_in(checker, ty, seen, false)
}

/// Whether `ty` is a function whose value is one closure word in every
/// instance of its type parameters: each parameter and the result is
/// `()` or a plain word ([`plain_word`]). The one rule for a function
/// over type parameters as a local, binding, field, payload or item, at
/// any depth: a shared body calls it with each `T` as the word ground
/// code passes, and reads back the words a ground closure returns
/// (#2473, #2485, #2530, #2493, #2571).
///
/// With `open_enum`, an `Option` / `Result` whose layout is the boxed
/// object (an open one, `Ok(2)`'s error type nothing pinned) is a plain
/// word too: one boxed word, no niche or pair (#1520). That holds where
/// every closure the body sees was made where the enum is open: a closure
/// literal's own type, a function argument (its adapter boxes a niche
/// result), and a value read from a nested open enum, whose generics are
/// clone-only (#2153, #2493, #2509).
pub(super) fn fun_words_in(checker: &Checker, ty: &Ty, seen: &mut Vec<String>, open_enum: bool) -> bool {
    match strip_readonly(ty) {
        // `() -> T` takes a unit parameter, and `T -> ()` returns one.
        Ty::Fun(param, ret) => {
            let unit = |t: &Ty| crate::layout::is_unit(strip_readonly(t));
            (unit(param) || plain_word(checker, param, seen, open_enum)) && (unit(ret) || plain_word(checker, ret, seen, open_enum))
        }
        _ => false,
    }
}

/// Whether a function declared with parameters `params` and result `ret`
/// over type parameters takes and returns, through its shared body, the
/// words ground code passes and reads at every instance: each is `()` or a
/// plain word ([`plain_word`]). Its `PolyFn` is then called by code that
/// knows only a ground instance of its type; else its value at that type is
/// its adapter there (#2755).
pub fn plain_sig(checker: &Checker, params: &[Ty], ret: &Ty) -> bool {
    let word = |t: &Ty| crate::layout::is_unit(strip_readonly(t)) || plain_word(checker, t, &mut Vec::new(), false);
    params.iter().all(word) && word(ret)
}

/// [`plain_sig`] of generic function (or method) `name`'s declaration.
pub fn plain_generic_fn(checker: &Checker, name: &str) -> bool {
    match (checker.fn_param_tys(name), checker.fn_return_ty(name)) {
        (Some(params), Some(ret)) => plain_sig(checker, &params, &apply_ty_prune(checker.subst(), &ret)),
        _ => false,
    }
}

/// A word a shared body and ground code hold alike for every instance of
/// its type parameters ([`fun_words_in`]'s parameters and result):
///
/// - a closed one-word type, or a closed enum (a call through the function
///   returns a one-word enum as that word, any other boxed);
/// - a bare type parameter (a shared generic body's own `T -> U`
///   parameter): the ground type's word;
/// - a generic class's instance over type parameters (`Cell<T>`, `Vec<T>`,
///   #2530): one object word on both sides, its fields of `T` the ground
///   type's words; a fixed array over them (`[T; 3]`) too, the same heap
///   array a `Vec` is (#2615);
/// - a function: one closure word;
/// - a tuple of such words or `()` (`(T, T)`, #2493): the tuple object,
///   whose items are each one word (a two-word value boxed), never a pair
///   at a word position (#1524);
/// - an `Option` / `Result` of a function and such words
///   (`Option<fn(T) -> T>`): the boxed object in every instance, since a
///   function is no niche payload (`layout::niche_heap_only`) and never
///   closed (no pair).
fn plain_word(checker: &Checker, t: &Ty, seen: &mut Vec<String>, open_enum: bool) -> bool {
    let class_word = |t: &Ty| match strip_readonly(t) {
        Ty::App(head, args) => {
            matches!(head.as_ref(), Ty::Con(name) if is_generic_class(checker, name) && checker.enum_variants(name).is_none())
                && args.iter().all(params_closed)
        }
        Ty::Array { element, .. } => params_closed(element),
        _ => false,
    };
    let word = (crate::layout::ty_is_closed(t) || matches!(strip_readonly(t), Ty::Fun(..) | Ty::Var(_)) || class_word(t))
        && matches!(
            classify_in(checker, t, seen),
            Some(ValueClass::Scalar | ValueClass::Opaque | ValueClass::Object | ValueClass::Aggregate)
        );
    if word {
        return true;
    }
    let item = |t: &Ty, seen: &mut Vec<String>| crate::layout::is_unit(strip_readonly(t)) || plain_word(checker, t, seen, open_enum);
    if let Ty::Tuple(items) = strip_readonly(t)
        && !items.is_empty()
    {
        return items.iter().all(|i| item(i, seen));
    }
    if classify_in(checker, t, seen) != Some(ValueClass::Enum) {
        return false;
    }
    let boxed = crate::layout::ty_is_closed(t) || open_enum && crate::layout::of(checker, t) == crate::layout::Layout::Word;
    let fun_enum = crate::typechecking::ty::builtin_enum_app(t).is_some_and(|app| match strip_readonly(&app) {
        Ty::App(_, args) => args.iter().any(|a| matches!(strip_readonly(a), Ty::Fun(..))) && args.iter().all(|a| item(a, seen)),
        _ => false,
    });
    boxed || fun_enum
}

/// Closed but for type parameters (a generic class's shared method body):
/// each parameter is one boxed word, and [`crate::layout::of`] gives a type
/// open in one its shared layout.
fn params_closed(ty: &Ty) -> bool {
    if !MEMO_ON.with(std::cell::Cell::get) {
        return params_closed_of(ty);
    }
    let key = ty as *const Ty as usize;
    let known = CLOSED.with(|c| {
        let mut stack = c.borrow_mut();
        let memo = stack.last_mut()?;
        let nodes = memo.nodes.get_or_insert_with(|| {
            let mut nodes = std::collections::HashSet::new();
            // SAFETY: `root` is the type `classify` holds for as long as
            // its entry is on the stack, and nothing changes it.
            let root = unsafe { &*memo.root };
            // A small type is walked as it is: the table costs more than
            // it saves.
            if larger_than(root, MEMO_NODES) {
                collect_nodes(root, &mut nodes);
            }
            nodes
        });
        nodes.contains(&key).then(|| memo.closed.get(&key).copied())
    });
    match known {
        Some(Some(closed)) => closed,
        Some(None) => {
            let closed = params_closed_of(ty);
            CLOSED.with(|c| {
                if let Some(memo) = c.borrow_mut().last_mut() {
                    memo.closed.insert(key, closed);
                }
            });
            closed
        }
        None => params_closed_of(ty),
    }
}

fn params_closed_of(ty: &Ty) -> bool {
    match strip_readonly(ty) {
        Ty::Fun(_, _) | Ty::Existential { .. } | Ty::Forall { .. } => false,
        Ty::Sum { name, variants } if crate::layout::is_user_sum(name) => crate::layout::user_sum_closed(variants, params_closed),
        t => t.all_children(params_closed),
    }
}

/// A tuple, array or `Vec` whose element types are closed but for type
/// parameters (elements are classified where they are read). With `unit`,
/// an item may be `()`.
fn aggregate<'t>(checker: &Checker, mut items: impl Iterator<Item = &'t Ty>, unit: bool, seen: &mut Vec<String>) -> Option<ValueClass> {
    items
        .all(|t| {
            (params_closed(t) || holds_packs(t) || mono_fun(checker, t, seen) || open_fun(checker, t, seen) || fun_holder(checker, t, false, seen))
                && classify_in(checker, t, seen).is_none_or(|class| is_word(class) || unit && class == ValueClass::Unit)
        })
        .then_some(ValueClass::Aggregate)
}

/// A monomorphic function type of plain words: its value is one closure
/// word, stored in an aggregate as that word.
pub(super) fn mono_fun(checker: &Checker, ty: &Ty, seen: &mut Vec<String>) -> bool {
    matches!(strip_readonly(ty), Ty::Fun(..)) && !ty.has_free_var() && fun_words(checker, ty, seen)
}

/// A function type over type parameters whose words are plain
/// ([`fun_words`]: a bare `T` is one boxed word), such as the elements of a
/// shared generic body's `Vec<unit -> T>`: one closure word each (#2136).
/// An open `Option` / `Result` (`Vec<unit -> Result<T, E>>`) is its one
/// object word here too: a ground caller reaches the mono clone, so every
/// closure the shared body sees was made where the enum is open (#2153).
pub(super) fn open_fun(checker: &Checker, ty: &Ty, seen: &mut Vec<String>) -> bool {
    matches!(strip_readonly(ty), Ty::Fun(..)) && fun_words_in(checker, ty, seen, true)
}

/// A class declared with type parameters (`class HashMap<K, V>`).
pub fn is_generic_class(checker: &Checker, name: &str) -> bool {
    if !checker.has_impl(name) {
        return false;
    }
    let ctors = &checker.generics().generic_type_ctors;
    ctors.contains_key(name) || checker.resolve_class_key(name).is_some_and(|key| ctors.contains_key(&key))
}

/// An `Option` / `Result` payload whose word is known: [`params_closed`],
/// or a function of plain words or an existential (one heap word each), so
/// `Option<fn() -> string>` is an enum value (#1991), and so is
/// `Option<fn(T) -> T>`: no niche, the boxed object in every instance
/// ([`plain_word`], #2493, #2571).
fn payload_closed(checker: &Checker, ty: &Ty, seen: &mut Vec<String>) -> bool {
    params_closed(ty) || holds_packs(ty) || fun_words(checker, ty, seen)
}

/// Closed but for existentials, at the top or as items of a tuple, record
/// or fixed array (`(Tr, int)`): each existential is its one pack word
/// wherever it is held, so the product is laid out as a closed one
/// (#2744).
fn holds_packs(ty: &Ty) -> bool {
    match strip_readonly(ty) {
        Ty::Existential { .. } => true,
        Ty::Tuple(items) => items.iter().all(holds_packs),
        Ty::Record { fields } => fields.iter().all(|(_, f)| holds_packs(f)),
        Ty::Array { element, .. } => holds_packs(element),
        t => params_closed(t),
    }
}

/// A user class the lowering builds and reads: not generic, with every
/// field type closed.
fn object_class(checker: &Checker, name: &str, seen: &mut Vec<String>) -> Option<ValueClass> {
    let key = checker.resolve_class_key(name)?;
    let ctors = &checker.generics().generic_type_ctors;
    if ctors.contains_key(&key) || ctors.contains_key(name) {
        return None;
    }
    // A class whose function field mentions the class (`fn(Node) -> ()`)
    // reaches it again: assume the object word while its fields are
    // checked (#1989).
    let marker = format!("class {key}");
    if seen.contains(&marker) {
        return Some(ValueClass::Object);
    }
    let fields = checker.class_fields(&key)?;
    seen.push(marker);
    let ok = fields.iter().all(|(_, ty)| closed_field(checker, ty, seen));
    seen.pop();
    ok.then_some(ValueClass::Object)
}

/// A class field type the lowering stores as one known word: closed, an
/// existential, a monomorphic function, a `forall` function (one `PolyFn`
/// word, #2725), or a `Vec` / `Option` / `Result` of those (`Vec<Comp>`,
/// `Option<fn() -> string>`, #1988, #1991).
pub(super) fn closed_field(checker: &Checker, ty: &Ty, seen: &mut Vec<String>) -> bool {
    if crate::layout::ty_is_closed(ty) || is_existential(ty) || mono_fun(checker, ty, seen) || matches!(strip_readonly(ty), Ty::Forall { .. }) {
        return true;
    }
    match strip_readonly(ty) {
        Ty::App(head, args)
            if matches!(head.as_ref(), Ty::Con(n) if n == common::BUILTIN_VEC_TYPE
                || common::is_builtin_option_enum(n)
                || common::is_builtin_result_enum(n)) =>
        {
            !ty.has_free_var() && args.iter().all(|a| closed_field(checker, a, seen))
        }
        _ => false,
    }
}

/// A bare-class existential (`Show`): one `[value, dictionary]` word, closed
/// as an element or a field though its packed type is not.
fn is_existential(ty: &Ty) -> bool {
    matches!(strip_readonly(ty), Ty::Existential { .. })
}

/// Whether `id` is a `Vec<T>`.
pub fn is_vec(body: &HirBody, checker: &Checker, id: HirId) -> bool {
    body.expr(id)
        .ty
        .as_ref()
        .is_some_and(|t| coil_ty::vec_element_ty(&apply_ty_prune(checker.subst(), t)).is_some())
}

/// A user enum the lowering builds and matches: not scalar-backed, not a
/// class, not builtin, with every payload field a handled non-unit type.
fn user_enum(checker: &Checker, name: &str, seen: &mut Vec<String>) -> Option<ValueClass> {
    if common::is_builtin_option_enum(name)
        || common::is_builtin_result_enum(name)
        || common::is_builtin_ffi_enum(name)
        || checker.is_scalar_enum(name)
        // An enum with an `impl` also has a class key; it is still an enum.
        || (checker.is_class(name) && checker.enum_variants(name).is_none())
    {
        return None;
    }
    if seen.iter().any(|s| s == name) {
        return Some(ValueClass::Enum);
    }
    let variants = checker.enum_variants(name)?;
    if variants.is_empty() {
        return None;
    }
    seen.push(name.to_string());
    let ok = variants.iter().all(|(_, _, payload)| {
        payload.iter().all(|field| closed_field(checker, field, seen) && classify_in(checker, field, seen).is_some())
    });
    seen.pop();
    ok.then_some(ValueClass::Enum)
}

/// `enum_name::variant`'s payload types at a ground instance of a generic
/// user enum (`Tree<int>`): the declared types with its parameters bound.
pub fn generic_enum_payload(checker: &Checker, enum_name: &str, variant: &str, args: &[Ty]) -> Option<Vec<Ty>> {
    let params = checker.type_ctor_params(enum_name)?;
    if params.len() != args.len() {
        return None;
    }
    let (_, _, payload) = checker.enum_variants(enum_name)?.into_iter().find(|(n, _, _)| n == variant)?;
    Some(payload.iter().map(|t| crate::layout::bind_params(t, params, args)).collect())
}

/// An instance of a generic user enum (`Tree<int>`, or `Tree<T>` in a
/// shared body): laid out as a closed enum with its parameters bound.
fn generic_user_enum(checker: &Checker, name: &str, args: &[Ty], seen: &mut Vec<String>) -> Option<ValueClass> {
    if checker.is_class(name) && checker.enum_variants(name).is_none() {
        return None;
    }
    // Open only in type parameters: a shared generic body's instance,
    // each parameter one boxed word.
    if !args.iter().all(params_closed) {
        return None;
    }
    let key = format!("{name}<{args:?}>");
    if seen.contains(&key) {
        return Some(ValueClass::Enum);
    }
    let variants = checker.enum_variants(name)?;
    if variants.is_empty() {
        return None;
    }
    // A nested open enum's instance in a shared body was made where its
    // parameters are open (its generics are clone-only).
    let open = args.iter().any(Ty::has_var)
        && crate::layout::nested_open_type(checker, &Ty::App(Box::new(Ty::Con(name.to_string())), args.to_vec()));
    seen.push(key);
    let ok = variants.iter().all(|(variant, _, _)| {
        generic_enum_payload(checker, name, variant, args).is_some_and(|payload| {
            payload
                .iter()
                .all(|field| generic_enum_field(checker, field, open, seen) && classify_in(checker, field, seen).is_some())
        })
    });
    seen.pop();
    ok.then_some(ValueClass::Enum)
}

/// A payload field of a generic user enum's instance (its parameters
/// bound) that the lowering stores as one known word:
///
/// - closed but for type parameters;
/// - at a ground instance, what a non-generic enum's field may be
///   ([`closed_field`]);
/// - a word that holds functions of plain words ([`fun_words_in`]): the
///   function itself, one closure word in the enum's object (at a ground
///   instance `F(fn(T) -> T)` is a monomorphic function, and in a shared
///   body a function called with each `T` its one word, the word ground
///   code passes it, #2473), or a `Vec`, array, tuple, `Option` or
///   `Result` of such words (`Vec<fn(T) -> T>`, `(fn() -> T, int)`,
///   `Option<fn(T) -> T>`, #2493), each item one word on both sides.
///
/// A function that holds a parameter below the top (`fn(T) -> Option<T>`)
/// makes the enum a nested open one (`layout::nested_open_enum`, `open`):
/// its generics are clone-only, so a shared body's instance was made
/// where the parameter is open and the closure returns the open enum's
/// object (#2493).
fn generic_enum_field(checker: &Checker, field: &Ty, open: bool, seen: &mut Vec<String>) -> bool {
    params_closed(field) || fun_holder(checker, field, open, seen) || !field.has_free_var() && closed_field(checker, field, seen)
}

/// Whether `ty` holds functions ([`fun_words_in`] with `open`) and
/// otherwise words closed but for type parameters, as a function, an
/// element of a `Vec` or array, a tuple item, or an `Option` / `Result`
/// payload: one word each, the same on both sides of a shared body.
pub(super) fn fun_holder(checker: &Checker, ty: &Ty, open: bool, seen: &mut Vec<String>) -> bool {
    let t = strip_readonly(ty);
    let part = |p: &Ty, seen: &mut Vec<String>| {
        crate::layout::is_unit(strip_readonly(p)) || params_closed(p) || fun_holder(checker, p, open, seen)
    };
    if let Some(element) = coil_ty::vec_element_ty(t) {
        return fun_holder(checker, element, open, seen);
    }
    // An `Option` / `Result`, applied or structural (`Some(inc)`'s type).
    if let Some(app) = crate::typechecking::ty::builtin_enum_app(t) {
        return matches!(&app, Ty::App(_, args) if !params_closed(&app) && args.iter().all(|a| part(a, seen)));
    }
    match t {
        Ty::Fun(..) => fun_words_in(checker, t, seen, open),
        Ty::Array { element, .. } => fun_holder(checker, element, open, seen),
        Ty::Tuple(items) => !items.is_empty() && items.iter().all(|i| part(i, seen)) && !params_closed(t),
        _ => false,
    }
}

/// A numeric `Range` / `RangeInclusive`: `[start, end]` in a direct
/// call's arguments and result and in an unboxed local, else one boxed word.
pub fn is_range_pair(ty: &Ty) -> bool {
    crate::layout::two_word_range_kind(ty).is_some()
}

/// A `Range<T>` / `RangeInclusive<T>` over an open type parameter `T`.
pub fn is_open_range(ty: &Ty) -> bool {
    crate::typechecking::ty::range_app(ty).is_some_and(|(elem, _)| matches!(strip_readonly(elem), Ty::Var(_)))
}

/// A `()`-typed local: bound by `let` for its initializer's effect only.
pub fn is_unit_local(body: &HirBody, checker: &Checker, local: LocalId) -> bool {
    body.local(local)
        .ty
        .as_ref()
        .and_then(|ty| classify(checker, ty))
        == Some(ValueClass::Unit)
}

/// `int`, `float`, `bool` or `byte`: one immediate word with a primitive
/// lane (`byte` shares the int lane).
pub fn is_scalar(ty: &Ty) -> bool {
    primitive(ty).is_some()
}

/// `Vec<byte>` (or the checker's internal unsized byte array): a growable
/// byte array.
pub fn is_byte_vec(ty: &Ty) -> bool {
    match strip_readonly(ty) {
        Ty::Array { element, length: coil_ty::ArrayLength::Dynamic } => is_byte(element),
        Ty::App(head, args) => {
            matches!(head.as_ref(), Ty::Con(n) if n == common::BUILTIN_VEC_TYPE) && matches!(args.as_slice(), [e] if is_byte(e))
        }
        _ => false,
    }
}

/// `[T; N]` with `N > 0`: a fixed array, which a local copies on
/// assignment.
pub fn is_fixed_array(checker: &Checker, ty: &Ty) -> bool {
    matches!(
        strip_readonly(&apply_ty_prune(checker.subst(), ty)),
        Ty::Array { length: coil_ty::ArrayLength::Static(n), .. } if *n > 0
    )
}

/// A byte array type: a string literal typed so is its bytes.
pub fn is_byte_array(ty: &Ty) -> bool {
    matches!(strip_readonly(ty), Ty::Array { element, .. } if is_byte(element))
}

pub fn is_byte(ty: &Ty) -> bool {
    matches!(strip_readonly(ty), Ty::Con(n) if n == coil_ty::BYTE)
}

/// The items of a `(a, b, ..)` literal argument.
pub fn tuple_items(body: &HirBody, id: HirId) -> Option<&[HirId]> {
    match &body.expr(id).kind {
        HirKind::Make { kind: MakeKind::Tuple, args } => Some(args),
        _ => None,
    }
}

/// The primitive name of a scalar type, as the cast opcodes key it.
pub fn primitive(ty: &Ty) -> Option<&'static str> {
    match strip_readonly(ty) {
        Ty::Con(n) if n == coil_ty::INT => Some(coil_ty::INT),
        Ty::Con(n) if n == coil_ty::FLOAT => Some(coil_ty::FLOAT),
        Ty::Con(n) if n == coil_ty::BOOL => Some(coil_ty::BOOL),
        Ty::Con(n) if n == coil_ty::BYTE => Some(coil_ty::BYTE),
        _ => None,
    }
}

/// A class or enum operand of a unary operator, which dispatches to the
/// type's trait instance (`impl Neg for V`).
pub(crate) fn user_operand(checker: &Checker, ty: &Ty) -> bool {
    // A variant built in place (`-Amt::Int(3)`) is typed by its constructor.
    let ty = match strip_readonly(ty) {
        Ty::Constructor { owner, .. } => owner.as_ref(),
        ty => ty,
    };
    match ty {
        Ty::Con(name) | Ty::Sum { name, .. } => checker.is_class(name) || checker.enum_variants(name).is_some(),
        Ty::App(head, _) => matches!(head.as_ref(), Ty::Con(name) if checker.is_class(name)),
        _ => false,
    }
}
