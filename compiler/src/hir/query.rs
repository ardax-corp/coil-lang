//! Queries over HIR bodies that HIR planning and MIR lowering share:
//! frame-slot locals (stack arrays, scalar-replaced classes), element-wise
//! aggregates, staging, `match` shapes, function values and FFI tags.

use super::{BinOp, Builtin, Callee, HirArm, HirBody, HirFlags, HirId, HirKind, HirPat, HirPatFields, IndexKind, Lit, LocalId, LocalKind, MakeKind, UnOp};
use std::collections::HashMap;
use crate::typechecking::infer::Checker;
use crate::typechecking::subst::apply_ty_prune;
use crate::typechecking::ty::{self as coil_ty, Ty, strip_readonly};
use crate::typechecking::AggregateArithInfo;
use super::classify::{fun_words_in, mono_fun, open_fun, poly_fun, primitive};

/// The class `let x = new C(..)` keeps in frame slots instead of a heap
/// object: no `fn drop()` and 1 to 32 fields, none of them a `()` (which no
/// slot holds as a word).
pub fn sroa_class(body: &HirBody, checker: &Checker, init: HirId) -> Option<String> {
    let HirKind::Make {
        kind: MakeKind::Class(name),
        args,
    } = &body.expr(init).kind
    else {
        return None;
    };
    if args.iter().any(|&a| is_unit_value(body, checker, a)) {
        return None;
    }
    let key = checker.resolve_class_key(name)?;
    let n = checker.class_fields(&key)?.len();
    // A flat two-word field takes two slots in the object (its tail), which
    // frame slots of one word per field do not model.
    let flat = checker.class_flat_tails(&key).iter().any(Option::is_some);
    (!checker.class_has_drop(&key) && !flat && (1..=32).contains(&n)).then_some(key)
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

/// `let a = [e1, .., en]` locals kept in `n` frame slots (a multi-slot
/// stack array).
#[derive(Default)]
pub struct StackArrays {
    /// Local to its length.
    pub len: HashMap<u32, usize>,
    /// A block statement to the locals whose slots are boxed into one
    /// array object just before it: the first statement after the `let`
    /// that uses the local other than through an index (its escape). From
    /// there on the local is that object (the hoisted Q1 box).
    pub box_at: HashMap<u32, Vec<u32>>,
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
    // An element-wise operand reads the slots unless the op takes the
    // packed path, which
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
    // closure or handle word has no slot type (#1331). So does a `()`,
    // which no local holds as a word.
    let no_slot = |e: &Ty| {
        let e = crate::typechecking::ty::strip_readonly(e);
        matches!(e, Ty::Fun(..)) || Checker::coroutine_parts(e).is_some() || crate::layout::is_unit(e)
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
    // A dynamic-length (`Vec`) element-wise operand is read by the loop
    // over the array object (`aggregate_dynamic`), never from slots
    // (#2584): its local stays the array object.
    let dynamic = |id: HirId| {
        body.expr(id).ty.as_ref().is_some_and(|t| match crate::typechecking::ty::strip_readonly(&apply_ty_prune(checker.subst(), t)) {
            Ty::Array { length, .. } => !matches!(length, crate::typechecking::ty::ArrayLength::Static(_)),
            t => crate::typechecking::ty::vec_element_ty(t).is_some(),
        })
    };
    for &id in slot_reads.keys() {
        if let HirKind::Local(local) = body.expr(HirId(id)).kind
            && dynamic(HirId(id))
        {
            refused.insert(local.0);
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
                    .find(|&s| body.any(s, &|id| uses.contains(&id.0)));
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
/// It is boxed into one object just before that statement (the hoisted Q2
/// box) and is that object from there on. Maps the statement to
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
            // A captured local is never kept in frame slots
            // ([`sroa_local`]), so it has no box point either.
            if sroa_class(body, checker, init).is_none()
                || body.local(local).kind != LocalKind::Let
                || body.local(local).captured
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
            if let Some(&first) = stmts[k + 1..].iter().chain(tail).find(|&&s| body.any(s, &whole)) {
                out.entry(first.0).or_default().push(local.0);
            }
        }
    }
    out
}

pub(super) fn reassigned(body: &HirBody, local: LocalId) -> bool {
    body.exprs.iter().any(|e| match e.kind {
        HirKind::Assign { place, .. } => body.expr(place).kind == HirKind::Local(local),
        _ => false,
    })
}

/// Whether `let local = init` keeps its fields in frame slots: a
/// [`sroa_class`] that is only a field base, or one [`class_boxes`] boxes.
pub fn sroa_local(body: &HirBody, checker: &Checker, boxed: &std::collections::HashSet<u32>, local: LocalId, init: HirId) -> bool {
    !body.local(local).captured
        // A local of a bare trait type holds its init's pack, which no
        // field slot does (#2744).
        && !body.local(local).ty.as_ref().is_some_and(|t| matches!(crate::typechecking::ty::strip_readonly(&apply_ty_prune(checker.subst(), t)), Ty::Existential { .. }))
        && sroa_class(body, checker, init).is_some()
        && (only_field_base(body, local) || boxed.contains(&local.0))
}

pub(super) fn writes_field_of(body: &HirBody, local: LocalId) -> bool {
    body.exprs.iter().any(|e| match e.kind {
        HirKind::Assign { place, .. } => matches!(body.expr(place).kind,
            HirKind::Field { base, .. } if body.expr(base).kind == HirKind::Local(local)),
        _ => false,
    })
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

/// Whether evaluating `id` may store into frame slots: a call, `match`,
/// `new` or string index anywhere inside it.
pub fn clobbers(body: &HirBody, stack: &HashMap<u32, usize>, id: HirId) -> bool {
    let mut found = false;
    body.visit(id, &mut |e| {
        found |= stack_select(body, stack, e) || matches!(
            &e.kind,
            HirKind::Call { .. }
                | HirKind::Match { .. }
                | HirKind::Make { kind: MakeKind::Class(_), .. }
                | HirKind::Index { kind: IndexKind::String, .. }
                | HirKind::Resume { .. }
                | HirKind::Yield { .. }
        ) || matches!(
            // Boxing ops of compiler-made bodies store into no frame slot.
            &e.kind,
            HirKind::Builtin { op, .. } if !matches!(op, Builtin::Retype | Builtin::Stringify { .. } | Builtin::Relayout | Builtin::Cow { .. })
        );
    });
    found
}

/// Whether the checker recorded `id` as a linear-algebra op (a matrix or
/// vector operator, or `dot` / `matmul` / ..).
pub fn linear_algebra(checker: &Checker, body: &HirBody, id: HirId) -> bool {
    let e = body.expr(id);
    let (start, end) = e.span;
    e.node.is_some_and(|n| checker.linear_algebra_at(n).is_some()) || checker.linear_algebra_for_span(start, end).is_some()
}

/// The checker's element-wise plan for an aggregate `a op b` / `-a` at `id`;
/// `None` for a linear-algebra op ([`linear_algebra`]).
pub fn aggregate_info(checker: &Checker, body: &HirBody, id: HirId) -> Option<AggregateArithInfo> {
    let e = body.expr(id);
    let (start, end) = e.span;
    if linear_algebra(checker, body, id) {
        return None;
    }
    let mut info = e
        .node
        .and_then(|n| checker.aggregate_arith_at(n))
        .or_else(|| checker.aggregate_arith_for_span(start, end))
        .cloned()
        .or_else(|| recover_aggregate(checker, body, id))?;
    // A mono clone keeps its generic body's nodes, so the checker's plan has
    // the element of the signature (`T`): the clone's types name the word.
    if let Some(elem) = aggregate_elem(checker, body, id) {
        info.kind.elem_is_float = crate::typechecking::aggregate_arith::elem_is_float(&elem);
        info.kind.elem_is_byte = crate::typechecking::aggregate_arith::elem_is_byte(&elem);
    }
    Some(info)
}

/// The ground element type of the element-wise op at `id`: of an operand,
/// else of the result (a named type, a generic one at ground arguments, or
/// a ground tuple or array of a nested aggregate, #2544). `None` while it
/// is an open type parameter.
pub fn aggregate_elem(checker: &Checker, body: &HirBody, id: HirId) -> Option<Ty> {
    use crate::typechecking::aggregate_arith::homogeneous_aggregate_elem;
    let operands = match body.expr(id).kind {
        HirKind::Bin { lhs, rhs, .. } => vec![lhs, rhs, id],
        HirKind::Un { operand, .. } => vec![operand, id],
        _ => return None,
    };
    operands
        .into_iter()
        .filter_map(|x| body.expr(x).ty.as_ref())
        .filter_map(|t| homogeneous_aggregate_elem(strip_readonly(&apply_ty_prune(checker.subst(), t))))
        .find(|elem| matches!(elem, Ty::Con(_)) || (matches!(elem, Ty::App(..) | Ty::Tuple(_) | Ty::Array { .. }) && !elem.has_var()))
}

/// The element-wise shape from the operand types where the checker kept
/// none (a mono clone of a generic body): only for operands it types
/// itself, locals and literals.
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

/// How an element-wise operand's elements are read: a non-empty array or
/// tuple literal's
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

/// The packed `HostInvoke` path: a static
/// shape of at least 8 elements, for `+ - * /` and negation.
pub fn aggregate_packed(info: &AggregateArithInfo) -> Option<usize> {
    use crate::typechecking::AggregateOp;
    if matches!(info.op, AggregateOp::Mod | AggregateOp::Pow) {
        return None;
    }
    let len = info.kind.shape.static_len()?;
    (8..=u16::MAX as usize).contains(&len).then_some(len)
}

/// Whether an argument after the first holds a clobbering index read
/// ([`staged_read`]): every argument is then staged into a temp first, so
/// an earlier one is not read after the clobber.
pub fn index_stages(body: &HirBody, stack: &HashMap<u32, usize>, args: &[HirId]) -> bool {
    args.iter().skip(1).any(|&arg| {
        let mut found = false;
        body.visit(arg, &mut |e| found |= staged_read(body, stack, e));
        found
    })
}

/// Whether the checker recorded the call at `id` as a trait method on an
/// existential pack.
pub fn existential_hint(body: &HirBody, checker: &Checker, id: HirId) -> bool {
    let e = body.expr(id);
    e.node.and_then(|n| checker.existential_method_call_at(n)).is_some()
        || checker.existential_method_call_for_span(e.span.0, e.span.1).is_some()
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

/// An arm that always returns or fails ([`super::cold::fails`]: a panic,
/// with or without statements before it), with no loop, closure, `defer`,
/// coroutine step or `break` / `continue` in it: lowering may lay it out
/// after the body as a cold early exit.
pub(crate) fn cold_exit(body: &HirBody, id: HirId) -> bool {
    plain(body, id) && (exits(body, id) || super::cold::fails(body, id, &|_| false))
}

/// No loop, closure, `defer`, coroutine step or `break` / `continue` in
/// `id`'s subtree.
pub(crate) fn plain(body: &HirBody, id: HirId) -> bool {
    let mut plain = true;
    body.visit(id, &mut |e| {
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
    plain
}

/// `a && b` / `a || b` may evaluate `b` eagerly (one `AND` / `OR`, no
/// branch) when `b` reads only locals, literals and fields with no trap and
/// no effect. Anything else short-circuits.
pub fn logic_eager(body: &HirBody, rhs: HirId) -> bool {
    // Every node of `rhs` must be eager: a worklist, since a chain of
    // `&&` is as deep as it is long.
    let mut work = vec![rhs];
    while let Some(id) = work.pop() {
        match &body.expr(id).kind {
            HirKind::Lit(_) | HirKind::Local(_) => {}
            // `x / k` and `x % k` trap only on a zero divisor, or `MIN / -1`.
            HirKind::Bin {
                op: BinOp::IntDiv | BinOp::IntRem,
                lhs,
                rhs,
            } => {
                if !matches!(body.expr(*rhs).kind, HirKind::Lit(Lit::Int(k)) if k != 0 && k != -1) {
                    return false;
                }
                work.push(*lhs);
            }
            // Other int arithmetic can overflow: `x != MAX && x + 1 > y` must
            // not compute `x + 1`.
            HirKind::Bin { op, lhs, rhs } => {
                if super::op_may_trap(body, id) || matches!(op, BinOp::StrConcat | BinOp::Overloaded(_)) {
                    return false;
                }
                work.extend([*rhs, *lhs]);
            }
            HirKind::Logic { lhs, rhs, .. } => work.extend([*rhs, *lhs]),
            HirKind::Un { operand, .. } => {
                if super::op_may_trap(body, id) {
                    return false;
                }
                work.push(*operand);
            }
            HirKind::Field { base, .. } => work.push(*base),
            _ => return false,
        }
    }
    true
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

/// A field store's base that only reads: `x`, a static `G`, `x.f`, or
/// `x[i]` of such a base with a [`pure_index`] (`xs[0].b`): a compound
/// store reads it twice.
pub(crate) fn read_base(body: &HirBody, id: HirId) -> bool {
    match &body.expr(id).kind {
        // A static's load reads its slot, as a local's does (#1557).
        HirKind::Local(_) | HirKind::Global { .. } => true,
        HirKind::Field { base, .. } => read_base(body, *base),
        HirKind::Index {
            base,
            index,
            kind: IndexKind::Array | IndexKind::Tuple,
        } => read_base(body, *base) && pure_index(body, *index),
        _ => false,
    }
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

/// `x % m` with a literal `m` and a dividend not proven non-negative: an
/// array index by it gets a Euclidean fix-up.
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

/// Whether a stack-array index needs no bounds check: the checker proved
/// it, or it is
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
/// trip count, for the body emitted once per value.
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
    // A lambda is planned for one emission, so its body cannot repeat.
    let lambda_or_defer = |k: HirId| matches!(hir.expr(k).kind, HirKind::Lambda { .. } | HirKind::Defer { .. });
    if !(0..=8).contains(&count) || has_own_jump(hir, body) || hir.any(body, &lambda_or_defer) {
        return None;
    }
    Some((*s, count as u32))
}

/// Each copy of a `defer` is its own site that every later exit checks,
/// so repeating one grows the exits with the square of the copies.
fn has_own_jump(hir: &HirBody, id: HirId) -> bool {
    match &hir.expr(id).kind {
        HirKind::Break | HirKind::Continue => true,
        HirKind::Loop { .. } | HirKind::ForIn { .. } => false,
        _ => hir.children(id).any(|k| has_own_jump(hir, k)),
    }
}

/// Whether `body` holds a `continue` of its own loop (nested loops' are
/// theirs), as `const_fold::body_has_continue`.
pub fn has_own_continue(hir: &HirBody, body: HirId) -> bool {
    match &hir.expr(body).kind {
        HirKind::Continue => true,
        HirKind::Loop { .. } | HirKind::ForIn { .. } => false,
        _ => hir.children(body).any(|k| has_own_continue(hir, k)),
    }
}

/// Whether `local` is assigned anywhere in `body`.
pub fn assigns_local(hir: &HirBody, body: HirId, local: LocalId) -> bool {
    let mut found = false;
    hir.visit(body, &mut |e| {
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
        HirKind::Global { name, .. } => checker.ffi_static_fn_id_variadic(name).unwrap_or(false),
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
/// call, else each argument's literal kind.
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

/// A one-byte string literal typed `byte`: the byte's code.
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

/// `typeof e`'s text: `e`'s ground type, or `None` for one with no name to
/// print.
pub fn typeof_text(body: &HirBody, checker: &Checker, arg: HirId) -> Option<String> {
    let ty = apply_ty_prune(checker.subst(), body.expr(arg).ty.as_ref()?);
    crate::typechecking::pretty::format_ty_fqn(&ty, &checker.generics().nominal_type_modules)
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
        .is_some_and(|t| crate::layout::is_unit(&apply_ty_prune(checker.subst(), t)))
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

/// The parts of the `while` whose `Loop` body is `loop_body`, as
/// [`while_shape`] and through the contract checks the builder runs ahead
/// of each test (`invariant` / `decreases`): `(checks, cond, body)`.
pub fn while_parts(body: &HirBody, loop_body: HirId) -> Option<(&[HirId], HirId, HirId)> {
    let HirKind::Block { stmts, tail: None } = &body.expr(loop_body).kind else {
        return None;
    };
    let (test, checks) = stmts.split_last()?;
    let HirKind::If { cond, then, els: Some(els) } = body.expr(*test).kind else {
        return None;
    };
    // The builder's own test: only it carries the loop's span and no node.
    let built = body.expr(*test).node.is_none() && body.expr(*test).span == body.expr(loop_body).span;
    (matches!(body.expr(els).kind, HirKind::Break) && (checks.is_empty() || built)).then_some((checks, cond, then))
}

/// The contract checks ahead of `for` loop `id`'s body, and the body as
/// written.
pub fn for_body_parts(body: &HirBody, id: HirId) -> Option<(&[HirId], HirId)> {
    let e = body.expr(id);
    let HirKind::ForIn { body: own, .. } = e.kind else {
        return None;
    };
    // The builder wraps a checked body in a block of its own: the loop's
    // span, no node, the checks and then the body.
    let wrap = body.expr(own);
    if wrap.node.is_none()
        && wrap.span == e.span
        && let HirKind::Block { stmts, tail: None } = &wrap.kind
        && let Some((written, checks)) = stmts.split_last()
        && !checks.is_empty()
    {
        return Some((checks, *written));
    }
    Some((&[], own))
}

/// Payload fields an arm's pattern names, in declaration order: `Some(l)`
/// for a binding, `None` for `_`. `Err` for a pattern that tests more than
/// the outer tag.
pub fn arm_fields(body: &HirBody, pat: &HirPat) -> Result<Vec<Option<super::LocalId>>, &'static str> {
    let field = |p: &HirPat| match p {
        HirPat::Wild => Ok(None),
        // A `()` payload (`Ok(ok)` of the `?` desugaring) binds no word.
        HirPat::Bind(l) if body.local(*l).ty.as_ref().is_some_and(crate::layout::is_unit) => Ok(None),
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

/// A never-reassigned parameter or `let` local holding a `PolyFn`: its
/// calls box each argument and may pass dictionaries.
pub fn polyfn_callee(body: &HirBody, checker: &Checker, f: HirId) -> bool {
    let HirKind::Local(local) = body.expr(f).kind else {
        return false;
    };
    let info = body.local(local);
    // A parameter is one only when declared `forall` (an open `T -> U`
    // parameter of a shared generic body is a plain closure).
    let ty = info.ty.as_ref().map(|t| apply_ty_prune(checker.subst(), t));
    ty.as_ref().is_some_and(|t| match info.kind {
        // A lambda's capture of one is one in the lambda (#2724).
        LocalKind::Param | LocalKind::Capture => matches!(strip_readonly(t), Ty::Forall { .. }),
        // A generic function's returned function the checker saw open at
        // the binding is a `PolyFn` too (`let f = capture_show(0)`),
        // whatever the local settles to (`maybe_record_polyfn_binding`).
        // A closure literal is never one, though its type may keep a
        // variable nothing pinned (`fn () => Ok(2)`'s error type, #1520):
        // it is `MakeFn`'s plain closure word.
        LocalKind::Let if let Some(source) = let_init_local(body, local) => polyfn_callee(body, checker, source),
        // A local of a `forall` type is one whatever it was read from (a
        // `forall` field, #2725).
        LocalKind::Let if matches!(strip_readonly(t), Ty::Forall { .. }) && !let_init_is_lambda(body, local) => true,
        LocalKind::Let => {
            poly_fun(t) && !let_init_is_lambda(body, local) && !let_init_is_element(body, local) && !let_init_is_field(body, local)
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

/// The local a `let` copies (`let g = f`): the same word, so a `PolyFn`
/// exactly when that local is one (#2555).
fn let_init_local(body: &HirBody, local: LocalId) -> Option<HirId> {
    body.exprs.iter().find_map(|e| match &e.kind {
        HirKind::Let { local: l, init: Some(init) } if *l == local && matches!(body.expr(*init).kind, HirKind::Local(_)) => Some(*init),
        _ => None,
    })
}

/// A `let` bound to a field read (`let g = self.f`): the closure word the
/// object stored, never a `PolyFn` (#2485).
fn let_init_is_field(body: &HirBody, local: LocalId) -> bool {
    body.exprs.iter().any(|e| {
        matches!(&e.kind, HirKind::Let { local: l, init: Some(init) }
            if *l == local && matches!(body.expr(*init).kind, HirKind::Field { .. }))
    })
}

/// A `let` bound to an element read by index (`let b = bodies[i]`): the
/// closure word the `Vec` stored, never a `PolyFn` (#2136).
fn let_init_is_element(body: &HirBody, local: LocalId) -> bool {
    body.exprs.iter().any(|e| {
        matches!(&e.kind, HirKind::Let { local: l, init: Some(init) }
            if *l == local && matches!(body.expr(*init).kind, HirKind::Index { .. }))
    })
}

/// Whether `local` is a `let` of a field of, or a pattern's binding in a
/// `let` or a match on, a nested open enum (`layout::nested_open_type`): a generic
/// body's instance of one was made where its parameters are open, so a
/// function it holds returns the open enum's object (#2493, #2509).
fn binds_from_nested(body: &HirBody, checker: &Checker, local: LocalId) -> bool {
    let nested = |id: HirId| {
        body.expr(id)
            .ty
            .as_ref()
            .is_some_and(|t| crate::layout::nested_open_type(checker, &apply_ty_prune(checker.subst(), t)))
    };
    body.exprs.iter().any(|e| match &e.kind {
        HirKind::Let { local: l, init: Some(init) } if *l == local => {
            matches!(body.expr(*init).kind, HirKind::Field { base, .. } if nested(base))
        }
        // `let (f, _) = p;` of a nested open value (#2493).
        HirKind::LetPat { pat, init } => pat.binds(local) && nested(*init),
        HirKind::Match { scrutinee, arms } => arms.iter().any(|arm| arm.pat.binds(local)) && nested(*scrutinee),
        _ => false,
    })
}

/// Whether `local` is a `for` loop's binding of each element (`for f in fs`).
fn for_binds_element(body: &HirBody, local: LocalId) -> bool {
    body.exprs
        .iter()
        .any(|e| matches!(&e.kind, HirKind::ForIn { pat: HirPat::Bind(l), .. } if *l == local))
}

pub fn indirect_callee(body: &HirBody, checker: &Checker, f: HirId) -> bool {
    indirect_callee_in(body, checker, f, &mut Vec::new())
}

/// `indirect_callee`, with the function locals whose check is under way:
/// one copied from another (#1783) is reached again on a cycle
/// (`f = g; g = f`), where it holds only what its other values put there.
fn indirect_callee_in(body: &HirBody, checker: &Checker, f: HirId, seen: &mut Vec<LocalId>) -> bool {
    // An element of a `Vec`, array or tuple of monomorphic functions, or a
    // `const` or `static` holding one (#1685): the closure word it was
    // stored as.
    if let HirKind::Index { .. } | HirKind::Global { .. } = body.expr(f).kind {
        let index = matches!(body.expr(f).kind, HirKind::Index { .. });
        return body.expr(f).ty.as_ref().is_some_and(|t| {
            let t = apply_ty_prune(checker.subst(), t);
            mono_fun(checker, &t, &mut Vec::new()) || index && open_fun(checker, &t, &mut Vec::new())
        });
    }
    // A field holding a function, called in place as `(o.f)(x)` (#2472):
    // the closure word stored there, as a `let` of it.
    if let HirKind::Field { .. } = body.expr(f).kind {
        return field_fun(body, checker, f);
    }
    // A call's returned closure called in place (#1533), as a `let` of it.
    if let HirKind::Call { .. } = body.expr(f).kind {
        return body.expr(f).ty.as_ref().is_some_and(|t| {
            let t = apply_ty_prune(checker.subst(), t);
            matches!(strip_readonly(&t), Ty::Fun(..)) && fun_words_in(checker, &t, &mut Vec::new(), false)
        }) && returned_closure(body, checker, f, seen);
    }
    // A lambda literal called in place (#1799), as a `let` bound to it.
    if let HirKind::Lambda { .. } = body.expr(f).kind {
        return body.expr(f).ty.as_ref().is_some_and(|t| {
            let t = apply_ty_prune(checker.subst(), t);
            matches!(strip_readonly(&t), Ty::Fun(..)) && fun_words_in(checker, &t, &mut Vec::new(), true)
        });
    }
    let HirKind::Local(local) = body.expr(f).kind else {
        return false;
    };
    if seen.contains(&local) {
        return true;
    }
    let info = body.local(local);
    let assigned: Vec<HirId> = body
        .exprs
        .iter()
        .filter_map(|e| match e.kind {
            HirKind::Assign { place, value } if body.expr(place).kind == HirKind::Local(local) => Some(value),
            _ => None,
        })
        .collect();
    if !info
        .ty
        .as_ref()
        .is_some_and(|t| {
            let t = apply_ty_prune(checker.subst(), t);
            matches!(strip_readonly(&t), Ty::Fun(..))
                // A shared generic body's `T -> Option<U>` parameter returns
                // the open enum's boxed word, as a lambda bound to a `let`.
                && fun_words_in(
                    checker,
                    &t,
                    &mut Vec::new(),
                    info.kind == LocalKind::Param
                        || info.kind == LocalKind::Let && let_init_is_lambda(body, local) && assigned.is_empty()
                        // An element of a shared body's `Vec<unit -> Result<T, E>>`:
                        // the closure word stored there (#2153).
                        || assigned.is_empty() && (let_init_is_element(body, local) || for_binds_element(body, local))
                        // Read from a nested open enum's field or payload (#2493, #2509).
                        || assigned.is_empty() && binds_from_nested(body, checker, local)
                        // A copy of another function local holds its word,
                        // which that local's own check below admits (#2555).
                        || info.kind == LocalKind::Let && assigned.is_empty() && let_init_local(body, local).is_some(),
                )
        })
    {
        return false;
    }
    // Each value stored into it is a closure word too (#1759): a named
    // function, a lambda, a returned closure.
    seen.push(local);
    let words = assigned.iter().all(|&value| closure_word(body, checker, value, seen))
        && match info.kind {
            // A `for` or destructuring binding of a monomorphic function
            // element: the closure word it was stored as. A captured one is
            // its closure word, as the enclosing body's own call through it
            // would be.
            LocalKind::Param | LocalKind::Pattern | LocalKind::Capture => true,
            LocalKind::Let => body.exprs.iter().any(|e| match &e.kind {
                HirKind::Let { local: l, init: Some(init) } if *l == local => closure_word(body, checker, *init, seen),
                _ => false,
            }),
            _ => false,
        };
    seen.pop();
    words
}

/// A field read whose value is a closure word a call takes: a monomorphic
/// function, or a generic class's function over its parameters read in a
/// generic body (`f: fn(T) -> T`, #2485), whose words are plain: each bare
/// `T` is the one word ground code passes for it, so the closure a ground
/// instance stored is called as it is.
///
/// A function holding a parameter below the top (`fn(T) -> Option<T>`)
/// makes its class a nested open enum (`layout::nested_open_enum`): its
/// methods and every generic over it are clone-only, so the instance a
/// generic body reads it from was made where the parameter is open, and
/// the closure returns the open enum's object (#2509).
fn field_fun(body: &HirBody, checker: &Checker, field: HirId) -> bool {
    let HirKind::Field { base, .. } = &body.expr(field).kind else {
        return false;
    };
    let nested = body
        .expr(*base)
        .ty
        .as_ref()
        .is_some_and(|t| crate::layout::nested_open_type(checker, &apply_ty_prune(checker.subst(), t)));
    body.expr(field).ty.as_ref().is_some_and(|t| {
        let t = apply_ty_prune(checker.subst(), t);
        matches!(strip_readonly(&t), Ty::Fun(..)) && fun_words_in(checker, &t, &mut Vec::new(), nested)
    })
}

/// Whether `value`, bound to or stored into a function-typed local, is a
/// plain closure word a call through the local can take.
fn closure_word(body: &HirBody, checker: &Checker, value: HirId, seen: &mut Vec<LocalId>) -> bool {
    match &body.expr(value).kind {
        HirKind::Call { .. } => returned_closure(body, checker, value, seen),
        // Another function local that is one itself (#1783): its copy.
        HirKind::Local(_) => indirect_callee_in(body, checker, value, seen),
        // A named function read as a value is `MakeFn` (codegen refuses a
        // generic one, whose `MakePolyFn` calls differently).
        HirKind::Global { .. } | HirKind::Lambda { .. } => true,
        // A function field or element: the closure word stored there.
        HirKind::Field { .. } => field_fun(body, checker, value),
        // An element of a shared generic body's `Vec<unit -> T>` too (#2136).
        HirKind::Index { .. } => body.expr(value).ty.as_ref().is_some_and(|t| {
            let t = apply_ty_prune(checker.subst(), t);
            mono_fun(checker, &t, &mut Vec::new()) || open_fun(checker, &t, &mut Vec::new())
        }),
        // A `match` or `if` whose every branch gives one, or leaves (#2565):
        // the word of the branch taken.
        HirKind::Match { arms, .. } => arms.iter().all(|arm| branch_closure_word(body, checker, arm.body, seen)),
        HirKind::If { then, els: Some(els), .. } => {
            branch_closure_word(body, checker, *then, seen) && branch_closure_word(body, checker, *els, seen)
        }
        HirKind::Block { tail: Some(tail), .. } => closure_word(body, checker, *tail, seen),
        _ => false,
    }
}

/// A branch of a `match` or `if` bound to a function local: a closure
/// word, or a branch that never gives a value (`return`, `panic`).
fn branch_closure_word(body: &HirBody, checker: &Checker, value: HirId, seen: &mut Vec<LocalId>) -> bool {
    matches!(body.expr(value).ty, Some(Ty::Never)) || closure_word(body, checker, value, seen)
}

/// A call whose result is a plain closure word: a value call through an
/// indirect callee, or a function's or ground method's returned closure.
fn returned_closure(body: &HirBody, checker: &Checker, call: HirId, seen: &mut Vec<LocalId>) -> bool {
    match &body.expr(call).kind {
        // A `PolyFn` call's at a ground function type: codegen adapts the
        // shared body's closure to it (#1589).
        HirKind::Call {
            callee: Callee::Value(g), ..
        } => {
            indirect_callee_in(body, checker, *g, seen)
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
            // A generic class's method at a ground instance too (`a.get()`
            // on a `Slot<int>` returning its `fn(T) -> T` field, #2485),
            // where the result is a monomorphic function.
            let class = match owner.as_ref().map(strip_readonly) {
                Some(Ty::Con(owner)) => Some(owner),
                Some(Ty::App(head, _)) => match head.as_ref() {
                    Ty::Con(owner)
                        if body.expr(call).ty.as_ref().is_some_and(|t| {
                            let t = apply_ty_prune(checker.subst(), t);
                            mono_fun(checker, &t, &mut Vec::new())
                        }) =>
                    {
                        Some(owner)
                    }
                    _ => None,
                },
                _ => None,
            };
            class.is_some_and(|owner| checker.has_method(owner, name) && !checker.is_generic_fn(&format!("{owner}::{name}")))
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
/// arms on the backing word.
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
                !body.local(*l).ty.as_ref().is_some_and(crate::layout::is_unit)
                    && matches!(body.expr(arm.body).kind, HirKind::Local(r) if r == *l)
            }
            _ => false,
        },
        _ => false,
    }
}

/// Whether some arm tests a sub-pattern of its payload (a literal or an
/// inner variant): outer-tag dispatch cannot tell such arms apart, so the
/// match tests arm by arm.
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

/// A stack-array read lowered as a select over the slots (any index but an
/// in-range literal): it stores temps, so it counts as a call for staging
/// ([`clobbers`]).
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

#[cfg(test)]
#[path = "query.tests.rs"]
mod tests;
