//! Which HIR bodies the lowering in `codegen/emit_hir.rs` takes.
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
//! or body shape keeps the AST codegen for the whole function,
//! and the reason it was refused is counted in `--opt-stats`.
//!
//! The VM shares one stack between locals and operands, so a store to a new
//! slot is only safe with no operand live below it. [`refusal`] therefore
//! also tracks how many operands each node sits on and refuses bindings,
//! assignments, jumps, slot-binding matches and staged variant builds that
//! are not at operand depth zero. A binary operator whose right side calls,
//! matches or builds a variant stages its left side through a temp (as the
//! AST codegen does), so that right side runs at depth zero.

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
        Ty::Tuple(items) if !items.is_empty() => aggregate(checker, items.iter(), seen),
        // A record: one dict word, its fields read and written by name.
        Ty::Record { fields } if !fields.is_empty() => fields
            .iter()
            .all(|(_, f)| classify_in(checker, f, seen).is_some_and(is_word))
            .then_some(ValueClass::Opaque),
        Ty::Array { element, .. } => aggregate(checker, std::iter::once(element.as_ref()), seen),
        // A bare-class existential (`Show x`): one `[value, dictionary]`
        // tuple word, only moved and called through its dictionary.
        Ty::Existential { .. } => Some(ValueClass::Opaque),
        // `Matrix<D>` is its data at run time.
        Ty::App(..) if crate::typechecking::aggregate_arith::unwrap_matrix_ty(ty).is_some() => {
            classify_in(checker, crate::typechecking::aggregate_arith::unwrap_matrix_ty(ty)?, seen)
        }
        Ty::App(..) if coil_ty::vec_element_ty(ty).is_some() => {
            aggregate(checker, coil_ty::vec_element_ty(ty).into_iter(), seen)
        }
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
            for (i, arg) in args.iter().enumerate() {
                match classify_in(checker, arg, seen)? {
                    // `Ok(())` is the only unit payload.
                    ValueClass::Unit if !(result && i == 0) => return None,
                    _ => {}
                }
            }
            params_closed(ty).then_some(ValueClass::Enum)
        }
        Ty::Sum { name, variants } => {
            if common::is_builtin_option_enum(name) || common::is_builtin_result_enum(name) {
                for (variant, payload) in variants {
                    for field in payload.field_types() {
                        match classify_in(checker, field, seen)? {
                            ValueClass::Unit if variant != "Ok" => return None,
                            _ => {}
                        }
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
                super::layout::ty_is_closed(t) && classify_in(checker, t, seen) == Some(ValueClass::Enum)
            };
            (unit(param) || plain(param, seen)) && (unit(ret) || plain(ret, seen) || closed_enum(ret, seen))
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
        Ty::List(inner) | Ty::Constructor { owner: inner, .. } => params_closed(inner),
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
/// parameters (elements are classified where they are read).
fn aggregate<'t>(checker: &Checker, mut items: impl Iterator<Item = &'t Ty>, seen: &mut Vec<String>) -> Option<ValueClass> {
    items
        .all(|t| {
            params_closed(t) && classify_in(checker, t, seen).is_none_or(is_word)
        })
        .then_some(ValueClass::Aggregate)
}

/// A class declared with type parameters (`class HashMap<K, V>`).
pub fn is_generic_class(checker: &Checker, name: &str) -> bool {
    if !checker.is_class(name) {
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
        .all(|(_, ty)| super::layout::ty_is_closed(ty))
        .then_some(ValueClass::Object)
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
    // `len(a)` of a fixed-length array folds to a constant (`hir_len_call`),
    // so its argument reads no slots.
    let folded_len: HashSet<u32> = body
        .exprs
        .iter()
        .filter_map(|e| match &e.kind {
            HirKind::Call {
                callee: Callee::Named { name, .. } | Callee::Method { name },
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

/// `len(x)` / `x.len()` of a plain local: `ArrayLen`, or a constant for a
/// fixed-size type.
impl Walk<'_> {
    /// A `Vec` method: `push` is inlined as `ArrayPush` (its value staged
    /// with the receiver when it may clobber); the others are `CALL`s to
    /// the builtin thunks. `pop` / `remove` are `HostInvoke`s when the call's
    /// `Option` is niche-packed (codegen refuses the boxed form).
    fn vec_method(&mut self, name: &str, args: &[HirId], depth: u32) -> Check {
        match (name, args) {
            ("push", [recv, value]) => {
                let staged = push_stages(self.body, &self.stack, *value);
                if staged && depth != 0 {
                    return Err("staged-push");
                }
                self.value(*recv, depth)?;
                self.word(*value)?;
                self.value(*value, if staged { 0 } else { depth + 1 })
            }
            ("len" | "capacity" | "clear", [_]) | ("reserve", [_, _]) | ("insert", [_, _, _]) => {
                self.args(args, depth, depth == 0)
            }
            // The host native's id goes under the arguments.
            ("pop", [_]) | ("remove", [_, _]) => self.args(args, depth + 1, false),
            _ => Err("vec-method"),
        }
    }

    fn len(&mut self, arg: HirId, depth: u32) -> Check {
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
            HirKind::Call { .. } | HirKind::Field { .. } | HirKind::Index { .. } | HirKind::Global { .. } if structural => {
                self.word(arg)?;
                self.value(arg, depth)
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

/// Whether `len(x)` dispatches to a user `Length` instance: `x` has a
/// known type that is not structural, so it is a plain call.
pub fn user_len(body: &HirBody, checker: &Checker, arg: HirId) -> bool {
    body.expr(arg).ty.as_ref().is_some_and(|t| {
        let t = apply_ty_prune(checker.subst(), t);
        super::layout::ty_is_closed(&t) && !Checker::is_structural_len_ty_for_codegen(&t)
    })
}

/// Whether `x.len()` is the structural `len(x)` (not a `Vec` method).
pub fn structural_len(body: &HirBody, checker: &Checker, recv: HirId) -> bool {
    body.expr(recv).ty.as_ref().is_some_and(|t| {
        let t = apply_ty_prune(checker.subst(), t);
        Checker::is_structural_len_ty_for_codegen(&t) && coil_ty::vec_element_ty(&t).is_none()
    })
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
                | HirKind::Builtin { .. }
                | HirKind::Resume { .. }
                | HirKind::Yield { .. }
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

/// A `Vec` push stages its receiver and value through temps when the value
/// may clobber or is a boxed variant make that stages its own arguments
/// (more than one, not all literals or locals).
pub fn push_stages(body: &HirBody, stack: &HashMap<u32, usize>, value: HirId) -> bool {
    clobbers(body, stack, value)
        || matches!(&body.expr(value).kind, HirKind::Make { kind: MakeKind::Variant { .. }, args }
            if args.len() > 1 && !args.iter().all(|&a| matches!(body.expr(a).kind, HirKind::Lit(_) | HirKind::Local(_))))
}

/// Whether a call at `depth` stages its arguments through temps: at the
/// top of the stack, when one of them builds an object or applies an
/// operator through temps (a user type's instance, an aggregate), or one
/// after the first holds a `match` (`?`, `??`), which binds slots with no
/// operand below it.
pub fn stages_args(body: &HirBody, checker: &Checker, args: &[HirId], depth: u32) -> bool {
    depth == 0
        && args.iter().enumerate().any(|(i, &arg)| {
            let mut found = false;
            visit(body, arg, &mut |e| {
                found |= matches!(
                    &e.kind,
                    HirKind::Make { kind: MakeKind::Class(_), .. } | HirKind::Bin { op: BinOp::Overloaded(_), .. }
                ) || (i != 0 && matches!(&e.kind, HirKind::Match { .. }))
                    || shows_through_temps(body, checker, e)
                    || staged_make(body, e);
            });
            found
        })
}

/// Whether a call at the top of the stack stages its arguments because one
/// after the first holds a clobbering index read ([`staged_read`]), which
/// a live argument would sit under (the AST stages every argument when
/// one may clobber).
pub fn index_stages(body: &HirBody, stack: &HashMap<u32, usize>, args: &[HirId], depth: u32) -> bool {
    depth == 0
        && args.iter().skip(1).any(|&arg| {
            let mut found = false;
            visit(body, arg, &mut |e| found |= staged_read(body, stack, e));
            found
        })
}

/// A trait method call on an existential pack that is not a local: the
/// pack goes to a temp, which needs an empty operand stack below it (a
/// local pack is just loaded once per use).
pub fn existential_staged(body: &HirBody, checker: &Checker, id: HirId) -> bool {
    let e = body.expr(id);
    let HirKind::Call { args, .. } = &e.kind else {
        return false;
    };
    existential_hint(body, checker, id) && !args.first().is_some_and(|&a| matches!(body.expr(a).kind, HirKind::Local(_)))
}

/// Whether the checker recorded the call at `id` as a trait method on an
/// existential pack.
pub fn existential_hint(body: &HirBody, checker: &Checker, id: HirId) -> bool {
    let e = body.expr(id);
    e.node.and_then(|n| checker.existential_method_call_at(n)).is_some()
        || checker.existential_method_call_span(e.span.0, e.span.1).is_some()
}

/// A variant make whose several arguments are not all literals or locals:
/// it stages them through temps in source order, which need an empty
/// operand stack below them.
pub fn staged_make(body: &HirBody, e: &super::HirExpr) -> bool {
    let HirKind::Make {
        kind: MakeKind::Variant { .. },
        args,
    } = &e.kind
    else {
        return false;
    };
    args.len() > 1 && !args.iter().all(|&a| matches!(body.expr(a).kind, HirKind::Lit(_) | HirKind::Local(_)))
}

/// Whether `e` is a `format` call with a tuple or record argument: `%v`
/// shows it field by field through temps, which need an empty operand
/// stack below them, so the call stages its arguments and runs at depth
/// zero (the AST stages around every call).
pub fn shows_through_temps(body: &HirBody, checker: &Checker, e: &super::HirExpr) -> bool {
    use crate::typechecking::StringBuiltin;
    let HirKind::Call {
        callee: Callee::Named { name, .. },
        args,
    } = &e.kind
    else {
        return false;
    };
    let format = checker
        .string_fn_in_scope(name)
        .or_else(|| name.strip_prefix("string::").and_then(StringBuiltin::from_name));
    matches!(format, Some(StringBuiltin::Format))
        && args.iter().skip(1).any(|&a| {
            body.expr(a).ty.as_ref().is_some_and(|t| {
                matches!(strip_readonly(&apply_ty_prune(checker.subst(), t)), Ty::Tuple(_) | Ty::Record { .. })
            })
        })
}

/// Every node of `id`'s subtree, `id` first.
pub(crate) fn visit(body: &HirBody, id: HirId, f: &mut impl FnMut(&super::HirExpr)) {
    f(body.expr(id));
    for k in children(body, id) {
        visit(body, k, f);
    }
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
        // `x / k` and `x % k` trap only on a zero divisor.
        HirKind::Bin {
            op: BinOp::IntDiv | BinOp::IntRem,
            lhs,
            rhs,
        } => matches!(body.expr(*rhs).kind, HirKind::Lit(Lit::Int(k)) if k != 0) && logic_eager(body, *lhs),
        HirKind::Bin { op, lhs, rhs } => {
            !matches!(op, BinOp::IntPow | BinOp::StrConcat | BinOp::Overloaded(_))
                && logic_eager(body, *lhs)
                && logic_eager(body, *rhs)
        }
        HirKind::Logic { lhs, rhs, .. } => logic_eager(body, *lhs) && logic_eager(body, *rhs),
        HirKind::Un { operand, .. } => logic_eager(body, *operand),
        HirKind::Field { base, .. } => logic_eager(body, *base),
        _ => false,
    }
}

/// An index that is safe to evaluate twice (a compound assignment builds
/// its place twice): locals, literals and arithmetic on them.
fn pure_index(body: &HirBody, id: HirId) -> bool {
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
fn pure_base(body: &HirBody, id: HirId) -> bool {
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
            super::layout::ty_is_closed(field)
                && classify_in(checker, field, seen).is_some_and(is_word)
        })
    });
    seen.pop();
    ok.then_some(ValueClass::Enum)
}

/// `enum_name::variant`'s payload types at a ground instance of a generic
/// user enum (`Tree<int>`): the declared types with its parameters bound.
pub fn generic_enum_payload(checker: &Checker, enum_name: &str, variant: &str, args: &[Ty]) -> Option<Vec<Ty>> {
    let params = checker.generics().generic_type_ctors.get(enum_name)?;
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
                .all(|field| params_closed(field) && classify_in(checker, field, seen).is_some_and(is_word))
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

/// An irrefutable `let` pattern of tuples, records, names and `_`.
pub fn let_pat_shape(pat: &HirPat) -> bool {
    match pat {
        HirPat::Wild | HirPat::Bind(_) => true,
        HirPat::Tuple(items) => items.iter().all(let_pat_shape),
        HirPat::Record(fields) => fields.iter().all(|(_, p)| let_pat_shape(p)),
        HirPat::Int(_) | HirPat::Variant { .. } => false,
    }
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
    if !(0..=8).contains(&count) || has_own_jump(hir, body) || has_lambda(hir, body) {
        return None;
    }
    Some((*s, count as u32))
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

/// Whether `id` is a `block_on(h)` call (the prelude's, not a user `fn`).
pub fn block_on(body: &HirBody, checker: &Checker, id: HirId) -> bool {
    matches!(
        &body.expr(id).kind,
        HirKind::Call { callee: Callee::Named { name, .. }, args }
            if args.len() == 1
                && checker.prelude_fn_in_scope(name) == Some(crate::typechecking::PreludeFn::BlockOn)
    )
}

/// Depth refusals [`refusal_at`] names an operand for.
const STAGED: &[&str] = &["stack-select-depth", "adjust-value"];

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
        class_box_at: std::collections::HashSet::new(),
        stage_at: None,
    };
    let boxes = class_boxes(body, checker);
    walk.class_box_at = boxes.keys().copied().collect();
    walk.class_boxed = boxes.into_values().flatten().collect();
    let stacks = stack_arrays(body, checker);
    walk.stack = stacks.len;
    walk.box_at = stacks.box_at;
    walk.effect(root, 0)
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

/// The primitive name of a scalar type, as the cast opcodes key it.
/// `s as [byte]` / `s as Vec<byte>` on a string: the AST's `to_bytes` call.
pub fn string_to_bytes(checker: &Checker, from: &Ty, to: &Ty) -> bool {
    let from = apply_ty_prune(checker.subst(), from);
    let to = apply_ty_prune(checker.subst(), to);
    matches!(strip_readonly(&from), Ty::Con(s) if s == coil_ty::STRING)
        && matches!(
            strip_readonly(&to),
            Ty::Array { element, length: coil_ty::ArrayLength::Dynamic, .. }
                if matches!(element.as_ref(), Ty::Con(n) if n == coil_ty::BYTE)
        )
}

/// `[byte]` or `Vec<byte>`: a growable byte array.
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

/// `[byte; N]` or `[byte]`: a string literal typed so is its bytes.
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

/// `float` (the lane picks the `*F` opcodes).
pub fn is_float(ty: &Ty) -> bool {
    matches!(strip_readonly(ty), Ty::Con(n) if n == coil_ty::FLOAT)
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
/// calls box each argument and may pass dictionaries
/// (`Compiler::hir_polyfn_call`).
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
        LocalKind::Let => {
            poly_fun(t)
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

pub fn indirect_callee(body: &HirBody, checker: &Checker, f: HirId) -> bool {
    let HirKind::Local(local) = body.expr(f).kind else {
        return false;
    };
    let info = body.local(local);
    if !info
        .ty
        .as_ref()
        .is_some_and(|t| {
            let t = apply_ty_prune(checker.subst(), t);
            matches!(strip_readonly(&t), Ty::Fun(..)) && fun_words(checker, &t, &mut Vec::new())
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
        LocalKind::Param => true,
        LocalKind::Let => body.exprs.iter().any(|e| match &e.kind {
            // A named function read as a value is `MakeFn` (codegen refuses
            // a generic one, whose `MakePolyFn` calls differently).
            HirKind::Let { local: l, init: Some(init) } if *l == local => match &body.expr(*init).kind {
                HirKind::Call {
                    callee: Callee::Value(g), ..
                } => indirect_callee(body, checker, *g),
                // A non-generic function's returned closure (a generic one
                // may return a `PolyFn`, called with boxed args).
                HirKind::Call {
                    callee: Callee::Named { name, .. },
                    ..
                } => !checker.is_generic_fn(name) && !checker.is_overloaded(name),
                HirKind::Global { .. } | HirKind::Lambda { .. } => true,
                _ => false,
            },
            _ => false,
        }),
        _ => false,
    }
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
/// binding besides an identity arm's). Those matches lower with the
/// payload in frame slots, so they must start at operand depth zero.
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

/// A binary operator at depth zero stages its left operand through a temp
/// when the right one calls, matches or builds a variant, so the right one
/// runs with no operand below it (as the AST codegen does): it can then
/// inline, bind payload slots and stage args.
pub fn stages_rhs(body: &HirBody, stack: &HashMap<u32, usize>, rhs: HirId) -> bool {
    match &body.expr(rhs).kind {
        HirKind::Call { .. } | HirKind::Match { .. } | HirKind::Make { .. } => true,
        // Through field and index reads, as `expr_may_clobber_operand_stack`.
        HirKind::Index { base, index, .. } => {
            stack_select(body, stack, body.expr(rhs)) || stages_rhs(body, stack, *base) || stages_rhs(body, stack, *index)
        }
        HirKind::Field { base, .. } => stages_rhs(body, stack, *base),
        // An operator on a user type or aggregate stages through temps.
        HirKind::Bin {
            op: BinOp::Overloaded(_), ..
        } => true,
        HirKind::Bin { lhs, rhs, .. } | HirKind::Logic { lhs, rhs, .. } => {
            stages_rhs(body, stack, *lhs) || stages_rhs(body, stack, *rhs)
        }
        HirKind::Un { operand, .. } | HirKind::Cast { value: operand } => stages_rhs(body, stack, *operand),
        _ => false,
    }
}

/// String `a + b` at depth zero stages both operands through temps when
/// either holds a `match` (`?`, `??`) or a clobbering index read
/// ([`staged_read`]): it then runs with no operand below it, as the AST
/// does (`arg_emits_on_self_bytecode`).
pub fn concat_stages(body: &HirBody, stack: &HashMap<u32, usize>, lhs: HirId, rhs: HirId) -> bool {
    let mut found = false;
    for id in [lhs, rhs] {
        visit(body, id, &mut |e| found |= matches!(e.kind, HirKind::Match { .. }) || staged_read(body, stack, e));
    }
    found
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

fn rhs_depth(body: &HirBody, stack: &HashMap<u32, usize>, rhs: HirId, depth: u32) -> u32 {
    if depth == 0 && stages_rhs(body, stack, rhs) { 0 } else { depth + 1 }
}

type Check = Result<(), &'static str>;

struct Walk<'b> {
    body: &'b HirBody,
    checker: &'b Checker,
    loops: u32,
    /// Frame-slot stack arrays: local to length.
    stack: HashMap<u32, usize>,
    /// The operand a depth refusal names: staged into a temp, it lowers.
    stage_at: Option<HirId>,
    /// Block statements an escaping stack array is boxed before.
    box_at: HashMap<u32, Vec<u32>>,
    /// Frame-slot class locals boxed at their escape ([`class_boxes`]).
    class_boxed: std::collections::HashSet<u32>,
    /// The statements (or block tails) [`class_boxes`] boxes before.
    class_box_at: std::collections::HashSet<u32>,
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
            Some(Ty::Tuple(_) | Ty::Array { .. } | Ty::List(_))
        ) || self.class(id) == Some(ValueClass::Aggregate)
    }

    /// An element-wise `lhs op rhs` / `-lhs`, as the AST's
    /// `try_emit_aggregate_arith` emits it at the top of the stack: heap
    /// operands to temps, then each element (literal items and stack-array
    /// slots read directly) with the results so far below it, then
    /// `MakeTuple` / `MakeArray`; or the packed `HostInvoke`.
    fn aggregate_arith(&mut self, id: HirId, lhs: HirId, rhs: Option<HirId>, depth: u32) -> Check {
        use crate::typechecking::{AggregateArithKind as K, ScalarSide};
        let info = aggregate_info(self.checker, self.body, id).ok_or("operator-elementwise")?;
        if depth != 0 {
            return Err("operator-depth");
        }
        self.word(id)?;
        let operands: Vec<HirId> = std::iter::once(lhs).chain(rhs).collect();
        if aggregate_packed(&info).is_some() {
            if !matches!(info.kind, K::NegTuple { .. } | K::NegArray { .. }) && rhs.is_none() {
                return Err("operator-elementwise");
            }
            for (i, &operand) in operands.iter().enumerate() {
                self.word(operand)?;
                self.value(operand, 1 + i as u32)?;
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
                    self.value(operand, 0)?;
                }
                Ok(())
            }
            K::ZipArray { length, .. } => {
                let rhs = need_rhs(rhs)?;
                self.aggregate_src(lhs)?;
                self.aggregate_src(rhs)?;
                for i in 0..length {
                    self.aggregate_elem(lhs, i, i as u32)?;
                    self.aggregate_elem(rhs, i, i as u32 + 1)?;
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
                self.value(scalar, 0)?;
                let first = u32::from(matches!(scalar_on, ScalarSide::Left));
                for i in 0..n {
                    self.aggregate_elem(vec, i, i as u32 + first)?;
                }
                Ok(())
            }
            K::NegArray { length: Some(n), .. } => {
                self.aggregate_src(lhs)?;
                for i in 0..n {
                    self.aggregate_elem(lhs, i, i as u32)?;
                }
                Ok(())
            }
        }
    }

    /// A heap operand runs at depth zero into a temp; a direct one later.
    fn aggregate_src(&mut self, id: HirId) -> Check {
        match aggregate_src(self.body, &self.stack, id) {
            Some(_) => Ok(()),
            None => {
                self.word(id)?;
                self.value(id, 0)
            }
        }
    }

    /// Element `i` of `src`, pushed on `depth` results: a literal item must
    /// not store into frame slots (the AST compiles it in place).
    fn aggregate_elem(&mut self, src: HirId, i: usize, depth: u32) -> Check {
        match aggregate_src(self.body, &self.stack, src) {
            Some(AggregateSrc::Items(items)) => {
                let &item = items.get(i).ok_or("operator-elementwise")?;
                if clobbers(self.body, &self.stack, item) {
                    return Err("operator-elementwise");
                }
                self.scalar(item)?;
                self.value(item, depth)
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

    /// `items[i]` at `depth`, each pushed on the ones before it, or every
    /// item staged through a temp when a later one may clobber (as the
    /// AST's `emit_literal_items`).
    fn items(&mut self, items: &[HirId], depth: u32) -> Check {
        let staged = items.len() >= 2 && items[1..].iter().any(|&i| clobbers(self.body, &self.stack, i));
        if staged && depth != 0 {
            return Err("staged-literal");
        }
        for (i, &item) in items.iter().enumerate() {
            self.word(item)?;
            self.value(item, if staged { 0 } else { depth + i as u32 })?;
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

    /// Call arguments: plain values (no `name:` or `...`), each staged at
    /// depth zero when `staged`, else pushed on the ones before it.
    /// `dload(path)`, `declare(lib, name, (T, ..), R)` or
    /// `invoke(lib, f, (a, ..))`, as the AST's `emit_ffi_declare` /
    /// `emit_ffi_invoke`: the value operands in order, a signature's tags
    /// as constants, the argument tuple packed. A variadic `declare` or
    /// `invoke` and a function passed as a callback stay on the AST.
    fn ffi_call(&mut self, id: HirId, kind: crate::typechecking::FfiBuiltin, args: &[HirId], depth: u32) -> Check {
        use crate::typechecking::FfiBuiltin;
        let body = self.body;
        if self.class(id).is_none() {
            return Err("call-type");
        }
        match kind {
            FfiBuiltin::Dload => {
                let [path] = args else { return Err("callee-arity") };
                self.args(&[*path], depth, false)
            }
            FfiBuiltin::Declare => {
                let [lib, name, sig, ret] = args else { return Err("callee-arity") };
                let items = tuple_items(body, *sig).ok_or("ffi-signature")?;
                if items.iter().chain([ret]).any(|&t| ffi_tag(body, self.checker, t).is_none()) {
                    return Err("ffi-signature");
                }
                self.args(&[*lib, *name], depth, false)
            }
            FfiBuiltin::Invoke => {
                let [lib, f, values] = args else { return Err("callee-arity") };
                let items = tuple_items(body, *values).ok_or("ffi-arguments")?;
                let HirKind::Local(local) = body.expr(*f).kind else {
                    return Err("ffi-function");
                };
                if self.checker.ffi_fn_id_variadic(&body.local(local).name).unwrap_or(false) {
                    return Err("callee-variadic");
                }
                self.args(&[*lib, *f], depth, false)?;
                // A function named as an argument is its code pointer
                // (codegen checks the name is one).
                let values: Vec<HirId> = items.iter().copied().filter(|&a| !matches!(body.expr(a).kind, HirKind::Global { .. })).collect();
                for (i, &a) in items.iter().enumerate() {
                    if values.contains(&a) {
                        self.args(&[a], depth + 2 + i as u32, false)?;
                    }
                }
                Ok(())
            }
        }
    }

    fn args(&mut self, args: &[HirId], depth: u32, staged: bool) -> Check {
        for (i, &arg) in args.iter().enumerate() {
            if matches!(
                self.body.expr(arg).kind,
                HirKind::Named { .. } | HirKind::Spread(_)
            ) {
                return Err("call-argument");
            }
            self.word(arg)?;
            self.value(arg, if staged { 0 } else { depth + i as u32 })?;
        }
        Ok(())
    }

    /// `id` pushes its value on top of `depth` live operands.
    fn value(&mut self, id: HirId, depth: u32) -> Check {
        let body = self.body;
        // A numeric range moves as `[start, end]` or a boxed object; the
        // re-encodings between them stage through temps, so a range value
        // runs with no operand below it.
        if depth != 0 && self.ty(id).is_some_and(is_range_pair) {
            return Err("range-depth");
        }
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
                if depth != 0 {
                    return Err("operator-depth");
                }
                self.word(id)?;
                for (i, &operand) in [*lhs, *rhs].iter().enumerate() {
                    self.word(operand)?;
                    self.value(operand, 1 + i as u32)?;
                }
                Ok(())
            }
            HirKind::Un { operand, .. } if linear_algebra(self.checker, body, id) => {
                if depth != 0 {
                    return Err("operator-depth");
                }
                self.word(id)?;
                self.word(*operand)?;
                self.value(*operand, 1)
            }
            HirKind::Bin { op, lhs, rhs } => {
                // A user type's operator: a call of its trait instance, or
                // (`==` / `!=` without one) the VM's structural `EQ` /
                // `NEQ`; the codegen plan picks (`Compiler::hir_operator`).
                // Either stages through temps, so it runs at depth zero.
                if let BinOp::Overloaded(sym) = op {
                    // `==` / `!=` on arrays compare structurally (`EQ`), as
                    // the AST; other operators on them are element-wise.
                    if !matches!(*sym, "==" | "!=") && (self.elementwise(*lhs) || self.elementwise(*rhs)) {
                        return self.aggregate_arith(id, *lhs, Some(*rhs), depth);
                    }
                    if !matches!(*sym, "==" | "!=" | "<" | ">" | "<=" | ">=" | "+" | "-" | "*" | "/") {
                        return Err("operator");
                    }
                    if depth != 0 {
                        return Err("operator-depth");
                    }
                    self.word(*lhs)?;
                    self.word(*rhs)?;
                    self.value(*lhs, 0)?;
                    return self.value(*rhs, rhs_depth(body, &self.stack, *rhs, 0));
                }
                // `a + b` is `FORMAT "%s%s"` over both (the format string
                // sits under them); `==` / `!=` compare strings with `EQ`.
                let string = |id: HirId| matches!(self.ty(id).map(strip_readonly), Some(Ty::Con(n)) if n == coil_ty::STRING);
                let concat = matches!(op, BinOp::StrConcat);
                if concat || (matches!(op, BinOp::Eq | BinOp::Ne) && string(*lhs)) {
                    if !string(*lhs) || !string(*rhs) {
                        return Err("operand-type");
                    }
                    if concat && depth == 0 && concat_stages(body, &self.stack, *lhs, *rhs) {
                        self.value(*lhs, 0)?;
                        return self.value(*rhs, 0);
                    }
                    let base = depth + u32::from(concat);
                    self.value(*lhs, base)?;
                    return self.value(*rhs, rhs_depth(body, &self.stack, *rhs, base));
                }
                self.scalar(*lhs)?;
                self.scalar(*rhs)?;
                let shift = matches!(op, BinOp::Shl | BinOp::Shr);
                if !shift && self.ty(*lhs) != self.ty(*rhs) {
                    return Err("mixed-operands");
                }
                self.value(*lhs, depth)?;
                self.value(*rhs, rhs_depth(body, &self.stack, *rhs, depth))
            }
            HirKind::Logic { lhs, rhs, .. } => {
                self.scalar(*lhs)?;
                self.scalar(*rhs)?;
                self.value(*lhs, depth)?;
                if logic_eager(body, *rhs) {
                    self.value(*rhs, rhs_depth(body, &self.stack, *rhs, depth))
                } else {
                    // Short-circuit: the jump consumes `lhs` before `rhs` runs.
                    self.value(*rhs, depth)
                }
            }
            HirKind::Un { op: UnOp::Neg, operand } if self.elementwise(*operand) => {
                self.aggregate_arith(id, *operand, None, depth)
            }
            HirKind::Un { operand, .. } => {
                self.scalar(*operand)?;
                self.value(*operand, depth)
            }
            // Between scalars: one cast opcode (none for a same-type cast).
            // `s as [byte]`: the `to_bytes` native id, then the string.
            HirKind::Cast { value }
                if self
                    .ty(*value)
                    .zip(self.ty(id))
                    .is_some_and(|(from, to)| string_to_bytes(self.checker, from, to)) =>
            {
                self.value(*value, depth + 1)
            }
            // `"ab" as [byte]`: the literal is already its bytes.
            HirKind::Cast { value }
                if matches!(body.expr(*value).kind, HirKind::Lit(Lit::Str(_)))
                    && self.ty(*value).is_some_and(is_byte_array)
                    && self.ty(id).is_some_and(is_byte_vec) =>
            {
                Ok(())
            }
            // `"ab" as Vec<byte>` of a literal typed `[byte; N]`: its bytes'
            // array, unchanged.
            HirKind::Cast { value }
                if matches!(body.expr(*value).kind, HirKind::Make { kind: MakeKind::Array, .. })
                    && self.ty(*value).is_some_and(is_byte_array)
                    && self.ty(id).is_some_and(|t| is_byte_array(t) || is_byte_vec(t)) =>
            {
                self.value(*value, depth)
            }
            // `v as [byte]` of a `Vec<byte>` (or back): one array object,
            // passed through as the AST does.
            HirKind::Cast { value } if self.ty(*value).is_some_and(is_byte_vec) && self.ty(id).is_some_and(is_byte_vec) => {
                self.value(*value, depth)
            }
            HirKind::Cast { value } => {
                let from = self.ty(*value).and_then(primitive).ok_or("cast")?;
                let to = self.ty(id).and_then(primitive).ok_or("cast")?;
                if from != to && cast_opcode(from, to).is_none() {
                    return Err("cast");
                }
                self.value(*value, depth)
            }
            HirKind::Call {
                callee: Callee::Named { name, .. },
                args,
            } if name == "len" && args.len() == 1 && !user_len(body, self.checker, args[0]) => self.len(args[0], depth),
            HirKind::Call {
                callee: Callee::Method { name },
                args,
            } if name == "len" && args.len() == 1 && structural_len(body, self.checker, args[0]) => self.len(args[0], depth),
            HirKind::Call { callee, args } if let Some(kind) = ffi_builtin(self.checker, callee) => self.ffi_call(id, kind, args, depth),
            HirKind::Call {
                callee: Callee::Named { .. },
                args,
            } => {
                if self.class(id).is_none() {
                    return Err("call-type");
                }
                // A `new` argument leaves its object in a temp on top of the
                // stack, so every argument stages through a temp, as the AST
                // does when one may clobber the operand stack.
                if depth != 0 && existential_staged(body, self.checker, id) {
                    return Err("existential-depth");
                }
                let shows = shows_through_temps(body, self.checker, body.expr(id));
                if shows && depth != 0 {
                    return Err("format-show");
                }
                // `block_on(h)` drives `h` through two temps, which a live
                // operand would sit under.
                if depth != 0 && block_on(body, self.checker, id) {
                    return Err("block-on-depth");
                }
                // An existential or linear-algebra call keeps its operands
                // on the stack, so a clobbering index read stays refused.
                let indexed = index_stages(body, &self.stack, args, depth);
                if indexed && (linear_algebra(self.checker, body, id) || existential_hint(body, self.checker, id)) {
                    return Err("staged-index");
                }
                self.args(args, depth, shows || indexed || stages_args(body, self.checker, args, depth))
            }
            // `recv.m(args)` stages the receiver and each argument through
            // temps at depth zero, as the AST codegen does.
            HirKind::Call {
                callee: Callee::Method { name },
                args,
            } => {
                if self.class(id).is_none() {
                    return Err("call-type");
                }
                if depth != 0 && existential_staged(body, self.checker, id) {
                    return Err("existential-depth");
                }
                let recv = *args.first().ok_or("method-receiver")?;
                if is_vec(body, self.checker, recv) {
                    return self.vec_method(name, args, depth);
                }
                // Any one-word receiver may name a ground trait instance's
                // method; codegen resolves inherent methods on objects only.
                // `().m()`: the unit receiver is an empty tuple object.
                if !self.shared_receiver(recv) && self.object(recv).is_err() && !is_unit_make(body, recv) {
                    self.word(recv).map_err(|_| "receiver-type")?;
                }
                if is_unit_make(body, recv) {
                    return self.args(&args[1..], depth + 1, depth == 0);
                }
                self.args(args, depth, depth == 0)
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
                self.word(id)?;
                self.args(args, depth, false)?;
                self.value(*f, depth + args.len() as u32)
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
                self.value(*base, depth + 1)?;
                self.value(*index, depth + 2)
            }
            HirKind::Index { base, index, kind } => {
                if !matches!(kind, IndexKind::Array | IndexKind::Tuple) {
                    return Err("index-kind");
                }
                self.word(id)?;
                self.aggregate(*base)?;
                self.scalar(*index)?;
                if self.stack_base(*base) {
                    // A slot `LOAD`, or the index to a temp and a select,
                    // which runs with no operand below it.
                    if depth != 0 && stack_select(body, &self.stack, body.expr(id)) {
                        self.stage_at = Some(id);
                        return Err("stack-select-depth");
                    }
                    // Once boxed: the box, then the index above it.
                    self.value(*index, depth + 1)?;
                    return self.value(*index, depth);
                }
                if matches!(body.expr(*base).kind, HirKind::Make { .. }) {
                    return Err("index-of-literal");
                }
                // A clobbering index stages base and index through temps.
                let staged = clobbers(body, &self.stack, *index);
                if staged && depth != 0 {
                    return Err("staged-index");
                }
                self.value(*base, depth)?;
                self.value(*index, if staged { 0 } else { depth + 1 })
            }
            HirKind::Make {
                kind: MakeKind::Tuple | MakeKind::Array,
                args,
            } if !args.is_empty() || matches!(&body.expr(id).kind, HirKind::Make { kind: MakeKind::Array, .. }) => {
                self.word(id)?;
                self.items(args, depth)
            }
            HirKind::Field { base, .. } => {
                self.word(id)?;
                // A record variant's field reads `LoadField` on the boxed
                // enum (codegen checks the field resolves).
                if self.class(*base) != Some(ValueClass::Enum) {
                    self.object(*base)?;
                }
                // `new C(..).f` stages each argument through a temp and
                // reads the field's (codegen), or builds the object, which
                // runs at depth zero either way.
                if depth != 0 && matches!(body.expr(*base).kind, HirKind::Make { .. }) {
                    return Err("field-of-new");
                }
                self.value(*base, depth)
            }
            // `{a: x, b: y}`: each value then its name, then `MakeDict`.
            HirKind::Make {
                kind: MakeKind::Record(_),
                args,
            } => {
                self.word(id)?;
                for (i, &arg) in args.iter().enumerate() {
                    self.word(arg)?;
                    self.value(arg, depth + 2 * i as u32)?;
                }
                Ok(())
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
                self.value(lo, 0)?;
                self.value(hi, 1)
            }
            HirKind::Make {
                kind: MakeKind::Class(_),
                args,
            } => {
                // The object stays in a temp that must be the top of stack.
                if depth != 0 {
                    return Err("nested-new");
                }
                // A generic class's instance is built like any object, its
                // type-parameter fields boxed.
                if !self.shared_receiver(id) {
                    self.object(id)?;
                }
                self.args(args, 0, true)
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
                // A boxed make stages complex args through temps in source
                // order; those `STORE`s need no operands below them.
                let staged = staged_make(body, body.expr(id));
                if depth != 0 && staged {
                    return Err("staged-make");
                }
                for (i, &arg) in args.iter().enumerate() {
                    // `Ok(())`: the emitter checks the layout carries it.
                    if is_unit_make(body, arg) {
                        continue;
                    }
                    // `Ok(check(x)?)`: a `()` payload runs for its effect
                    // (codegen takes it into a niche unit `Result` only).
                    if depth == 0 && args.len() == 1 && is_unit_value(body, self.checker, arg) {
                        self.effect(arg, 0)?;
                        continue;
                    }
                    self.word(arg)?;
                    // Staged args each run at depth zero into a temp.
                    self.value(arg, if staged { 0 } else { depth + i as u32 })?;
                }
                Ok(())
            }
            HirKind::Match { scrutinee, arms } => self.match_(id, *scrutinee, arms, depth, true),
            HirKind::If {
                cond,
                then,
                els: Some(els),
            } => {
                self.word(id)?;
                self.scalar(*cond)?;
                self.value(*cond, depth)?;
                self.value(*then, depth)?;
                self.value(*els, depth)
            }
            HirKind::Block {
                stmts,
                tail: Some(tail),
            } => {
                for &s in stmts {
                    self.effect(s, depth)?;
                }
                // An escape boxes before a statement, not a value.
                if self.box_at.contains_key(&tail.0) {
                    return Err("stack-escape-tail");
                }
                // A class local boxes before a value tail as before a
                // statement: through a temp, so only on an empty stack.
                if self.class_box_at.contains(&tail.0) && depth != 0 {
                    return Err("class-escape-tail");
                }
                self.value(*tail, depth)
            }
            // `yield v` / `yield from h`: the operand, then `YieldCoro` /
            // `YieldFromCoro`; its value is the word the next resume sends.
            HirKind::Yield { value, .. } => {
                self.word(id)?;
                self.word(*value)?;
                self.value(*value, depth)
            }
            // `resume h [with v]`: the sent value, the handle, `ResumeCoro`.
            HirKind::Resume { handle, value } => {
                self.word(id)?;
                if let Some(v) = value {
                    self.word(*v)?;
                    self.value(*v, depth)?;
                }
                self.word(*handle)?;
                self.value(*handle, depth + u32::from(value.is_some()))
            }
            // `readonly e`: `e`'s value.
            HirKind::Builtin {
                op: Builtin::Readonly,
                args,
            } => {
                let [inner] = args.as_slice() else {
                    return Err("builtin");
                };
                self.value(*inner, depth)
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
                self.value(*handle, depth)
            }
            // Leaves control flow, so it never pushes on the fall-through path.
            HirKind::Break
            | HirKind::Continue
            | HirKind::Return(_)
            | HirKind::Builtin {
                op: Builtin::Panic, ..
            } => self.effect(id, depth),
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
            _ => Err(kind_name(&body.expr(id).kind)),
        }
    }

    fn match_(
        &mut self,
        id: HirId,
        scrutinee: HirId,
        arms: &[HirArm],
        depth: u32,
        value: bool,
    ) -> Check {
        if arms.is_empty() {
            return Err("match-empty");
        }
        if has_nested_test(arms) {
            // Arm by arm, with the scrutinee and the payloads an arm opens
            // in frame slots: only on an empty operand stack.
            if self.class(scrutinee) != Some(ValueClass::Enum) {
                return Err("match-type");
            }
            if value {
                self.word(id)?;
            }
            if depth != 0 {
                return Err("nested-match");
            }
            for arm in arms {
                nested_pattern(&arm.pat)?;
            }
            self.value(scrutinee, 0)?;
            for arm in arms {
                if value {
                    self.value(arm.body, 0)?;
                } else {
                    self.effect(arm.body, 0)?;
                }
            }
            return Ok(());
        }
        if is_scalar_match(self.checker, self.body, scrutinee, arms) {
            if value {
                self.word(id)?;
            }
            // A binding arm stores the scrutinee into a fresh slot.
            if depth != 0 && arms.iter().any(|a| matches!(a.pat, HirPat::Bind(_))) {
                return Err("nested-match");
            }
            self.value(scrutinee, depth)?;
            for arm in arms {
                if value {
                    self.value(arm.body, depth)?;
                } else {
                    self.effect(arm.body, depth)?;
                }
            }
            return Ok(());
        }
        if self.class(scrutinee) != Some(ValueClass::Enum) {
            return Err("match-type");
        }
        if value {
            self.word(id)?;
        }
        for arm in arms {
            arm_fields(self.body, &arm.pat)?;
        }
        if depth != 0 && match_needs_slots(self.body, arms) {
            return Err("nested-match");
        }
        self.value(scrutinee, depth)?;
        for arm in arms {
            if value {
                self.value(arm.body, depth)?;
            } else {
                self.effect(arm.body, depth)?;
            }
        }
        Ok(())
    }

    /// `id` runs for its effect and leaves the operand stack as it found it.
    fn effect(&mut self, id: HirId, depth: u32) -> Check {
        let body = self.body;
        match &body.expr(id).kind {
            HirKind::Block { stmts, tail } => {
                for &s in stmts {
                    self.effect(s, depth)?;
                }
                match tail {
                    Some(t) => self.effect(*t, depth),
                    None => Ok(()),
                }
            }
            HirKind::Let {
                local,
                init: Some(init),
            } => {
                if depth != 0 {
                    return Err("nested-let");
                }
                if is_unit_local(body, self.checker, *local) {
                    return self.effect(*init, depth);
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
                        self.value(arg, 0)?;
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
                    return self.args(args, 0, true);
                }
                self.word(*init)?;
                self.value(*init, depth)
            }
            HirKind::Let { init: None, .. } => Err("uninitialized-let"),
            // `let (a, b) = t` / `let { x } = r`: the value to a temp, then
            // each name read from it (`Index` / `GetField`).
            HirKind::LetPat { pat, init } => {
                if depth != 0 {
                    return Err("nested-let");
                }
                if !let_pat_shape(pat) {
                    return Err("let-pattern");
                }
                self.word(*init)?;
                self.value(*init, 0)
            }
            HirKind::Assign { place, value } => {
                if depth != 0 {
                    return Err("nested-assign");
                }
                let compound = body.expr(id).flags.contains(HirFlags::COMPOUND);
                match &body.expr(*place).kind {
                    // A stack array's slots: the source's, or each item.
                    HirKind::Local(local) if self.stack.contains_key(&local.0) => match &body.expr(*value).kind {
                        HirKind::Make { args, .. } => {
                            for &arg in args {
                                self.word(arg)?;
                                self.value(arg, 0)?;
                            }
                            Ok(())
                        }
                        _ => Ok(()),
                    },
                    HirKind::Local(_) => {
                        self.word(*value)?;
                        self.value(*value, depth)
                    }
                    // `base.f = v`: the value, then the base on top of it.
                    HirKind::Field { base, .. } => {
                        if !read_base(body, *base) {
                            return Err("assign-base");
                        }
                        self.word(*place)?;
                        self.object(*base)?;
                        self.word(*value)?;
                        self.value(*value, depth)?;
                        self.value(*base, depth + 1)
                    }
                    // `base[i] = v`: the value to a temp, then base and index.
                    HirKind::Index { base, index, kind } => {
                        if !matches!(kind, IndexKind::Array | IndexKind::Tuple) {
                            return Err("index-kind");
                        }
                        if compound && (!read_base(body, *base) || !pure_index(body, *index)) {
                            return Err("assign-base");
                        }
                        self.word(*place)?;
                        self.aggregate(*base)?;
                        self.scalar(*index)?;
                        self.word(*value)?;
                        self.value(*value, 0)?;
                        if self.stack_base(*base) {
                            // The value to a temp, then the index (above
                            // the box once boxed).
                            self.value(*index, 1)?;
                            return self.value(*index, 0);
                        }
                        // Codegen stages base and index through temps unless
                        // the index is a local or an int literal.
                        let bare = matches!(body.expr(*index).kind, HirKind::Local(_) | HirKind::Lit(Lit::Int(_)));
                        self.value(*base, 0)?;
                        self.value(*index, u32::from(bare))
                    }
                    // `STATIC = v`: the value, then `StoreStatic`.
                    HirKind::Global { .. } => {
                        self.word(*place)?;
                        self.word(*value)?;
                        self.value(*value, 0)
                    }
                    _ => Err("assign-place"),
                }
            }
            HirKind::If { cond, then, els } => {
                self.scalar(*cond)?;
                self.value(*cond, depth)?;
                self.effect(*then, depth)?;
                match els {
                    Some(e) => self.effect(*e, depth),
                    None => Ok(()),
                }
            }
            HirKind::Loop { body: inner } => {
                if depth != 0 {
                    return Err("nested-loop");
                }
                self.loops += 1;
                let r = self.effect(*inner, depth);
                self.loops -= 1;
                r
            }
            HirKind::ForIn {
                pat,
                iterable,
                body: inner,
                kind,
            } => {
                if depth != 0 {
                    return Err("nested-loop");
                }
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
                            self.value(*iterable, 0)?;
                            self.loops += 1;
                            let r = self.effect(*inner, depth);
                            self.loops -= 1;
                            return r;
                        };
                        for &a in &args {
                            self.scalar(a)?;
                            self.value(a, 0)?;
                        }
                    }
                    Some(ForInKind::Array) => {
                        self.aggregate(*iterable)?;
                        self.value(*iterable, 0)?;
                    }
                    // Its elements to an array, then the array loop.
                    Some(ForInKind::Tuple { .. }) => {
                        if !matches!(self.ty(*iterable).map(strip_readonly), Some(Ty::Tuple(_))) {
                            return Err("for-in-tuple");
                        }
                        self.word(*iterable)?;
                        self.value(*iterable, 0)?;
                    }
                    // `ResumeCoro` / `DoneCoro` on the handle in a temp.
                    Some(ForInKind::Coroutine) => {
                        self.word(*iterable)?;
                        self.value(*iterable, 0)?;
                    }
                    // `DictEntries`, then the array loop over `(key, value)`.
                    Some(ForInKind::Dict) => {
                        self.word(*iterable)?;
                        self.value(*iterable, 0)?;
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
                        self.value(*iterable, 0)?;
                    }
                    _ => return Err("for-in"),
                }
                self.loops += 1;
                let r = self.effect(*inner, depth);
                self.loops -= 1;
                r
            }
            HirKind::Break | HirKind::Continue => {
                if depth != 0 || self.loops == 0 {
                    return Err("jump");
                }
                Ok(())
            }
            // `panic msg`: the message, then `Panic`.
            HirKind::Builtin {
                op: Builtin::Panic,
                args,
            } => {
                if depth != 0 {
                    return Err("nested-panic");
                }
                let [msg] = args.as_slice() else {
                    return Err("builtin");
                };
                self.word(*msg)?;
                self.value(*msg, 0)
            }
            HirKind::Return(value) => {
                if depth != 0 {
                    return Err("nested-return");
                }
                match returned_value(body, *value) {
                    Some(v) if is_unit_value(body, self.checker, v) => self.effect(v, 0),
                    Some(v) => {
                        if self.class(v).is_none() {
                            return Err("return-type");
                        }
                        self.value(v, depth)
                    }
                    None => Ok(()),
                }
            }
            HirKind::Match { scrutinee, arms } => self.match_(id, *scrutinee, arms, depth, false),
            // `defer`: a thunk each later `return` calls with the captures
            // (`let`s and parameters) as its frame. Control can only fall
            // out of its body.
            HirKind::Defer { captures, body: inner } => {
                if depth != 0 {
                    return Err("nested-defer");
                }
                for cap in captures {
                    let Some(local) = cap else {
                        return Err("defer-capture");
                    };
                    if !matches!(body.local(*local).kind, LocalKind::Let | LocalKind::Param)
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
                let checked = self.effect(*inner, 0);
                self.loops = loops;
                checked
            }
            HirKind::Local(local) if is_unit_local(body, self.checker, *local) => Ok(()),
            // A statement `yield` leaves nothing: a send lands only where a
            // receiving `yield` takes it.
            HirKind::Yield { value, .. } => {
                self.word(*value)?;
                self.value(*value, depth)
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
                op: Builtin::Done | Builtin::Readonly | Builtin::TypeOf,
                ..
            } => self.value(id, depth),
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
            MakeKind::List => "make-list",
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
        HirKind::Unsupported(_) => "unsupported",
    }
}

#[cfg(test)]
#[path = "lower.tests.rs"]
mod tests;
