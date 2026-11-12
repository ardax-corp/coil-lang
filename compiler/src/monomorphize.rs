//! Monomorphization planning for ground generic call sites.
//!
//! This module is intentionally analysis-first. It decides which generic calls
//! are safe and small enough to specialize; `Compiler` owns the code
//! emission for each accepted specialization.

use std::collections::{BTreeSet, HashMap};

use parser::{
    SimpleSpan,
    ast::{Expression, Output, TypeParam},
};

use crate::typechecking::{Checker, DefId, Ty};
use crate::typechecking::subst::apply_ty_prune;

/// Default backstop on the clones one module's plan (and, separately, a
/// program's instance and foreign clones) make: `--clone-budget`. Real code
/// stays far below it; what reaches it is fan-out through many generic
/// functions at distinct types. Unbounded growth is cut earlier by the
/// recursion guard in [`collect_transitive`].
pub const MAX_TOTAL_SPECIALIZATIONS: usize = 4096;

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

    /// The id `ty` was interned under, if it was.
    fn id_of(&self, ty: &Ty) -> Option<TyId> {
        self.ids.get(&mono_key_ty(ty)).copied()
    }
}

/// The type a mono key names for `ty`: an enum value's type as its applied
/// enum type, as an annotation spells it. A variant's constructor type
/// (`Option::Some(1)`, `Pick::Zero`) is its owner's, and a structural sum
/// is the enum's name, so `f(Option::Some(1))`, `f(Option::None)` at
/// `Option<int>` and `f(x)` with `Option<int> x` share one clone (#1477).
pub fn mono_key_ty(ty: &Ty) -> Ty {
    match ty {
        Ty::Constructor { owner, .. } => mono_key_ty(owner),
        Ty::Sum { name, .. } => Ty::Con(name.clone()),
        // `readonly` is a type-checking qualifier with no runtime form, so
        // `f(x)` and `f(readonly x)` share one clone (#1555).
        Ty::Readonly(inner) => mono_key_ty(inner),
        Ty::Forall { .. } => ty.clone(),
        _ => ty.map_children(mono_key_ty),
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
    pub reason: MonoCapReason,
}

/// Why a ground call takes the shared generic body (E0804).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MonoCapReason {
    /// The clone would repeat a function already on its own chain of clones
    /// at other types: polymorphic recursion, whose types grow without end.
    Recursive,
    /// The clone budget (`--clone-budget`, [`MAX_TOTAL_SPECIALIZATIONS`]) reached.
    Budget,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MonoPlan {
    pub intern: TyInterner,
    /// In plan order; set once, with [`Self::index`] built from it.
    specializations: Vec<MonoSpecialization>,
    index: MonoIndex,
    pub retargets: Vec<MonoRetarget>,
    pub escaped_generic_fns: BTreeSet<String>,
    pub cap_hits: Vec<MonoCapHit>,
    /// A keyed generic function (one that calls `symbol::fn_key()`) read
    /// as a value: its shared body has no type arguments for the key.
    pub keyed_values: Vec<(String, parser::SimpleSpan)>,
}

/// [`MonoPlan::specializations`] positions by what lookups ask for, each
/// list ascending; a map to one position keeps the first.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct MonoIndex {
    by_fn: HashMap<String, Vec<usize>>,
    by_def: HashMap<DefId, Vec<usize>>,
    by_subst: HashMap<(String, Vec<TyId>), usize>,
    by_args: HashMap<(String, Vec<TyId>), usize>,
}

impl MonoIndex {
    fn of(specializations: &[MonoSpecialization]) -> Self {
        let mut index = Self::default();
        for (i, spec) in specializations.iter().enumerate() {
            index.by_fn.entry(spec.fn_name.clone()).or_default().push(i);
            index.by_def.entry(spec.key.def_id).or_default().push(i);
            index.by_subst.entry((spec.fn_name.clone(), spec.key.subst.clone())).or_insert(i);
            index.by_args.entry((spec.fn_name.clone(), spec.arg_types.clone())).or_insert(i);
        }
        index
    }
}

impl MonoPlan {
    pub fn is_empty(&self) -> bool {
        self.specializations.is_empty()
    }

    /// The spec at `index[key]`, the key's types interned.
    fn spec_at(&self, index: &HashMap<(String, Vec<TyId>), usize>, fn_name: &str, tys: &[Ty]) -> Option<&MonoSpecialization> {
        let ids = tys.iter().map(|ty| self.intern.id_of(ty)).collect::<Option<Vec<_>>>()?;
        index.get(&(fn_name.to_string(), ids)).map(|&i| &self.specializations[i])
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
        self.spec_at(&self.index.by_args, fn_name, arg_types)
    }

    /// The specialization of `fn_name` at type arguments `subst` (one per
    /// type parameter, in declaration order).
    pub fn specialization_for_subst(&self, fn_name: &str, subst: &[Ty]) -> Option<&MonoSpecialization> {
        self.spec_at(&self.index.by_subst, fn_name, subst)
    }

    pub fn specializations_for_fn<'a>(
        &'a self,
        fn_name: &'a str,
    ) -> impl Iterator<Item = &'a MonoSpecialization> + 'a {
        let list = self.index.by_fn.get(fn_name).map_or(&[][..], Vec::as_slice);
        list.iter().map(|&i| &self.specializations[i])
    }

    pub fn specializations_for_def(&self, def_id: DefId) -> impl Iterator<Item = &MonoSpecialization> {
        let list = self.index.by_def.get(&def_id).map_or(&[][..], Vec::as_slice);
        list.iter().map(|&i| &self.specializations[i])
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
    /// Names the body calls by a bare identifier.
    calls: BTreeSet<String>,
    /// The body calls `symbol::fn_key()`, or a keyed generic function: its
    /// shared body has no key, so every ground call gets its clone, past
    /// the clone budget.
    keyed: bool,
    /// `#[hot]`, or called by name from a hot function, and not `#[cold]`:
    /// every ground call gets its clone, past the clone budget
    /// (`docs/internals/opt-hints.md`).
    hot: bool,
    /// A parameter or the result holds an open `Option` / `Result` below
    /// its top (`Vec<Option<T>>`): the shared body and ground code hold
    /// that value differently where it is a niche word, so every ground
    /// call gets its clone, past the clone budget (#2441,
    /// `docs/internals/layout.md`, "Open enums"). The same for a function
    /// below the top whose result is a bare type parameter
    /// (`Vec<unit -> T>`), which no adapter reaches at `T = ()` (#2498).
    /// And for a generator over its type parameters
    /// ([`crate::layout::open_coroutine`]), or a function calling one by
    /// name: its shared body cannot make a value nobody produced (#2738).
    split: bool,
    /// A `gen fn`: a read of it as a value at ground types makes its
    /// coroutine through an adapter calling its clone (#2747).
    coro: bool,
}

#[derive(Clone, Debug)]
struct MonoCandidate {
    span: SimpleSpan,
    specialization: MonoSpecialization,
    /// A call through a `let g = wrap;` alias (#916), or of a keyed generic
    /// function, which has no shared-body fallback past the clone budget.
    value: bool,
    /// Found in a clone whose chain of clones already holds this function
    /// at other types (polymorphic recursion): no clone of its own.
    recursive: bool,
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
    mark_keyed(checker, &mut sigs);
    mark_hot(ast, &mut sigs);
    mark_open_coroutines(checker, &mut sigs);

    let mut candidates = Vec::new();
    collect_candidates(ast, checker, &|e| ground_ty(checker, e), &sigs, &mut intern, &mut candidates);
    let budget = checker.host_grants().clone_budget();
    collect_transitive(ast, checker, &sigs, &mut intern, &mut candidates, budget);

    let (specializations, retargets, cap_hits) = apply_caps(candidates, budget);
    let mut keyed_values = Vec::new();
    collect_keyed_values(ast, &sigs, false, &mut keyed_values);
    MonoPlan {
        intern,
        index: MonoIndex::of(&specializations),
        specializations,
        retargets,
        escaped_generic_fns: escaped,
        cap_hits,
        keyed_values,
    }
}

/// The generic calls inside each planned clone's body whose arguments are
/// ground at the clone's type arguments (`fn twice<T>(T x) { wrap(x) }`
/// planned at `int` needs `wrap<int>`, #915): a clone calling the shared
/// body instead would hand back an `Option` around a boxed `T`.
///
/// Each clone carries the functions on its chain of clones. A call to one of
/// them at new types is polymorphic recursion (`f<T>` calling `g<(T, int)>`
/// calling `f<((T, int), int)>`), whose types grow without end: it is marked
/// `recursive` and keeps the shared body, so the walk is bounded by the
/// number of generic functions along any chain.
fn collect_transitive(
    ast: &Output,
    checker: &Checker,
    sigs: &HashMap<String, GenericFnSig>,
    intern: &mut TyInterner,
    candidates: &mut Vec<MonoCandidate>,
    budget: usize,
) {
    let mut seen: BTreeSet<MonoKey> = BTreeSet::new();
    let mut work = Vec::new();
    for c in candidates.iter() {
        if seen.insert(c.specialization.key.clone()) {
            work.push((c.specialization.clone(), vec![c.specialization.key.def_id]));
        }
    }
    while let Some((spec, chain)) = work.pop() {
        let Some(sig) = sigs.get(&spec.fn_name) else {
            continue;
        };
        if seen.len() > budget && !sig.hot && !sig.split {
            continue;
        }
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
        for mut c in found {
            let key = &c.specialization.key;
            if !seen.contains(key) {
                if chain.contains(&key.def_id) {
                    c.recursive = true;
                } else {
                    seen.insert(key.clone());
                    let mut next = chain.clone();
                    next.push(key.def_id);
                    work.push((c.specialization.clone(), next));
                }
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
/// A method of that name is not it (#2548).
fn with_fn_body(node: &Output, name: &str, f: &mut dyn FnMut(&Output)) -> bool {
    let mut found = false;
    match node.1.as_ref() {
        Expression::Function { name: n, body, .. } => {
            let Some(body) = body else {
                return false;
            };
            if *n == name {
                f(body);
                return true;
            }
            return with_fn_body(body, name, f);
        }
        Expression::Implementation { .. } | Expression::TypeClass { .. } | Expression::TypeClassImpl { .. } => {
            for_each_method_body(node, &mut |body| {
                if !found {
                    found = with_fn_body(body, name, f);
                }
            });
        }
        _ => walk_children(node, &mut |child| {
            if !found {
                found = with_fn_body(child, name, f);
            }
        }),
    }
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
            is_coro,
            ..
        } => {
            if !type_params.is_empty()
                && let Some(def_id) = checker
                    .def_id_of(name)
                    .or_else(|| checker.interned_def(module, name))
                {
                    let mut sig = signature_from_function(type_params, args);
                    if let Some(body) = body {
                        call_names(body, &mut sig.calls);
                    }
                    sig.def_id = def_id;
                    sig.fn_name = (*name).to_string();
                    sig.coro = *is_coro && body.is_some();
                    sig.split = checker
                        .fn_param_tys(name)
                        .into_iter()
                        .flatten()
                        .chain(checker.fn_return_ty(name))
                        .any(|ty| {
                            crate::layout::nested_open_enum(checker, &ty, true, &crate::layout::nested_open_look(checker), &mut |_| true)
                                || crate::layout::nested_fn_var_result(&ty, true, &mut |_| true)
                                || crate::layout::nested_fn_field(checker, &ty, &crate::layout::nested_open_look(checker))
                        })
                        || checker.fn_return_ty(name).is_some_and(|ty| crate::layout::nested_fn_in_result(&ty, true));
                    sigs.insert(name.to_string(), sig.clone());
                    if !module.is_empty() {
                        sigs.insert(format!("{module}::{name}"), sig);
                    }
                }
            if let Some(body) = body {
                collect_generic_functions(module, body, checker, sigs);
            }
        }
        // A method is no free function: its bare name may be one's, whose
        // sig it must not replace (#2548). Its body may call or declare one.
        Expression::Implementation { .. } | Expression::TypeClass { .. } | Expression::TypeClassImpl { .. } => {
            for_each_method_body(node, &mut |body| collect_generic_functions(module, body, checker, sigs));
        }
        _ => walk_children(node, &mut |child| {
            collect_generic_functions(module, child, checker, sigs)
        }),
    }
}

/// Run `f` on the body of each method of the `impl` / trait block `node`.
fn for_each_method_body(node: &Output, f: &mut dyn FnMut(&Output)) {
    walk_children(node, &mut |child| {
        let child = match child.1.as_ref() {
            Expression::Method(_, inner) => inner,
            _ => child,
        };
        match child.1.as_ref() {
            Expression::Function { body: Some(body), .. } => f(body),
            Expression::Function { body: None, .. } => {}
            _ => f(child),
        }
    });
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
        calls: BTreeSet::new(),
        keyed: false,
        hot: false,
        split: false,
        coro: false,
    }
}

/// Every name `node` calls by a bare identifier (lambdas included).
fn call_names(node: &Output, out: &mut BTreeSet<String>) {
    if let Expression::Call { name, .. } = node.1.as_ref()
        && let Expression::Identifier(n) = name.1.as_ref()
    {
        out.insert((*n).to_string());
    }
    walk_children(node, &mut |child| call_names(child, out));
}

/// The generic functions in `seed`, and those calling one of them by name,
/// directly or through another.
fn callers_of(sigs: &HashMap<String, GenericFnSig>, mut seed: BTreeSet<String>) -> BTreeSet<String> {
    loop {
        let more: Vec<String> = sigs
            .iter()
            .filter(|(name, sig)| !seed.contains(*name) && sig.calls.iter().any(|c| seed.contains(c)))
            .map(|(name, _)| name.clone())
            .collect();
        if more.is_empty() {
            return seed;
        }
        seed.extend(more);
    }
}

/// Mark the generic functions that call `symbol::fn_key()`, directly or
/// through another keyed generic function.
fn mark_keyed(checker: &Checker, sigs: &mut HashMap<String, GenericFnSig>) {
    let fn_key = checker
        .prelude_fn_in_scope("fn_key")
        .is_some_and(|k| k == crate::typechecking::PreludeFn::FnKey);
    let seed = sigs
        .iter()
        .filter(|(_, sig)| fn_key && sig.calls.contains("fn_key"))
        .map(|(name, _)| name.clone())
        .collect();
    for name in callers_of(sigs, seed) {
        if let Some(sig) = sigs.get_mut(&name) {
            sig.keyed = true;
        }
    }
}

/// Mark clone-only ([`GenericFnSig::split`]) the generic generators over
/// their type parameters, and the generic functions calling one by name: a
/// shared body there would make the value nobody produced from a word
/// (#2738).
fn mark_open_coroutines(checker: &Checker, sigs: &mut HashMap<String, GenericFnSig>) {
    let seed = sigs
        .keys()
        .filter(|name| checker.fn_return_ty(name).is_some_and(|ty| crate::layout::open_coroutine(&apply_ty_prune(checker.subst(), &ty))))
        .cloned()
        .collect();
    for name in callers_of(sigs, seed) {
        if let Some(sig) = sigs.get_mut(&name) {
            sig.split = true;
        }
    }
}

/// Mark the generic functions that are hot: `#[hot]` ones, and ones a
/// `#[hot]` function (generic or not) calls by name. A `#[cold]` one
/// stays cold.
fn mark_hot(ast: &Output, sigs: &mut HashMap<String, GenericFnSig>) {
    fn walk(node: &Output, hot: &mut BTreeSet<String>, cold: &mut BTreeSet<String>) {
        if let Expression::Function { attrs, name, body, .. } = node.1.as_ref() {
            if attrs.iter().any(|a| a.name == "cold") {
                cold.insert((*name).to_string());
            } else if attrs.iter().any(|a| a.name == "hot") {
                hot.insert((*name).to_string());
                if let Some(body) = body {
                    call_names(body, hot);
                }
            }
        }
        // A method is no free function: a hint on it must not reach a free
        // function of the same bare name (#2570). What a hot method calls by
        // name stays hot, and its body may declare functions.
        if matches!(node.1.as_ref(), Expression::Implementation { .. } | Expression::TypeClass { .. } | Expression::TypeClassImpl { .. }) {
            walk_children(node, &mut |child| {
                let child = match child.1.as_ref() {
                    Expression::Method(_, inner) => inner,
                    _ => child,
                };
                match child.1.as_ref() {
                    Expression::Function { attrs, body, .. } => {
                        if let Some(body) = body {
                            if attrs.iter().any(|a| a.name == "hot") && !attrs.iter().any(|a| a.name == "cold") {
                                call_names(body, hot);
                            }
                            walk(body, hot, cold);
                        }
                    }
                    _ => walk(child, hot, cold),
                }
            });
            return;
        }
        walk_children(node, &mut |child| walk(child, hot, cold));
    }
    let (mut hot, mut cold) = (BTreeSet::new(), BTreeSet::new());
    walk(ast, &mut hot, &mut cold);
    for (name, sig) in sigs.iter_mut() {
        let bare = name.rsplit("::").next().unwrap_or(name);
        sig.hot = !cold.contains(bare) && (hot.contains(name.as_str()) || hot.contains(bare));
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
                value: sig.alias || sig.keyed || sig.hot || sig.split,
                recursive: false,
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

    // A generic generator read as a value at ground types (#2747): the
    // value's adapter calls its clone at those types, planned here as a
    // call's is, past the clone budget as an alias's call is.
    if let Expression::Identifier(fn_name) = node.1.as_ref()
        && let Some(sig) = sigs.get(*fn_name).filter(|sig| sig.coro && !sig.alias)
        && let Some(specialization) = candidate_for_value(fn_name, sig, node, checker, ty_of, intern)
    {
        out.push(MonoCandidate {
            span: node.0,
            specialization,
            value: true,
            recursive: false,
        });
        return;
    }

    walk_children(node, &mut |child| {
        collect_candidates(child, checker, ty_of, sigs, intern, out)
    });
}

/// The clone a read of generic `fn_name` as a value at the ground function
/// type of `node` takes: its type arguments bound from that type's
/// parameters and result.
fn candidate_for_value(
    fn_name: &str,
    sig: &GenericFnSig,
    node: &Output,
    checker: &Checker,
    ty_of: &dyn Fn(&Output) -> Option<Ty>,
    intern: &mut TyInterner,
) -> Option<MonoSpecialization> {
    if sig.type_params.is_empty() || sig.param_is_rest.iter().any(|&r| r) {
        return None;
    }
    let mut read = mono_key_ty(&ty_of(node)?);
    let arity = sig.param_type_params.len();
    let mut arg_types = Vec::with_capacity(arity);
    for _ in 0..arity.max(1) {
        let Ty::Fun(param, ret) = read else {
            return None;
        };
        arg_types.push(*param);
        read = *ret;
    }
    if arity == 0 {
        arg_types.clear();
    }
    let vars = checker.fn_type_param_vars(fn_name, sig.type_params.len())?;
    let (params, ret) = checker.fn_call_sig(fn_name, arity)?;
    if params.len() != arg_types.len() {
        return None;
    }
    let mut map = HashMap::new();
    for (param, ty) in params.iter().zip(&arg_types) {
        crate::Compiler::bind_scheme_vars(param, ty, &mut map);
    }
    crate::Compiler::bind_scheme_vars(&ret, &read, &mut map);
    let subst = vars.iter().map(|v| map.get(v).and_then(type_arg)).collect::<Option<Vec<_>>>()?;
    let subst_ids: Vec<TyId> = subst.into_iter().map(|ty| intern.intern(ty)).collect();
    let arg_ids: Vec<TyId> = arg_types.into_iter().map(|ty| intern.intern(ty)).collect();
    Some(MonoSpecialization {
        key: MonoKey {
            def_id: sig.def_id,
            subst: subst_ids,
        },
        fn_name: if sig.fn_name.is_empty() { fn_name.to_string() } else { sig.fn_name.clone() },
        arg_types: arg_ids,
    })
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

    // `f(...t)` / `f(1, ...[2, 3])` (#1680): each spread's items in place,
    // as codegen flattens them (`hir_flatten_spreads`).
    let spread = |a: &&Output| matches!(peel(a).1.as_ref(), Expression::Spread(_));
    let fixed_tys: Vec<Option<Ty>> = if !has_rest && fixed_args.iter().any(spread) {
        spread_arg_tys(&fixed_args, ty_of)?
    } else {
        fixed_args.iter().map(|a| ty_of(a)).collect()
    };
    if !has_rest {
        if fixed_tys.len() != sig.param_type_params.len() {
            return None;
        }
    } else if fixed_tys.len() < fixed_count {
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
    for (arg_ty, tp_idx) in fixed_tys.iter().zip(fixed_tps.iter()) {
        let arg_ty = arg_ty.clone()?;
        bind(&mut subst, *tp_idx, &arg_ty)?;
        arg_types.push(arg_ty);
    }

    if pack_rest {
        // One key slot per rest formal: the *element* ground type (not the
        // packed `[T]` / `[T; N]`), matching `mono_call_offset`.
        let rest_tp = sig.param_type_params.get(fixed_count).copied().flatten();
        // `f(...vec)` into a rest (#1728): the `Vec` is the pack, as
        // codegen takes it (`hir_call_shapes`), so its element binds.
        let whole = match rest_args.as_slice() {
            [arg] => match peel(arg).1.as_ref() {
                Expression::Spread(inner) => ty_of(inner).as_ref().and_then(crate::typechecking::ty::vec_element_ty).cloned(),
                _ => None,
            },
            _ => None,
        };
        if let Some(elem) = whole {
            bind(&mut subst, rest_tp, &elem)?;
            arg_types.push(elem);
        } else if rest_args.is_empty() {
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
    // A `let g = wrap;` alias's signature is `wrap`'s (#2447).
    if !has_rest && subst.iter().any(Option::is_none) {
        let name = if sig.alias { sig.fn_name.as_str() } else { fn_name };
        bind_from_signature(name, &mut subst, &fixed_tys, call, checker, ty_of);
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
    arg_tys: &[Option<Ty>],
    call: &Output,
    checker: &Checker,
    ty_of: &dyn Fn(&Output) -> Option<Ty>,
) {
    // A returned closure's parameters are the result's (#1590).
    let (Some(vars), Some((params, ret))) = (
        checker.fn_type_param_vars(fn_name, subst.len()),
        checker.fn_call_sig(fn_name, arg_tys.len()),
    ) else {
        return;
    };
    if params.len() != arg_tys.len() {
        return;
    }
    let mut map = HashMap::new();
    for (param, ty) in params.iter().zip(arg_tys) {
        if let Some(ty) = ty {
            crate::Compiler::bind_scheme_vars(param, ty, &mut map);
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

/// The argument types of `args` with each spread flattened: a tuple or
/// array literal's items, or a tuple value's fields. `None` for a spread of
/// anything else.
fn spread_arg_tys(args: &[&Output], ty_of: &dyn Fn(&Output) -> Option<Ty>) -> Option<Vec<Option<Ty>>> {
    let mut tys = Vec::with_capacity(args.len());
    for arg in args {
        let Expression::Spread(inner) = peel(arg).1.as_ref() else {
            tys.push(ty_of(arg));
            continue;
        };
        match peel(inner).1.as_ref() {
            Expression::Tuple(items) | Expression::Array(items) => tys.extend(items.iter().map(ty_of)),
            _ => match ty_of(inner)? {
                Ty::Tuple(items) => tys.extend(items.into_iter().map(Some)),
                _ => return None,
            },
        }
    }
    Some(tys)
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
    if ty.has_var() {
        return None;
    }
    Some(ty.clone())
}

/// A type a type parameter is cloned at: ground, and not a function (a
/// clone over a function-typed `T` is left to the shared body).
fn type_arg(ty: &Ty) -> Option<Ty> {
    concrete_ty(ty).filter(|ty| !matches!(ty, Ty::Fun(..)))
}


fn apply_caps(
    candidates: Vec<MonoCandidate>,
    budget: usize,
) -> (
    Vec<MonoSpecialization>,
    Vec<MonoRetarget>,
    Vec<MonoCapHit>,
) {
    let mut seen = BTreeSet::new();
    let mut specializations = Vec::new();
    let mut retargets = Vec::new();
    let mut cap_hits = Vec::new();

    // A recursive call whose types another path clones anyway calls that
    // clone, so recursive candidates go last.
    let (plain, recursive): (Vec<_>, Vec<_>) = candidates.into_iter().partition(|c| !c.recursive);
    for candidate in plain.into_iter().chain(recursive) {
        let key = candidate.specialization.key.clone();
        if seen.contains(&key) {
            retargets.push(MonoRetarget {
                call_span_start: candidate.span.start,
                key,
            });
            continue;
        }

        let reason = if candidate.recursive {
            Some(MonoCapReason::Recursive)
        } else if specializations.len() >= budget {
            Some(MonoCapReason::Budget)
        } else {
            None
        };
        if let Some(reason) = reason
            && !candidate.value
        {
            cap_hits.push(MonoCapHit {
                call_span: candidate.span,
                fn_name: candidate.specialization.fn_name.clone(),
                reason,
            });
            continue;
        }

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

/// Each read of a keyed generic function that is not a call's callee.
fn collect_keyed_values(
    node: &Output,
    sigs: &HashMap<String, GenericFnSig>,
    is_call_target: bool,
    out: &mut Vec<(String, SimpleSpan)>,
) {
    match node.1.as_ref() {
        Expression::Identifier(name) if !is_call_target && sigs.get(*name).is_some_and(|s| s.keyed) => {
            out.push((name.to_string(), node.0));
        }
        Expression::Call { name, args } => {
            collect_keyed_values(name, sigs, true, out);
            if let Some(args) = args {
                for arg in args {
                    collect_keyed_values(arg, sigs, false, out);
                }
            }
        }
        _ => walk_children(node, &mut |child| collect_keyed_values(child, sigs, false, out)),
    }
}

fn walk_children<F>(node: &Output, f: &mut F)
where
    F: FnMut(&Output),
{
    // Walks recurse through here once per level of the tree.
    common::deep_stack(|| visit_children(node, f));
}

fn visit_children<F>(node: &Output, f: &mut F)
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
        | Expression::Comptime(e)
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
        // Expanded before this runs; its body is the expansion program's.
        Expression::ComptimeBlock(_) | Expression::RegionMark => {}
        Expression::TypeApp { args, .. } => {
            for arg in args {
                f(arg);
            }
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
            parser::ast::EnumConstructPayload::Unit | parser::ast::EnumConstructPayload::Empty => {}
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
        | Expression::Constant(..)
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
        Expression::TypeFn { params, ret, .. } => {
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

    /// Whether the planner marks the generic function `name` hot.
    fn hot_of(src: &str, name: &str) -> bool {
        let ast = Pratt::default().parse(src).expect("parse failed");
        let mut checker = Checker::new();
        let _ = checker.check_program(&ast);
        let mut sigs = HashMap::new();
        collect_generic_functions("", &ast, &checker, &mut sigs);
        mark_hot(&ast, &mut sigs);
        sigs.get(name).unwrap_or_else(|| panic!("no generic fn {name}")).hot
    }

    #[test]
    fn a_hot_method_does_not_heat_a_same_named_free_function() {
        let free = "fn wrap<T>(T a) -> int { return 2; } fn side<T>(T a) -> int { return 3; } ";
        let hot_method = "class K { pub k: int, } impl K { #[hot] pub fn wrap<T>(T a) -> int { return side(a); } } ";
        assert!(!hot_of(&format!("{free}{hot_method}"), "wrap"));
        assert!(!hot_of(&format!("{hot_method}{free}"), "wrap"));
        // What a hot method calls by name is hot, as a hot function's.
        assert!(hot_of(&format!("{free}{hot_method}"), "side"));
        // A cold method does not cool a free function that is hot.
        let cold_method = "class K { pub k: int, } impl K { #[cold] pub fn wrap<T>(T a) -> int { return 1; } } ";
        assert!(hot_of(&format!("#[hot] fn wrap<T>(T a) -> int {{ return 2; }} {cold_method}"), "wrap"));
        // The free function's own hint still marks it.
        assert!(hot_of("#[hot] fn wrap<T>(T a) -> int { return 2; } ", "wrap"));
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

    #[test]
    fn plans_one_clone_per_ground_type_past_eight() {
        let classes: String = (0..20).map(|i| format!("class C{i} {{ pub v: int, }} ")).collect();
        let calls: String = (0..20).map(|i| format!("id(new C{i}({i})); ")).collect();
        let plan = plan(&format!("{classes} fn id<T>(T x) -> T {{ return x; }} fn main() {{ {calls} }}"));
        assert_eq!(plan.specializations.len(), 20, "{plan:?}");
        assert!(plan.cap_hits.is_empty(), "{:?}", plan.cap_hits);
    }

    #[test]
    fn mutual_polymorphic_recursion_stops_at_the_first_repeat() {
        // `f<int>` -> `g<(int, int)>` -> `f<((int, int), int)>` -> ... grows
        // without end; the repeat of `f` on its own chain takes the shared body.
        let plan = plan(
            "fn f<T>(T x, int n) -> int { if n == 0 { return 0; } return 1 + g((x, 1), n - 1); } \
             fn g<T>(T x, int n) -> int { if n == 0 { return 0; } return 1 + f((x, 2), n - 1); } \
             fn main() { f(1, 20); }",
        );
        let names: Vec<_> = plan.specializations.iter().map(|s| s.fn_name.as_str()).collect();
        assert_eq!(names, ["f", "g"], "{plan:?}");
        assert_eq!(plan.cap_hits.len(), 1, "{:?}", plan.cap_hits);
        assert_eq!(plan.cap_hits[0].fn_name, "f");
    }

    fn dummy_candidate(i: usize, def: u32, fn_name: &str) -> MonoCandidate {
        MonoCandidate {
            value: false,
            recursive: false,
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
    fn one_function_has_no_count_cap() {
        let candidates = (0..100).map(|i| dummy_candidate(i, 1, "f")).collect();

        let (specializations, retargets, cap_hits) = apply_caps(candidates, MAX_TOTAL_SPECIALIZATIONS);
        assert_eq!(specializations.len(), 100);
        assert_eq!(retargets.len(), 100);
        assert!(cap_hits.is_empty());
    }

    #[test]
    fn recursive_candidate_calls_a_clone_another_path_made() {
        let mut rec = dummy_candidate(1, 1, "f");
        rec.recursive = true;
        let fresh = {
            let mut c = dummy_candidate(2, 1, "f");
            c.recursive = true;
            c
        };
        let candidates = vec![rec, dummy_candidate(1, 1, "f"), fresh];

        let (specializations, retargets, cap_hits) = apply_caps(candidates, MAX_TOTAL_SPECIALIZATIONS);
        assert_eq!(specializations.len(), 1);
        assert_eq!(retargets.len(), 2);
        assert_eq!(cap_hits.len(), 1);
        assert_eq!(cap_hits[0].reason, MonoCapReason::Recursive);
    }

    #[test]
    fn total_cap_limits_specializations() {
        let candidates = (0..(MAX_TOTAL_SPECIALIZATIONS + 2))
            .map(|i| dummy_candidate(i, i as u32 + 1, &format!("f{i}")))
            .collect();

        let (specializations, retargets, cap_hits) = apply_caps(candidates, MAX_TOTAL_SPECIALIZATIONS);
        assert_eq!(specializations.len(), MAX_TOTAL_SPECIALIZATIONS);
        assert_eq!(retargets.len(), MAX_TOTAL_SPECIALIZATIONS);
        assert_eq!(cap_hits.len(), 2);
        assert!(cap_hits.iter().all(|h| h.reason == MonoCapReason::Budget));
    }
}
