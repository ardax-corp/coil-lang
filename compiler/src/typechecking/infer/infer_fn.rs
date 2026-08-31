//! Function-expression inference. `infer_inner` remains the dispatcher.

use std::ops::Range;

use parser::ast::{Expression, Output, TypeParam};
use reporting::{ErrorCode, Message};

use crate::typechecking::ty::{Scheme, Ty, unit as unit_ty};

use super::*;

impl Checker {
    #[inline(never)]
    #[allow(clippy::too_many_arguments)]
    pub(super) fn infer_function_expr(
        &mut self,
        attrs: &[parser::ast::Attribute],
        name: &str,
        is_coro: bool,
        is_static: bool,
        type_params: &[TypeParam],
        args: &Output,
        returns: &Option<Output>,
        where_constraints: &[parser::ast::WhereConstraint],
        body: &Option<Output>,
        range: Range<usize>,
    ) -> Ty {
        if is_static {
            return self.error_with_help(
                ErrorCode::GenericTypeError,
                "`static fn` is only allowed inside an `impl` block".to_string(),
                range,
                Some(
                    "declare static methods as `impl Class { static fn ... }`".to_string(),
                ),
            );
        }
        if name == "main" {
            self.main_decl_span = Some(range.clone());
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

        self.infer_function(
            name,
            type_params,
            args,
            returns.as_ref(),
            where_constraints,
            body.as_ref(),
            &range,
            None,
            is_coro,
            None,
            false,
        );

        self.registering_overloadable_fn = prev_overloadable;
        unit_ty()
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

        // File-level imports are global names, not closure captures.
        // Rebind virtual + disk-module schemes after isolating the env.
        let import_rebinds =
            self.snapshot_file_level_imports(&mut uncaptured, range.clone());

        let saved_frames = self.env.take_and_isolate();
        let prev_uncaptured = self.lambda_uncaptured_outer.replace(uncaptured);
        self.rebind_file_level_imports(import_rebinds);
        for (n, ty) in &cap_bindings {
            self.env.insert_top(n.clone(), Scheme::mono(ty.clone()));
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
        let body_ty = self.infer(body);
        self.unify(&ret_slot, &body_ty, &range, "lambda body");
        self.current_return_ty = prev_ret;
        self.lambda_uncaptured_outer = prev_uncaptured;
        self.env.restore_frames(saved_frames);

        let ret = apply_ty_prune(&self.subst, &ret_slot);
        let mut fun_ty = ret;
        for (_, arg_ty) in arg_tys.iter().rev() {
            fun_ty = Ty::Fun(Box::new(arg_ty.clone()), Box::new(fun_ty));
        }
        Self::seal_nullary_fun_ty(fun_ty, arg_tys.len(), false)
    }


    pub(super) fn infer_function(
        &mut self,
        name: &str,
        type_params: &[parser::ast::TypeParam],
        args: &Output,
        returns: Option<&Output>,
        where_constraints: &[parser::ast::WhereConstraint],
        body: Option<&Output>,
        range: &Range<usize>,
        self_ty: Option<&Ty>,
        is_coro: bool,
        // When set, this is an inherent `impl` method. Bare `name` must
        // not shadow imports (`use thread::{send}` → `send`); recursion uses
        // `self.name(...)` / `Owner::name(...)` instead.
        method_owner: Option<&str>,
        is_static_method: bool,
    ) -> Ty {
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
        self.type_params_in_scope.push(param_frame);
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
            let args: Vec<Ty> = wc.args.iter().map(|a| self.parse_type_name(a)).collect();
            param_constraints.push(Constraint {
                class: wc.class.to_string(),
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
        self.current_tuple_pack = Self::tuple_pack_ty_for_args(args, &mut self.counter);
        let arg_tys = self.parse_arg_list(args);
        self.current_tuple_pack = None;
        // Record declaration-order param names for named call-site args.
        self.fn_param_names.insert(
            name.to_string(),
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
        self.fn_has_rest.insert(name.to_string(), has_rest);
        self.fn_tuple_rest.insert(name.to_string(), has_tuple_rest);
        let (ret_ty, yield_slot, send_slot) = if is_coro {
            let yield_ty = Ty::Var(self.counter.fresh());
            let send_ty = Ty::Var(self.counter.fresh());
            // Honor `async fn -> T`: unify declared T with the yield/
            // return slot so annotation mismatches are diagnosed.
            if let Some(r) = returns {
                let declared = self.parse_return_type_name(r);
                self.unify(
                    &yield_ty,
                    &declared,
                    &r.0.into_range(),
                    "async fn return type",
                );
            }
            let coro = self.coroutine_type(yield_ty.clone(), send_ty.clone());
            (coro, Some(yield_ty), Some(send_ty))
        } else {
            (
                match returns {
                    Some(r) => self.parse_return_type_name(r),
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

        // Monomorphic recursion: bind a fresh α so the body can call this
        // function. Inherent methods bind `Owner::name` only — never the
        // bare name — so `use thread::{send}` is not shadowed by
        // `impl Foo { fn send(...) { send(...) } }`.
        let alpha = self.counter.fresh();

        // Result/Option mode from an annotated return type. Bare
        // `return v` unifies against the Ok / payload slot; the
        // function's own type remains `Result<T,E>` / `Option<T>`.
        let prev_result_mode = self.fn_result_mode.take();
        let prev_option_mode = self.fn_option_mode.take();
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
        self.yield_receives_used = false;
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
            self.forward_free_fn_schemes.remove(name);
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
        let _ = self.infer(body);
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
                    self.unify(&ret, &unit_ty(), &range, "missing return");
                } else if self.fn_result_mode.is_some()
                    && let Some((ok, _)) = result_ok_err(&ret)
                {
                    let ok = apply_ty_prune(&self.subst, &ok);
                    if matches!(&ok, Ty::Var(_)) {
                        self.unify(&ok, &unit_ty(), &range, "missing return");
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
                    let ret_s = crate::typechecking::pretty::format_ty_for_diag(&self.subst, &ret);
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
        fun_ty = Self::seal_nullary_fun_ty(fun_ty, arg_tys.len(), self_ty.is_some());
        self.unify(&Ty::Var(alpha), &fun_ty, range, "function type");
        self.reject_free_generic_option_return(name, is_generic, &param_vars, &fun_ty, range);

        if !is_generic {
            let resolved = apply_ty_prune(&self.subst, &fun_ty);
            if let Some(owner) = method_owner {
                self.env
                    .insert_top(format!("{owner}::{name}"), Scheme::mono(resolved));
            } else {
                let scheme = Scheme::mono(resolved);
                self.env.insert_top(name.to_string(), scheme.clone());
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
                            constraint
                        ),
                        range.clone(),
                    ));
                }
            } else {
                resolved_param_constraints.push(constraint);
            }
        }

        // Pop type param scope.
        self.active_constraints.truncate(prev_constraints_len);
        self.type_params_in_scope.pop();

        // If generic, build a poly scheme and re-insert into env.
        if is_generic {
            let mut bounds = param_vars;
            bounds.extend(fn_assoc_projections.iter().map(|p| p.var));
            let mut kinds = param_kinds;
            kinds.extend(std::iter::repeat_n(Kind::Type, fn_assoc_projections.len()));
            let scheme = Scheme::poly_with_kinds_and_assoc(
                bounds,
                kinds,
                resolved_param_constraints.clone(),
                fn_assoc_projections,
                fun_ty.clone(),
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

        // ── Overload-set registration ──────────────────────────────────────
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
