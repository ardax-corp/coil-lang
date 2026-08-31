//! Monomorphization planning for ground generic call sites.
//!
//! This module is intentionally analysis-first. It decides which generic calls
//! are safe and small enough to specialize; `Compiler` owns the bytecode
//! emission for each accepted specialization.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use parser::{
    SimpleSpan,
    ast::{Expression, Output, TypeParam},
};

use crate::typechecking::{Checker, DefId, Ty};
use crate::typechecking::subst::apply_ty_prune;

pub const MAX_SPECIALIZATIONS_PER_FN: usize = 8;
pub const MAX_TOTAL_SPECIALIZATIONS: usize = 64;

/// Interned type identity for monomorphize keys (not `Ty::to_string()`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TyId(pub u32);

#[derive(Clone, Debug, Default)]
pub struct TyInterner {
    tys: Vec<Ty>,
    ids: HashMap<Ty, TyId>,
}

impl PartialEq for TyInterner {
    fn eq(&self, other: &Self) -> bool {
        self.tys == other.tys
    }
}
impl Eq for TyInterner {}

impl TyInterner {
    pub fn intern(&mut self, ty: Ty) -> TyId {
        if let Some(&id) = self.ids.get(&ty) {
            return id;
        }
        let id = TyId(self.tys.len() as u32);
        self.ids.insert(ty.clone(), id);
        self.tys.push(ty);
        id
    }

    pub fn get(&self, id: TyId) -> Option<&Ty> {
        self.tys.get(id.0 as usize)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MonoKey {
    pub def_id: DefId,
    pub subst: Vec<TyId>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MonoSpecialization {
    pub key: MonoKey,
    pub fn_name: String,
    pub arg_types: Vec<TyId>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MonoRetarget {
    pub call_span_start: usize,
    pub key: MonoKey,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MonoCapHit {
    pub call_span: parser::SimpleSpan,
    pub fn_name: String,
    pub per_fn: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MonoPlan {
    pub intern: TyInterner,
    pub specializations: Vec<MonoSpecialization>,
    pub retargets: Vec<MonoRetarget>,
    pub escaped_generic_fns: BTreeSet<String>,
    pub cap_hits: Vec<MonoCapHit>,
}

impl MonoPlan {
    pub fn is_empty(&self) -> bool {
        self.specializations.is_empty()
    }

    pub fn ty(&self, id: TyId) -> Option<&Ty> {
        self.intern.get(id)
    }

    pub fn specialization_for_call(
        &self,
        fn_name: &str,
        arg_types: &[Ty],
    ) -> Option<&MonoSpecialization> {
        self.specializations.iter().find(|spec| {
            spec.fn_name == fn_name
                && spec.arg_types.len() == arg_types.len()
                && spec
                    .arg_types
                    .iter()
                    .zip(arg_types)
                    .all(|(id, ty)| self.intern.get(*id) == Some(ty))
        })
    }

    pub fn specializations_for_fn<'a>(
        &'a self,
        fn_name: &'a str,
    ) -> impl Iterator<Item = &'a MonoSpecialization> + 'a {
        self.specializations
            .iter()
            .filter(move |spec| spec.fn_name == fn_name)
    }

    pub fn specializations_for_def(&self, def_id: DefId) -> impl Iterator<Item = &MonoSpecialization> {
        self.specializations
            .iter()
            .filter(move |spec| spec.key.def_id == def_id)
    }
}

#[derive(Clone, Debug)]
struct GenericFnSig {
    def_id: DefId,
    fn_name: String,
    type_params: Vec<String>,
    type_param_bounds: Vec<Vec<String>>,
    /// For each formal: which type-parameter index it references (if any).
    param_type_params: Vec<Option<usize>>,
    /// Parallel to `param_type_params`: true when the formal is `T... name`.
    param_is_rest: Vec<bool>,
}

#[derive(Clone, Debug)]
struct MonoCandidate {
    span: SimpleSpan,
    specialization: MonoSpecialization,
}

/// Explicit monomorphize pass: after check, before emit.
///
/// Only generic functions whose type parameters carry at least one **opcode**
/// bound (`Num` / `Ord` / `Eq` and operator supertraits) are specialized.
/// Unbounded `id<T>` stays on the shared `BoxValue`/`UnboxValue` path. User
/// traits, `Show`, and `Length` stay on dictionary passing (COI-78).
/// Keys are [`DefId`] + interned [`Ty`] ids from checker subst, not Display.
pub fn run_monomorphize_pass(module: &str, ast: &Output, checker: &Checker) -> MonoPlan {
    let mut intern = TyInterner::default();
    let mut sigs = HashMap::new();
    collect_generic_functions(module, ast, checker, &mut sigs);

    let mut escaped = BTreeSet::new();
    collect_escaped_generic_refs(ast, &sigs, false, &mut escaped);

    let mut candidates = Vec::new();
    collect_candidates(ast, checker, &sigs, &mut intern, &mut candidates);

    let (specializations, retargets, cap_hits) = apply_caps(candidates);
    MonoPlan {
        intern,
        specializations,
        retargets,
        escaped_generic_fns: escaped,
        cap_hits,
    }
}

/// Alias kept for in-crate tests.
pub fn plan_monomorphization(module: &str, ast: &Output, checker: &Checker) -> MonoPlan {
    run_monomorphize_pass(module, ast, checker)
}

fn collect_generic_functions(
    module: &str,
    node: &Output,
    checker: &Checker,
    sigs: &mut HashMap<String, GenericFnSig>,
) {
    match node.1.as_ref() {
        Expression::Function {
            docs: _,
            name,
            type_params,
            args,
            body,
            ..
        } => {
            if !type_params.is_empty() {
                if let Some(def_id) = checker
                    .def_id_of(name)
                    .or_else(|| checker.interned_def(module, name))
                {
                    let mut sig = signature_from_function(type_params, args);
                    sig.def_id = def_id;
                    sig.fn_name = (*name).to_string();
                    sigs.insert(name.to_string(), sig.clone());
                    if !module.is_empty() {
                        sigs.insert(format!("{module}::{name}"), sig);
                    }
                }
            }
            if let Some(body) = body {
                collect_generic_functions(module, body, checker, sigs);
            }
        }
        _ => walk_children(node, &mut |child| {
            collect_generic_functions(module, child, checker, sigs)
        }),
    }
}

fn signature_from_function(type_params: &[TypeParam<'_>], args: &Output) -> GenericFnSig {
    let type_param_names = type_params
        .iter()
        .map(|tp| tp.name.to_string())
        .collect::<Vec<_>>();
    let type_param_bounds = type_params
        .iter()
        .map(|tp| tp.bounds.iter().map(|b| b.to_string()).collect::<Vec<_>>())
        .collect::<Vec<_>>();

    let mut param_type_params = Vec::new();
    let mut param_is_rest = Vec::new();
    if let Expression::Fragment(children) = args.1.as_ref() {
        for child in children {
            if let Expression::Argument { ty, is_rest, .. } = child.1.as_ref() {
                param_type_params.push(
                    ty.as_ref()
                        .and_then(|t| type_param_ref_index(t, &type_param_names)),
                );
                param_is_rest.push(*is_rest);
            }
        }
    }

    GenericFnSig {
        def_id: DefId::from_u32(0),
        fn_name: String::new(),
        type_params: type_param_names,
        type_param_bounds,
        param_type_params,
        param_is_rest,
    }
}

fn type_param_ref_index(ty: &Output, type_params: &[String]) -> Option<usize> {
    match ty.1.as_ref() {
        Expression::Identifier(name) | Expression::Type(name) => {
            type_params.iter().position(|tp| tp == name)
        }
        _ => None,
    }
}

fn collect_candidates(
    node: &Output,
    checker: &Checker,
    sigs: &HashMap<String, GenericFnSig>,
    intern: &mut TyInterner,
    out: &mut Vec<MonoCandidate>,
) {
    if let Expression::Call { name, args } = node.1.as_ref() {
        if let Expression::Identifier(fn_name) = name.1.as_ref()
            && let Some(sig) = sigs.get(*fn_name)
            && let Some(specialization) =
                candidate_for_call(*fn_name, sig, args.as_deref(), checker, intern)
        {
            out.push(MonoCandidate {
                span: node.0,
                specialization,
            });
        }
        if let Some(args) = args {
            for arg in args {
                collect_candidates(arg, checker, sigs, intern, out);
            }
        }
        return;
    }

    walk_children(node, &mut |child| {
        collect_candidates(child, checker, sigs, intern, out)
    });
}

fn candidate_for_call(
    fn_name: &str,
    sig: &GenericFnSig,
    args: Option<&[Output]>,
    checker: &Checker,
    intern: &mut TyInterner,
) -> Option<MonoSpecialization> {
    if sig.type_params.is_empty() || sig.type_param_bounds.iter().all(|bounds| bounds.is_empty()) {
        return None;
    }

    // Dictionary bodies are not specialized (COI-78):
    // - user-defined typeclasses always use dict tuples
    // - `Show` / `Length` always use dict / CallIndirect; specializing them
    //   leaves call sites with an open `Ty::Var`
    // Num / Ord / Eq still monomorphize so arithmetic becomes direct opcodes.
    let requires_dictionary_body = sig.type_param_bounds.iter().any(|bounds| {
        bounds
            .iter()
            .any(|b| b == "Show" || b == "Length" || !Checker::is_builtin_class(b))
    });
    if requires_dictionary_body {
        return None;
    }

    let args = args.unwrap_or(&[]);
    let has_rest = sig.param_is_rest.last().copied().unwrap_or(false);
    let fixed_count = if has_rest {
        sig.param_type_params.len().saturating_sub(1)
    } else {
        sig.param_type_params.len()
    };

    // Reorder/pack like codegen (`split_call_args_for_rest`) so named calls
    // (`add(b: 2, a: 1)`) and rest packs share the same mono key.
    let (fixed_args, rest_args, pack_rest) =
        split_call_args_for_mono(fn_name, args, checker, has_rest, fixed_count)?;

    if !has_rest {
        if fixed_args.len() != sig.param_type_params.len() {
            return None;
        }
    } else if fixed_args.len() < fixed_count {
        return None;
    }

    let mut subst: Vec<Option<Ty>> = vec![None; sig.type_params.len()];
    let mut arg_types = Vec::with_capacity(sig.param_type_params.len());

    let bind = |subst: &mut [Option<Ty>], tp_idx: Option<usize>, arg_ty: &Ty| -> Option<()> {
        if let Some(tp_idx) = tp_idx {
            match &subst[tp_idx] {
                Some(existing) if existing != arg_ty => return None,
                Some(_) => {}
                None => subst[tp_idx] = Some(arg_ty.clone()),
            }
        }
        Some(())
    };

    let fixed_tps = if has_rest {
        &sig.param_type_params[..fixed_count]
    } else {
        sig.param_type_params.as_slice()
    };
    for (arg, tp_idx) in fixed_args.iter().zip(fixed_tps.iter()) {
        let arg_ty = ground_ty(checker, arg)?;
        bind(&mut subst, *tp_idx, &arg_ty)?;
        arg_types.push(arg_ty);
    }

    if pack_rest {
        // One key slot per rest formal: the *element* ground type (not the
        // packed `[T]` / `[T; N]`), matching `mono_call_offset`.
        let rest_tp = sig.param_type_params.get(fixed_count).copied().flatten();
        if rest_args.is_empty() {
            let elem = rest_tp.and_then(|i| subst[i].clone())?;
            arg_types.push(elem);
        } else {
            let mut elem_ty: Option<Ty> = None;
            for arg in &rest_args {
                let t = ground_ty(checker, arg)?;
                match &elem_ty {
                    None => elem_ty = Some(t.clone()),
                    Some(prev) if prev != &t => return None,
                    _ => {}
                }
                bind(&mut subst, rest_tp, &t)?;
            }
            arg_types.push(elem_ty?);
        }
    } else if has_rest {
        // Declares rest but call did not pack — not a mono candidate.
        return None;
    }

    let subst = subst.into_iter().collect::<Option<Vec<_>>>()?;
    let subst_ids: Vec<TyId> = subst.into_iter().map(|ty| intern.intern(ty)).collect();
    let arg_ids: Vec<TyId> = arg_types.into_iter().map(|ty| intern.intern(ty)).collect();
    Some(MonoSpecialization {
        key: MonoKey {
            def_id: sig.def_id,
            subst: subst_ids,
        },
        fn_name: if sig.fn_name.is_empty() {
            fn_name.to_string()
        } else {
            sig.fn_name.clone()
        },
        arg_types: arg_ids,
    })
}

/// Mirror of codegen `split_call_args_for_rest` for mono planning.
fn split_call_args_for_mono<'a>(
    fn_name: &str,
    args: &'a [Output<'a>],
    checker: &Checker,
    has_rest: bool,
    fixed_count: usize,
) -> Option<(Vec<&'a Output<'a>>, Vec<&'a Output<'a>>, bool)> {
    let has_named = args
        .iter()
        .any(|a| matches!(a.1.as_ref(), Expression::NamedArg(..)));
    if !has_named && !has_rest {
        return Some((args.iter().collect(), Vec::new(), false));
    }
    let param_names = checker.fn_param_names(fn_name)?;
    let rest_name = if has_rest {
        param_names.get(fixed_count).map(|s| s.as_str())
    } else {
        None
    };
    let mut slots: Vec<Option<&'a Output<'a>>> = vec![None; fixed_count];
    let mut rest = Vec::new();
    let mut next_pos = 0usize;
    for arg in args {
        match arg.1.as_ref() {
            Expression::NamedArg(name, value) => {
                if rest_name == Some(*name) {
                    rest.push(value);
                    continue;
                }
                if let Some(idx) = param_names[..fixed_count].iter().position(|p| p == *name) {
                    slots[idx] = Some(value);
                }
            }
            _ => {
                while next_pos < fixed_count && slots[next_pos].is_some() {
                    next_pos += 1;
                }
                if next_pos < fixed_count {
                    slots[next_pos] = Some(arg);
                    next_pos += 1;
                } else if has_rest {
                    rest.push(arg);
                    next_pos += 1;
                } else {
                    next_pos += 1;
                }
            }
        }
    }
    let pack_rest = has_rest
        && (has_named || next_pos >= fixed_count || args.len() >= fixed_count || fixed_count == 0);
    let fixed: Vec<_> = slots.into_iter().flatten().collect();
    if pack_rest {
        Some((fixed, rest, true))
    } else {
        Some((fixed, Vec::new(), false))
    }
}

/// Ground type from checker subst (literals only as a fallback). Not Display.
pub fn ground_ty(checker: &Checker, expr: &Output) -> Option<Ty> {
    match expr.1.as_ref() {
        Expression::NamedArg(_, inner)
        | Expression::Group(inner)
        | Expression::Expr(inner)
        | Expression::Statement(inner) => return ground_ty(checker, inner),
        _ => {}
    }

    if let Some(ty) = checker.lookup_for_codegen_span(expr.0.start, expr.0.end) {
        if let Some(ty) = concrete_ty(&ty) {
            return Some(ty);
        }
    }

    match expr.1.as_ref() {
        Expression::Integer(_) => Some(Ty::Con("int".into())),
        Expression::Float(_) => Some(Ty::Con("float".into())),
        Expression::String(_) => Some(Ty::Con("string".into())),
        Expression::Bool(_) => Some(Ty::Con("bool".into())),
        Expression::Identifier(name) => checker
            .codegen_var_type(name)
            .and_then(|ty| concrete_ty(&apply_ty_prune(checker.subst(), ty))),
        Expression::Tuple(items) => {
            let mut elems = Vec::with_capacity(items.len());
            for it in items {
                elems.push(ground_ty(checker, it)?);
            }
            if elems.is_empty() {
                return None;
            }
            Some(Ty::Tuple(elems))
        }
        Expression::Array(items) => {
            if items.is_empty() {
                return None;
            }
            let elem = ground_ty(checker, &items[0])?;
            if items
                .iter()
                .any(|it| ground_ty(checker, it).as_ref() != Some(&elem))
            {
                return None;
            }
            Some(crate::typechecking::ty::array_fixed(elem, items.len()))
        }
        _ => None,
    }
}

fn concrete_ty(ty: &Ty) -> Option<Ty> {
    if contains_var(ty) || matches!(ty, Ty::Fun(_, _)) {
        return None;
    }
    Some(ty.clone())
}

fn contains_var(ty: &Ty) -> bool {
    match ty {
        Ty::Var(_) => true,
        Ty::Fun(a, b) => contains_var(a) || contains_var(b),
        Ty::App(head, args) => contains_var(head) || args.iter().any(contains_var),
        Ty::List(inner) => contains_var(inner),
        Ty::Sum { variants, .. } => variants.iter().any(|(_, payload)| match payload {
            crate::typechecking::ty::EnumVariantPayloadTy::Unit => false,
            crate::typechecking::ty::EnumVariantPayloadTy::Tuple(items) => {
                items.iter().any(contains_var)
            }
            crate::typechecking::ty::EnumVariantPayloadTy::Record(fields) => {
                fields.iter().any(|(_, ty)| contains_var(ty))
            }
        }),
        Ty::Constructor { owner, .. } => contains_var(owner),
        Ty::Tuple(items) => items.iter().any(contains_var),
        Ty::Array { element, .. } => contains_var(element),
        Ty::Record { fields } => fields.iter().any(|(_, ty)| contains_var(ty)),
        Ty::Forall { body, .. } => contains_var(body),
        Ty::Readonly(inner) => contains_var(inner),
        Ty::Con(_) | Ty::Existential { .. } | Ty::Never => false,
    }
}

fn apply_caps(
    candidates: Vec<MonoCandidate>,
) -> (
    Vec<MonoSpecialization>,
    Vec<MonoRetarget>,
    Vec<MonoCapHit>,
) {
    let mut seen = BTreeSet::new();
    let mut per_fn: BTreeMap<DefId, usize> = BTreeMap::new();
    let mut specializations = Vec::new();
    let mut retargets = Vec::new();
    let mut cap_hits = Vec::new();

    for candidate in candidates {
        let key = candidate.specialization.key.clone();
        if seen.contains(&key) {
            retargets.push(MonoRetarget {
                call_span_start: candidate.span.start,
                key,
            });
            continue;
        }

        let count = per_fn.entry(key.def_id).or_default();
        let per_fn_hit = *count >= MAX_SPECIALIZATIONS_PER_FN;
        let total_hit = specializations.len() >= MAX_TOTAL_SPECIALIZATIONS;
        if per_fn_hit || total_hit {
            cap_hits.push(MonoCapHit {
                call_span: candidate.span,
                fn_name: candidate.specialization.fn_name.clone(),
                per_fn: per_fn_hit,
            });
            continue;
        }

        *count += 1;
        seen.insert(key.clone());
        specializations.push(candidate.specialization.clone());
        retargets.push(MonoRetarget {
            call_span_start: candidate.span.start,
            key,
        });
    }

    (specializations, retargets, cap_hits)
}

fn collect_escaped_generic_refs(
    node: &Output,
    sigs: &HashMap<String, GenericFnSig>,
    is_call_target: bool,
    escaped: &mut BTreeSet<String>,
) {
    match node.1.as_ref() {
        Expression::Identifier(name) if !is_call_target && sigs.contains_key(*name) => {
            escaped.insert(name.to_string());
        }
        Expression::Call { name, args } => {
            collect_escaped_generic_refs(name, sigs, true, escaped);
            if let Some(args) = args {
                for arg in args {
                    collect_escaped_generic_refs(arg, sigs, false, escaped);
                }
            }
        }
        _ => walk_children(node, &mut |child| {
            collect_escaped_generic_refs(child, sigs, false, escaped)
        }),
    }
}

fn walk_children<F>(node: &Output, f: &mut F)
where
    F: FnMut(&Output),
{
    match node.1.as_ref() {
        Expression::Program(items)
        | Expression::Block(items)
        | Expression::Fragment(items)
        | Expression::List(items)
        | Expression::Tuple(items)
        | Expression::Array(items)
        | Expression::Declare(items)
        | Expression::Invoke(items) => {
            for item in items {
                f(item);
            }
        }
        Expression::Expr(e)
        | Expression::Group(e)
        | Expression::Statement(e)
        | Expression::ExprStatement(e)
        | Expression::Return(e)
        | Expression::ImplicitReturn(e)
        | Expression::Raise(e)
        | Expression::Panic(e)
        | Expression::TypeOf(e)
        | Expression::Try(e)
        | Expression::Yield(e)
        | Expression::YieldFrom(e)
        | Expression::Negate(e)
        | Expression::Not(e)
        | Expression::LogicalNot(e)
        | Expression::Positive(e)
        | Expression::Adjust { target: e, .. }
        | Expression::Member(e)
        | Expression::Dload(e)
        | Expression::Done(e)
        | Expression::Noop(e)
        | Expression::Method(_, e)
        | Expression::OptionalAccess(e, _)
        | Expression::Access(e, _) => f(e),
        Expression::Defer { body, .. } => f(body),
        Expression::Assignment(lhs, rhs) | Expression::CompoundAssign(lhs, _, rhs) => {
            f(lhs);
            f(rhs);
        }
        Expression::Add(l, r)
        | Expression::Sub(l, r)
        | Expression::Mul(l, r)
        | Expression::Div(l, r)
        | Expression::Mod(l, r)
        | Expression::Pow(l, r)
        | Expression::Shl(l, r)
        | Expression::Shr(l, r)
        | Expression::Xor(l, r)
        | Expression::And(l, r)
        | Expression::Or(l, r)
        | Expression::BitAnd(l, r)
        | Expression::BitOr(l, r)
        | Expression::Eq(l, r)
        | Expression::Neq(l, r)
        | Expression::Le(l, r)
        | Expression::Gt(l, r)
        | Expression::Leq(l, r)
        | Expression::Geq(l, r)
        | Expression::Coalesce(l, r)
        | Expression::Cast(l, r) => {
            f(l);
            f(r);
        }
        Expression::Index(l, r) => {
            f(l);
            if let Some(r) = r {
                f(r);
            }
        }
        Expression::Range { start, end, .. } => {
            f(start);
            f(end);
        }
        Expression::Resume(target, arg) => {
            f(target);
            if let Some(arg) = arg {
                f(arg);
            }
        }
        Expression::If(branches) => {
            for branch in branches {
                f(branch);
            }
        }
        Expression::Branch(cond, body) => {
            if let Some(cond) = cond {
                f(cond);
            }
            f(body);
        }
        Expression::Call { name, args } => {
            f(name);
            if let Some(args) = args {
                for arg in args {
                    f(arg);
                }
            }
        }
        Expression::Loop {
            iterable,
            body,
            identifier,
        } => {
            f(iterable);
            if let Some(identifier) = identifier {
                f(identifier);
            }
            f(body);
        }
        Expression::For {
            init,
            cond,
            step,
            body,
        } => {
            if let Some(init) = init {
                f(init);
            }
            f(cond);
            if let Some(step) = step {
                f(step);
            }
            f(body);
        }
        Expression::Match { scrutinee, arms } => {
            f(scrutinee);
            for arm in arms {
                f(&arm.body);
            }
        }
        Expression::Function {
            docs: _,
            args,
            body,
            returns,
            ..
        } => {
            f(args);
            if let Some(returns) = returns {
                f(returns);
            }
            if let Some(body) = body {
                f(body);
            }
        }
        Expression::Lambda { args, body, .. } => {
            f(args);
            f(body);
        }
        Expression::TestCase { name, body } => {
            f(name);
            f(body);
        }
        Expression::TypeApp { args, .. } => {
            for arg in args {
                f(arg);
            }
        }
        Expression::TypeFun(arg, ret) => {
            f(arg);
            f(ret);
        }
        Expression::Class { fields, .. } => {
            for field in fields {
                f(field);
            }
        }
        Expression::Implementation { methods, .. } | Expression::TypeClass { methods, .. } => {
            for method in methods {
                f(method);
            }
        }
        Expression::TypeClassImpl { args, methods, .. } => {
            for arg in args {
                f(arg);
            }
            for method in methods {
                f(method);
            }
        }
        Expression::TypeAlias { ty, .. } | Expression::AssocTypeDef { ty, .. } => f(ty),
        Expression::AssocTypeDecl { .. } => {}
        Expression::TypeProjection { args, .. } => {
            for arg in args {
                f(arg);
            }
        }
        Expression::EnumDecl { variants, .. } => {
            for variant in variants {
                f(variant);
            }
        }
        Expression::Dict(fields) => {
            for field in fields {
                f(&field.value);
            }
        }
        Expression::Instantiate(class, args) => {
            f(class);
            if let Some(args) = args {
                for arg in args {
                    f(arg);
                }
            }
        }
        Expression::Construct { fields, .. } => match fields {
            parser::ast::EnumConstructPayload::Unit => {}
            parser::ast::EnumConstructPayload::Tuple(items) => {
                for item in items {
                    f(item);
                }
            }
            parser::ast::EnumConstructPayload::Record(fields) => {
                for field in fields {
                    f(&field.value);
                }
            }
        },
        Expression::EnumVariant { .. }
        | Expression::Use { .. }
        | Expression::Module(_, _)
        | Expression::Comment(_)
        | Expression::Integer(_)
        | Expression::Float(_)
        | Expression::String(_)
        | Expression::Bool(_)
        | Expression::Identifier(_)
        | Expression::Type(_)
        | Expression::Default(_)
        | Expression::Break
        | Expression::Continue
        | Expression::Variable(_, _)
        | Expression::Constant(_, _)
        | Expression::Argument { .. }
        | Expression::Field { .. }
        | Expression::QualifiedAccess { .. }
        | Expression::ExternBlock { .. }
        | Expression::ExternStruct(_)
        | Expression::Forall { .. } => {}
        Expression::LetDestructure { rhs, .. } => f(rhs),
        Expression::Readonly(inner) => f(inner),
        Expression::StaticDecl { ty, init, .. } => {
            if let Some(ty) = ty {
                f(ty);
            }
            f(init);
        }
        Expression::NamedArg(_, value) => f(value),
        Expression::Spread(inner) => f(inner),
        Expression::TypeFnSig { params, ret } => {
            f(params);
            f(ret);
        }
        Expression::AttrDecl {
            docs: _,
            args,
            returns,
            body,
            ..
        } => {
            f(args);
            if let Some(returns) = returns {
                f(returns);
            }
            f(body);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use parser::Pratt;

    fn plan(src: &str) -> MonoPlan {
        let src = format!("use io::{{stdout, write}}; use string::{{format, to_bytes}}; {src}");
        let ast = Pratt::default().parse(src.as_str()).expect("parse failed");
        let mut checker = Checker::new();
        let _ = checker.check_program(&ast);
        plan_monomorphization("", &ast, &checker)
    }

    #[test]
    fn plans_ground_bounded_generic_call() {
        let plan = plan(
            "fn add<T: Num>(T a, T b) -> T { return a + b; } \
             fn main() { write(stdout(), to_bytes(format(\"%i\", add(1, 2)))); }",
        );
        assert_eq!(plan.specializations.len(), 1);
        assert_eq!(plan.specializations[0].fn_name, "add");
        assert_eq!(
            plan.ty(plan.specializations[0].key.subst[0]),
            Some(&Ty::Con("int".into()))
        );
        assert_eq!(plan.specializations[0].arg_types.len(), 2);
        assert_eq!(
            plan.ty(plan.specializations[0].arg_types[0]),
            Some(&Ty::Con("int".into()))
        );
        assert_eq!(
            plan.ty(plan.specializations[0].arg_types[1]),
            Some(&Ty::Con("int".into()))
        );
        assert_eq!(plan.retargets.len(), 1);
        assert!(plan.cap_hits.is_empty());
    }

    #[test]
    fn plans_rest_only_num_generic_with_element_arg_type() {
        // Rest-only formals pack at the call site; the mono key uses the
        // *element* ground type (one slot per formal), not the packed array.
        let plan = plan(
            "fn twice_first<T: Num>(T... xs) -> T { return xs[0] + xs[0]; } \
             fn main() { write(stdout(), to_bytes(format(\"%i\", twice_first(21)))); }",
        );
        assert_eq!(plan.specializations.len(), 1);
        assert_eq!(plan.specializations[0].fn_name, "twice_first");
        assert_eq!(
            plan.ty(plan.specializations[0].key.subst[0]),
            Some(&Ty::Con("int".into()))
        );
        assert_eq!(plan.specializations[0].arg_types.len(), 1);
        assert_eq!(
            plan.ty(plan.specializations[0].arg_types[0]),
            Some(&Ty::Con("int".into()))
        );
    }

    #[test]
    fn plans_named_arg_ground_bounded_generic_call() {
        let plan = plan(
            "fn add<T: Num>(T a, T b) -> T { return a + b; } \
             fn main() { write(stdout(), to_bytes(format(\"%i\", add(b: 2, a: 1)))); }",
        );
        assert_eq!(plan.specializations.len(), 1);
        assert_eq!(
            plan.ty(plan.specializations[0].key.subst[0]),
            Some(&Ty::Con("int".into()))
        );
        assert_eq!(plan.specializations[0].arg_types.len(), 2);
    }

    #[test]
    fn leaves_unbounded_id_on_shared_path_for_mvp() {
        let plan = plan("fn id<T>(T x) -> T { return x; } fn main() { id(1); }");
        assert!(plan.specializations.is_empty());
    }

    #[test]
    fn does_not_plan_user_trait_ground_call() {
        let plan = plan(
            "trait Describable<T> { fn describe_val(T x) -> int; } \
             impl Describable<int> { fn describe_val(int x) -> int { return x; } } \
             fn show<T: Describable>(T x) -> int { return x.describe_val(); } \
             fn main() { show(42); }",
        );
        assert!(
            plan.specializations.is_empty(),
            "user-trait generics stay on dictionaries: {plan:?}"
        );
    }

    #[test]
    fn does_not_plan_show_or_length_ground_call() {
        let show = plan("fn show_it<T: Show>(T x) -> T { return x; } fn main() { show_it(1); }");
        assert!(
            show.specializations.is_empty(),
            "Show stays on dictionaries: {show:?}"
        );
        let length = plan("fn n<T: Length>(T x) -> int { return 0; } fn main() { n(\"ab\"); }");
        assert!(
            length.specializations.is_empty(),
            "Length stays on dictionaries: {length:?}"
        );
    }

    /// COI-78: a dictionary bound anywhere on the signature blocks mono, even
    /// when a sibling `Num` bound would otherwise specialize.
    #[test]
    fn does_not_plan_when_num_mixed_with_show_or_user_trait() {
        let with_show = plan(
            "fn mix<T: Num + Show>(T a, T b) -> T { return a + b; } \
             fn main() { mix(1, 2); }",
        );
        assert!(
            with_show.specializations.is_empty(),
            "Num+Show must stay on dictionaries: {with_show:?}"
        );

        let with_user = plan(
            "trait Tagged<T> { fn tag(T x) -> int; } \
             impl Tagged<int> { fn tag(int x) -> int { return x; } } \
             fn mix<T: Num + Tagged>(T a, T b) -> T { return a + b; } \
             fn main() { mix(1, 2); }",
        );
        assert!(
            with_user.specializations.is_empty(),
            "Num+user-trait must stay on dictionaries: {with_user:?}"
        );
    }

    /// COI-78 positive side: Ord / Eq remain opcode monomorphization candidates.
    #[test]
    fn plans_ground_ord_and_eq_calls() {
        let ord = plan(
            "fn less<T: Ord>(T a, T b) -> bool { return a < b; } \
             fn main() { less(1, 2); }",
        );
        assert_eq!(ord.specializations.len(), 1);
        assert_eq!(ord.specializations[0].fn_name, "less");
        assert_eq!(
            ord.ty(ord.specializations[0].key.subst[0]),
            Some(&Ty::Con("int".into()))
        );

        let eq = plan(
            "fn same<T: Eq>(T a, T b) -> bool { return a == b; } \
             fn main() { same(1, 1); }",
        );
        assert_eq!(eq.specializations.len(), 1);
        assert_eq!(eq.specializations[0].fn_name, "same");
        assert_eq!(
            eq.ty(eq.specializations[0].key.subst[0]),
            Some(&Ty::Con("int".into()))
        );
    }

    #[test]
    fn rejects_conflicting_type_param_instantiation() {
        let plan = plan(
            "fn same<T: Eq>(T a, T b) -> T { return a; } \
             fn main() { same(1, \"x\"); }",
        );
        assert!(plan.specializations.is_empty());
    }

    #[test]
    fn records_escaped_generic_refs() {
        let plan =
            plan("fn add<T: Num>(T a, T b) -> T { return a + b; } fn main() { let f = add; }");
        assert!(plan.escaped_generic_fns.contains("add"));
        assert!(plan.specializations.is_empty());
    }

    fn dummy_candidate(i: usize, def: u32, fn_name: &str) -> MonoCandidate {
        MonoCandidate {
            span: SimpleSpan::from(i..i + 1),
            specialization: MonoSpecialization {
                key: MonoKey {
                    def_id: DefId::from_u32(def),
                    subst: vec![TyId(i as u32)],
                },
                fn_name: fn_name.to_string(),
                arg_types: vec![TyId(i as u32)],
            },
        }
    }

    #[test]
    fn per_function_cap_limits_specializations() {
        let candidates = (0..(MAX_SPECIALIZATIONS_PER_FN + 2))
            .map(|i| dummy_candidate(i, 1, "f"))
            .collect();

        let (specializations, retargets, cap_hits) = apply_caps(candidates);
        assert_eq!(specializations.len(), MAX_SPECIALIZATIONS_PER_FN);
        assert_eq!(retargets.len(), MAX_SPECIALIZATIONS_PER_FN);
        assert_eq!(cap_hits.len(), 2);
        assert!(cap_hits.iter().all(|h| h.per_fn));
    }

    #[test]
    fn total_cap_limits_specializations() {
        let candidates = (0..(MAX_TOTAL_SPECIALIZATIONS + 2))
            .map(|i| dummy_candidate(i, i as u32 + 1, &format!("f{i}")))
            .collect();

        let (specializations, retargets, cap_hits) = apply_caps(candidates);
        assert_eq!(specializations.len(), MAX_TOTAL_SPECIALIZATIONS);
        assert_eq!(retargets.len(), MAX_TOTAL_SPECIALIZATIONS);
        assert_eq!(cap_hits.len(), 2);
        assert!(cap_hits.iter().all(|h| !h.per_fn));
    }
}
