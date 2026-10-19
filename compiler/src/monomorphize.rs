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
    /// Interns `ty` as its [`mono_key_ty`], so a variant's constructor
    /// type and the enum's applied type are one key (#1477).
    pub fn intern(&mut self, ty: Ty) -> TyId {
        let ty = mono_key_ty(&ty);
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

/// The type a mono key names for `ty`: an enum value's type as its applied
/// enum type, as an annotation spells it. A variant's constructor type
/// (`Option::Some(1)`, `Pick::Zero`) is its owner's, and a structural sum
/// is `Option<T>` / `Result<T, E>` for the builtins and the enum's name
/// otherwise, so `f(Option::Some(1))`, `f(Option::None)` at `Option<int>`
/// and `f(x)` with `Option<int> x` share one clone (#1477).
pub fn mono_key_ty(ty: &Ty) -> Ty {
    use crate::typechecking::ty::{option_app_ty, option_inner, result_app_ty, result_ok_err};
    let rec = |items: &[Ty]| items.iter().map(mono_key_ty).collect::<Vec<_>>();
    match ty {
        Ty::Constructor { owner, .. } => mono_key_ty(owner),
        Ty::Sum { name, .. } if common::is_builtin_option_enum(name) => match option_inner(ty) {
            Some(inner) => option_app_ty(mono_key_ty(&inner)),
            None => ty.clone(),
        },
        Ty::Sum { name, .. } if common::is_builtin_result_enum(name) => match result_ok_err(ty) {
            Some((ok, err)) => result_app_ty(mono_key_ty(&ok), mono_key_ty(&err)),
            None => ty.clone(),
        },
        Ty::Sum { name, .. } => Ty::Con(name.clone()),
        Ty::App(head, args) => Ty::App(Box::new(mono_key_ty(head)), rec(args)),
        Ty::Fun(a, b) => Ty::Fun(Box::new(mono_key_ty(a)), Box::new(mono_key_ty(b))),
        Ty::Tuple(items) => Ty::Tuple(rec(items)),
        Ty::Array { element, length } => Ty::Array {
            element: Box::new(mono_key_ty(element)),
            length: *length,
        },
        Ty::Record { fields } => Ty::Record {
            fields: fields.iter().map(|(n, t)| (n.clone(), mono_key_ty(t))).collect(),
        },
        // `readonly` is a type-checking qualifier with no runtime form, so
        // `f(x)` and `f(readonly x)` share one clone (#1555).
        Ty::Readonly(inner) => mono_key_ty(inner),
        Ty::Var(_) | Ty::Con(_) | Ty::Existential { .. } | Ty::Forall { .. } | Ty::Never => ty.clone(),
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

    /// Intern lookup used by monomorphize tests; codegen uses `intern.get`.
    #[cfg(test)]
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
                    .all(|(id, ty)| self.intern.get(*id) == Some(&mono_key_ty(ty)))
        })
    }

    /// The specialization of `fn_name` at type arguments `subst` (one per
    /// type parameter, in declaration order).
    pub fn specialization_for_subst(&self, fn_name: &str, subst: &[Ty]) -> Option<&MonoSpecialization> {
        self.specializations.iter().find(|spec| {
            spec.fn_name == fn_name
                && spec.key.subst.len() == subst.len()
                && spec.key.subst.iter().zip(subst).all(|(id, ty)| self.intern.get(*id) == Some(&mono_key_ty(ty)))
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
    /// For each formal: which type-parameter index it references (if any).
    param_type_params: Vec<Option<usize>>,
    /// Parallel to `param_type_params`: true when the formal is `T... name`.
    param_is_rest: Vec<bool>,
    /// A `let g = wrap;` local named for `wrap`, whose calls the HIR makes
    /// calls of `wrap` (#916).
    alias: bool,
}

#[derive(Clone, Debug)]
struct MonoCandidate {
    span: SimpleSpan,
    specialization: MonoSpecialization,
    /// A call through a `let g = wrap;` alias, which has no shared-body
    /// fallback past the caps (#916).
    value: bool,
}

/// Explicit monomorphize pass: after check, before emit.
///
/// Generic functions with ground type arguments specialize to a direct CALL.
/// Unbounded `id<T>` and bounded `add<T: Num>` both clone a ground body.
/// Dictionaries stay on the shared generic body when a type argument is still
/// a parameter (PolyFn / CallIndirect), or when a specialization cap is hit.
/// Keys are [`DefId`] + interned [`Ty`] ids from checker subst, not Display.
pub fn run_monomorphize_pass(module: &str, ast: &Output, checker: &Checker) -> MonoPlan {
    let mut intern = TyInterner::default();
    let mut sigs = HashMap::new();
    collect_generic_functions(module, ast, checker, &mut sigs);

    let mut escaped = BTreeSet::new();
    collect_escaped_generic_refs(ast, &sigs, false, &mut escaped);
    collect_generic_fn_aliases(ast, checker, &mut sigs);

    let mut candidates = Vec::new();
    collect_candidates(ast, checker, &|e| ground_ty(checker, e), &sigs, &mut intern, &mut candidates);
    collect_transitive(ast, checker, &sigs, &mut intern, &mut candidates);

    let (specializations, retargets, cap_hits) = apply_caps(candidates);
    MonoPlan {
        intern,
        specializations,
        retargets,
        escaped_generic_fns: escaped,
        cap_hits,
    }
}

/// The generic calls inside each planned clone's body whose arguments are
/// ground at the clone's type arguments (`fn twice<T>(T x) { wrap(x) }`
/// planned at `int` needs `wrap<int>`, #915): a clone calling the shared
/// body instead would hand back an `Option` around a boxed `T`.
fn collect_transitive(
    ast: &Output,
    checker: &Checker,
    sigs: &HashMap<String, GenericFnSig>,
    intern: &mut TyInterner,
    candidates: &mut Vec<MonoCandidate>,
) {
    let mut seen: BTreeSet<MonoKey> = BTreeSet::new();
    let mut work = Vec::new();
    for c in candidates.iter() {
        if seen.insert(c.specialization.key.clone()) {
            work.push(c.specialization.clone());
        }
    }
    while let Some(spec) = work.pop() {
        if seen.len() > MAX_TOTAL_SPECIALIZATIONS {
            break;
        }
        let Some(sig) = sigs.get(&spec.fn_name) else {
            continue;
        };
        let Some(params) = checker.fn_param_tys(&spec.fn_name) else {
            continue;
        };
        let mut map = HashMap::new();
        for (param, tp) in params.iter().zip(&sig.param_type_params) {
            if let (Ty::Var(v), Some(i)) = (apply_ty_prune(checker.subst(), param), tp)
                && let Some(ty) = spec.key.subst.get(*i).and_then(|id| intern.get(*id))
            {
                map.insert(v, ty.clone());
            }
        }
        // Type parameters no bare formal names, by declaration order.
        for (var, id) in checker
            .fn_type_param_vars(&spec.fn_name, spec.key.subst.len())
            .unwrap_or_default()
            .into_iter()
            .zip(&spec.key.subst)
        {
            if let Some(ty) = intern.get(*id) {
                map.entry(var).or_insert_with(|| ty.clone());
            }
        }
        if map.is_empty() {
            continue;
        }
        let ty_of = |e: &Output| ground_ty_at(checker, e, &map);
        let mut found = Vec::new();
        with_fn_body(ast, &spec.fn_name, &mut |body| {
            collect_candidates(body, checker, &ty_of, sigs, intern, &mut found)
        });
        for c in found {
            if seen.insert(c.specialization.key.clone()) {
                work.push(c.specialization.clone());
            }
            candidates.push(c);
        }
    }
}

/// [`ground_ty`] inside a generic body, its type parameters bound by `map`.
fn ground_ty_at(checker: &Checker, expr: &Output, map: &HashMap<crate::typechecking::ty::TyVarId, Ty>) -> Option<Ty> {
    match expr.1.as_ref() {
        Expression::NamedArg(_, inner)
        | Expression::Group(inner)
        | Expression::Expr(inner)
        | Expression::Statement(inner) => return ground_ty_at(checker, inner, map),
        _ => {}
    }
    if let Some(ty) = ground_ty(checker, expr) {
        return Some(ty);
    }
    let ty = checker.lookup_for_codegen_span(expr.0.start, expr.0.end)?;
    concrete_ty(&crate::Compiler::apply_ty_var_map(&ty, map))
}

/// Run `f` on the body of the function named `name`; whether it was found.
fn with_fn_body(node: &Output, name: &str, f: &mut dyn FnMut(&Output)) -> bool {
    if let Expression::Function { name: n, body, .. } = node.1.as_ref() {
        let Some(body) = body else {
            return false;
        };
        if *n == name {
            f(body);
            return true;
        }
        return with_fn_body(body, name, f);
    }
    let mut found = false;
    walk_children(node, &mut |child| {
        if !found {
            found = with_fn_body(child, name, f);
        }
    });
    found
}

/// Alias kept for in-crate tests.
#[cfg(test)]
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
            if !type_params.is_empty()
                && let Some(def_id) = checker
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
        param_type_params,
        param_is_rest,
        alias: false,
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
    ty_of: &dyn Fn(&Output) -> Option<Ty>,
    sigs: &HashMap<String, GenericFnSig>,
    intern: &mut TyInterner,
    out: &mut Vec<MonoCandidate>,
) {
    if let Expression::Call { name, args } = node.1.as_ref() {
        if let Expression::Identifier(fn_name) = name.1.as_ref()
            && let Some(sig) = sigs.get(*fn_name)
            && let Some(specialization) =
                candidate_for_call(fn_name, sig, node, args.as_deref(), checker, ty_of, intern)
        {
            out.push(MonoCandidate {
                span: node.0,
                specialization,
                value: sig.alias,
            });
        }
        // A call in callee position (`maker(4)()`, #1533) is a call too.
        if !matches!(name.1.as_ref(), Expression::Identifier(_)) {
            collect_candidates(name, checker, ty_of, sigs, intern, out);
        }
        if let Some(args) = args {
            for arg in args {
                collect_candidates(arg, checker, ty_of, sigs, intern, out);
            }
        }
        return;
    }

    walk_children(node, &mut |child| {
        collect_candidates(child, checker, ty_of, sigs, intern, out)
    });
}

/// `let g = wrap;` for a generic `wrap` with a type parameter nested in its
/// signature: `g` plans like `wrap`, since the HIR calls `wrap` by name
/// through it (`alias_generic_fn_calls`, #916). A shadowed or reassigned
/// `g` at worst plans a clone no call uses.
fn collect_generic_fn_aliases(node: &Output, checker: &Checker, sigs: &mut HashMap<String, GenericFnSig>) {
    if let Expression::Fragment(items) = node.1.as_ref()
        && let [decl, init] = items.as_slice()
        && let Expression::Variable(name, _) = decl.1.as_ref()
        && let Expression::Identifier(f) = peel(init).1.as_ref()
        && !sigs.contains_key(*name)
        && checker.generic_fn_value_nested(f)
        && let Some(sig) = sigs.get(*f)
    {
        let mut sig = sig.clone();
        sig.alias = true;
        sigs.insert(name.to_string(), sig);
    }
    walk_children(node, &mut |child| collect_generic_fn_aliases(child, checker, sigs));
}

fn peel<'a, 'e>(node: &'a Output<'e>) -> &'a Output<'e> {
    match node.1.as_ref() {
        Expression::Expr(inner) | Expression::Group(inner) | Expression::Statement(inner) => peel(inner),
        _ => node,
    }
}

fn candidate_for_call(
    fn_name: &str,
    sig: &GenericFnSig,
    call: &Output,
    args: Option<&[Output]>,
    checker: &Checker,
    ty_of: &dyn Fn(&Output) -> Option<Ty>,
    intern: &mut TyInterner,
) -> Option<MonoSpecialization> {
    if sig.type_params.is_empty() {
        return None;
    }
    // Argument and call types as keys name them, so `same(Option::Some(1),
    // Option::None)` binds `T` once (#1477).
    let ty_of = &|e: &Output| ty_of(e).map(|ty| mono_key_ty(&ty));

    // Ground calls specialize to a direct CALL. Dictionaries stay on the
    // shared generic body when a type argument is still a parameter.
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
            type_arg(arg_ty)?;
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
        let arg_ty = ty_of(arg)?;
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
                let t = ty_of(arg)?;
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

    // A type parameter no bare formal names (only inside `Vec<T>` or
    // `Option<T>`, or only in the result): bound by matching the signature
    // against the ground argument and call types.
    if !has_rest && subst.iter().any(Option::is_none) {
        bind_from_signature(fn_name, &mut subst, &fixed_args, call, checker, ty_of);
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

/// Fill the unbound entries of `subst` (one per type parameter of
/// `fn_name`) from its signature: each formal's type against its argument's
/// ground type, the result type against the call's. A parameter bound two
/// ways must agree; else it stays unbound.
fn bind_from_signature(
    fn_name: &str,
    subst: &mut [Option<Ty>],
    args: &[&Output],
    call: &Output,
    checker: &Checker,
    ty_of: &dyn Fn(&Output) -> Option<Ty>,
) {
    let (Some(vars), Some(params), Some(ret)) = (
        checker.fn_type_param_vars(fn_name, subst.len()),
        checker.fn_param_tys(fn_name),
        checker.fn_return_ty(fn_name),
    ) else {
        return;
    };
    // A function of no parameters is typed `unit -> T`.
    let params = if args.is_empty() && params == [Ty::Con("unit".into())] { Vec::new() } else { params };
    if params.len() != args.len() {
        return;
    }
    let mut map = HashMap::new();
    for (param, arg) in params.iter().zip(args) {
        if let Some(ty) = ty_of(arg) {
            crate::Compiler::bind_scheme_vars(param, &ty, &mut map);
        }
    }
    if let Some(ty) = ty_of(call) {
        crate::Compiler::bind_scheme_vars(&ret, &ty, &mut map);
    }
    for (slot, var) in subst.iter_mut().zip(vars) {
        if slot.is_none()
            && let Some(ty) = map.get(&var).and_then(type_arg)
        {
            *slot = Some(ty);
        }
    }
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

    if let Some(ty) = checker.lookup_for_codegen_span(expr.0.start, expr.0.end)
        && let Some(ty) = concrete_ty(&ty) {
            return Some(ty);
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

/// A ground type: no type variable left. A ground function type is one
/// (an argument `fn (int x) => ..` keys its call like any other).
fn concrete_ty(ty: &Ty) -> Option<Ty> {
    if contains_var(ty) {
        return None;
    }
    Some(ty.clone())
}

/// A type a type parameter is cloned at: ground, and not a function (a
/// clone over a function-typed `T` is left to the shared body).
fn type_arg(ty: &Ty) -> Option<Ty> {
    concrete_ty(ty).filter(|ty| !matches!(ty, Ty::Fun(..)))
}

fn contains_var(ty: &Ty) -> bool {
    match ty {
        Ty::Var(_) => true,
        Ty::Fun(a, b) => contains_var(a) || contains_var(b),
        Ty::App(head, args) => contains_var(head) || args.iter().any(contains_var),
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
        if (per_fn_hit || total_hit) && !candidate.value {
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
            contracts: _,
            iterable,
            body,
            identifier,
            pattern: _,
        } => {
            f(iterable);
            if let Some(identifier) = identifier {
                f(identifier);
            }
            f(body);
        }
        Expression::Match { scrutinee, arms } => {
            f(scrutinee);
            for arm in arms {
                f(&arm.body);
            }
        }
        Expression::IfLet {
            scrutinee,
            then_arm,
            else_arm,
        } => {
            f(scrutinee);
            f(&then_arm.body);
            f(&else_arm.body);
        }
        Expression::WhileLet {
            scrutinee,
            then_arm,
            on_miss,
        } => {
            f(scrutinee);
            f(&then_arm.body);
            f(&on_miss.body);
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
        Expression::Quote { parts, .. } => {
            for part in parts {
                if let parser::ast::QuotePart::Splice(e) | parser::ast::QuotePart::Repeat { list: e, .. } = part {
                    f(e);
                }
            }
        }
        Expression::AttrDecl {
            docs: _,
            args,
            returns,
            body,
            ..
        }
        | Expression::DeriveDecl {
            args,
            returns,
            body,
            ..
        }
        | Expression::FnMacroDecl {
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
        Expression::MacroCall { args, .. } => args.iter().for_each(f),
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
    fn plans_unbounded_id_as_direct_call() {
        let plan = plan("fn id<T>(T x) -> T { return x; } fn main() { id(1); }");
        assert_eq!(
            plan.specializations.len(),
            1,
            "ground unbounded helpers specialize to CALL: {plan:?}"
        );
        assert_eq!(plan.specializations[0].fn_name, "id");
        assert_eq!(
            plan.ty(plan.specializations[0].key.subst[0]),
            Some(&Ty::Con("int".into()))
        );
    }

    #[test]
    fn plans_user_trait_ground_call() {
        let plan = plan(
            "trait Describable<T> { fn describe_val(T x) -> int; } \
             impl Describable for int { pub fn describe_val(int x) -> int { return x; } } \
             fn show<T: Describable>(T x) -> int { return x.describe_val(); } \
             fn main() { show(42); }",
        );
        assert_eq!(
            plan.specializations.len(),
            1,
            "ground user-trait calls specialize to CALL: {plan:?}"
        );
    }

    #[test]
    fn plans_show_or_length_ground_call() {
        let show = plan("fn show_it<T: Show>(T x) -> T { return x; } fn main() { show_it(1); }");
        assert_eq!(
            show.specializations.len(),
            1,
            "Show ground calls specialize: {show:?}"
        );
        let length = plan("fn n<T: Length>(T x) -> int { return 0; } fn main() { n(\"ab\"); }");
        assert_eq!(
            length.specializations.len(),
            1,
            "Length ground calls specialize: {length:?}"
        );
    }

    #[test]
    fn plans_when_num_mixed_with_show_or_user_trait() {
        let with_show = plan(
            "fn mix<T: Num + Show>(T a, T b) -> T { return a + b; } \
             fn main() { mix(1, 2); }",
        );
        assert_eq!(
            with_show.specializations.len(),
            1,
            "Num+Show ground calls specialize: {with_show:?}"
        );

        let with_user = plan(
            "trait Tagged<T> { fn tag(T x) -> int; } \
             impl Tagged for int { pub fn tag(int x) -> int { return x; } } \
             fn mix<T: Num + Tagged>(T a, T b) -> T { return a + b; } \
             fn main() { mix(1, 2); }",
        );
        assert_eq!(
            with_user.specializations.len(),
            1,
            "Num+user-trait ground calls specialize: {with_user:?}"
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
    fn variant_constructor_args_share_the_enum_clone() {
        // `Option::Some(1)`, a typed `Option::None` and an `Option<int>`
        // value are one instantiation of `id`: one clone, keyed by the
        // applied enum type, not the variant's constructor type (#1477).
        let plan = plan(
            "fn id<T>(T x) -> T { return x; } \
             fn main() { \
                 let a = id(Option::Some(1)); \
                 let b: Option<int> = id(Option::None); \
                 let x: Option<int> = Option::Some(2); \
                 let c = id(x); \
             }",
        );
        let ids: Vec<_> = plan.specializations_for_fn("id").collect();
        assert_eq!(ids.len(), 1, "{:?}", ids.iter().map(|s| plan.ty(s.key.subst[0])).collect::<Vec<_>>());
        assert_eq!(
            plan.ty(ids[0].key.subst[0]),
            Some(&Ty::App(Box::new(Ty::Con("Option".into())), vec![Ty::Con("int".into())]))
        );
        // All three calls go to that clone.
        assert_eq!(plan.retargets.len(), 3);
        assert!(plan.retargets.iter().all(|r| r.key == ids[0].key));
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
            value: false,
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
