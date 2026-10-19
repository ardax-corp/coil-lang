//! Which HIR bodies MIR lowering takes (planned in `codegen/emit_hir.rs`).
//!
//! Phase 2 lowers the core: scalar literals and locals, primitive-lane
//! operators, direct calls, `if`, loops, `break` / `continue` and `return`.
//! Phase 3 adds enums: `Make` of a variant, `match` on an enum (so the `?`
//! and `??` desugarings), `Option` / `Result` in every layout, string
//! literals as opaque values, and Result-mode bodies. Phase 4 adds
//! non-generic user classes: `new`, field reads and writes, inherent method
//! and static calls, method and test bodies, and `let p = new C(..)` kept in
//! frame slots when `p` only ever has its fields read or written; then
//! tuples, arrays and `Vec`; then `byte` scalars (on the int lane, one-byte
//! string literals included) and casts between scalars; then each mono
//! clone of a generic function, at its instance's types. Any other node, type
//! or body shape is refused, and the reason is counted in `--opt-stats`;
//! with no AST codegen left, a refused body is an internal error
//! (`OptimizeOptions::require_mir`).
//!
//! MIR keeps values in registers, so where a node sits among its siblings
//! does not matter: a binding, assignment, jump, match or build lowers
//! inside any operand.

use super::{BinOp, BodyKind, Builtin, Callee, HirArm, HirBody, HirFlags, HirId, HirKind, HirPat, HirPatFields, IndexKind, Lit, LocalId, LocalKind, MakeKind, UnOp};
use std::collections::HashMap;
use crate::codegen::primitive_cast_opcode as cast_opcode;
use crate::typechecking::infer::{Checker, ForInCounted, ForInKind};
use crate::typechecking::subst::apply_ty_prune;
use crate::typechecking::ty::{self as coil_ty, Ty, strip_readonly};
use crate::typechecking::AggregateArithInfo;

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
    classify_in(checker, &ty, &mut Vec::new())
}

fn classify_in(checker: &Checker, ty: &Ty, seen: &mut Vec<String>) -> Option<ValueClass> {
    let ty = strip_readonly(ty);
    if is_scalar(ty) {
        return Some(ValueClass::Scalar);
    }
    if super::layout::is_unit(ty) {
        return Some(ValueClass::Unit);
    }
    match ty {
        Ty::Con(n) if n == coil_ty::STRING => Some(ValueClass::Opaque),
        // A foreign pointer: one word, only moved and passed to `extern`s.
        Ty::Con(n) if n == "ptr" && !checker.is_class(n) && checker.enum_variants(n).is_none() => Some(ValueClass::Opaque),
        // Host handles (`io` streams, threads, channels, locks): one word.
        Ty::Con(n) if is_host_handle(n) && !checker.is_class(n) && checker.enum_variants(n).is_none() => {
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
        Ty::Array { element, .. } => aggregate(checker, std::iter::once(element.as_ref()), false, seen),
        // A bare-class existential (`Show x`): one `[value, dictionary]`
        // tuple word, only moved and called through its dictionary.
        Ty::Existential { .. } => Some(ValueClass::Opaque),
        // `Matrix<D>` is its data at run time.
        Ty::App(..) if crate::typechecking::aggregate_arith::unwrap_matrix_ty(ty).is_some() => {
            classify_in(checker, crate::typechecking::aggregate_arith::unwrap_matrix_ty(ty)?, seen)
        }
        Ty::App(..) if coil_ty::vec_element_ty(ty).is_some() => {
            aggregate(checker, coil_ty::vec_element_ty(ty).into_iter(), false, seen)
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
            params_closed(ty).then_some(ValueClass::Enum)
        }
        Ty::Sum { name, variants } => {
            if common::is_builtin_option_enum(name) || common::is_builtin_result_enum(name) {
                for (_, payload) in variants {
                    for field in payload.field_types() {
                        classify_in(checker, field, seen)?;
                    }
                }
                return params_closed(ty).then_some(ValueClass::Enum);
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
        Ty::Con(name) if checker.is_class(name) && checker.enum_variants(name).is_none() => object_class(checker, name),
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
        t @ Ty::Fun(..) => !crate::typechecking::subst::ftv(t).is_empty(),
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

/// [`fun_words`], with `open_enum` also taking an `Option` / `Result`
/// whose type variable nothing pinned (`Ok(2)`'s error type): one boxed
/// word, no niche or pair (#1520). Only a closure literal's own type may:
/// its body and its calls see the one open type, where a parameter's
/// caller may pass a closure returning the pinned enum's pair.
fn fun_words_in(checker: &Checker, ty: &Ty, seen: &mut Vec<String>, open_enum: bool) -> bool {
    // A bare type parameter (a shared generic body's own `T -> U`
    // parameter) is one boxed word too.
    let plain = |t: &Ty, seen: &mut Vec<String>| {
        (super::layout::ty_is_closed(t) || matches!(strip_readonly(t), Ty::Fun(..) | Ty::Var(_)))
            && matches!(
                classify_in(checker, t, seen),
                Some(ValueClass::Scalar | ValueClass::Opaque | ValueClass::Object | ValueClass::Aggregate)
            )
    };
    match strip_readonly(ty) {
        // `() -> T` takes a unit parameter, and `T -> ()` returns one.
        Ty::Fun(param, ret) => {
            let unit = |t: &Ty| super::layout::is_unit(strip_readonly(t));
            let closed_enum = |t: &Ty, seen: &mut Vec<String>| {
                (super::layout::ty_is_closed(t) || open_enum && super::layout::of(checker, t) == super::layout::Layout::Word)
                    && classify_in(checker, t, seen) == Some(ValueClass::Enum)
            };
            // A closure literal's tuple of type parameters (a shared generic
            // body's `fn () use (x) => (x, x)`) is its one heap tuple word:
            // an open tuple is never a pair (#1524).
            let open_tuple = |t: &Ty, seen: &mut Vec<String>| {
                open_enum
                    && matches!(strip_readonly(t), Ty::Tuple(_))
                    && !super::layout::ty_is_closed(t)
                    && params_closed(t)
                    && classify_in(checker, t, seen) == Some(ValueClass::Aggregate)
            };
            (unit(param) || plain(param, seen) || closed_enum(param, seen))
                && (unit(ret) || plain(ret, seen) || closed_enum(ret, seen) || open_tuple(ret, seen))
        }
        _ => false,
    }
}

/// Closed but for type parameters (a generic class's shared method body):
/// each parameter is one boxed word, and [`super::layout::of`] gives a type
/// open in one the same layout the AST codegen uses.
fn params_closed(ty: &Ty) -> bool {
    match strip_readonly(ty) {
        Ty::Var(_) => true,
        Ty::Fun(_, _) | Ty::Existential { .. } | Ty::Forall { .. } => false,
        Ty::Constructor { owner: inner, .. } => params_closed(inner),
        Ty::App(_, args) => args.iter().all(params_closed),
        Ty::Tuple(items) => items.iter().all(params_closed),
        Ty::Record { fields } => fields.iter().all(|(_, f)| params_closed(f)),
        Ty::Array { element, .. } => params_closed(element),
        Ty::Sum { variants, .. } => variants
            .iter()
            .all(|(_, p)| p.field_types().into_iter().all(params_closed)),
        Ty::Con(_) | Ty::Never => true,
        Ty::Readonly(_) => unreachable!("stripped"),
    }
}

/// A tuple, array or `Vec` whose element types are closed but for type
/// parameters (elements are classified where they are read). With `unit`,
/// an item may be `()`.
fn aggregate<'t>(checker: &Checker, mut items: impl Iterator<Item = &'t Ty>, unit: bool, seen: &mut Vec<String>) -> Option<ValueClass> {
    items
        .all(|t| {
            (params_closed(t) || is_existential(t) || mono_fun(checker, t, seen))
                && classify_in(checker, t, seen).is_none_or(|class| is_word(class) || unit && class == ValueClass::Unit)
        })
        .then_some(ValueClass::Aggregate)
}

/// A monomorphic function type of plain words: its value is one closure
/// word, stored in an aggregate as that word.
fn mono_fun(checker: &Checker, ty: &Ty, seen: &mut Vec<String>) -> bool {
    matches!(strip_readonly(ty), Ty::Fun(..)) && crate::typechecking::subst::ftv(ty).is_empty() && fun_words(checker, ty, seen)
}

/// A class declared with type parameters (`class HashMap<K, V>`).
pub fn is_generic_class(checker: &Checker, name: &str) -> bool {
    if !checker.has_impl(name) {
        return false;
    }
    let ctors = &checker.generics().generic_type_ctors;
    ctors.contains_key(name) || checker.resolve_class_key(name).is_some_and(|key| ctors.contains_key(&key))
}

/// A user class the lowering builds and reads: not generic, with every
/// field type closed.
fn object_class(checker: &Checker, name: &str) -> Option<ValueClass> {
    let key = checker.resolve_class_key(name)?;
    let ctors = &checker.generics().generic_type_ctors;
    if ctors.contains_key(&key) || ctors.contains_key(name) {
        return None;
    }
    let fields = checker.class_fields(&key)?;
    fields
        .iter()
        .all(|(_, ty)| super::layout::ty_is_closed(ty) || is_existential(ty) || mono_fun(checker, ty, &mut Vec::new()))
        .then_some(ValueClass::Object)
}

/// A bare-class existential (`Show`): one `[value, dictionary]` word, closed
/// as an element or a field though its packed type is not.
fn is_existential(ty: &Ty) -> bool {
    matches!(strip_readonly(ty), Ty::Existential { .. })
}

/// The class `let x = new C(..)` keeps in frame slots instead of a heap
/// object (as the AST codegen does): no `fn drop()` and 1 to 32 fields.
pub fn sroa_class(body: &HirBody, checker: &Checker, init: HirId) -> Option<String> {
    let HirKind::Make {
        kind: MakeKind::Class(name),
        ..
    } = &body.expr(init).kind
    else {
        return None;
    };
    let key = checker.resolve_class_key(name)?;
    let n = checker.class_fields(&key)?.len();
    (!checker.class_has_drop(&key) && (1..=32).contains(&n)).then_some(key)
}

/// Whether `local` is only ever the base of a field read or write (so its
/// fields can live in frame slots with no object behind them).
pub fn only_field_base(body: &HirBody, local: LocalId) -> bool {
    let bases: std::collections::HashSet<u32> = body
        .exprs
        .iter()
        .filter_map(|e| match e.kind {
            HirKind::Field { base, .. } => Some(base.0),
            _ => None,
        })
        .collect();
    let assigned = body.exprs.iter().any(|e| match e.kind {
        HirKind::Assign { place, .. } => body.expr(place).kind == HirKind::Local(local),
        _ => false,
    });
    !assigned
        && body
            .exprs
            .iter()
            .enumerate()
            .all(|(i, e)| e.kind != HirKind::Local(local) || bases.contains(&(i as u32)))
}

/// `let a = [e1, .., en]` locals kept in `n` frame slots (the AST's
/// multi-slot stack array).
#[derive(Default)]
pub struct StackArrays {
    /// Local to its length.
    pub len: HashMap<u32, usize>,
    /// A block statement to the locals whose slots are boxed into one
    /// array object just before it: the first statement after the `let`
    /// that uses the local other than through an index (its escape). From
    /// there on the local is that object, as the AST's hoisted Q1 box.
    pub box_at: HashMap<u32, Vec<u32>>,
}

/// Whether some node of `id`'s subtree satisfies `f`.
fn any_id(body: &HirBody, id: HirId, f: &impl Fn(HirId) -> bool) -> bool {
    f(id) || children(body, id).into_iter().any(|k| any_id(body, k, f))
}

/// The stack arrays of `body`: each `let a = [..]` with 1..=32 items, or
/// `let b = a` of such an `a` (a slot copy), that is never destructured or
/// spread, and whose escape (a `for` over it among them), if any, is a
/// later statement of the block that binds it. `a = b` between two stack arrays of one length and
/// `a = [..]` of that many items store into the slots; a stack array that
/// takes part in a copy or such a store never escapes.
pub fn stack_arrays(body: &HirBody, checker: &Checker) -> StackArrays {
    use std::collections::HashSet;
    // `a.len()` of a fixed-length array folds to a constant (`hir_len_call`),
    // so its receiver reads no slots.
    let folded_len: HashSet<u32> = body
        .exprs
        .iter()
        .filter_map(|e| match &e.kind {
            HirKind::Call {
                callee: Callee::Method { name },
                args,
            } if name == "len" && args.len() == 1 => Some(args[0]),
            _ => None,
        })
        .filter(|&arg| {
            matches!(body.expr(arg).kind, HirKind::Local(_))
                && body.expr(arg).ty.as_ref().is_some_and(|t| {
                    matches!(
                        crate::typechecking::ty::strip_readonly(&apply_ty_prune(checker.subst(), t)),
                        Ty::Array {
                            length: crate::typechecking::ty::ArrayLength::Static(_),
                            ..
                        }
                    )
                })
        })
        .map(|arg| arg.0)
        .collect();
    let bases: HashSet<u32> = body
        .exprs
        .iter()
        .filter_map(|e| match e.kind {
            HirKind::Index {
                base,
                kind: IndexKind::Array,
                ..
            } => Some(base.0),
            _ => None,
        })
        .collect();
    // An element-wise operand reads the slots (the AST's
    // `prepare_aggregate_src`) unless the op takes the packed path, which
    // needs the array object: a static length of at least 8, for `+ - * /`
    // and negation. The value is that minimum length, or 0 for any.
    let mut slot_reads: HashMap<u32, usize> = HashMap::new();
    for e in &body.exprs {
        match e.kind {
            HirKind::Bin {
                op: BinOp::Overloaded(sym @ ("+" | "-" | "*" | "/" | "%" | "**")),
                lhs,
                rhs,
            } => {
                let packed = if matches!(sym, "%" | "**") { 0 } else { 8 };
                slot_reads.insert(lhs.0, packed);
                slot_reads.insert(rhs.0, packed);
            }
            HirKind::Un { op: UnOp::Neg, operand } => {
                slot_reads.insert(operand.0, 8);
            }
            _ => {}
        }
    }
    // Candidates: block-statement lets of a literal (its length) or of a
    // local (a copy, its source).
    enum Init {
        Literal(usize),
        Copy(LocalId, HirId),
    }
    let mut cands: HashMap<u32, Init> = HashMap::new();
    for e in &body.exprs {
        let HirKind::Block { stmts, .. } = &e.kind else {
            continue;
        };
        for &stmt in stmts {
            let HirKind::Let {
                local,
                init: Some(init),
            } = body.expr(stmt).kind
            else {
                continue;
            };
            if body.local(local).kind != LocalKind::Let || body.local(local).captured {
                continue;
            }
            match &body.expr(init).kind {
                HirKind::Make {
                    kind: MakeKind::Array,
                    args,
                } if (1..=32).contains(&args.len()) => {
                    cands.insert(local.0, Init::Literal(args.len()));
                }
                HirKind::Local(src) => {
                    cands.insert(local.0, Init::Copy(*src, init));
                }
                _ => {}
            }
        }
    }
    // A function or coroutine element stays in the array object: its
    // closure or handle word has no slot type (#1331).
    let no_slot = |e: &Ty| {
        let e = crate::typechecking::ty::strip_readonly(e);
        matches!(e, Ty::Fun(..)) || Checker::coroutine_parts(e).is_some()
    };
    cands.retain(|&local, _| {
        !body.local(LocalId(local)).ty.as_ref().is_some_and(|t| match crate::typechecking::ty::strip_readonly(&apply_ty_prune(checker.subst(), t)) {
            Ty::Array { element, .. } => no_slot(element),
            t => crate::typechecking::ty::vec_element_ty(t).is_some_and(no_slot),
        })
    });
    // Stores into a local: `dst = value`.
    let mut stores: Vec<(u32, HirId, HirId)> = Vec::new();
    let mut refused: HashSet<u32> = HashSet::new();
    for (i, e) in body.exprs.iter().enumerate() {
        match e.kind {
            HirKind::Assign { place, value } => {
                if let HirKind::Local(local) = body.expr(place).kind {
                    if body.expr(HirId(i as u32)).flags.contains(HirFlags::COMPOUND) {
                        refused.insert(local.0);
                    } else {
                        stores.push((local.0, place, value));
                    }
                }
            }
            HirKind::LetPat { init, .. } | HirKind::Spread(init) => {
                if let HirKind::Local(local) = body.expr(init).kind {
                    refused.insert(local.0);
                }
            }
            // A copy into a local that is no candidate.
            HirKind::Let { local, init: Some(init) } if !cands.contains_key(&local.0) => {
                if let HirKind::Local(src) = body.expr(init).kind {
                    refused.insert(src.0);
                }
            }
            _ => {}
        }
    }
    // Reads of each local that are not an index base or element-wise operand.
    let mut reads: HashMap<u32, Vec<u32>> = HashMap::new();
    let mut slot_uses: HashMap<u32, Vec<(u32, usize)>> = HashMap::new();
    for (i, e) in body.exprs.iter().enumerate() {
        match e.kind {
            HirKind::Local(local) if slot_reads.contains_key(&(i as u32)) => {
                slot_uses.entry(local.0).or_default().push((i as u32, slot_reads[&(i as u32)]))
            }
            HirKind::Local(local) if !bases.contains(&(i as u32)) && !folded_len.contains(&(i as u32)) => reads.entry(local.0).or_default().push(i as u32),
            _ => {}
        }
    }
    // Each candidate's length: a literal's item count, a copy's source's.
    fn resolve(cands: &HashMap<u32, Init>, local: u32, depth: usize) -> Option<usize> {
        match cands.get(&local)? {
            Init::Literal(n) => Some(*n),
            Init::Copy(src, _) if depth < 64 => resolve(cands, src.0, depth + 1),
            Init::Copy(..) => None,
        }
    }
    let mut len: HashMap<u32, usize> = cands
        .keys()
        .filter(|l| !refused.contains(l))
        .filter_map(|&l| resolve(&cands, l, 0).map(|n| (l, n)))
        .collect();
    // Drop candidates to a fixed point: a copy needs a stack source, and a
    // copy whose target is not one refuses its source (the two would
    // share one object); a store needs a stack source of the same length
    // or a literal of that many items; both ends of a copy or store never
    // escape.
    loop {
        let before = len.len();
        for (&local, init) in &cands {
            if let Init::Copy(src, _) = init {
                if len.contains_key(&local) && !len.contains_key(&src.0) {
                    len.remove(&local);
                }
                if !len.contains_key(&local) {
                    len.remove(&src.0);
                }
            }
        }
        for &(dst, _, value) in &stores {
            let Some(&n) = len.get(&dst) else { continue };
            let ok = match &body.expr(value).kind {
                HirKind::Local(src) => len.get(&src.0) == Some(&n),
                HirKind::Make {
                    kind: MakeKind::Array,
                    args,
                } => args.len() == n,
                _ => false,
            };
            if !ok {
                len.remove(&dst);
            }
        }
        // The reads a copy or store consumes are not escapes.
        let mut linked: HashSet<u32> = HashSet::new();
        let mut link_ids: HashSet<u32> = HashSet::new();
        for (&local, init) in &cands {
            if let Init::Copy(src, id) = init
                && len.contains_key(&local)
            {
                linked.extend([local, src.0]);
                link_ids.insert(id.0);
            }
        }
        for &(dst, place, value) in &stores {
            if len.contains_key(&dst) {
                linked.insert(dst);
                link_ids.insert(place.0);
                if let HirKind::Local(src) = body.expr(value).kind {
                    linked.insert(src.0);
                    link_ids.insert(value.0);
                }
            }
        }
        for local in &linked {
            let Some(&n) = len.get(local) else { continue };
            let escapes = reads.get(local).into_iter().flatten().any(|id| !link_ids.contains(id));
            let packed = slot_uses.get(local).into_iter().flatten().any(|&(_, min)| min != 0 && n >= min);
            if escapes || packed {
                len.remove(local);
            }
        }
        if len.len() == before {
            break;
        }
    }
    // Reads consumed by a surviving copy or store.
    let mut link_ids: HashSet<u32> = HashSet::new();
    for (&local, init) in &cands {
        if let Init::Copy(_, id) = init
            && len.contains_key(&local)
        {
            link_ids.insert(id.0);
        }
    }
    for &(dst, place, value) in &stores {
        if len.contains_key(&dst) {
            link_ids.insert(place.0);
            link_ids.insert(value.0);
        }
    }
    for ids in reads.values_mut() {
        ids.retain(|id| !link_ids.contains(id));
    }
    let mut out = StackArrays::default();
    for e in &body.exprs {
        let HirKind::Block { stmts, tail } = &e.kind else {
            continue;
        };
        for (k, &stmt) in stmts.iter().enumerate() {
            let HirKind::Let { local, .. } = body.expr(stmt).kind else {
                continue;
            };
            let Some(&n) = len.get(&local.0) else {
                continue;
            };
            let packed = slot_uses.get(&local.0).into_iter().flatten().filter(|&&(_, min)| min != 0 && n >= min).map(|&(id, _)| id);
            let uses: HashSet<u32> = reads.get(&local.0).into_iter().flatten().copied().chain(packed).collect();
            if !uses.is_empty() {
                let first = stmts[k + 1..]
                    .iter()
                    .chain(tail)
                    .copied()
                    .find(|&s| any_id(body, s, &|id| uses.contains(&id.0)));
                let Some(first) = first else {
                    continue;
                };
                out.box_at.entry(first.0).or_default().push(local.0);
            }
            out.len.insert(local.0, n);
        }
    }
    out
}

/// Whether some `local.field = ..` writes a field of `local`.
/// Frame-slot class locals that escape: each `let x = new C(..)` (a
/// [`sroa_class`], never reassigned) whose fields are written and that is
/// also used whole, first by a later statement of the block that binds it.
/// It is boxed into one object just before that statement (the AST's
/// hoisted Q2 box) and is that object from there on. Maps the statement to
/// the locals boxed before it.
pub fn class_boxes(body: &HirBody, checker: &Checker) -> HashMap<u32, Vec<u32>> {
    let mut out: HashMap<u32, Vec<u32>> = HashMap::new();
    for e in &body.exprs {
        let HirKind::Block { stmts, tail } = &e.kind else {
            continue;
        };
        for (k, &stmt) in stmts.iter().enumerate() {
            let HirKind::Let {
                local,
                init: Some(init),
            } = body.expr(stmt).kind
            else {
                continue;
            };
            if sroa_class(body, checker, init).is_none()
                || body.local(local).kind != LocalKind::Let
                || only_field_base(body, local)
                || !writes_field_of(body, local)
                || reassigned(body, local)
            {
                continue;
            }
            let whole = |id: HirId| {
                matches!(body.expr(id).kind, HirKind::Local(l) if l == local)
                    && !body.exprs.iter().any(|f| matches!(f.kind, HirKind::Field { base, .. } if base == id))
            };
            if let Some(&first) = stmts[k + 1..].iter().chain(tail).find(|&&s| any_id(body, s, &whole)) {
                out.entry(first.0).or_default().push(local.0);
            }
        }
    }
    out
}

fn reassigned(body: &HirBody, local: LocalId) -> bool {
    body.exprs.iter().any(|e| match e.kind {
        HirKind::Assign { place, .. } => body.expr(place).kind == HirKind::Local(local),
        _ => false,
    })
}

/// Whether `let local = init` keeps its fields in frame slots: a
/// [`sroa_class`] that is only a field base, or one [`class_boxes`] boxes.
pub fn sroa_local(body: &HirBody, checker: &Checker, boxed: &std::collections::HashSet<u32>, local: LocalId, init: HirId) -> bool {
    !body.local(local).captured
        && sroa_class(body, checker, init).is_some()
        && (only_field_base(body, local) || boxed.contains(&local.0))
}

fn writes_field_of(body: &HirBody, local: LocalId) -> bool {
    body.exprs.iter().any(|e| match e.kind {
        HirKind::Assign { place, .. } => matches!(body.expr(place).kind,
            HirKind::Field { base, .. } if body.expr(base).kind == HirKind::Local(local)),
        _ => false,
    })
}

/// `x.len()` of a plain local: `ArrayLen`, or a constant for a fixed-size
/// type.
impl Walk<'_> {
    /// A `Vec` method: `push` is inlined as `ArrayPush`; the others are
    /// `CALL`s to the builtin thunks. `pop` / `remove` are `HostInvoke`s
    /// when the call's `Option` is niche-packed (codegen refuses the boxed
    /// form).
    fn vec_method(&mut self, name: &str, args: &[HirId]) -> Check {
        match (name, args) {
            ("push", [recv, value]) => {
                self.value(*recv)?;
                self.word(*value)?;
                self.value(*value)
            }
            ("capacity" | "clear", [_]) | ("reserve", [_, _]) | ("insert", [_, _, _]) | ("pop", [_]) | ("remove", [_, _]) => {
                self.args(args)
            }
            _ => Err("vec-method"),
        }
    }

    fn len(&mut self, arg: HirId) -> Check {
        let structural = self
            .ty(arg)
            .is_some_and(|t| crate::typechecking::infer::Checker::is_structural_len_ty_for_codegen(&apply_ty_prune(self.checker.subst(), t)));
        match self.body.expr(arg).kind {
            HirKind::Local(local) if structural && self.body.local(local).kind != super::LocalKind::Const => Ok(()),
            // Not const-foldable (the AST folds only literals and const
            // names): the value is pushed, then measured or popped.
            // Folded to its byte length, as the AST's `eval_len_operand`.
            HirKind::Lit(Lit::Str(_)) if structural => Ok(()),
            // An array, tuple or record literal folds to its item count
            // unevaluated, as `eval_len_operand`.
            HirKind::Make {
                kind: MakeKind::Array | MakeKind::Tuple | MakeKind::Record(_),
                ..
            } => Ok(()),
            // Any other operand (a call, a `?`, a `const` local, a string
            // concatenation, …) is evaluated, then measured (#1071, #1147).
            _ if structural => {
                self.word(arg)?;
                self.value(arg)
            }
            _ => Err("len-argument"),
        }
    }
}

/// Whether `id` is a `Vec<T>`.
pub fn is_vec(body: &HirBody, checker: &Checker, id: HirId) -> bool {
    body.expr(id)
        .ty
        .as_ref()
        .is_some_and(|t| coil_ty::vec_element_ty(&apply_ty_prune(checker.subst(), t)).is_some())
}

/// Whether `x.len()` is the structural length read: `x` an array, tuple,
/// record, string or `Vec` (`ArrayLen`, or a constant for a fixed-size
/// type). The one HIR form of a length read; any other `len` is a method
/// call (a user `Length` instance or a bound).
pub fn structural_len(body: &HirBody, checker: &Checker, recv: HirId) -> bool {
    body.expr(recv)
        .ty
        .as_ref()
        .is_some_and(|t| Checker::is_structural_len_ty_for_codegen(&apply_ty_prune(checker.subst(), t)))
}

/// Whether evaluating `id` may store into frame slots (the AST's
/// `expr_may_clobber_operand_stack`): a call, `match`, `new` or string
/// index anywhere inside it.
pub fn clobbers(body: &HirBody, stack: &HashMap<u32, usize>, id: HirId) -> bool {
    let mut found = false;
    visit(body, id, &mut |e| {
        found |= stack_select(body, stack, e) || matches!(
            &e.kind,
            HirKind::Call { .. }
                | HirKind::Match { .. }
                | HirKind::Make { kind: MakeKind::Class(_), .. }
                | HirKind::Index { kind: IndexKind::String, .. }
                | HirKind::Resume { .. }
                | HirKind::Yield { .. }
        ) || matches!(
            // Boxing ops of compiler-made bodies keep the stack as it is.
            &e.kind,
            HirKind::Builtin { op, .. } if !matches!(op, Builtin::Unbox { .. } | Builtin::Box { .. } | Builtin::Stringify | Builtin::Relayout)
        );
    });
    found
}

/// The checker's element-wise plan for an aggregate `a op b` / `-a` at `id`
/// (the AST's `try_emit_aggregate_arith`); `None` for a matrix op, which the
/// AST emits first and HIR leaves to it.
/// Whether the checker recorded `id` as a linear-algebra op (a matrix or
/// vector operator, or `dot` / `matmul` / ..).
pub fn linear_algebra(checker: &Checker, body: &HirBody, id: HirId) -> bool {
    let e = body.expr(id);
    let (start, end) = e.span;
    e.node.is_some_and(|n| checker.linear_algebra_at(n).is_some()) || checker.linear_algebra_span(start, end).is_some()
}

pub fn aggregate_info(checker: &Checker, body: &HirBody, id: HirId) -> Option<AggregateArithInfo> {
    let e = body.expr(id);
    let (start, end) = e.span;
    if linear_algebra(checker, body, id) {
        return None;
    }
    e.node
        .and_then(|n| checker.aggregate_arith_at(n))
        .or_else(|| checker.aggregate_arith_span(start, end))
        .cloned()
        .or_else(|| recover_aggregate(checker, body, id))
}

/// The element-wise shape from the operand types where the checker kept
/// none (a mono clone of a generic body), as the AST's
/// `recover_aggregate_arith`: only for operands it types itself, locals and
/// literals.
fn recover_aggregate(checker: &Checker, body: &HirBody, id: HirId) -> Option<AggregateArithInfo> {
    use crate::typechecking::{AggregateOp, aggregate_arith::recover};
    fn typed(body: &HirBody, id: HirId) -> bool {
        match &body.expr(id).kind {
            HirKind::Local(_) | HirKind::Lit(Lit::Int(_) | Lit::Float(_)) => true,
            HirKind::Make {
                kind: MakeKind::Tuple,
                args,
            } => args.iter().all(|&a| typed(body, a)),
            HirKind::Make {
                kind: MakeKind::Array,
                args,
            } => !args.is_empty() && args.iter().all(|&a| typed(body, a)),
            _ => false,
        }
    }
    let ty = |x: HirId| {
        typed(body, x)
            .then(|| body.expr(x).ty.as_ref())
            .flatten()
            .map(|t| apply_ty_prune(checker.subst(), t))
    };
    match body.expr(id).kind {
        HirKind::Bin {
            op: BinOp::Overloaded(sym),
            lhs,
            rhs,
        } => recover(&ty(lhs)?, Some(&ty(rhs)?), AggregateOp::from_str(sym)?),
        HirKind::Un { op: UnOp::Neg, operand } => recover(&ty(operand)?, None, AggregateOp::Neg),
        _ => None,
    }
}

/// How an element-wise operand's elements are read, as the AST's
/// `aggregate_src_is_direct_elems`: a non-empty array or tuple literal's
/// items, a frame-slot stack array's slots, else (`None`) a heap value
/// in a temp read with `Index`.
pub enum AggregateSrc {
    Items(Vec<HirId>),
    Slots(LocalId),
}

pub fn aggregate_src(body: &HirBody, stack: &HashMap<u32, usize>, id: HirId) -> Option<AggregateSrc> {
    match &body.expr(id).kind {
        HirKind::Make {
            kind: MakeKind::Array | MakeKind::Tuple,
            args,
        } if !args.is_empty() => Some(AggregateSrc::Items(args.clone())),
        HirKind::Local(local) if stack.contains_key(&local.0) => Some(AggregateSrc::Slots(*local)),
        _ => None,
    }
}

/// The packed `HostInvoke` path (`try_emit_packed_aggregate_arith`): a static
/// shape of at least 8 elements, for `+ - * /` and negation.
pub fn aggregate_packed(info: &AggregateArithInfo) -> Option<usize> {
    use crate::typechecking::{AggregateArithKind as K, AggregateOp};
    if matches!(info.op, AggregateOp::Mod | AggregateOp::Pow) {
        return None;
    }
    let len = match info.kind {
        K::ZipTuple { arity, .. } | K::BroadcastTuple { arity, .. } | K::NegTuple { arity, .. } => arity,
        K::ZipArray { length, .. } => length,
        K::BroadcastArray { length: Some(n), .. } | K::NegArray { length: Some(n), .. } => n,
        K::BroadcastArray { length: None, .. } | K::NegArray { length: None, .. } => return None,
    };
    (8..=u16::MAX as usize).contains(&len).then_some(len)
}

/// Whether an argument after the first holds a clobbering index read
/// ([`staged_read`]), which a live argument would sit under (the AST
/// stages every argument when one may clobber).
pub fn index_stages(body: &HirBody, stack: &HashMap<u32, usize>, args: &[HirId]) -> bool {
    args.iter().skip(1).any(|&arg| {
        let mut found = false;
        visit(body, arg, &mut |e| found |= staged_read(body, stack, e));
        found
    })
}

/// Whether the checker recorded the call at `id` as a trait method on an
/// existential pack.
pub fn existential_hint(body: &HirBody, checker: &Checker, id: HirId) -> bool {
    let e = body.expr(id);
    e.node.and_then(|n| checker.existential_method_call_at(n)).is_some()
        || checker.existential_method_call_span(e.span.0, e.span.1).is_some()
}

/// Each `break` / `continue` outside every loop of `body` and of the
/// anonymous `fn`s in it (E0801), with whether it is a `break`.
pub fn stray_jumps(module: &super::HirModule, body: &HirBody) -> Vec<(HirId, bool)> {
    fn walk(module: &super::HirModule, body: &HirBody, id: HirId, loops: u32, out: &mut Vec<(HirId, bool)>) {
        match &body.expr(id).kind {
            HirKind::Break | HirKind::Continue if loops == 0 => {
                out.push((id, matches!(body.expr(id).kind, HirKind::Break)));
            }
            HirKind::Lambda { body: lam } => {
                let lam = &module.bodies[*lam];
                if let Some(root) = lam.root {
                    walk(module, lam, root, 0, out);
                }
            }
            kind => {
                let inner = loops + u32::from(matches!(kind, HirKind::Loop { .. } | HirKind::ForIn { .. }));
                for k in children(body, id) {
                    walk(module, body, k, inner, out);
                }
            }
        }
    }
    let mut out = Vec::new();
    if let Some(root) = body.root {
        walk(module, body, root, 0, &mut out);
    }
    out
}

/// Reads of a type-only `let x: T;` local of a non-scalar type that may run
/// before the local is assigned (#1273). A scalar starts at zero; any other
/// word starts as a null placeholder that must never be read. Every path to
/// a read must assign the local first; an assignment in a loop body counts
/// only inside that body.
pub fn unassigned_reads(module: &super::HirModule, body: &HirBody) -> Vec<HirId> {
    use std::collections::BTreeSet;
    type Set = BTreeSet<u32>;
    struct Flow<'a> {
        module: &'a super::HirModule,
        body: &'a HirBody,
        out: Vec<HirId>,
    }
    impl Flow<'_> {
        fn read(&mut self, at: HirId, local: LocalId, s: &Set) {
            if s.contains(&local.0) && !self.out.contains(&at) {
                self.out.push(at);
            }
        }
        fn seq(&mut self, ids: &[HirId], s: Set) -> Option<Set> {
            ids.iter().try_fold(s, |s, &k| self.flow(k, s))
        }
        /// The locals still unassigned after `id`; `None` when it never
        /// completes (a `return`, `break` or `continue` on every path).
        fn flow(&mut self, id: HirId, s: Set) -> Option<Set> {
            let join = |a: Option<Set>, b: Option<Set>| match (a, b) {
                (Some(a), Some(b)) => Some(a.union(&b).copied().collect()),
                (a, b) => a.or(b),
            };
            match &self.body.expr(id).kind {
                HirKind::Local(l) => {
                    self.read(id, *l, &s);
                    Some(s)
                }
                HirKind::Let { local, init: None } => {
                    let mut s = s;
                    let scalar = self.body.local(*local).ty.as_ref().and_then(primitive).is_some_and(|p| {
                        matches!(p, coil_ty::INT | coil_ty::FLOAT | coil_ty::BOOL | coil_ty::BYTE)
                    });
                    if !scalar {
                        s.insert(local.0);
                    }
                    Some(s)
                }
                HirKind::Assign { place, value } if matches!(self.body.expr(*place).kind, HirKind::Local(_)) => {
                    let HirKind::Local(l) = self.body.expr(*place).kind else { unreachable!() };
                    let mut s = self.flow(*value, s)?;
                    s.remove(&l.0);
                    Some(s)
                }
                HirKind::If { cond, then, els } => {
                    let s = self.flow(*cond, s)?;
                    let a = self.flow(*then, s.clone());
                    let b = match els {
                        Some(e) => self.flow(*e, s),
                        None => Some(s),
                    };
                    join(a, b)
                }
                HirKind::Logic { lhs, rhs, .. } => {
                    let s = self.flow(*lhs, s)?;
                    let r = self.flow(*rhs, s.clone());
                    join(Some(s), r)
                }
                HirKind::Match { scrutinee, arms } => {
                    let s = self.flow(*scrutinee, s)?;
                    let mut out = None;
                    for arm in arms {
                        let a = self.flow(arm.body, s.clone());
                        out = join(out, a);
                    }
                    out
                }
                HirKind::Loop { body: b } => {
                    let _ = self.flow(*b, s.clone());
                    Some(s)
                }
                HirKind::ForIn { iterable, body: b, .. } => {
                    let s = self.flow(*iterable, s)?;
                    let _ = self.flow(*b, s.clone());
                    Some(s)
                }
                HirKind::Break | HirKind::Continue => None,
                HirKind::Return(v) => {
                    if let Some(v) = v {
                        self.flow(*v, s)?;
                    }
                    None
                }
                // A capture reads the local when the closure is made.
                HirKind::Lambda { body: lam } => {
                    for &(outer, _) in &self.module.bodies[*lam].captures {
                        self.read(id, outer, &s);
                    }
                    Some(s)
                }
                HirKind::Defer { captures, .. } => {
                    for l in captures.iter().flatten() {
                        self.read(id, *l, &s);
                    }
                    Some(s)
                }
                HirKind::Builtin { op: Builtin::Panic, args } => {
                    self.seq(args, s)?;
                    None
                }
                _ => {
                    let kids = children(self.body, id);
                    self.seq(&kids, s)
                }
            }
        }
    }
    let mut flow = Flow { module, body, out: Vec::new() };
    if let Some(root) = body.root {
        let _ = flow.flow(root, BTreeSet::new());
    }
    flow.out
}

/// Every node of `id`'s subtree, `id` first.
pub(crate) fn visit(body: &HirBody, id: HirId, f: &mut impl FnMut(&super::HirExpr)) {
    f(body.expr(id));
    for k in children(body, id) {
        visit(body, k, f);
    }
}

/// Whether `id` always leaves the function by `return`.
pub(crate) fn exits(body: &HirBody, id: HirId) -> bool {
    match &body.expr(id).kind {
        HirKind::Return(_) => true,
        HirKind::Block { stmts, tail } => stmts.iter().chain(tail).any(|&s| exits(body, s)),
        HirKind::If { then, els: Some(els), .. } => exits(body, *then) && exits(body, *els),
        _ => false,
    }
}

/// An `if` arm that always returns, with no loop, closure, `defer`,
/// coroutine step or `break` / `continue` in it: lowering may lay it out
/// after the body as a cold early exit.
pub(crate) fn cold_exit(body: &HirBody, id: HirId) -> bool {
    let mut plain = true;
    visit(body, id, &mut |e| {
        plain &= !matches!(
            e.kind,
            HirKind::Loop { .. }
                | HirKind::ForIn { .. }
                | HirKind::Break
                | HirKind::Continue
                | HirKind::Lambda { .. }
                | HirKind::Yield { .. }
                | HirKind::Resume { .. }
                | HirKind::Defer { .. }
                | HirKind::Unsupported(_)
        );
    });
    plain && exits(body, id)
}

/// The direct subexpressions of `id`.
pub(crate) fn children(body: &HirBody, id: HirId) -> Vec<HirId> {
    let e = body.expr(id);
    let mut kids: Vec<HirId> = Vec::new();
    match &e.kind {
        HirKind::Field { base, .. } => kids.push(*base),
        HirKind::Index { base, index, .. } => kids.extend([*base, *index]),
        HirKind::Bin { lhs, rhs, .. } | HirKind::Logic { lhs, rhs, .. } => kids.extend([*lhs, *rhs]),
        HirKind::Un { operand, .. } | HirKind::Cast { value: operand } => kids.push(*operand),
        HirKind::Call { callee, args } => {
            if let Callee::Value(v) = callee {
                kids.push(*v);
            }
            kids.extend(args);
        }
        HirKind::Named { value, .. } | HirKind::Spread(value) => kids.push(*value),
        HirKind::Make { args, .. } | HirKind::Builtin { args, .. } => kids.extend(args),
        HirKind::Block { stmts, tail } => kids.extend(stmts.iter().chain(tail)),
        HirKind::Let { init, .. } => kids.extend(init),
        HirKind::LetPat { init, .. } => kids.push(*init),
        HirKind::Assign { place, value } | HirKind::Append { base: place, value } => kids.extend([*place, *value]),
        HirKind::If { cond, then, els } => kids.extend([*cond, *then].into_iter().chain(*els)),
        HirKind::Loop { body: b } | HirKind::Defer { body: b, .. } => kids.push(*b),
        HirKind::ForIn { iterable, body: b, .. } => kids.extend([*iterable, *b]),
        HirKind::Return(v) => kids.extend(v),
        HirKind::Match { scrutinee, arms } => kids.extend(std::iter::once(*scrutinee).chain(arms.iter().map(|a| a.body))),
        HirKind::Yield { value, .. } => kids.push(*value),
        HirKind::Resume { handle, value } => kids.extend(std::iter::once(*handle).chain(*value)),
        _ => {}
    }
    kids
}

/// `a && b` / `a || b` may evaluate `b` eagerly (one `AND` / `OR`, no
/// branch) when `b` reads only locals, literals and fields with no trap and
/// no effect. Anything else short-circuits.
pub fn logic_eager(body: &HirBody, rhs: HirId) -> bool {
    match &body.expr(rhs).kind {
        HirKind::Lit(_) | HirKind::Local(_) => true,
        // `x / k` and `x % k` trap only on a zero divisor, or `MIN / -1`.
        HirKind::Bin {
            op: BinOp::IntDiv | BinOp::IntRem,
            lhs,
            rhs,
        } => matches!(body.expr(*rhs).kind, HirKind::Lit(Lit::Int(k)) if k != 0 && k != -1) && logic_eager(body, *lhs),
        // Other int arithmetic can overflow: `x != MAX && x + 1 > y` must
        // not compute `x + 1`.
        HirKind::Bin { op, lhs, rhs: r } => {
            !super::op_may_trap(body, rhs)
                && !matches!(op, BinOp::StrConcat | BinOp::Overloaded(_))
                && logic_eager(body, *lhs)
                && logic_eager(body, *r)
        }
        HirKind::Logic { lhs, rhs, .. } => logic_eager(body, *lhs) && logic_eager(body, *rhs),
        HirKind::Un { operand, .. } => !super::op_may_trap(body, rhs) && logic_eager(body, *operand),
        HirKind::Field { base, .. } => logic_eager(body, *base),
        _ => false,
    }
}

/// An index that is safe to evaluate twice (a compound assignment builds
/// its place twice): locals, literals and arithmetic on them.
pub(crate) fn pure_index(body: &HirBody, id: HirId) -> bool {
    match &body.expr(id).kind {
        HirKind::Local(_) | HirKind::Lit(_) => true,
        HirKind::Bin { lhs, rhs, .. } => pure_index(body, *lhs) && pure_index(body, *rhs),
        HirKind::Un { operand, .. } | HirKind::Cast { value: operand } => pure_index(body, *operand),
        HirKind::Field { base, .. } => pure_base(body, *base),
        _ => false,
    }
}

/// A place base that reads no state twice: `x` or `x.f.g`. A compound
/// assignment builds its place twice, so the base must be safe to repeat.
pub(crate) fn pure_base(body: &HirBody, id: HirId) -> bool {
    match &body.expr(id).kind {
        HirKind::Local(_) => true,
        HirKind::Field { base, .. } => pure_base(body, *base),
        _ => false,
    }
}

/// A field store's base that only reads: `x`, `x.f`, or `x[i]` of such a
/// base with a [`pure_index`] (`xs[0].b`). The AST pushes it above the
/// value, and a compound store builds it twice.
fn read_base(body: &HirBody, id: HirId) -> bool {
    match &body.expr(id).kind {
        HirKind::Local(_) => true,
        HirKind::Field { base, .. } => read_base(body, *base),
        HirKind::Index {
            base,
            index,
            kind: IndexKind::Array | IndexKind::Tuple,
        } => read_base(body, *base) && pure_index(body, *index),
        _ => false,
    }
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
        payload.iter().all(|field| {
            super::layout::ty_is_closed(field) && classify_in(checker, field, seen).is_some()
        })
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
    Some(payload.iter().map(|t| super::layout::bind_params(t, params, args)).collect())
}

/// An instance of a generic user enum (`Tree<int>`, or `Tree<T>` in a
/// shared body): laid out as a closed enum with its parameters bound, as
/// the AST's instance is.
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
    seen.push(key);
    let ok = variants.iter().all(|(variant, _, _)| {
        generic_enum_payload(checker, name, variant, args).is_some_and(|payload| {
            payload
                .iter()
                .all(|field| params_closed(field) && classify_in(checker, field, seen).is_some())
        })
    });
    seen.pop();
    ok.then_some(ValueClass::Enum)
}

/// A numeric `Range` / `RangeInclusive`: `[start, end]` in a direct
/// call's arguments and result and in an unboxed local, else one boxed word.
pub fn is_range_pair(ty: &Ty) -> bool {
    crate::typechecking::return_layout::two_word_range_kind(ty).is_some()
}

/// A `Range<T>` / `RangeInclusive<T>` over an open type parameter `T`.
pub fn is_open_range(ty: &Ty) -> bool {
    crate::typechecking::ty::range_app(ty).is_some_and(|(elem, _)| matches!(strip_readonly(elem), Ty::Var(_)))
}

/// An irrefutable `let` pattern of tuples, records, names and `_`.
pub fn let_pat_shape(pat: &HirPat) -> bool {
    match pat {
        HirPat::Wild | HirPat::Bind(_) => true,
        HirPat::Tuple(items) => items.iter().all(let_pat_shape),
        HirPat::Record(fields) => fields.iter().all(|(_, p)| let_pat_shape(p)),
        HirPat::Int(_) | HirPat::Variant { .. } => false,
    }
}

/// `x % m` with a literal `m` and a dividend not proven non-negative:
/// the AST adds a Euclidean fix-up after an array index.
pub fn index_euclid(hir: &HirBody, index: HirId) -> Option<i32> {
    let HirKind::Bin {
        op: BinOp::IntRem,
        lhs,
        rhs,
    } = hir.expr(index).kind
    else {
        return None;
    };
    let HirKind::Lit(Lit::Int(m)) = hir.expr(rhs).kind else {
        return None;
    };
    if m <= 0 || m > i32::MAX as i64 {
        return None;
    }
    let nonneg = match hir.expr(lhs).kind {
        HirKind::Lit(Lit::Int(n)) => n >= 0,
        _ => hir.expr(lhs).flags.contains(HirFlags::NONNEG),
    };
    (!nonneg).then_some(m as i32)
}

/// Whether a stack-array index needs no bounds check, as the AST's
/// `stack_array_index_proven`: the checker proved it, or it is
/// `i % m` with `0 < m <= n`.
pub fn stack_index_proven(hir: &HirBody, node: HirId, index: HirId, n: usize) -> bool {
    if hir.expr(node).flags.contains(HirFlags::IN_BOUNDS) {
        return true;
    }
    let HirKind::Bin {
        op: BinOp::IntRem,
        rhs,
        ..
    } = hir.expr(index).kind
    else {
        return false;
    };
    matches!(hir.expr(rhs).kind, HirKind::Lit(Lit::Int(m)) if m > 0 && m as usize <= n)
}

/// `[start, end]` of a `for` over a range literal.
pub fn range_bounds(body: &HirBody, iterable: HirId) -> Option<[HirId; 2]> {
    match &body.expr(iterable).kind {
        HirKind::Make {
            kind: MakeKind::Range { .. },
            args,
        } => <[HirId; 2]>::try_from(args.as_slice()).ok(),
        _ => None,
    }
}

/// `for x in a..b` over int literals with at most eight trips, no
/// `break` or `continue` of its own and no lambda: the first value and the
/// trip count, for the body emitted once per value as the AST's
/// `emit_for_in_range` does.
pub fn unrolled_range(hir: &HirBody, iterable: HirId, body: HirId, inclusive: bool) -> Option<(i64, u32)> {
    let [lo, hi] = range_bounds(hir, iterable)?;
    let (HirKind::Lit(Lit::Int(s)), HirKind::Lit(Lit::Int(e))) = (&hir.expr(lo).kind, &hir.expr(hi).kind) else {
        return None;
    };
    let count = if inclusive {
        e.saturating_sub(*s).saturating_add(1)
    } else {
        e.saturating_sub(*s)
    };
    if !(0..=8).contains(&count) || has_own_jump(hir, body) || has_lambda(hir, body) || has_defer(hir, body) {
        return None;
    }
    Some((*s, count as u32))
}

/// Each copy of a `defer` is its own site that every later exit checks,
/// so repeating one grows the exits with the square of the copies.
fn has_defer(hir: &HirBody, id: HirId) -> bool {
    matches!(hir.expr(id).kind, HirKind::Defer { .. }) || children(hir, id).into_iter().any(|k| has_defer(hir, k))
}

fn has_own_jump(hir: &HirBody, id: HirId) -> bool {
    match &hir.expr(id).kind {
        HirKind::Break | HirKind::Continue => true,
        HirKind::Loop { .. } | HirKind::ForIn { .. } => false,
        _ => children(hir, id).into_iter().any(|k| has_own_jump(hir, k)),
    }
}

/// A lambda is planned for one emission, so its body cannot repeat.
fn has_lambda(hir: &HirBody, id: HirId) -> bool {
    matches!(hir.expr(id).kind, HirKind::Lambda { .. }) || children(hir, id).into_iter().any(|k| has_lambda(hir, k))
}

/// Whether `body` holds a `continue` of its own loop (nested loops' are
/// theirs), as `const_fold::body_has_continue`.
pub fn has_own_continue(hir: &HirBody, body: HirId) -> bool {
    match &hir.expr(body).kind {
        HirKind::Continue => true,
        HirKind::Loop { .. } | HirKind::ForIn { .. } => false,
        _ => children(hir, body).into_iter().any(|k| has_own_continue(hir, k)),
    }
}

/// Whether `local` is assigned anywhere in `body`.
pub fn assigns_local(hir: &HirBody, body: HirId, local: LocalId) -> bool {
    let mut found = false;
    visit(hir, body, &mut |e| {
        if let HirKind::Assign { place, .. } = &e.kind
            && matches!(hir.expr(*place).kind, HirKind::Local(l) if l == local)
        {
            found = true;
        }
    });
    found
}

/// Whether the fn id `f` (a local, or a class field a `declare` was stored
/// in) names a variadic `declare`.
pub fn ffi_fn_variadic(body: &HirBody, checker: &Checker, f: HirId) -> bool {
    match &body.expr(f).kind {
        HirKind::Local(local) => checker.ffi_fn_id_variadic(&body.local(*local).name).unwrap_or(false),
        HirKind::Field { base, name } => body
            .expr(*base)
            .ty
            .as_ref()
            .map(|t| apply_ty_prune(checker.subst(), t))
            .and_then(|t| match strip_readonly(&t) {
                Ty::Con(class) => checker.ffi_field_fn_id_variadic(class, name),
                _ => None,
            })
            .unwrap_or(false),
        _ => false,
    }
}

/// A variadic `invoke`'s per-argument FFI type tags: the checker's for the
/// call, else each argument's literal kind (the AST's
/// `resolve_variadic_ffi_tags`).
pub fn ffi_variadic_tags(body: &HirBody, checker: &Checker, call: HirId, items: &[HirId]) -> Option<Vec<(u32, u32)>> {
    use common::tag;
    if let Some(tags) = checker.variadic_arg_tags_at(body.expr(call).span) {
        return Some(tags.to_vec());
    }
    items
        .iter()
        .map(|&a| match body.expr(a).kind {
            HirKind::Lit(Lit::Float(_)) => Some((tag::FLOAT, 0)),
            HirKind::Lit(Lit::Str(_)) => Some((tag::STRING, 0)),
            HirKind::Lit(Lit::Bool(_)) => Some((tag::BOOL, 0)),
            HirKind::Lit(Lit::Int(_)) => Some((tag::INT, 0)),
            _ => None,
        })
        .collect()
}

/// Depth refusals [`refusal_at`] names an operand for.
const STAGED: &[&str] = &["assign-index", "adjust-value", "assign-value"];

/// Why `body` is outside the lowered subset, or `None` when it is inside.
pub fn refusal(body: &HirBody, checker: &Checker) -> Option<&'static str> {
    refusal_at(body, checker).map(|(reason, _)| reason)
}

/// [`refusal`], with the operand that would lower if it were staged into a
/// temp ahead of its statement ([`super::stage`]).
pub fn refusal_at(body: &HirBody, checker: &Checker) -> Option<(&'static str, Option<HirId>)> {
    if !matches!(body.kind, BodyKind::Function | BodyKind::Method | BodyKind::Test | BodyKind::Lambda | BodyKind::Static) {
        return Some(("body-kind", None));
    }
    if body.pinned_param {
        return Some(("pinned-type-param", None));
    }
    // A lambda's captures sit in its frame's first slots.
    if !body.captures.is_empty() && body.kind != BodyKind::Lambda {
        return Some(("captures", None));
    }
    if body.ret.as_ref().and_then(|ty| classify(checker, ty)).is_none() {
        return Some(("return-type", None));
    }
    // A `()` local (`let _ = f()`, the `Ok(ok)` of a `?`) has no slot: it
    // is only read as a statement, and a value read is refused as a value
    // type.
    if body.locals.iter().any(|l| l.ty.as_ref().and_then(|ty| classify(checker, ty)).is_none()) {
        return Some(("local-type", None));
    }
    let root = body.root?;
    let mut walk = Walk {
        body,
        checker,
        loops: 0,
        stack: HashMap::new(),
        box_at: HashMap::new(),
        class_boxed: std::collections::HashSet::new(),
        stage_at: None,
    };
    walk.class_boxed = class_boxes(body, checker).into_values().flatten().collect();
    let stacks = stack_arrays(body, checker);
    walk.stack = stacks.len;
    walk.box_at = stacks.box_at;
    walk.effect(root)
        .err()
        .map(|reason| (reason, walk.stage_at.filter(|_| STAGED.contains(&reason))))
}

/// The builtin opaque types the `io` and `thread` modules export.
fn is_host_handle(name: &str) -> bool {
    matches!(name, coil_ty::STREAM | "Thread" | "Sender" | "Receiver" | "Mutex" | "RwLock")
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

/// The `dload` / `declare` / `invoke` a call to `name` is, after
/// `use ffi::{…}`.
pub fn ffi_builtin(checker: &Checker, callee: &Callee) -> Option<crate::typechecking::FfiBuiltin> {
    match callee {
        Callee::Named { name, .. } => checker.ffi_fn_in_scope(name),
        _ => None,
    }
}

/// The `(tag, aux)` constant a `declare` signature entry stands for, as
/// the checker's `ffi_type_tag_from_output`: a tag name, a builtin FFI
/// enum variant, or `[T]` / `(T, U)` as a pointer.
pub fn ffi_tag(body: &HirBody, checker: &Checker, id: HirId) -> Option<(u32, u32)> {
    match &body.expr(id).kind {
        HirKind::Global { name, .. } => checker.ffi_type_tag_from_name(name),
        HirKind::Make {
            kind: MakeKind::Variant { enum_name, variant, .. },
            args,
        } if args.is_empty() && common::is_builtin_ffi_enum(enum_name) => checker.ffi_type_tag_from_variant(enum_name, variant),
        HirKind::Make { kind: MakeKind::Array, args } if args.len() == 1 => Some((common::tag::PTR, 0)),
        HirKind::Make { kind: MakeKind::Tuple, .. } => Some((common::tag::PTR, 0)),
        _ => None,
    }
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

/// A one-byte string literal typed `byte`: pushed as its code.
pub fn byte_literal(raw: &str) -> Option<u8> {
    match crate::codegen::unescape_coil_string(raw).as_bytes() {
        [b] => Some(*b),
        _ => None,
    }
}

/// `Make Tuple []`: the unit value `()`.
pub fn is_unit_make(body: &HirBody, id: HirId) -> bool {
    matches!(
        &body.expr(id).kind,
        HirKind::Make { kind: MakeKind::Tuple, args } if args.is_empty()
    )
}

/// The word a `return` pushes: bare `return;` carries `()` (an empty tuple
/// make), which returns like `None`.
pub fn returned_value(body: &HirBody, value: Option<HirId>) -> Option<HirId> {
    value.filter(|v| !is_unit_make(body, *v))
}

/// A `()`-typed returned value (`return check(x)?`, a unit `match`): it runs
/// for its effect, then the `return` is a bare one.
pub fn is_unit_value(body: &HirBody, checker: &Checker, id: HirId) -> bool {
    body.expr(id)
        .ty
        .as_ref()
        .is_some_and(|t| super::layout::is_unit(&apply_ty_prune(checker.subst(), t)))
}

/// The `while` shape the builder desugars to: `Loop { Block { If(c, b, Break) } }`.
/// Returns `(cond, body)` so lowering can emit the test at the loop head.
pub fn while_shape(body: &HirBody, loop_body: HirId) -> Option<(HirId, HirId)> {
    let HirKind::Block { stmts, tail: None } = &body.expr(loop_body).kind else {
        return None;
    };
    let [test] = stmts.as_slice() else {
        return None;
    };
    let HirKind::If {
        cond,
        then,
        els: Some(els),
    } = &body.expr(*test).kind
    else {
        return None;
    };
    matches!(body.expr(*els).kind, HirKind::Break).then_some((*cond, *then))
}

/// Payload fields an arm's pattern names, in declaration order: `Some(l)`
/// for a binding, `None` for `_`. `Err` for a pattern that tests more than
/// the outer tag.
pub fn arm_fields(body: &HirBody, pat: &HirPat) -> Result<Vec<Option<super::LocalId>>, &'static str> {
    let field = |p: &HirPat| match p {
        HirPat::Wild => Ok(None),
        // A `()` payload (`Ok(ok)` of the `?` desugaring) binds no word.
        HirPat::Bind(l) if body.local(*l).ty.as_ref().is_some_and(super::layout::is_unit) => Ok(None),
        HirPat::Bind(l) => Ok(Some(*l)),
        _ => Err("pattern-nested"),
    };
    match pat {
        HirPat::Variant { fields, .. } => match fields {
            HirPatFields::Unit => Ok(Vec::new()),
            HirPatFields::Tuple(parts) => parts.iter().map(field).collect(),
            HirPatFields::Record(_) => Err("pattern-record"),
        },
        HirPat::Wild | HirPat::Bind(_) => Ok(Vec::new()),
        HirPat::Int(_) => Err("pattern-int"),
        HirPat::Tuple(_) | HirPat::Record(_) => Err("pattern"),
    }
}

/// A callee the AST calls with a plain `CallIndirect`: a parameter of a
/// monomorphic function type (a rank-n one is a `Forall`), or a `let` of
/// one from another indirect call, never reassigned. A `let` naming a
/// generic function, or a call returning a captured `PolyFn`, boxes its
/// arguments in the AST and stays there.
/// A never-reassigned parameter or `let` local holding a `PolyFn`: its
/// calls box each argument and may pass dictionaries.
pub fn polyfn_callee(body: &HirBody, checker: &Checker, f: HirId) -> bool {
    let HirKind::Local(local) = body.expr(f).kind else {
        return false;
    };
    let info = body.local(local);
    // A parameter is one only when declared `forall` (an open `T -> U`
    // parameter of a shared generic body is the AST's plain closure).
    let ty = info.ty.as_ref().map(|t| apply_ty_prune(checker.subst(), t));
    ty.as_ref().is_some_and(|t| match info.kind {
        LocalKind::Param => matches!(strip_readonly(t), Ty::Forall { .. }),
        // A generic function's returned function the checker saw open at
        // the binding is a `PolyFn` too (`let f = capture_show(0)`),
        // whatever the local settles to (`maybe_record_polyfn_binding`).
        // A closure literal is never one, though its type may keep a
        // variable nothing pinned (`fn () => Ok(2)`'s error type, #1520):
        // it is `MakeFn`'s plain closure word.
        LocalKind::Let => {
            poly_fun(t) && !let_init_is_lambda(body, local)
                || matches!(strip_readonly(t), Ty::Fun(..))
                    && body.exprs.iter().any(|e| match &e.kind {
                        HirKind::Let { local: l, init: Some(init) } if *l == local => {
                            let init = body.expr(*init);
                            matches!(&init.kind, HirKind::Call { callee: Callee::Named { name, .. }, .. } if checker.is_generic_fn(name))
                                && checker.is_polyfn_binding_at(e.span.0, e.span.1)
                        }
                        _ => false,
                    })
        }
        _ => false,
    })
        && !body.exprs.iter().any(|e| {
            matches!(&e.kind, HirKind::Assign { place, .. } if matches!(body.expr(*place).kind, HirKind::Local(l) if l == local))
        })
}

/// A `let` bound to a closure literal (`let f = fn () => ..`).
fn let_init_is_lambda(body: &HirBody, local: LocalId) -> bool {
    body.exprs.iter().any(|e| {
        matches!(&e.kind, HirKind::Let { local: l, init: Some(init) }
            if *l == local && matches!(body.expr(*init).kind, HirKind::Lambda { .. }))
    })
}

pub fn indirect_callee(body: &HirBody, checker: &Checker, f: HirId) -> bool {
    // An element of a `Vec`, array or tuple of monomorphic functions: the
    // closure word it was stored as.
    if let HirKind::Index { .. } = body.expr(f).kind {
        return body.expr(f).ty.as_ref().is_some_and(|t| {
            let t = apply_ty_prune(checker.subst(), t);
            mono_fun(checker, &t, &mut Vec::new())
        });
    }
    // A call's returned closure called in place (#1533), as a `let` of it.
    if let HirKind::Call { .. } = body.expr(f).kind {
        return body.expr(f).ty.as_ref().is_some_and(|t| {
            let t = apply_ty_prune(checker.subst(), t);
            matches!(strip_readonly(&t), Ty::Fun(..)) && fun_words_in(checker, &t, &mut Vec::new(), false)
        }) && returned_closure(body, checker, f);
    }
    let HirKind::Local(local) = body.expr(f).kind else {
        return false;
    };
    let info = body.local(local);
    if !info
        .ty
        .as_ref()
        .is_some_and(|t| {
            let t = apply_ty_prune(checker.subst(), t);
            matches!(strip_readonly(&t), Ty::Fun(..))
                && fun_words_in(checker, &t, &mut Vec::new(), info.kind == LocalKind::Let && let_init_is_lambda(body, local))
        })
    {
        return false;
    }
    let assigned = body.exprs.iter().any(|e| {
        matches!(&e.kind, HirKind::Assign { place, .. } if matches!(body.expr(*place).kind, HirKind::Local(l) if l == local))
    });
    if assigned {
        return false;
    }
    match info.kind {
        // A `for` or destructuring binding of a monomorphic function
        // element: the closure word it was stored as. A captured one is
        // its closure word, as the enclosing body's own call through it
        // would be.
        LocalKind::Param | LocalKind::Pattern | LocalKind::Capture => true,
        LocalKind::Let => body.exprs.iter().any(|e| match &e.kind {
            // A named function read as a value is `MakeFn` (codegen refuses
            // a generic one, whose `MakePolyFn` calls differently).
            HirKind::Let { local: l, init: Some(init) } if *l == local => match &body.expr(*init).kind {
                HirKind::Call { .. } => returned_closure(body, checker, *init),
                HirKind::Global { .. } | HirKind::Lambda { .. } => true,
                // A function field or element: the closure word stored there.
                HirKind::Field { .. } | HirKind::Index { .. } => body.expr(*init).ty.as_ref().is_some_and(|t| {
                    let t = apply_ty_prune(checker.subst(), t);
                    mono_fun(checker, &t, &mut Vec::new())
                }),
                _ => false,
            },
            _ => false,
        }),
        _ => false,
    }
}

/// A call whose result is a plain closure word: a value call through an
/// indirect callee, or a function's or ground method's returned closure.
fn returned_closure(body: &HirBody, checker: &Checker, call: HirId) -> bool {
    match &body.expr(call).kind {
        // A `PolyFn` call's at a ground function type: codegen adapts the
        // shared body's closure to it (#1589).
        HirKind::Call {
            callee: Callee::Value(g), ..
        } => {
            indirect_callee(body, checker, *g)
                || polyfn_callee(body, checker, *g)
                    && body.expr(call).ty.as_ref().is_some_and(|t| {
                        let t = apply_ty_prune(checker.subst(), t);
                        mono_fun(checker, &t, &mut Vec::new())
                    })
        }
        // A non-generic function's returned closure. A generic one's at a
        // ground function type is a plain closure too (#1527): a mono
        // clone's, or the shared body's wrapped in an adapter to that type
        // (#1532). One still open may be a `PolyFn`, called with boxed args.
        HirKind::Call {
            callee: Callee::Named { name, .. },
            args,
        } => {
            !checker.is_overloaded(name)
                && (!checker.is_generic_fn(name)
                    || generic_passes_closure(body, checker, name, args)
                    || body.expr(call).ty.as_ref().is_some_and(|t| {
                        let t = apply_ty_prune(checker.subst(), t);
                        mono_fun(checker, &t, &mut Vec::new())
                    }))
        }
        // A ground method's returned closure (#1007): not a bound or
        // existential dictionary call, and not a generic method.
        HirKind::Call {
            callee: Callee::Method { name },
            args,
        } => {
            let (start, end) = body.expr(call).span;
            let owner = args
                .first()
                .and_then(|r| body.expr(*r).ty.as_ref())
                .map(|t| apply_ty_prune(checker.subst(), t));
            matches!(owner.as_ref().map(strip_readonly), Some(Ty::Con(owner))
                if checker.has_method(owner, name) && !checker.is_generic_fn(&format!("{owner}::{name}")))
                && checker.bound_method_call_for_span(start, end).is_none()
                && checker.existential_method_call_for_span(start, end).is_none()
        }
        _ => false,
    }
}

/// A generic call whose result is one of its arguments as is (#1010): the
/// scheme returns a bare type variable, so the closure that comes back is
/// the caller's own plain closure. A generic function read as an argument
/// could come back as a `PolyFn`, so that stays refused.
fn generic_passes_closure(body: &HirBody, checker: &Checker, name: &str, args: &[HirId]) -> bool {
    checker.fn_param_tys(name).is_some_and(|p| p.len() == args.len())
        && matches!(checker.fn_return_ty(name), Some(Ty::Var(_)))
        && !args.iter().any(|a| match &body.expr(*a).kind {
            HirKind::Global { name, .. } => checker.is_generic_fn(name),
            HirKind::Local(_) => polyfn_callee(body, checker, *a),
            _ => false,
        })
}

/// A match of an `int` on integer literals, or of a scalar-backed enum on
/// its unit variants, closed by `default` or a binding: compare-and-branch
/// arms on the backing word like the AST's scalar match.
pub fn is_scalar_match(checker: &Checker, body: &HirBody, scrutinee: HirId, arms: &[HirArm]) -> bool {
    let Some(ty) = body.expr(scrutinee).ty.as_ref() else {
        return false;
    };
    if primitive(ty) == Some(coil_ty::INT) {
        return arms
            .iter()
            .all(|arm| matches!(arm.pat, HirPat::Int(_) | HirPat::Wild | HirPat::Bind(_)));
    }
    let scalar_enum = match strip_readonly(ty) {
        Ty::Constructor { owner, .. } => matches!(owner.as_ref(), Ty::Con(name) | Ty::Sum { name, .. } if checker.is_scalar_enum(name)),
        Ty::Con(name) | Ty::Sum { name, .. } => checker.is_scalar_enum(name),
        _ => false,
    };
    scalar_enum
        && arms.iter().all(|arm| match &arm.pat {
            HirPat::Wild | HirPat::Bind(_) => true,
            HirPat::Variant {
                enum_name,
                variant,
                fields,
                ..
            } => {
                let unit = match fields {
                    HirPatFields::Unit => true,
                    HirPatFields::Tuple(parts) => parts.is_empty(),
                    HirPatFields::Record(_) => false,
                };
                unit && checker.scalar_for(enum_name, variant).is_some()
            }
            _ => false,
        })
}

/// `Variant(x) => x`: the payload word is the arm's value, so the arm needs
/// no slot.
pub fn is_identity_arm(body: &HirBody, arm: &HirArm) -> bool {
    match &arm.pat {
        HirPat::Variant {
            fields: HirPatFields::Tuple(parts),
            ..
        } => match parts.as_slice() {
            [HirPat::Bind(l)] => {
                !body.local(*l).ty.as_ref().is_some_and(super::layout::is_unit)
                    && matches!(body.expr(arm.body).kind, HirKind::Local(r) if r == *l)
            }
            _ => false,
        },
        _ => false,
    }
}

/// Whether some arm tests a sub-pattern of its payload (a literal or an
/// inner variant): outer-tag dispatch cannot tell such arms apart, so the
/// match tests arm by arm, as the AST's `compile_match_sequential`.
pub fn has_nested_test(arms: &[HirArm]) -> bool {
    let tests = |p: &HirPat| matches!(p, HirPat::Int(_) | HirPat::Variant { .. });
    arms.iter().any(|arm| match &arm.pat {
        HirPat::Variant {
            fields: HirPatFields::Tuple(parts),
            ..
        } => parts.iter().any(tests),
        HirPat::Variant {
            fields: HirPatFields::Record(fields),
            ..
        } => fields.iter().any(|(_, p)| tests(p)),
        _ => false,
    })
}

/// A pattern the arm-by-arm match tests: variants, literals, bindings and
/// `_`, nested to any depth.
pub fn nested_pattern(pat: &HirPat) -> Result<(), &'static str> {
    match pat {
        HirPat::Wild | HirPat::Bind(_) | HirPat::Int(_) => Ok(()),
        HirPat::Variant { fields, .. } => match fields {
            HirPatFields::Unit => Ok(()),
            HirPatFields::Tuple(parts) => parts.iter().try_for_each(nested_pattern),
            HirPatFields::Record(fields) => fields.iter().try_for_each(|(_, p)| nested_pattern(p)),
        },
        HirPat::Tuple(_) | HirPat::Record(_) => Err("pattern-nested"),
    }
}

/// Whether some arm binds a local the arm body reads from a slot (any
/// binding besides an identity arm's): those matches lower with the
/// payload in frame slots.
pub fn match_needs_slots(body: &HirBody, arms: &[HirArm]) -> bool {
    arms.iter().any(|arm| {
        if is_identity_arm(body, arm) {
            return false;
        }
        match &arm.pat {
            HirPat::Bind(_) => true,
            pat => arm_fields(body, pat).is_ok_and(|f| f.iter().any(Option::is_some)),
        }
    })
}

/// `v[i]` with an index that may clobber (`v[f(j)]`): it stages base and
/// index through temps, so it runs with no operand below it.
pub fn staged_read(body: &HirBody, stack: &HashMap<u32, usize>, e: &super::HirExpr) -> bool {
    matches!(e.kind, HirKind::Index { index, kind, .. } if !matches!(kind, IndexKind::String) && clobbers(body, stack, index))
}

/// A stack-array read the AST lowers as a select over the slots (any index
/// but an in-range literal): it stores temps, so it counts as a call for
/// staging (`expr_may_clobber_operand_stack`).
pub fn stack_select(body: &HirBody, stack: &HashMap<u32, usize>, e: &super::HirExpr) -> bool {
    let HirKind::Index { base, index, .. } = e.kind else {
        return false;
    };
    let HirKind::Local(local) = body.expr(base).kind else {
        return false;
    };
    let Some(&n) = stack.get(&local.0) else {
        return false;
    };
    !matches!(body.expr(index).kind, HirKind::Lit(Lit::Int(i)) if (0..n as i64).contains(&i))
}

type Check = Result<(), &'static str>;

struct Walk<'b> {
    body: &'b HirBody,
    checker: &'b Checker,
    loops: u32,
    /// Frame-slot stack arrays: local to length.
    stack: HashMap<u32, usize>,
    /// The operand a [`STAGED`] refusal names: staged into a temp, it lowers.
    stage_at: Option<HirId>,
    /// Block statements an escaping stack array is boxed before.
    box_at: HashMap<u32, Vec<u32>>,
    /// Frame-slot class locals boxed at their escape ([`class_boxes`]).
    class_boxed: std::collections::HashSet<u32>,
}

impl Walk<'_> {
    fn ty(&self, id: HirId) -> Option<&Ty> {
        self.body.expr(id).ty.as_ref()
    }

    fn class(&self, id: HirId) -> Option<ValueClass> {
        self.ty(id).and_then(|ty| classify(self.checker, ty))
    }

    fn scalar(&self, id: HirId) -> Check {
        if self.ty(id).is_some_and(is_scalar) {
            Ok(())
        } else {
            Err("operand-type")
        }
    }

    /// A value a local, argument or payload can hold.
    /// Whether `base` names a frame-slot stack array.
    fn stack_base(&self, base: HirId) -> bool {
        matches!(self.body.expr(base).kind, HirKind::Local(local) if self.stack.contains_key(&local.0))
    }

    fn elementwise(&self, id: HirId) -> bool {
        matches!(
            self.ty(id).map(strip_readonly),
            Some(Ty::Tuple(_) | Ty::Array { .. })
        ) || self.class(id) == Some(ValueClass::Aggregate)
    }

    /// An element-wise `lhs op rhs` / `-lhs`, as the AST's
    /// `try_emit_aggregate_arith` emits it at the top of the stack: heap
    /// operands to temps, then each element (literal items and stack-array
    /// slots read directly) with the results so far below it, then
    /// `MakeTuple` / `MakeArray`; or the packed `HostInvoke`.
    fn aggregate_arith(&mut self, id: HirId, lhs: HirId, rhs: Option<HirId>) -> Check {
        use crate::typechecking::{AggregateArithKind as K, ScalarSide};
        let info = aggregate_info(self.checker, self.body, id).ok_or("operator-elementwise")?;
        self.word(id)?;
        let operands: Vec<HirId> = std::iter::once(lhs).chain(rhs).collect();
        if aggregate_packed(&info).is_some() {
            if !matches!(info.kind, K::NegTuple { .. } | K::NegArray { .. }) && rhs.is_none() {
                return Err("operator-elementwise");
            }
            for &operand in &operands {
                self.word(operand)?;
                self.value(operand)?;
            }
            return Ok(());
        }
        let need_rhs = |rhs: Option<HirId>| rhs.ok_or("operator-elementwise");
        match info.kind {
            // Tuples, and arrays of dynamic length: every operand to a temp.
            K::NegTuple { .. } | K::ZipTuple { .. } | K::BroadcastTuple { .. } | K::NegArray { length: None, .. } | K::BroadcastArray { length: None, .. } => {
                if !matches!(info.kind, K::NegTuple { .. } | K::NegArray { .. }) {
                    need_rhs(rhs)?;
                }
                for &operand in &operands {
                    // A stack array's slots are not boxed for this use.
                    if self.stack_base(operand) {
                        return Err("operator-elementwise");
                    }
                    self.word(operand)?;
                    self.value(operand)?;
                }
                Ok(())
            }
            K::ZipArray { length, .. } => {
                let rhs = need_rhs(rhs)?;
                self.aggregate_src(lhs)?;
                self.aggregate_src(rhs)?;
                for i in 0..length {
                    self.aggregate_elem(lhs, i)?;
                    self.aggregate_elem(rhs, i)?;
                }
                Ok(())
            }
            K::BroadcastArray { length: Some(n), scalar_on, .. } => {
                let rhs = need_rhs(rhs)?;
                let (vec, scalar) = match scalar_on {
                    ScalarSide::Right => (lhs, rhs),
                    ScalarSide::Left => (rhs, lhs),
                };
                self.aggregate_src(vec)?;
                self.scalar(scalar)?;
                self.value(scalar)?;
                for i in 0..n {
                    self.aggregate_elem(vec, i)?;
                }
                Ok(())
            }
            K::NegArray { length: Some(n), .. } => {
                self.aggregate_src(lhs)?;
                for i in 0..n {
                    self.aggregate_elem(lhs, i)?;
                }
                Ok(())
            }
        }
    }

    /// A heap operand runs first into a temp; a direct one later.
    fn aggregate_src(&mut self, id: HirId) -> Check {
        match aggregate_src(self.body, &self.stack, id) {
            Some(_) => Ok(()),
            None => {
                self.word(id)?;
                self.value(id)
            }
        }
    }

    /// Element `i` of `src`: a literal item must not store into frame
    /// slots (the AST compiles it in place).
    fn aggregate_elem(&mut self, src: HirId, i: usize) -> Check {
        match aggregate_src(self.body, &self.stack, src) {
            Some(AggregateSrc::Items(items)) => {
                let &item = items.get(i).ok_or("operator-elementwise")?;
                if clobbers(self.body, &self.stack, item) {
                    return Err("operator-elementwise");
                }
                self.scalar(item)?;
                self.value(item)
            }
            Some(AggregateSrc::Slots(local)) if i < self.stack[&local.0] => Ok(()),
            Some(AggregateSrc::Slots(_)) => Err("operator-elementwise"),
            None => Ok(()),
        }
    }

    fn word(&self, id: HirId) -> Check {
        match self.class(id) {
            Some(class) if is_word(class) => Ok(()),
            // A diverging value (a `match` whose arms all `raise`) never
            // reaches its join, so it fits any word.
            None if self.ty(id).is_some_and(|t| matches!(strip_readonly(t), Ty::Never)) => Ok(()),
            _ => {
                if std::env::var_os("COIL_HIR_WHY").is_some() {
                    eprintln!("    value {:?}: {:?}", self.body.expr(id).kind, self.ty(id));
                }
                Err("value-type")
            }
        }
    }

    fn aggregate(&self, id: HirId) -> Check {
        if self.class(id) == Some(ValueClass::Aggregate) {
            Ok(())
        } else {
            Err("index-base-type")
        }
    }

    /// Each of `items`, in order.
    fn items(&mut self, items: &[HirId]) -> Check {
        for &item in items {
            // A `()` item runs for its effect; the tuple stores the empty
            // tuple object (#1427).
            if is_unit_value(self.body, self.checker, item) {
                self.effect(item)?;
                continue;
            }
            self.word(item)?;
            self.value(item)?;
        }
        Ok(())
    }

    /// A generic class instance: its methods are one shared body.
    fn shared_receiver(&self, id: HirId) -> bool {
        self.class(id) == Some(ValueClass::Opaque)
            && match self.ty(id).map(strip_readonly) {
                Some(Ty::App(head, _)) => {
                    matches!(head.as_ref(), Ty::Con(name) if is_generic_class(self.checker, name))
                }
                // `self` in a shared method body.
                Some(Ty::Con(name)) => is_generic_class(self.checker, name),
                _ => false,
            }
    }

    /// A receiver whose fields are read and written in place: a plain
    /// object, or a generic class instance (its fields typed open in the
    /// class's parameters, as the shared body lays them out).
    /// A record value: fields by name (`GetField` / `SetField`).
    fn record(&self, id: HirId) -> bool {
        matches!(self.ty(id).map(strip_readonly), Some(Ty::Record { .. }))
    }

    fn object(&self, id: HirId) -> Check {
        if self.class(id) == Some(ValueClass::Object) || self.shared_receiver(id) || self.record(id) {
            Ok(())
        } else {
            Err("receiver-type")
        }
    }

    /// `dload(path)`, `declare(lib, name, (T, ..), R)` or
    /// `invoke(lib, f, (a, ..))`, as the AST's `emit_ffi_declare` /
    /// `emit_ffi_invoke`: the value operands in order, a signature's tags
    /// as constants, the argument tuple packed. A variadic `declare` takes
    /// a `bool` literal fifth; a variadic `invoke` passes its arguments' tags.
    fn ffi_call(&mut self, id: HirId, kind: crate::typechecking::FfiBuiltin, args: &[HirId]) -> Check {
        use crate::typechecking::FfiBuiltin;
        let body = self.body;
        if self.class(id).is_none() {
            return Err("call-type");
        }
        match kind {
            FfiBuiltin::Dload => {
                let [path] = args else { return Err("callee-arity") };
                self.args(&[*path])
            }
            FfiBuiltin::Declare => {
                let (lib, name, sig, ret) = match args {
                    [lib, name, sig, ret] => (lib, name, sig, ret),
                    [lib, name, sig, ret, variadic] if matches!(body.expr(*variadic).kind, HirKind::Lit(Lit::Bool(_))) => {
                        (lib, name, sig, ret)
                    }
                    _ => return Err("callee-arity"),
                };
                let items = tuple_items(body, *sig).ok_or("ffi-signature")?;
                if items.iter().chain([ret]).any(|&t| ffi_tag(body, self.checker, t).is_none()) {
                    return Err("ffi-signature");
                }
                self.args(&[*lib, *name])
            }
            FfiBuiltin::Invoke => {
                let [lib, f, values] = args else { return Err("callee-arity") };
                let items = tuple_items(body, *values).ok_or("ffi-arguments")?;
                if !matches!(body.expr(*f).kind, HirKind::Local(_) | HirKind::Field { .. }) {
                    return Err("ffi-function");
                }
                if ffi_fn_variadic(body, self.checker, *f) && ffi_variadic_tags(body, self.checker, id, items).is_none()
                {
                    return Err("callee-variadic");
                }
                self.args(&[*lib, *f])?;
                // A function named as an argument is its code pointer
                // (codegen checks the name is one).
                let values: Vec<HirId> = items.iter().copied().filter(|&a| !matches!(body.expr(a).kind, HirKind::Global { .. })).collect();
                for &a in items {
                    if values.contains(&a) {
                        self.args(&[a])?;
                    }
                }
                Ok(())
            }
        }
    }

    /// Call arguments: plain values (no `name:` or `...`).
    fn args(&mut self, args: &[HirId]) -> Check {
        for &arg in args {
            if matches!(
                self.body.expr(arg).kind,
                HirKind::Named { .. } | HirKind::Spread(_)
            ) {
                return Err("call-argument");
            }
            // A `()` argument runs for its effect; the call passes the
            // empty tuple object (#1086).
            if is_unit_value(self.body, self.checker, arg) {
                self.effect(arg)?;
                continue;
            }
            self.word(arg)?;
            self.value(arg)?;
        }
        Ok(())
    }

    /// `id` for its value.
    fn value(&mut self, id: HirId) -> Check {
        let body = self.body;
        match &body.expr(id).kind {
            HirKind::Lit(Lit::Int(_) | Lit::Float(_) | Lit::Bool(_)) => self.scalar(id),
            HirKind::Lit(Lit::Str(raw)) => {
                if matches!(self.ty(id).map(strip_readonly), Some(Ty::Con(n)) if n == coil_ty::STRING)
                    || (self.ty(id).and_then(primitive) == Some(coil_ty::BYTE) && byte_literal(raw).is_some())
                    || self.ty(id).is_some_and(|t| is_byte_array(t) && is_byte_vec(t))
                {
                    Ok(())
                } else {
                    Err("literal")
                }
            }
            HirKind::Lit(_) => Err("literal"),
            HirKind::Local(_) => self.word(id),
            // A `const` read or a `static let` load: the codegen plan finds
            // which (`Compiler::hir_global_const` / `hir_global_static`).
            HirKind::Global { .. } => self.word(id),
            // An anonymous `fn`: its captures, then `MakeFn` (codegen plans
            // the body).
            HirKind::Lambda { .. } => self.word(id),
            // A matrix operator the checker recorded as linear algebra:
            // the packed kernel's id under the operands, or each operand
            // to a temp (codegen picks, as for the builtin calls).
            HirKind::Bin { lhs, rhs, .. } if linear_algebra(self.checker, body, id) => {
                self.word(id)?;
                for operand in [*lhs, *rhs] {
                    self.word(operand)?;
                    self.value(operand)?;
                }
                Ok(())
            }
            HirKind::Un { operand, .. } if linear_algebra(self.checker, body, id) => {
                self.word(id)?;
                self.word(*operand)?;
                self.value(*operand)
            }
            HirKind::Bin { op, lhs, rhs } => {
                // A user type's operator: a call of its trait instance, or
                // (`==` / `!=` without one) the VM's structural `EQ` /
                // `NEQ`; the codegen plan picks (`Compiler::hir_operator`).
                if let BinOp::Overloaded(sym) = op {
                    // `==` / `!=` on arrays compare structurally (`EQ`), as
                    // the AST; other operators on them are element-wise.
                    if !matches!(*sym, "==" | "!=") && (self.elementwise(*lhs) || self.elementwise(*rhs)) {
                        return self.aggregate_arith(id, *lhs, Some(*rhs));
                    }
                    if !matches!(*sym, "==" | "!=" | "<" | ">" | "<=" | ">=")
                        && crate::typechecking::generics::Generics::operator_trait(sym).is_none()
                    {
                        return Err("operator");
                    }
                    self.word(*lhs)?;
                    self.word(*rhs)?;
                    self.value(*lhs)?;
                    return self.value(*rhs);
                }
                // `a + b` is `FORMAT "%s%s"` over both; `==` / `!=` compare
                // strings with `EQ`.
                let string = |id: HirId| matches!(self.ty(id).map(strip_readonly), Some(Ty::Con(n)) if n == coil_ty::STRING);
                let concat = matches!(op, BinOp::StrConcat);
                if concat || (matches!(op, BinOp::Eq | BinOp::Ne) && string(*lhs)) {
                    if !string(*lhs) || !string(*rhs) {
                        return Err("operand-type");
                    }
                    self.value(*lhs)?;
                    return self.value(*rhs);
                }
                self.scalar(*lhs)?;
                self.scalar(*rhs)?;
                let shift = matches!(op, BinOp::Shl | BinOp::Shr);
                if !shift && self.ty(*lhs) != self.ty(*rhs) {
                    return Err("mixed-operands");
                }
                self.value(*lhs)?;
                self.value(*rhs)
            }
            HirKind::Logic { lhs, rhs, .. } => {
                self.scalar(*lhs)?;
                self.scalar(*rhs)?;
                self.value(*lhs)?;
                self.value(*rhs)
            }
            HirKind::Un { op: UnOp::Neg, operand } if self.elementwise(*operand) => {
                self.aggregate_arith(id, *operand, None)
            }
            // A type parameter's word in a shared body: its `Neg` / `BitNot`
            // dictionary call (`Compiler::hir_operator_at`).
            HirKind::Un { op: UnOp::Neg | UnOp::BitNot, operand } if matches!(self.ty(*operand).map(strip_readonly), Some(Ty::Var(_))) => {
                self.word(*operand)?;
                self.value(*operand)
            }
            // A user type's `-v` / `~v`: a call of its `Neg` / `BitNot`
            // instance.
            HirKind::Un { op: UnOp::Neg | UnOp::BitNot, operand }
                if self.ty(*operand).is_some_and(|t| user_operand(self.checker, t)) =>
            {
                self.word(*operand)?;
                self.value(*operand)
            }
            HirKind::Un { operand, .. } => {
                self.scalar(*operand)?;
                self.value(*operand)
            }
            // Between scalars: one cast opcode (none for a same-type cast).
            // `"ab" as Vec<byte>`: the literal is already its bytes.
            HirKind::Cast { value }
                if matches!(body.expr(*value).kind, HirKind::Lit(Lit::Str(_)))
                    && self.ty(*value).is_some_and(is_byte_array)
                    && self.ty(id).is_some_and(is_byte_vec) =>
            {
                Ok(())
            }
            // The same cast once the literal is built as its `[byte; N]`
            // array: that array, unchanged.
            HirKind::Cast { value }
                if matches!(body.expr(*value).kind, HirKind::Make { kind: MakeKind::Array, .. })
                    && self.ty(*value).is_some_and(is_byte_array)
                    && self.ty(id).is_some_and(|t| is_byte_array(t) || is_byte_vec(t)) =>
            {
                self.value(*value)
            }
            // `v as Vec<byte>` of a `Vec<byte>`: the same array object.
            HirKind::Cast { value } if self.ty(*value).is_some_and(is_byte_vec) && self.ty(id).is_some_and(is_byte_vec) => {
                self.value(*value)
            }
            HirKind::Cast { value } => {
                let from = self.ty(*value).and_then(primitive).ok_or("cast")?;
                let to = self.ty(id).and_then(primitive).ok_or("cast")?;
                if from != to && cast_opcode(from, to).is_none() {
                    return Err("cast");
                }
                self.value(*value)
            }
            HirKind::Call {
                callee: Callee::Method { name },
                args,
            } if name == "len" && args.len() == 1 && structural_len(body, self.checker, args[0]) => self.len(args[0]),
            HirKind::Call { callee, args } if let Some(kind) = ffi_builtin(self.checker, callee) => self.ffi_call(id, kind, args),
            HirKind::Call {
                callee: Callee::Named { .. },
                args,
            } => {
                if self.class(id).is_none() {
                    return Err("call-type");
                }
                // An existential or linear-algebra call keeps its operands
                // on the stack, so a clobbering index read stays refused.
                if index_stages(body, &self.stack, args) && (linear_algebra(self.checker, body, id) || existential_hint(body, self.checker, id)) {
                    return Err("staged-index");
                }
                self.args(args)
            }
            // `recv.m(args)`.
            HirKind::Call {
                callee: Callee::Method { name },
                args,
            } => {
                if self.class(id).is_none() {
                    return Err("call-type");
                }
                let recv = *args.first().ok_or("method-receiver")?;
                if is_vec(body, self.checker, recv) {
                    return self.vec_method(name, args);
                }
                // Any one-word receiver may name a ground trait instance's
                // method; codegen resolves inherent methods on objects only.
                // `().m()`: the unit receiver is an empty tuple object.
                let unit_recv = is_unit_value(body, self.checker, recv);
                if !self.shared_receiver(recv) && self.object(recv).is_err() && !unit_recv {
                    self.word(recv).map_err(|_| "receiver-type")?;
                }
                if unit_recv {
                    self.effect(recv)?;
                    return self.args(&args[1..]);
                }
                self.args(args)
            }
            // `f(args)` through a function-value local: the args, the
            // function word, then `CallIndirect`, as the AST does.
            HirKind::Call {
                callee: Callee::Value(f),
                args,
            } => {
                if !indirect_callee(body, self.checker, *f) && !polyfn_callee(body, self.checker, *f) {
                    return Err("callee-value");
                }
                // A `()` result is the word `CallIndirect` leaves, dropped (#1214).
                if !is_unit_value(body, self.checker, id) {
                    self.word(id)?;
                }
                self.args(args)?;
                self.value(*f)
            }
            // `s[i]`: the `string_byte_at` native id, then string and
            // index above it (codegen checks the byte against `-1`).
            HirKind::Index {
                base,
                index,
                kind: IndexKind::String,
            } => {
                self.word(id)?;
                self.word(*base)?;
                self.scalar(*index)?;
                self.value(*base)?;
                self.value(*index)
            }
            HirKind::Index { base, index, kind } => {
                if !matches!(kind, IndexKind::Array | IndexKind::Tuple) {
                    return Err("index-kind");
                }
                // A tuple's `()` item is read for the base's and index's
                // effects only (#1427).
                if !(*kind == IndexKind::Tuple && is_unit_value(body, self.checker, id)) {
                    self.word(id)?;
                }
                self.aggregate(*base)?;
                self.scalar(*index)?;
                // A slot `LOAD`, or a select over the slots.
                if self.stack_base(*base) {
                    return self.value(*index);
                }
                if matches!(body.expr(*base).kind, HirKind::Make { .. }) {
                    return Err("index-of-literal");
                }
                self.value(*base)?;
                self.value(*index)
            }
            HirKind::Make {
                kind: MakeKind::Tuple | MakeKind::Array,
                args,
            } if !args.is_empty() || matches!(&body.expr(id).kind, HirKind::Make { kind: MakeKind::Array, .. }) => {
                self.word(id)?;
                self.items(args)
            }
            HirKind::Field { base, .. } => {
                // A record's `()` field is read for the base's effects only.
                if !(self.record(*base) && is_unit_value(body, self.checker, id)) {
                    self.word(id)?;
                }
                // A record variant's field reads `LoadField` on the boxed
                // enum (codegen checks the field resolves).
                if self.class(*base) != Some(ValueClass::Enum) {
                    self.object(*base)?;
                }
                self.value(*base)
            }
            // `{a: x, b: y}`: each value then its name, then `MakeDict`.
            HirKind::Make {
                kind: MakeKind::Record(_),
                args,
            } => {
                self.word(id)?;
                for &arg in args {
                    // A `()` value runs for its effect; the record stores
                    // the empty tuple object.
                    if is_unit_value(body, self.checker, arg) {
                        self.effect(arg)?;
                        continue;
                    }
                    self.word(arg)?;
                    self.value(arg)?;
                }
                Ok(())
            }
            // `a..b` over a shared generic body's open `T` (#1230): the
            // typed range object, its bounds the unboxed words.
            HirKind::Make {
                kind: MakeKind::Range { .. },
                args,
            } if self.ty(id).is_some_and(is_open_range) => {
                let [lo, hi] = <[HirId; 2]>::try_from(args.as_slice()).map_err(|_| "make-range")?;
                self.word(lo)?;
                self.word(hi)?;
                self.value(lo)?;
                self.value(hi)
            }
            HirKind::Make {
                kind: MakeKind::Range { .. },
                args,
            } => {
                if !self.ty(id).is_some_and(is_range_pair) {
                    return Err("make-range");
                }
                let [lo, hi] = <[HirId; 2]>::try_from(args.as_slice()).map_err(|_| "make-range")?;
                self.scalar(lo)?;
                self.scalar(hi)?;
                self.value(lo)?;
                self.value(hi)
            }
            HirKind::Make {
                kind: MakeKind::Class(_),
                args,
            } => {
                // A generic class's instance is built like any object, its
                // type-parameter fields boxed.
                if !self.shared_receiver(id) {
                    self.object(id)?;
                }
                self.args(args)
            }
            HirKind::Make {
                kind: MakeKind::Variant { enum_name, variant, .. },
                args,
            } => {
                // A scalar enum's variant pushes its backing constant.
                if args.is_empty() && self.checker.scalar_for(enum_name, variant).is_some() {
                    return self.word(id);
                }
                if self.class(id) != Some(ValueClass::Enum) {
                    return Err("make-type");
                }
                for &arg in args {
                    // `Ok(())`: the emitter checks the layout carries it.
                    if is_unit_make(body, arg) {
                        continue;
                    }
                    // `Ok(check(x)?)`: a `()` payload runs for its effect
                    // (codegen takes it into a niche unit `Result` only).
                    if args.len() == 1 && is_unit_value(body, self.checker, arg) {
                        self.effect(arg)?;
                        continue;
                    }
                    self.word(arg)?;
                    self.value(arg)?;
                }
                Ok(())
            }
            HirKind::Match { scrutinee, arms } => self.match_(id, *scrutinee, arms, true),
            HirKind::If {
                cond,
                then,
                els: Some(els),
            } => {
                self.word(id)?;
                self.scalar(*cond)?;
                self.value(*cond)?;
                self.value(*then)?;
                self.value(*els)
            }
            HirKind::Block {
                stmts,
                tail: Some(tail),
            } => {
                for &s in stmts {
                    self.effect(s)?;
                }
                // An escape boxes before a statement, not a value.
                if self.box_at.contains_key(&tail.0) {
                    return Err("stack-escape-tail");
                }
                self.value(*tail)
            }
            // `yield v` / `yield from h`: the operand, then `YieldCoro` /
            // `YieldFromCoro`; its value is the word the next resume sends.
            HirKind::Yield { value, .. } => {
                self.word(id)?;
                self.word(*value)?;
                self.value(*value)
            }
            // `resume h [with v]`: the sent value, the handle, `ResumeCoro`.
            HirKind::Resume { handle, value } => {
                self.word(id)?;
                if let Some(v) = value {
                    self.word(*v)?;
                    self.value(*v)?;
                }
                self.word(*handle)?;
                self.value(*handle)
            }
            // `readonly e`: `e`'s value.
            HirKind::Builtin {
                op: Builtin::Readonly,
                args,
            } => {
                let [inner] = args.as_slice() else {
                    return Err("builtin");
                };
                self.value(*inner)
            }
            // `UnboxValue` / `BoxValue` / `STRINGIFY` of a dictionary-entry
            // word: the word, then the op.
            HirKind::Builtin {
                op: Builtin::Unbox { .. } | Builtin::Box { .. } | Builtin::Stringify | Builtin::Relayout,
                args,
            } => {
                let [inner] = args.as_slice() else {
                    return Err("builtin");
                };
                self.word(id)?;
                self.value(*inner)
            }
            // `typeof e`: a string constant; `e` is not evaluated.
            HirKind::Builtin {
                op: Builtin::TypeOf,
                args,
            } if args.len() == 1 => Ok(()),
            // `done(h)`: the handle, `DoneCoro`.
            HirKind::Builtin {
                op: Builtin::Done,
                args,
            } => {
                let [handle] = args.as_slice() else {
                    return Err("builtin");
                };
                self.word(*handle)?;
                self.value(*handle)
            }
            // Leaves control flow, so it never pushes on the fall-through path.
            HirKind::Break
            | HirKind::Continue
            | HirKind::Return(_)
            | HirKind::Builtin {
                op: Builtin::Panic, ..
            } => self.effect(id),
            // `x++` / `--x` on an int or float local: one `INC` / `DEC`,
            // which leaves the old or new value, as the AST.
            HirKind::Assign { place, .. } if body.expr(id).flags.contains(HirFlags::ADJUST) => {
                // Any other place splits into statements ahead of this one.
                if !matches!(body.expr(*place).kind, HirKind::Local(_)) || self.stack_base(*place) {
                    self.stage_at = Some(id);
                    return Err("adjust-value");
                }
                match self.ty(id).and_then(primitive) {
                    Some(coil_ty::INT | coil_ty::FLOAT) => Ok(()),
                    _ => Err("assign"),
                }
            }
            // `x = e` / `x op= e` as a value splits into `let t = e; x = t;`
            // ahead of its statement, leaving `t`.
            HirKind::Assign { .. } => {
                self.stage_at = Some(id);
                Err("assign-value")
            }
            _ => Err(kind_name(&body.expr(id).kind)),
        }
    }

    fn match_(
        &mut self,
        id: HirId,
        scrutinee: HirId,
        arms: &[HirArm],
        value: bool,
    ) -> Check {
        if arms.is_empty() {
            return Err("match-empty");
        }
        if has_nested_test(arms) {
            // Arm by arm, with the scrutinee and the payloads an arm opens
            // in frame slots.
            if self.class(scrutinee) != Some(ValueClass::Enum) {
                return Err("match-type");
            }
            for arm in arms {
                nested_pattern(&arm.pat)?;
            }
        } else if !is_scalar_match(self.checker, self.body, scrutinee, arms) {
            if self.class(scrutinee) != Some(ValueClass::Enum) {
                return Err("match-type");
            }
            for arm in arms {
                arm_fields(self.body, &arm.pat)?;
            }
        }
        if value {
            self.word(id)?;
        }
        self.value(scrutinee)?;
        for arm in arms {
            if value {
                self.value(arm.body)?;
            } else {
                self.effect(arm.body)?;
            }
        }
        Ok(())
    }

    /// `id` for its effect.
    fn effect(&mut self, id: HirId) -> Check {
        let body = self.body;
        match &body.expr(id).kind {
            HirKind::Block { stmts, tail } => {
                for &s in stmts {
                    self.effect(s)?;
                }
                match tail {
                    Some(t) => self.effect(*t),
                    None => Ok(()),
                }
            }
            HirKind::Let {
                local,
                init: Some(init),
            } => {
                if is_unit_local(body, self.checker, *local) {
                    return self.effect(*init);
                }
                // `let a = [..]` keeps the elements in frame slots in the AST;
                // only one that is never more than indexed lowers.
                if self.stack.contains_key(&local.0) {
                    // A copy loads the source's slots.
                    let HirKind::Make { args, .. } = &body.expr(*init).kind else {
                        return Ok(());
                    };
                    for &arg in args {
                        self.word(arg)?;
                        self.value(arg)?;
                    }
                    return Ok(());
                }
                // With no escape the fields live in frame slots (as the AST's
                // unboxed class local); an escaping one is an object from the
                // start, unless its fields are written first: the AST stores
                // those into slots and builds the object once, at the escape
                // ([`class_boxes`]).
                if sroa_class(body, self.checker, *init).is_some()
                    && !only_field_base(body, *local)
                    && writes_field_of(body, *local)
                    && !self.class_boxed.contains(&local.0)
                {
                    return Err("class-escape");
                }
                if sroa_local(body, self.checker, &self.class_boxed, *local, *init) {
                    let HirKind::Make { args, .. } = &body.expr(*init).kind else {
                        unreachable!()
                    };
                    self.object(*init)?;
                    return self.args(args);
                }
                self.word(*init)?;
                self.value(*init)
            }
            // A type-only `let x: T;` of a scalar starts at zero.
            HirKind::Let { local, init: None } => match body.local(*local).ty.as_ref().and_then(primitive) {
                Some(coil_ty::INT | coil_ty::FLOAT | coil_ty::BOOL | coil_ty::BYTE) => Ok(()),
                // Any other one-word type starts as a null placeholder that
                // `unassigned_reads` proves is never read (#1273).
                _ if body.local(*local).ty.as_ref().and_then(|ty| classify(self.checker, ty)).is_some_and(|c| is_word(c) && c != ValueClass::Enum) =>
                {
                    Ok(())
                }
                _ => Err("uninitialized-let"),
            },
            HirKind::Clear(_) => Ok(()),
            // `let (a, b) = t` / `let { x } = r`: the value to a temp, then
            // each name read from it (`Index` / `GetField`).
            HirKind::LetPat { pat, init } => {
                if !let_pat_shape(pat) {
                    return Err("let-pattern");
                }
                self.word(*init)?;
                self.value(*init)
            }
            HirKind::Assign { place, value } => {
                let compound = body.expr(id).flags.contains(HirFlags::COMPOUND);
                match &body.expr(*place).kind {
                    // A stack array's slots: the source's, or each item.
                    HirKind::Local(local) if self.stack.contains_key(&local.0) => match &body.expr(*value).kind {
                        HirKind::Make { args, .. } => {
                            for &arg in args {
                                self.word(arg)?;
                                self.value(arg)?;
                            }
                            Ok(())
                        }
                        _ => Ok(()),
                    },
                    HirKind::Local(_) => {
                        self.word(*value)?;
                        self.value(*value)
                    }
                    // `base.f = v`: the value, then the base on top of it.
                    HirKind::Field { base, .. } => {
                        if !read_base(body, *base) {
                            return Err("assign-base");
                        }
                        // A record's `()` field stores the empty tuple
                        // object once the value has run (#1510).
                        if self.record(*base) && is_unit_value(body, self.checker, *value) {
                            self.object(*base)?;
                            self.effect(*value)?;
                            return self.value(*base);
                        }
                        self.word(*place)?;
                        self.object(*base)?;
                        self.word(*value)?;
                        self.value(*value)?;
                        self.value(*base)
                    }
                    // `base[i] = v`: the value to a temp, then base and index.
                    HirKind::Index { base, index, kind } => {
                        if !matches!(kind, IndexKind::Array | IndexKind::Tuple) {
                            return Err("index-kind");
                        }
                        if compound && (!read_base(body, *base) || !pure_index(body, *index)) {
                            // Each index with an effect runs once into a
                            // temp, when the rest of the place repeats (#1266).
                            self.stage_at = Some(id);
                            return Err("assign-index");
                        }
                        self.word(*place)?;
                        self.aggregate(*base)?;
                        self.scalar(*index)?;
                        self.word(*value)?;
                        self.value(*value)?;
                        if !self.stack_base(*base) {
                            self.value(*base)?;
                        }
                        self.value(*index)
                    }
                    // `STATIC = v`: the value, then `StoreStatic`.
                    HirKind::Global { .. } => {
                        self.word(*place)?;
                        self.word(*value)?;
                        self.value(*value)
                    }
                    _ => Err("assign-place"),
                }
            }
            HirKind::If { cond, then, els } => {
                self.scalar(*cond)?;
                self.value(*cond)?;
                self.effect(*then)?;
                match els {
                    Some(e) => self.effect(*e),
                    None => Ok(()),
                }
            }
            HirKind::Loop { body: inner } => {
                self.loops += 1;
                let r = self.effect(*inner);
                self.loops -= 1;
                r
            }
            HirKind::ForIn {
                pat,
                iterable,
                body: inner,
                kind,
            } => {
                // A destructuring pattern binds from each array element.
                let array = matches!(
                    kind,
                    Some(
                        ForInKind::Array
                            | ForInKind::Dict
                            | ForInKind::Custom {
                                counted: Some(ForInCounted::Array | ForInCounted::Dict),
                                ..
                            }
                    )
                );
                if !matches!(pat, HirPat::Bind(_)) && !(array && let_pat_shape(pat)) {
                    return Err("for-in-pattern");
                }
                match kind {
                    Some(ForInKind::Range { .. }) => {
                        let Some(args) = range_bounds(body, *iterable) else {
                            // A range value: its `[start, end]` seeds the latch.
                            if !self.ty(*iterable).is_some_and(is_range_pair) {
                                return Err("for-in-range");
                            }
                            self.value(*iterable)?;
                            self.loops += 1;
                            let r = self.effect(*inner);
                            self.loops -= 1;
                            return r;
                        };
                        for &a in &args {
                            self.scalar(a)?;
                            self.value(a)?;
                        }
                    }
                    Some(ForInKind::Array) => {
                        self.aggregate(*iterable)?;
                        self.value(*iterable)?;
                    }
                    // Its elements to an array, then the array loop.
                    Some(ForInKind::Tuple { .. }) => {
                        if !matches!(self.ty(*iterable).map(strip_readonly), Some(Ty::Tuple(_))) {
                            return Err("for-in-tuple");
                        }
                        self.word(*iterable)?;
                        self.value(*iterable)?;
                    }
                    // `ResumeCoro` / `DoneCoro` on the handle in a temp.
                    Some(ForInKind::Coroutine) => {
                        self.word(*iterable)?;
                        self.value(*iterable)?;
                    }
                    // `DictEntries`, then the array loop over `(key, value)`.
                    Some(ForInKind::Dict) => {
                        self.word(*iterable)?;
                        self.value(*iterable)?;
                    }
                    // A user `into_iter` returning an array, dict or numeric
                    // range: the iterable as is, the `CALL`, then that loop.
                    Some(ForInKind::Custom {
                        counted: Some(ForInCounted::Array | ForInCounted::Dict | ForInCounted::Range { .. }),
                        ..
                    })
                    // Or an iterator: `into_iter`, then `next` until `None`.
                    | Some(ForInKind::Custom {
                        next_fqn: Some(_),
                        counted: None,
                        ..
                    }) => {
                        if self.class(*iterable) == Some(ValueClass::Enum) {
                            return Err("for-in-custom");
                        }
                        self.word(*iterable)?;
                        self.value(*iterable)?;
                    }
                    _ => return Err("for-in"),
                }
                self.loops += 1;
                let r = self.effect(*inner);
                self.loops -= 1;
                r
            }
            HirKind::Break | HirKind::Continue => {
                if self.loops == 0 {
                    return Err("jump");
                }
                Ok(())
            }
            // `PRINT` of a string.
            HirKind::Builtin {
                op: Builtin::Print,
                args,
            } => {
                let [msg] = args.as_slice() else {
                    return Err("builtin");
                };
                self.word(*msg)?;
                self.value(*msg)
            }
            // `panic msg`: the message, then `Panic`.
            HirKind::Builtin {
                op: Builtin::Panic,
                args,
            } => {
                // A second argument (a literal) marks a failed `requires`.
                let (&[msg] | &[msg, _]) = args.as_slice() else {
                    return Err("builtin");
                };
                self.word(msg)?;
                self.value(msg)
            }
            HirKind::Return(value) => match returned_value(body, *value) {
                Some(v) if is_unit_value(body, self.checker, v) => self.effect(v),
                Some(v) => {
                    if self.class(v).is_none() {
                        return Err("return-type");
                    }
                    self.value(v)
                }
                None => Ok(()),
            },
            HirKind::Match { scrutinee, arms } => self.match_(id, *scrutinee, arms, false),
            // `defer`: a thunk each later `return` calls with the captures
            // (`let`s and parameters) as its frame. Control can only fall
            // out of its body.
            HirKind::Defer { captures, body: inner } => {
                for cap in captures {
                    let Some(local) = cap else {
                        return Err("defer-capture");
                    };
                    // A lambda's own capture is a frame slot too (#1137).
                    if !matches!(body.local(*local).kind, LocalKind::Let | LocalKind::Param | LocalKind::Capture)
                        || is_unit_local(body, self.checker, *local)
                        || self.stack.contains_key(&local.0)
                    {
                        return Err("defer-capture");
                    }
                }
                let exits = |e: HirId| {
                    matches!(
                        body.expr(e).kind,
                        HirKind::Return(_)
                            | HirKind::Break
                            | HirKind::Continue
                            | HirKind::Yield { .. }
                            | HirKind::Defer { .. }
                            | HirKind::Lambda { .. }
                    )
                };
                if any_id(body, *inner, &exits) {
                    return Err("defer-body");
                }
                let loops = std::mem::replace(&mut self.loops, 0);
                let checked = self.effect(*inner);
                self.loops = loops;
                checked
            }
            HirKind::Local(local) if is_unit_local(body, self.checker, *local) => Ok(()),
            // A statement `resume` of a `()` coroutine: `ResumeCoro` still
            // pushes a word, which is dropped.
            HirKind::Resume { handle, value } if is_unit_value(body, self.checker, id) => {
                if let Some(v) = value {
                    self.word(*v)?;
                    self.value(*v)?;
                }
                self.word(*handle)?;
                self.value(*handle)
            }
            // A statement `yield` leaves nothing: a send lands only where a
            // receiving `yield` takes it.
            HirKind::Yield { value, .. } => {
                self.word(*value)?;
                self.value(*value)
            }
            // A `()` statement (a unit `match` arm, a nested item) does nothing.
            _ if is_unit_make(body, id) || matches!(body.expr(id).kind, HirKind::Lit(Lit::Unit)) => Ok(()),
            HirKind::Lit(_)
            | HirKind::Local(_)
            | HirKind::Bin { .. }
            | HirKind::Logic { .. }
            | HirKind::Un { .. }
            | HirKind::Cast { .. }
            | HirKind::Make { .. }
            | HirKind::Field { .. }
            | HirKind::Index { .. }
            | HirKind::Call { .. }
            | HirKind::Resume { .. }
            | HirKind::Lambda { .. }
            | HirKind::Builtin {
                op: Builtin::Done | Builtin::Readonly | Builtin::TypeOf | Builtin::Unbox { .. } | Builtin::Box { .. } | Builtin::Stringify | Builtin::Relayout,
                ..
            } => self.value(id),
            other => Err(kind_name(other)),
        }
    }
}

/// Fallback reason for a node kind the lowering does not take.
fn kind_name(kind: &HirKind) -> &'static str {
    match kind {
        HirKind::Lit(_) => "literal",
        HirKind::Local(_) => "local",
        HirKind::Global { .. } => "global",
        HirKind::Field { .. } => "field",
        HirKind::Index { .. } => "index",
        HirKind::Bin { .. } => "operator",
        HirKind::Logic { .. } => "logic",
        HirKind::Un { .. } => "unary",
        HirKind::Cast { .. } => "cast",
        HirKind::Call { .. } => "call",
        HirKind::Named { .. } => "named-argument",
        HirKind::Spread(_) => "spread",
        HirKind::Make { kind, .. } => match kind {
            MakeKind::Range { .. } => "make-range",
            MakeKind::Record(_) => "make-record",
            MakeKind::Tuple => "make-unit",
            _ => "make",
        },
        HirKind::Block { .. } => "block-value",
        HirKind::Let { .. } => "let",
        HirKind::LetPat { .. } => "let-pattern",
        HirKind::Assign { .. } => "assign",
        HirKind::Append { .. } => "append",
        HirKind::If { .. } => "if-value",
        HirKind::Loop { .. } => "loop",
        HirKind::ForIn { .. } => "for-in",
        HirKind::Break | HirKind::Continue => "jump",
        HirKind::Return(_) => "return",
        HirKind::Match { .. } => "match",
        HirKind::Lambda { .. } => "lambda",
        HirKind::Yield { .. } => "yield",
        HirKind::Resume { .. } => "resume",
        HirKind::Defer { .. } => "defer",
        HirKind::Builtin { .. } => "builtin",
        HirKind::Clear(_) => "clear",
        HirKind::Unsupported(_) => "unsupported",
    }
}

#[cfg(test)]
#[path = "lower.tests.rs"]
mod tests;

/// A class or enum operand of a unary operator, which dispatches to the
/// type's trait instance (`impl Neg for V`).
pub(crate) fn user_operand(checker: &Checker, ty: &Ty) -> bool {
    match strip_readonly(ty) {
        Ty::Con(name) | Ty::Sum { name, .. } => checker.is_class(name) || checker.enum_variants(name).is_some(),
        Ty::App(head, _) => matches!(head.as_ref(), Ty::Con(name) if checker.is_class(name)),
        _ => false,
    }
}
