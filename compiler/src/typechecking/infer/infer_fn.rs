//! Function-expression inference. `infer_inner` remains the dispatcher.

use std::ops::Range;

use parser::ast::{Contract, ContractKind, Expression, Output, TypeParam};
use reporting::{ErrorCode, Message};

use crate::typechecking::ty::{Scheme, Ty, unit as unit_ty};

use super::*;

pub(super) struct InferFunctionExprArgs<'a> {
    pub(super) attrs: &'a [parser::ast::Attribute<'a>],
    pub(super) name: &'a str,
    pub(super) is_coro: bool,
    pub(super) is_static: bool,
    pub(super) type_params: &'a [TypeParam<'a>],
    pub(super) args: &'a Output<'a>,
    pub(super) returns: &'a Option<Output<'a>>,
    pub(super) where_constraints: &'a [parser::ast::WhereConstraint<'a>],
    pub(super) contracts: &'a [Contract<'a>],
    pub(super) body: &'a Option<Output<'a>>,
    pub(super) range: Range<usize>,
}

pub(super) struct InferFunctionArgs<'a> {
    pub name: &'a str,
    pub type_params: &'a [parser::ast::TypeParam<'a>],
    pub args: &'a Output<'a>,
    pub returns: Option<&'a Output<'a>>,
    pub where_constraints: &'a [parser::ast::WhereConstraint<'a>],
    pub body: Option<&'a Output<'a>>,
    pub range: &'a Range<usize>,
    pub self_ty: Option<&'a Ty>,
    pub is_coro: bool,
    /// Inherent `impl` method owner. Bare `name` must not shadow imports.
    pub method_owner: Option<&'a str>,
    /// An inherent method's `impl` parameters (`impl Cell<T>`), for the
    /// fix-it of a `where` bound on one of them.
    pub impl_params: &'a [TypeParam<'a>],
    pub is_static_method: bool,
    /// `requires` / `ensures` clauses, typed after the body.
    pub contracts: &'a [Contract<'a>],
}

/// The contract clauses of a function or method declaration node.
pub(super) fn decl_contracts<'a>(node: &'a Output<'a>) -> &'a [Contract<'a>] {
    match node.1.as_ref() {
        Expression::Function { contracts, .. } => contracts,
        Expression::Method(_, inner) => decl_contracts(inner),
        _ => &[],
    }
}

impl Checker {
    #[inline(never)]
    pub(super) fn infer_function_expr(&mut self, args: InferFunctionExprArgs<'_>) -> Ty {
        let InferFunctionExprArgs {
            attrs,
            name,
            is_coro,
            is_static,
            type_params,
            args,
            returns,
            where_constraints,
            contracts,
            body,
            range,
        } = args;
        // `static fn` is a trait method without a `Self`-typed receiver when
        // it appears in a `trait` body (called as `Owner::m(..)`).
        if is_static && self.current_typeclass.is_none() {
            return self.error_with_help(
                ErrorCode::GenericTypeError,
                "`static fn` is only allowed inside an `impl` block".to_string(),
                range,
                Some("declare static methods as `impl Class { static fn ... }`".to_string()),
            );
        }
        if name == "main" {
            self.main_decl_span = Some(range.clone());
            // The entry point is called with nothing on the stack: a
            // parameter would silently read as its type's default (#2091).
            if self.impl_owner.is_none()
                && self.current_typeclass.is_none()
                && let Expression::Fragment(params) = args.1.as_ref()
                && !params.is_empty()
            {
                let mut msg = Message::error(
                    ErrorCode::GenericTypeError,
                    "`main` takes no parameters".to_string(),
                    args.0.into_range(),
                );
                msg.push(Label::new(
                    "the entry point is called without arguments".to_string(),
                    args.0.into_range(),
                ));
                msg.with_help(
                    "write `fn main()`; read program arguments with `env::args()` (`use env::args;`)"
                        .to_string(),
                );
                self.messages.push(msg);
            }
        }
        let prev_overloadable = self.registering_overloadable_fn;
        self.registering_overloadable_fn = self.current_typeclass.is_none();

        let test_desc = parser::ast::attr_test_desc(attrs, name);
        if test_desc.is_some() {
            self.messages.push({
                let mut m = Message::error(
                    ErrorCode::GenericTypeError,
                    "`#[test]` on `fn` is not supported; use `test(\"desc\") { … }`".to_string(),
                    range.clone(),
                );
                m.push(Label::new(
                    "write a harness test case instead of decorating a function".to_string(),
                    range.clone(),
                ));
                m
            });
        }

        self.infer_function(InferFunctionArgs {
            name,
            type_params,
            args,
            returns: returns.as_ref(),
            where_constraints,
            body: body.as_ref(),
            range: &range,
            self_ty: None,
            is_coro,
            method_owner: None,
            impl_params: &[],
            is_static_method: false,
            contracts,
        });

        self.registering_overloadable_fn = prev_overloadable;
        unit_ty()
    }

    /// Type `requires` / `ensures` clauses: each is a `bool`, and `ensures`
    /// sees the returned value as `result`. Params are in scope.
    fn infer_contracts(
        &mut self,
        contracts: &[Contract<'_>],
        arg_tys: &[(String, Ty)],
        ret_ty: &Ty,
        is_coro: bool,
    ) {
        let has_ensures = contracts.iter().any(|c| c.kind == ContractKind::Ensures);
        if has_ensures && arg_tys.iter().any(|(n, _)| n == "result") {
            let span = contracts.iter().find(|c| c.kind == ContractKind::Ensures).expect("ensures").span;
            self.messages.push(Message::error(
                ErrorCode::GenericTypeError,
                "a parameter named `result` hides the returned value in `ensures`; rename the parameter"
                    .to_string(),
                span.into_range(),
            ));
        }
        for c in contracts {
            let range = c.span.into_range();
            let ensures = c.kind == ContractKind::Ensures;
            if ensures && is_coro {
                self.messages.push(Message::error(
                    ErrorCode::GenericTypeError,
                    "`ensures` on a `gen fn` is not supported yet".to_string(),
                    range.clone(),
                ));
            }
            if ensures {
                self.push_scope();
                self.env
                    .insert_top("result".to_string(), Scheme::mono(ret_ty.clone()));
            }
            let prev_expected = self.current_expected.take();
            self.in_ensures = ensures;
            let ty = self.infer(&c.expr);
            self.in_ensures = false;
            self.current_expected = prev_expected;
            self.unify(&crate::typechecking::ty::boolean(), &ty, &range, c.kind.keyword());
            if ensures {
                self.pop_scope();
            }
        }
    }

    /// A class `invariant` is a `bool` over `self`, read with the class's
    /// own (private) fields visible.
    pub(super) fn infer_class_invariants(
        &mut self,
        key: &str,
        type_params: &[parser::ast::TypeParam<'_>],
        invariants: &[Contract<'_>],
    ) {
        let self_ty = if type_params.is_empty() {
            Ty::Con(key.to_string())
        } else {
            let frame = self.type_params_in_scope.last().cloned().unwrap_or_default();
            let args = type_params
                .iter()
                .map(|tp| frame.get(tp.name).map_or_else(|| Ty::Var(self.counter.fresh()), |v| Ty::Var(*v)))
                .collect();
            Ty::App(Box::new(Ty::Con(key.to_string())), args)
        };
        let prev_owner = self.impl_owner.replace(key.to_string());
        self.push_scope();
        self.env.insert_top("self".to_string(), Scheme::mono(self_ty));
        for c in invariants {
            let prev_expected = self.current_expected.take();
            let ty = self.infer(&c.expr);
            self.current_expected = prev_expected;
            self.unify(&crate::typechecking::ty::boolean(), &ty, &c.span.into_range(), c.kind.keyword());
        }
        self.pop_scope();
        self.impl_owner = prev_owner;
    }

    /// A loop's `invariant` is a `bool` and its `decreases` an `int`, in the
    /// scope around the loop (a `for` binding is not visible).
    pub(super) fn infer_loop_contracts(&mut self, contracts: &[Contract<'_>]) {
        for c in contracts {
            let prev_expected = self.current_expected.take();
            let ty = self.infer(&c.expr);
            self.current_expected = prev_expected;
            let want = match c.kind {
                ContractKind::Decreases => crate::typechecking::ty::int(),
                _ => crate::typechecking::ty::boolean(),
            };
            self.unify(&want, &ty, &c.span.into_range(), c.kind.keyword());
        }
    }

    #[inline(never)]
    pub(super) fn infer_lambda(
        &mut self,
        args: &Output,
        captures: &[&str],
        body: &Output,
        range: Range<usize>,
    ) -> Ty {
        // Resolve capture types from the outer env before isolating.
        let mut cap_bindings: Vec<(String, Ty)> = Vec::new();
        for cap in captures {
            match self.env.lookup(cap).cloned() {
                Some(scheme) => {
                    let ty = self.instantiate_ty(&scheme);
                    cap_bindings.push((cap.to_string(), ty));
                }
                None => {
                    return self.error(
                        ErrorCode::UnknownValue,
                        format!("Cannot find value `{}` in this scope", cap),
                        range,
                    );
                }
            }
        }
        let arg_tys = self.parse_arg_list(args);
        let mut uncaptured = self.env.all_names();
        for (n, _) in &cap_bindings {
            uncaptured.remove(n);
        }
        for (n, _) in &arg_tys {
            uncaptured.remove(n);
        }

        // File-level imports and module-visible named `fn`s are globals,
        // not closure captures. Rebind after isolating the env.
        let prev_statics = std::mem::take(&mut self.lambda_rebound_statics);
        let import_rebinds = self.snapshot_file_level_imports(&mut uncaptured, range.clone());

        let const_caps: Vec<bool> = cap_bindings.iter().map(|(n, _)| self.env.is_const(n)).collect();
        let saved_frames = self.env.take_and_isolate();
        let prev_uncaptured = self.lambda_uncaptured_outer.replace(uncaptured);
        self.rebind_file_level_imports(import_rebinds);
        for ((n, ty), is_const) in cap_bindings.iter().zip(const_caps) {
            self.env.insert_top(n.clone(), Scheme::mono(ty.clone()));
            if is_const {
                self.env.mark_top_const(n);
            }
            self.record_codegen_var_type(n.clone(), ty.clone());
        }
        for (n, ty) in &arg_tys {
            self.env.insert_top(n.clone(), Scheme::mono(ty.clone()));
            self.record_codegen_var_type(n.clone(), ty.clone());
        }
        // Match codegen: consume Fragment + Argument IDs before body.
        self.assign_fn_arg_node_ids(args, &arg_tys);

        let ret_slot = Ty::Var(self.counter.fresh());
        let prev_ret = self.current_return_ty.replace(ret_slot.clone());
        let prev_in_test = std::mem::replace(&mut self.in_test_case, false);
        // A lambda's `return` returns from the lambda: its `raise` / `?`
        // put the lambda, not the enclosing function, in result or option
        // mode (#1164).
        let prev_result_mode = self.fn_result_mode.take();
        let prev_option_mode = self.fn_option_mode.take();
        let body_ty = self.infer(body);
        self.in_test_case = prev_in_test;
        self.unify(&ret_slot, &body_ty, &range, "lambda body");
        if let Some((ok, _)) = std::mem::replace(&mut self.fn_result_mode, prev_result_mode) {
            self.note_result_mode_fn(&Self::lambda_mode_key(range.start), &ok);
        }
        self.fn_option_mode = prev_option_mode;
        self.current_return_ty = prev_ret;
        self.lambda_uncaptured_outer = prev_uncaptured;
        self.lambda_rebound_statics = prev_statics;
        self.env.restore_frames(saved_frames);

        let ret = apply_ty_prune(&self.subst, &ret_slot);
        let mut fun_ty = ret;
        for (_, arg_ty) in arg_tys.iter().rev() {
            fun_ty = Ty::Fun(Box::new(arg_ty.clone()), Box::new(fun_ty));
        }
        Self::seal_nullary_fun_ty(fun_ty, arg_tys.len(), false)
    }

    /// A single-parameter bound is written on the parameter it bounds:
    /// `T: Num` on the function's own parameter, or a bounded impl block
    /// (`impl Cell<T: Num> { … }`) for an impl's parameter. `where` is for
    /// multi-parameter traits only.
    fn reject_unary_where(
        &mut self,
        wc: &parser::ast::WhereConstraint<'_>,
        name: &str,
        type_params: &[TypeParam<'_>],
        method_owner: Option<&str>,
        impl_params: &[TypeParam<'_>],
    ) {
        let [arg] = wc.args.as_slice() else {
            return;
        };
        let class = wc.class;
        let arg_name = match arg.1.as_ref() {
            Expression::Type(n) => Some(*n),
            _ => None,
        };
        let with_bound = |tp: &TypeParam<'_>| {
            let mut bounds: Vec<&str> = tp.bounds.to_vec();
            bounds.push(class);
            format!("{}: {}", tp.name, bounds.join(" + "))
        };
        let own = arg_name.and_then(|n| type_params.iter().find(|tp| tp.name == n));
        let outer = arg_name.and_then(|n| impl_params.iter().find(|tp| tp.name == n));
        let help = match (own, outer, method_owner) {
            (Some(tp), _, _) => format!("drop it from `where` and bound the parameter: `<{}>`", with_bound(tp)),
            (None, Some(tp), Some(owner)) => {
                let head: Vec<String> = impl_params
                    .iter()
                    .map(|p| {
                        if p.name == tp.name {
                            with_bound(p)
                        } else if p.bounds.is_empty() {
                            p.name.to_string()
                        } else {
                            format!("{}: {}", p.name, p.bounds.join(" + "))
                        }
                    })
                    .collect();
                let owner = owner.rsplit("::").next().unwrap_or(owner);
                format!(
                    "move `{}` into a separate bounded impl block: `impl {}<{}> {{ … }}`",
                    name,
                    owner,
                    head.join(", ")
                )
            }
            _ => "bound a type parameter instead: `T: Trait` in the function's or the impl's parameter list"
                .to_string(),
        };
        let mut msg = Message::error(
            ErrorCode::GenericTypeError,
            format!(
                "`where` is for multi-parameter traits; a single-parameter bound is written on the parameter, not `where {}<{}>`",
                class,
                arg_name.unwrap_or("…")
            ),
            arg.0.into_range(),
        );
        msg.with_help(help);
        self.messages.push(msg);
    }

    pub(super) fn infer_function(&mut self, args: InferFunctionArgs<'_>) -> Ty {
        let InferFunctionArgs {
            name,
            type_params,
            args,
            returns,
            where_constraints,
            body,
            range,
            self_ty,
            is_coro,
            method_owner,
            impl_params,
            is_static_method,
            contracts,
        } = args;
        if name == "drop" && method_owner.is_none() {
            self.messages.push(Message::error(
                ErrorCode::InvalidDrop,
                "fn drop(self) is only allowed as an inherent class method".to_string(),
                range.clone(),
            ));
        }
        let Some(body) = body else {
            return self.error_with_help(
                ErrorCode::GenericTypeError,
                "Function declaration must have a body".into(),
                range.clone(),
                Some("add a block `{ … }` or declare FFI with `extern \"lib\" { fn …; }`".into()),
            );
        };
        // A `... xs` template's body is checked per instance (#1281).
        let template_key = match method_owner {
            Some(owner) => format!("{owner}::{name}"),
            None => name.to_string(),
        };
        if self.is_variadic_template(&template_key) {
            return Ty::Var(self.counter.fresh());
        }
        // Set up type parameter environment.
        let is_generic = !type_params.is_empty();
        let mut param_vars: Vec<TyVarId> = Vec::new();
        let mut param_frame: HashMap<String, TyVarId> = HashMap::new();

        let mut param_kinds: Vec<Kind> = Vec::new();
        for tp in type_params {
            let var = self.counter.fresh();
            let kind = self.resolve_type_param_kind(tp);
            self.set_var_kind(var, kind.clone());
            param_frame.insert(tp.name.to_string(), var);
            param_vars.push(var);
            param_kinds.push(kind);
        }

        // Push param frame so parse_type_name resolves T → Var(id).
        self.push_type_params(param_frame);
        let mut param_constraints: Vec<Constraint> = Vec::new();
        for (tp, var) in type_params.iter().zip(param_vars.iter()) {
            // Binder bounds `T: Num` desugar to unary constraints. Bounds
            // may also name an earlier constraint parameter: `T: c`.
            for bound in &tp.bounds {
                if let Some(constraint) = self.constraint_from_bound(bound, Ty::Var(*var), range) {
                    param_constraints.push(constraint);
                }
            }
        }
        // `where Class<T1, T2>` constraints (parsed after returns).
        for wc in where_constraints {
            self.reject_unary_where(wc, name, type_params, method_owner, impl_params);
            let args: Vec<Ty> = wc.args.iter().map(|a| self.parse_type_name(a)).collect();
            param_constraints.push(Constraint {
                class: self.canonical_trait_name(wc.class),
                args,
            });
        }
        let prev_constraints_len = self.active_constraints.len();
        self.active_constraints
            .extend(param_constraints.iter().cloned());
        self.abstract_constraint_bindings.push(HashMap::new());

        let collect_fn_assoc = is_generic && self.current_assoc_projections.is_none();
        let prev_fn_assoc = if collect_fn_assoc {
            let prev = self.current_assoc_projections.take();
            self.current_assoc_projections = Some(Vec::new());
            prev
        } else {
            None
        };
        self.current_tuple_pack = self
            .variadic_instance_pack(&template_key)
            .or_else(|| Self::tuple_pack_ty_for_args(args, &mut self.counter));
        let arg_tys = self.parse_arg_list(args);
        self.current_tuple_pack = None;
        // Record declaration-order param names for named call-site args.
        // A method's signature tables are keyed `Owner::method`, the key its
        // calls resolve to: under its bare name they would overwrite a free
        // function of the same name, whose bare calls then read the method's
        // arity (#1981).
        self.fn_param_names.insert(
            template_key.clone(),
            arg_tys.iter().map(|(n, _)| n.clone()).collect(),
        );
        let has_rest = matches!(args.1.as_ref(), Expression::Fragment(children)
        if children.last().is_some_and(|c| {
            matches!(c.1.as_ref(), Expression::Argument { is_rest: true, .. })
        }));
        let has_tuple_rest = matches!(args.1.as_ref(), Expression::Fragment(children)
        if children.last().is_some_and(|c| {
            matches!(c.1.as_ref(), Expression::Argument { ty: None, is_rest: true, .. })
        }));
        self.fn_has_rest.insert(template_key.clone(), has_rest);
        let program_key = match method_owner {
            Some(_) => template_key.clone(),
            None => self.qualify_module_name(name),
        };
        if has_rest && !has_tuple_rest && method_owner.is_none() {
            self.program_typed_rest
                .insert(program_key.clone(), arg_tys.iter().map(|(n, _)| n.clone()).collect());
        }
        self.program_fn_params
            .insert(program_key, (arg_tys.len() - usize::from(has_rest), has_rest));
        self.fn_tuple_rest.insert(template_key.clone(), has_tuple_rest);
        let mut declared_send_ty = false;
        let (ret_ty, yield_slot, send_slot) = if is_coro {
            let yield_ty = Ty::Var(self.counter.fresh());
            let send_ty = Ty::Var(self.counter.fresh());
            // Honor `gen fn -> T`: unify declared T with the yield/
            // return slot so annotation mismatches are diagnosed.
            if let Some(r) = returns {
                let declared = self.parse_type_name(r);
                // `-> coroutine<Y, S>` spells the whole handle type;
                // a bare `-> T` is the yield type.
                let (declared_yield, declared_send) = match Self::coroutine_parts(&declared) {
                    Some((y, s)) => (y, Some(s)),
                    None => (declared, None),
                };
                self.unify(
                    &yield_ty,
                    &declared_yield,
                    &r.0.into_range(),
                    "gen fn return type",
                );
                if let Some(s) = declared_send {
                    self.unify(&send_ty, &s, &r.0.into_range(), "gen fn send type");
                    declared_send_ty = true;
                }
            }
            let coro = self.coroutine_type(yield_ty.clone(), send_ty.clone());
            (coro, Some(yield_ty), Some(send_ty))
        } else {
            (
                match returns {
                    Some(r) => self.parse_type_name(r),
                    None => Ty::Var(self.counter.fresh()),
                },
                None,
                None,
            )
        };
        let fn_assoc_projections = if collect_fn_assoc {
            let projections = self.current_assoc_projections.take().unwrap_or_default();
            self.current_assoc_projections = prev_fn_assoc;
            projections
        } else {
            Vec::new()
        };

        // Build the declared function type: arg1 -> ... -> argN -> ret,
        // with self prepended for methods.
        let mut fun_ty = ret_ty.clone();
        for (_, arg_ty) in arg_tys.iter().rev() {
            fun_ty = Ty::Fun(Box::new(arg_ty.clone()), Box::new(fun_ty));
        }
        if let Some(self_ty) = self_ty {
            fun_ty = Ty::Fun(Box::new(self_ty.clone()), Box::new(fun_ty));
        }
        self.weak_checks.enter_fn(param_vars.clone(), fun_ty.clone());

        // Monomorphic recursion: bind a fresh α so the body can call this
        // function. Inherent methods bind `Owner::name` only — never the
        // bare name — so `use thread::{send}` is not shadowed by
        // `impl Foo { fn send(...) { send(...) } }`.
        let alpha = self.counter.fresh();
        // A declared signature types the body's own recursive calls up
        // front (#1191): `let r = f(rest); r.push(..)` needs `r`'s type
        // before the post-body unify below would give it.
        if !is_coro && returns.is_some() {
            let declared = Self::seal_nullary_fun_ty(fun_ty.clone(), arg_tys.len(), self_ty.is_some());
            self.unify(&Ty::Var(alpha), &declared, range, "function type");
        }

        // Result/Option mode from an annotated return type. Bare
        // `return v` unifies against the Ok / payload slot; the
        // function's own type remains `Result<T,E>` / `Option<T>`.
        let prev_result_mode = self.fn_result_mode.take();
        let prev_option_mode = self.fn_option_mode.take();
        // A nested function or lambda is not the test body itself.
        let prev_in_test = std::mem::replace(&mut self.in_test_case, false);
        let return_slot = if is_coro {
            yield_slot.clone().unwrap_or_else(unit_ty)
        } else if let Some((ok, err)) = result_ok_err(&ret_ty) {
            self.fn_result_mode = Some((ok.clone(), err));
            ok
        } else if is_option_ty(&ret_ty) {
            if let Some(inner) = option_inner(&ret_ty) {
                self.fn_option_mode = Some(inner);
            }
            ret_ty.clone()
        } else {
            ret_ty.clone()
        };

        let prev_ret = self.current_return_ty.replace(return_slot);
        let prev_yield = self.current_yield_ty.take();
        let prev_send = self.current_send_ty.take();
        let prev_yield_receives = self.yield_receives_used;
        // A declared send type is kept even when no `yield` result is used.
        self.yield_receives_used = declared_send_ty;
        if let Some(yield_ty) = yield_slot {
            self.current_yield_ty = Some(yield_ty);
        }
        if let Some(send_ty) = send_slot {
            self.current_send_ty = Some(send_ty);
        }
        let prev_async = self.async_depth;
        if is_coro {
            self.async_functions.insert(name.to_string());
            self.async_depth += 1;
        }

        if let Some(owner) = method_owner {
            let fqn = format!("{}::{}", owner, name);
            self.env.insert_top(fqn, Scheme::mono(Ty::Var(alpha)));
            // Stub so `self.name(...)` / `Owner::name(...)` resolve while
            // the body is inferred (real scheme is written by infer_impl).
            self.methods
                .entry(owner.to_string())
                .or_default()
                .entry(name.to_string())
                .or_insert_with(|| (Visibility::Private, Scheme::mono(Ty::Var(alpha))));
            if is_static_method {
                self.static_methods
                    .entry(owner.to_string())
                    .or_default()
                    .insert(name.to_string());
            }
        } else {
            // A forward call instantiated the stub's parameters (#1670).
            if let Some(stub) = self.forward_free_fn_schemes.remove(name) {
                self.weak_checks.alias(&stub.bounds, &param_vars);
            }
            self.env
                .insert_top(name.to_string(), Scheme::mono(Ty::Var(alpha)));
        }

        self.push_scope();
        let mut baseline = std::collections::HashSet::new();
        if let Some(self_ty) = self_ty {
            // Method receiver — env binding for the body + side-table for
            // codegen Access/Call. Static methods pass `None` and must not
            // see `self` (it is no longer bound on the outer impl frame).
            self.env
                .insert_top("self".to_string(), Scheme::mono(self_ty.clone()));
            self.record_codegen_var_type("self".to_string(), self_ty.clone());
            baseline.insert("self".to_string());
        }
        for (arg_name, arg_ty) in &arg_tys {
            self.env
                .insert_top(arg_name.clone(), Scheme::mono(arg_ty.clone()));
            self.record_codegen_var_type(arg_name.clone(), arg_ty.clone());
            baseline.insert(arg_name.clone());
        }
        self.fn_codegen_baselines.push(baseline);
        // Consume Fragment + Argument NodeIds so body infer stays lockstep
        // with codegen `do_compile(args)` (same skip of type-annotation children).
        if method_owner.is_none() {
            self.assign_fn_arg_node_ids(args, &arg_tys);
        }
        let prev_function = if method_owner.is_none() {
            self.current_function.replace(name.to_string())
        } else {
            None
        };
        let open_mark = self.open_operands.len();
        let empty_mark = self.open_empty_arrays.len();
        let mixed_mark = self.mixed_literals.len();
        let goals_mark = self.deferred_goals.len();
        self.fn_body_depth += 1;
        let _ = self.infer(body);
        self.fn_body_depth -= 1;
        self.check_deferred_goals(goals_mark);
        self.infer_contracts(contracts, &arg_tys, &ret_ty, is_coro);
        self.check_mixed_literals(mixed_mark);
        self.check_open_operands(open_mark);
        self.check_open_empty_arrays(empty_mark);
        if method_owner.is_none() {
            self.current_function = prev_function;
        }
        let body_is_stub = self.current_typeclass.is_some()
            && (matches!(
                body.1.as_ref(),
                Expression::Block(stmts) if stmts.is_empty()
            ) || matches!(body.1.as_ref(), Expression::Noop(_)));
        if !is_coro && !body_is_stub {
            let lookup = |name: &str| self.const_fold_env.get(name).copied();
            let cf = crate::typechecking::control_flow::analyze_fn_body(body, &lookup);
            self.messages.extend(cf.messages);
            if !cf.always_exits {
                let ret = self
                    .current_return_ty
                    .as_ref()
                    .map(|t| apply_ty_prune(&self.subst, t))
                    .unwrap_or_else(unit_ty);
                // Unannotated / still-open returns that fall through are unit,
                // not an invented typed value (codegen used to emit `CONST 0`).
                if matches!(&ret, Ty::Var(_)) {
                    self.unify(&ret, &unit_ty(), range, "missing return");
                } else if self.fn_result_mode.is_some()
                    && let Some((ok, _)) = result_ok_err(&ret)
                {
                    let ok = apply_ty_prune(&self.subst, &ok);
                    if matches!(&ok, Ty::Var(_)) {
                        self.unify(&ok, &unit_ty(), range, "missing return");
                    }
                }
                let ret = self
                    .current_return_ty
                    .as_ref()
                    .map(|t| apply_ty_prune(&self.subst, t))
                    .unwrap_or_else(unit_ty);
                let allow_fallthrough = matches!(&ret, Ty::Con(n) if n == "unknown")
                    || matches!(&ret, Ty::Never)
                    || matches!(&ret, Ty::Con(n) if n == crate::typechecking::ty::UNIT)
                    || matches!(&ret, Ty::Tuple(items) if items.is_empty())
                    || (self.fn_result_mode.is_some()
                        && result_ok_err(&ret)
                            .map(|(ok, _)| {
                                let ok = apply_ty_prune(&self.subst, &ok);
                                matches!(&ok, Ty::Con(n) if n == crate::typechecking::ty::UNIT)
                                    || matches!(&ok, Ty::Tuple(items) if items.is_empty())
                            })
                            .unwrap_or(false));
                if !allow_fallthrough {
                    let ret_s = self.diag_ty(&ret);
                    let mut message = Message::error(
                        ErrorCode::ReturnMismatch,
                        format!(
                            "function `{name}` reaches the end without returning a value of type `{ret_s}`"
                        ),
                        body.0.into_range(),
                    );
                    message.push(Label::new(
                        "add an explicit `return` on every path".to_string(),
                        body.0.into_range(),
                    ));
                    self.messages.push(message);
                }
            }
        }
        self.fn_codegen_baselines.pop();
        self.pop_scope();

        if is_coro {
            self.async_depth = prev_async;
            if let (Some(yield_ty), Some(send_ty)) =
                (self.current_yield_ty.take(), self.current_send_ty.take())
            {
                let resolved_yield = apply_ty_prune(&self.subst, &yield_ty);
                let mut resolved_send = apply_ty_prune(&self.subst, &send_ty);
                if !self.yield_receives_used {
                    self.unify(&resolved_send, &unit_ty(), range, "coroutine send type");
                    resolved_send = unit_ty();
                }
                fun_ty = {
                    let mut ft = self.coroutine_type(resolved_yield, resolved_send);
                    for (_, arg_ty) in arg_tys.iter().rev() {
                        ft = Ty::Fun(Box::new(arg_ty.clone()), Box::new(ft));
                    }
                    if let Some(self_ty) = self_ty {
                        ft = Ty::Fun(Box::new(self_ty.clone()), Box::new(ft));
                    }
                    ft
                };
            }
            self.yield_receives_used = prev_yield_receives;
        } else if let Some((ok, err)) = self.fn_result_mode.clone() {
            // Body used raise/? — rebuild fun_ty with Result return.
            let ok = apply_ty_prune(&self.subst, &ok);
            let err = apply_ty_prune(&self.subst, &err);
            let result_ret = result_ty(ok.clone(), err);
            fun_ty = result_ret.clone();
            for (_, arg_ty) in arg_tys.iter().rev() {
                fun_ty = Ty::Fun(Box::new(arg_ty.clone()), Box::new(fun_ty));
            }
            if let Some(self_ty) = self_ty {
                fun_ty = Ty::Fun(Box::new(self_ty.clone()), Box::new(fun_ty));
            }
            self.note_result_mode_fn(name, &ok);
            let _ = result_ret;
        } else if let Some(inner) = self.fn_option_mode.clone() {
            let inner = apply_ty_prune(&self.subst, &inner);
            let opt_ret = option_ty(inner);
            // If annotated/inferred return was already Option, keep
            // fun_ty; otherwise rebuild.
            let resolved_ret = apply_ty_prune(&self.subst, &ret_ty);
            if !is_option_ty(&resolved_ret) {
                fun_ty = opt_ret;
                for (_, arg_ty) in arg_tys.iter().rev() {
                    fun_ty = Ty::Fun(Box::new(arg_ty.clone()), Box::new(fun_ty));
                }
                if let Some(self_ty) = self_ty {
                    fun_ty = Ty::Fun(Box::new(self_ty.clone()), Box::new(fun_ty));
                }
            }
            self.option_mode_fns.insert(name.to_string());
        }
        self.current_yield_ty = prev_yield;
        self.current_send_ty = prev_send;

        self.current_return_ty = prev_ret;
        self.fn_result_mode = prev_result_mode;
        self.fn_option_mode = prev_option_mode;
        self.in_test_case = prev_in_test;
        fun_ty = Self::seal_nullary_fun_ty(fun_ty, arg_tys.len(), self_ty.is_some());
        self.unify(&Ty::Var(alpha), &fun_ty, range, "function type");

        if !is_generic {
            let resolved = apply_ty_prune(&self.subst, &fun_ty);
            if let Some(owner) = method_owner {
                self.env
                    .insert_top(format!("{owner}::{name}"), Scheme::mono(resolved));
            } else {
                let scheme = Scheme::mono(resolved);
                self.env.insert_top(name.to_string(), scheme.clone());
                // Same FQN insert as the generic branch: namespaced compile
                // looks up `module::fn` for Result/Option return layout.
                if !self.current_module.is_empty() {
                    let fqn = format!("{}::{}", self.current_module, name);
                    self.env.insert_top(fqn, scheme.clone());
                }
                self.record_free_fn_scheme(name, scheme);
            }
        }

        let abstract_bindings = self.abstract_constraint_bindings.pop().unwrap_or_default();
        let mut resolved_param_constraints = Vec::with_capacity(param_constraints.len());
        for constraint in param_constraints {
            if self.constraint_param_kind(&constraint.class).is_some() {
                if let Some(concrete) = abstract_bindings.get(&constraint.class) {
                    resolved_param_constraints.push(Constraint {
                        class: concrete.clone(),
                        args: constraint.args,
                    });
                } else {
                    self.messages.push(Message::error(
                        ErrorCode::GenericTypeError,
                        format!(
                            "Cannot satisfy abstract constraint `{}`; no concrete trait was selected",
                            self.instance_signature(&constraint.class, &constraint.args)
                        ),
                        range.clone(),
                    ));
                }
            } else {
                resolved_param_constraints.push(constraint);
            }
        }

        // Pop type param scope.
        self.weak_checks.leave_fn();
        self.active_constraints.truncate(prev_constraints_len);
        self.type_params_in_scope.pop();

        // If generic, build a poly scheme and re-insert into env.
        if is_generic {
            self.reroot_type_params(&param_vars);
            let named: Vec<(String, TyVarId)> =
                type_params.iter().map(|tp| tp.name.to_string()).zip(param_vars.iter().copied()).collect();
            self.reject_bound_type_params(&named, &mut Vec::new(), range);
            self.reject_bound_projections(&named, range);
            let mut bounds = param_vars;
            bounds.extend(fn_assoc_projections.iter().map(|p| p.var));
            let mut kinds = param_kinds;
            kinds.extend(std::iter::repeat_n(Kind::Type, fn_assoc_projections.len()));
            let scheme = Scheme::poly_with_kinds_and_assoc(
                bounds,
                kinds,
                resolved_param_constraints.clone(),
                fn_assoc_projections,
                apply_ty_prune(&self.subst, &fun_ty),
            );
            // Non-entry modules also register under `module::name` so later
            // files can `use` the real poly scheme (not a dummy Var).
            let fqn = if self.current_module.is_empty() {
                name.to_string()
            } else {
                format!("{}::{}", self.current_module, name)
            };
            self.generic_fns.insert(name.to_string());
            self.generics.generic_fns.insert(name.to_string());
            if fqn != name {
                self.generic_fns.insert(fqn.clone());
                self.generics.generic_fns.insert(fqn.clone());
            }
            self.env.insert_top(name.to_string(), scheme.clone());
            if fqn != name {
                self.env.insert_top(fqn.clone(), scheme.clone());
            }
            self.record_free_fn_scheme(name, scheme);

            // Every constraint is a trailing dictionary argument. Builtin
            // classes use compiler-generated implementation thunks, while
            // user classes use source-declared methods; their calling ABI is
            // intentionally identical.
            let dict_n = resolved_param_constraints.len();
            self.fn_dict_arity.insert(name.to_string(), dict_n);
            if fqn != name {
                self.fn_dict_arity.insert(fqn, dict_n);
            }
        }

        // Only genuine top-level user functions are registered here.
        // Trait / typeclass bodies suppress via `registering_overloadable_fn`.
        // Inherent `impl` methods register under `Owner::method` in `infer_impl`.
        if self.registering_overloadable_fn {
            let param_names_for_overload =
                self.fn_param_names.get(name).cloned().unwrap_or_default();
            let fixed_arity_for_overload = if has_rest {
                param_names_for_overload.len().saturating_sub(1)
            } else {
                param_names_for_overload.len()
            };
            let candidate_scheme = match self.env.lookup(name) {
                Some(s) => s.clone(),
                None => Scheme::mono(fun_ty.clone()),
            };
            // Non-entry modules register under `module::name` so same-arity
            // helpers in `bytes` / `text` (e.g. `starts_with`) do not collide.
            let overload_key = if self.current_module.is_empty() {
                name.to_string()
            } else {
                format!("{}::{}", self.current_module, name)
            };
            self.register_overload_candidate(
                &overload_key,
                OverloadCandidate {
                    id: 0, // assigned in register_overload_candidate
                    fixed_arity: fixed_arity_for_overload,
                    is_rest: has_rest,
                    scheme: candidate_scheme,
                    param_names: param_names_for_overload,
                },
                range,
            );
        }

        fun_ty
    }

    /// Insert `candidate` into `overload_sets[key]`, emitting
    /// [`ErrorCode::DuplicateOverload`] on arity-range or parameter-type overlap.
    ///
    /// Same fixed arity is allowed when parameter types are distinct (e.g.
    /// `sum(int)` vs `sum(float)`). Identical / unifiable parameter lists at
    /// the same arity still conflict.
    pub(crate) fn register_overload_candidate(
        &mut self,
        key: &str,
        mut new_candidate: OverloadCandidate,
        range: &Range<usize>,
    ) {
        // Pre-registered by `pre_register_overloads`: keep its id, take the
        // inferred scheme.
        if let Some(&(id, ..)) = self.overload_decl_by_span.get(&(range.start, range.end))
            && let Some(existing) = self
                .overload_sets
                .get_mut(key)
                .and_then(|c| c.iter_mut().find(|c| c.id == id))
        {
            existing.scheme = new_candidate.scheme.clone();
            let def = self.intern_overload_def(key, id);
            self.schemes_by_def.insert(def, new_candidate.scheme);
            return;
        }
        new_candidate.id = self
            .overload_sets
            .get(key)
            .map(|c| c.len() as u32)
            .unwrap_or(0);
        let mut conflict = false;
        let existing_list = self.overload_sets.get(key).cloned().unwrap_or_default();
        for existing in &existing_list {
            let overlap = if existing.is_rest && new_candidate.is_rest {
                true
            } else if !existing.is_rest && !new_candidate.is_rest {
                if existing.fixed_arity != new_candidate.fixed_arity {
                    false
                } else {
                    Self::schemes_params_overlap(&existing.scheme, &new_candidate.scheme)
                }
            } else {
                let (fixed_n, rest_k) = if existing.is_rest {
                    (new_candidate.fixed_arity, existing.fixed_arity)
                } else {
                    (existing.fixed_arity, new_candidate.fixed_arity)
                };
                fixed_n >= rest_k
            };
            if overlap {
                conflict = true;
                let msg_text = if existing.is_rest && new_candidate.is_rest {
                    format!(
                        "Duplicate rest function `{}`: two rest-parameter overloads always conflict",
                        key
                    )
                } else if !existing.is_rest && !new_candidate.is_rest {
                    if existing.fixed_arity == new_candidate.fixed_arity
                        && Self::schemes_params_overlap(&existing.scheme, &new_candidate.scheme)
                    {
                        format!(
                            "Duplicate function `{}` with arity {} and overlapping parameter types",
                            key, new_candidate.fixed_arity
                        )
                    } else {
                        format!(
                            "Duplicate function `{}` with arity {}",
                            key, new_candidate.fixed_arity
                        )
                    }
                } else {
                    let (fixed_n, rest_k) = if existing.is_rest {
                        (new_candidate.fixed_arity, existing.fixed_arity)
                    } else {
                        (existing.fixed_arity, new_candidate.fixed_arity)
                    };
                    format!(
                        "Overload conflict for `{}`: fixed arity {} overlaps rest (min-arity {})",
                        key, fixed_n, rest_k
                    )
                };
                let mut err = Message::error(ErrorCode::DuplicateOverload, msg_text, range.clone());
                err.with_help(
                    "choose distinct arities or parameter types, or make the fixed overload have fewer params than the rest's fixed prefix"
                        .to_string(),
                );
                self.messages.push(err);
                break;
            }
        }
        if !conflict {
            let def = self.intern_overload_def(key, new_candidate.id);
            self.schemes_by_def
                .insert(def, new_candidate.scheme.clone());
            self.overload_decl_by_span.insert(
                (range.start, range.end),
                (
                    new_candidate.id,
                    new_candidate.fixed_arity,
                    new_candidate.is_rest,
                ),
            );
            self.overload_sets
                .entry(key.to_string())
                .or_default()
                .push(new_candidate);
        }
    }
}
