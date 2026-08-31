//! Typeclass and impl inference. `infer_inner` remains the dispatcher.

use std::ops::Range;

use parser::ast::{Expression, Output, TypeParam};
use reporting::{ErrorCode, Message};

use crate::typechecking::generics::{
    AssocTypeDecl, AssocTypeValue, InstanceDef, TypeClassDef, TypeClassMethodDef,
};
use crate::typechecking::ty::{Scheme, Ty, unit as unit_ty};

use super::*;

impl Checker {
    pub(super) fn infer_typeclass_decl(
        &mut self,
        name: &str,
        type_params: &[TypeParam],
        methods: &[Output],
        range: Range<usize>,
    ) -> Ty {
        // Collect associated type declarations and method defs.
        let mut assoc_types: Vec<AssocTypeDecl> = Vec::new();
        let method_defs: Vec<TypeClassMethodDef> = methods
            .iter()
            .filter_map(|m| match m.1.as_ref() {
                Expression::AssocTypeDecl {
                    name: aname,
                    type_params: assoc_params,
                } => {
                    if assoc_types.iter().any(|a| a.name == *aname) {
                        self.messages.push(Message::error(
                            ErrorCode::GenericTypeError,
                            format!(
                                "Duplicate associated type `{}` in trait `{}`",
                                aname, name
                            ),
                            m.0.into_range(),
                        ));
                    } else {
                        let param_kinds = assoc_params
                            .iter()
                            .map(|tp| self.resolve_type_param_kind(tp))
                            .collect::<Vec<_>>();
                        assoc_types.push(AssocTypeDecl::new(
                            aname.to_string(),
                            assoc_params.iter().map(|tp| tp.name.to_string()).collect(),
                            param_kinds,
                        ));
                    }
                    None
                }
                Expression::Function {
                    docs: _,
                    name: mname, body, ..
                } => {
                    let has_default = body.as_ref().is_some_and(
                        |b| !matches!(b.1.as_ref(), Expression::Block(v) if v.is_empty()),
                    );
                    Some(TypeClassMethodDef {
                        name: mname.to_string(),
                        has_default,
                    })
                }
                _ => None,
            })
            .collect();
        let param_names: Vec<String> =
            type_params.iter().map(|tp| tp.name.to_string()).collect();
        let param_kinds: Vec<Kind> = type_params
            .iter()
            .map(|tp| Kind::from(tp.kind.clone()))
            .collect();
        // Single-param classes: param bounds become direct superclasses
        // (`trait Ordered<T: Equal>` → superclasses: ["Equal"]).
        // Multi-param classes ignore param bounds for superclass
        // wiring (use `where` for those constraints later).
        let superclasses: Vec<String> = if type_params.len() == 1 {
            type_params[0]
                .bounds
                .iter()
                .map(|b| (*b).to_string())
                .collect()
        } else {
            Vec::new()
        };
        if let Some(previous) = self.generics.typeclass(name) {
            let is_prelude = Checker::is_builtin_class(name);
            if is_prelude && !self.builtin_name_in_scope(name) {
                // Short name was rebound (`use prelude::ops::Eq as …`);
                // allow the user trait to replace the builtin entry.
            } else {
                let mut msg = Message::error(
                    ErrorCode::GenericTypeError,
                    format!("Duplicate trait `{}`", name),
                    range.clone(),
                );
                if is_prelude && self.builtin_name_in_scope(name) {
                    msg.with_help(format!(
                        "`{}` is in the prelude; free the short name with `use {}::{} as OtherName;` before redefining, or pick a different name",
                        name,
                        previous.defined_module,
                        name
                    ));
                } else {
                    msg.with_help(format!(
                        "trait `{}` was already declared in module `{}`",
                        name, previous.defined_module
                    ));
                }
                self.messages.push(msg);
                for m in methods {
                    let _ = self.infer(m);
                }
                return unit_ty();
            }
        }
        let def = TypeClassDef {
            name: name.to_string(),
            defined_module: self.current_module.clone(),
            type_params: param_names,
            param_kinds: param_kinds.clone(),
            superclasses,
            assoc_types: assoc_types.clone(),
            methods: method_defs.clone(),
        };
        self.generics.typeclasses.insert(name.to_string(), def);

        // Build method schemes with trait parameters in scope.
        // Applied associated types are recorded as explicit projection
        // variables and quantified with the method scheme.
        let mut param_frame = HashMap::new();
        let mut param_vars = Vec::new();
        let mut class_kinds = Vec::new();
        for (i, type_param) in type_params.iter().enumerate() {
            let var = self.counter.fresh();
            let kind = param_kinds.get(i).cloned().unwrap_or(Kind::Type);
            self.set_var_kind(var, kind.clone());
            param_frame.insert(type_param.name.to_string(), var);
            param_vars.push(var);
            class_kinds.push(kind);
        }
        self.type_params_in_scope.push(param_frame);
        self.current_typeclass = Some(name.to_string());
        // ONE constraint over all class params (multi-param ready).
        let class_constraints: Vec<Constraint> = vec![Constraint {
            class: name.to_string(),
            args: param_vars.iter().map(|v| Ty::Var(*v)).collect(),
        }];
        for method in methods {
            if let Expression::Function {
                docs: _,
                name: method_name,
                type_params: method_params,
                args,
                returns,
                ..
            } = method.1.as_ref()
            {
                if *method_name == "drop" {
                    self.messages.push(Message::error(
                        ErrorCode::InvalidDrop,
                        "fn drop(self) is only allowed on inherent class impls, not traits"
                            .to_string(),
                        method.0.into_range(),
                    ));
                }
                // Method-level type params (e.g. `fn first<A>(F<A>) -> A`).
                let mut method_frame = HashMap::new();
                let mut method_vars = Vec::new();
                let mut method_kinds = Vec::new();
                for mp in method_params {
                    let var = self.counter.fresh();
                    let kind = self.resolve_type_param_kind(mp);
                    self.set_var_kind(var, kind.clone());
                    method_frame.insert(mp.name.to_string(), var);
                    method_vars.push(var);
                    method_kinds.push(kind);
                }
                let pushed_method = !method_frame.is_empty();
                if pushed_method {
                    self.type_params_in_scope.push(method_frame);
                }
                let prev_assoc = self.current_assoc_projections.take();
                self.current_assoc_projections = Some(Vec::new());
                let arg_tys = self.parse_arg_list(args);
                let ret_ty = returns
                    .as_ref()
                    .map(|ret| self.parse_type_name(ret))
                    .unwrap_or_else(unit_ty);
                let assoc_projections =
                    self.current_assoc_projections.take().unwrap_or_default();
                self.current_assoc_projections = prev_assoc;
                let fun_ty = arg_tys.iter().rev().fold(ret_ty, |ret, (_, arg)| {
                    Ty::Fun(Box::new(arg.clone()), Box::new(ret))
                });
                if pushed_method {
                    self.type_params_in_scope.pop();
                }
                let mut all_bounds = param_vars.clone();
                all_bounds.extend(method_vars);
                all_bounds.extend(assoc_projections.iter().map(|p| p.var));
                let mut all_kinds = class_kinds.clone();
                all_kinds.extend(method_kinds);
                all_kinds.extend(std::iter::repeat_n(Kind::Type, assoc_projections.len()));
                self.typeclass_method_schemes.insert(
                    (name.to_string(), method_name.to_string()),
                    Scheme::poly_with_kinds_and_assoc(
                        all_bounds,
                        all_kinds,
                        class_constraints.clone(),
                        assoc_projections,
                        fun_ty,
                    ),
                );
            }
        }

        // Walk method bodies (ID alignment + default body typecheck).
        // The class's own constraint is active so a default can call a
        // sibling method through the same dictionary.
        let active_len = self.active_constraints.len();
        self.active_constraints.extend(class_constraints);
        for m in methods {
            let _ = self.infer(m);
        }
        self.active_constraints.truncate(active_len);
        self.type_params_in_scope.pop();
        self.current_typeclass = None;
        unit_ty()
    }

    #[inline(never)]
    pub(super) fn infer_typeclass_impl(
        &mut self,
        class: &str,
        args: &[Output],
        methods: &[Output],
        range: Range<usize>,
    ) -> Ty {
        // Resolve instance heads (bare ctors stay `Con` for HKT).
        let arg_tys: Vec<Ty> = args.iter().map(|a| self.parse_instance_head(a)).collect();
        // Walk arg type expressions for ID alignment; cache head tys
        // so codegen FQNs match (not `Option<t0>` placeholders).
        for (a, ty) in args.iter().zip(arg_tys.iter()) {
            self.cache_forced_ty(a, ty.clone());
        }
        // Verify class exists.
        let class_def = self.generics.typeclass(class).cloned();
        if class_def.is_none() {
            self.messages.push(Message::error(
                ErrorCode::GenericTypeError,
                format!("Unknown trait `{}`", class),
                range.clone(),
            ));
        }
        if let Some(ref cdef) = class_def {
            self.validate_instance_head_kinds(cdef, &arg_tys, &range);
        }
        let orphaned = class_def
            .as_ref()
            .is_some_and(|cdef| !self.instance_satisfies_orphan_rule(cdef, args, &arg_tys));
        if orphaned {
            let instance = self.instance_signature(class, &arg_tys);
            let mut msg = Message::error(
                ErrorCode::GenericTypeError,
                format!(
                    "Orphan instance `{}` is not allowed in module `{}`",
                    instance, self.current_module
                ),
                range.clone(),
            );
            msg.with_help(
                "define the trait in this module, or define the nominal head of every non-variable instance argument here"
                    .to_string(),
            );
            self.messages.push(msg);
        }
        let overlapping = self
            .generics
            .find_overlapping_instance(class, &arg_tys)
            .cloned();
        let existing_idx = self.generics.instances.iter().position(|inst| {
            inst.class == class
                && inst.defined_module == self.current_module
                && inst.range == range
        });
        let same_decl = overlapping.as_ref().is_some_and(|existing| {
            existing_idx.is_some_and(|idx| {
                self.generics.instances[idx].defined_module == existing.defined_module
                    && self.generics.instances[idx].range == existing.range
            })
        });
        if let Some(existing) = overlapping.as_ref() {
            if !same_decl {
            let mut msg = Message::error(
                ErrorCode::GenericTypeError,
                format!(
                    "Overlapping instance `{}` conflicts with existing `{}`",
                    self.instance_signature(class, &arg_tys),
                    self.instance_signature(&existing.class, &existing.args)
                ),
                range.clone(),
            );
            msg.with_help(format!(
                "existing instance was declared in module `{}`",
                existing.defined_module
            ));
            msg.push(Label::new(
                "new instance declared here".to_string(),
                range.clone(),
            ));
            if existing.defined_module == self.current_module {
                msg.push(Label::new(
                    "existing overlapping instance declared here".to_string(),
                    existing.range.clone(),
                ));
            }
            self.messages.push(msg);
            }
        }
        // Build method_fqns, assoc_tys, and register instance.
        let mut method_fqns = HashMap::new();
        let mut method_names = Vec::new();
        let mut assoc_tys: HashMap<String, AssocTypeValue> = HashMap::new();
        let mut assoc_names: Vec<String> = Vec::new();
        let mut invalid_assoc_defs = false;

        // Pre-register a stub so recursive derived/hand-written
        // method bodies can discharge constraints against the
        // instance under construction. Assoc types are patched
        // onto the stub as they are collected (before methods
        // run), so projections stay valid during body infer.
        let args_pretty_for_fqn: String = arg_tys
            .iter()
            .map(|t| format!("{}", t))
            .collect::<Vec<_>>()
            .join("_");
        let mut stub_fqns = HashMap::new();
        for m in methods {
            let mname = match m.1.as_ref() {
                Expression::Function { name, .. } => Some(*name),
                Expression::Method(_, body) => match body.1.as_ref() {
                    Expression::Function { name, .. } => Some(*name),
                    _ => None,
                },
                _ => None,
            };
            if let Some(mname) = mname {
                stub_fqns.insert(
                    mname.to_string(),
                    format!("{}__{}__{}", class, args_pretty_for_fqn, mname),
                );
            }
        }
        let stub_idx = if let Some(idx) = existing_idx {
            Some(idx)
        } else if class_def.is_some() && !orphaned && overlapping.is_none() {
            self.generics.instances.push(InstanceDef {
                class: class.to_string(),
                defined_module: self.current_module.clone(),
                range: range.clone(),
                args: arg_tys.clone(),
                method_fqns: stub_fqns,
                assoc_tys: HashMap::new(),
            });
            Some(self.generics.instances.len() - 1)
        } else {
            None
        };

        for m in methods {
            match m.1.as_ref() {
                Expression::AssocTypeDef {
                    name: aname,
                    type_params: assoc_params,
                    ty,
                } => {
                    // Consume the AssocTypeDef wrapper NodeId, then the RHS.
                    let wrapper_id = self.ids.ids()[self.next_id_idx];
                    self.next_id_idx += 1;
                    self.cache.insert(wrapper_id, unit_ty());
                    let mut assoc_frame = HashMap::new();
                    let mut assoc_param_vars = Vec::new();
                    let mut assoc_param_kinds = Vec::new();
                    for tp in assoc_params {
                        let var = self.counter.fresh();
                        let kind = self.resolve_type_param_kind(tp);
                        self.set_var_kind(var, kind.clone());
                        assoc_frame.insert(tp.name.to_string(), var);
                        assoc_param_vars.push(var);
                        assoc_param_kinds.push(kind);
                    }
                    let pushed_assoc_params = !assoc_frame.is_empty();
                    if pushed_assoc_params {
                        self.type_params_in_scope.push(assoc_frame);
                    }
                    let resolved = self.parse_type_name(ty);
                    if pushed_assoc_params {
                        let _ = self.type_params_in_scope.pop();
                    }
                    self.cache_forced_ty(ty, resolved.clone());
                    if let Some(cdef) = class_def.as_ref()
                        && let Some(decl) = cdef.assoc_type(aname)
                    {
                        if decl.params.len() != assoc_params.len() {
                            invalid_assoc_defs = true;
                            self.messages.push(Message::error(
                                ErrorCode::GenericTypeError,
                                format!(
                                    "Associated type `{}` in instance of `{}` expects {} type parameter{}, got {}",
                                    aname,
                                    class,
                                    decl.params.len(),
                                    if decl.params.len() == 1 { "" } else { "s" },
                                    assoc_params.len()
                                ),
                                m.0.into_range(),
                            ));
                        }
                        for (i, (expected, actual)) in decl
                            .param_kinds
                            .iter()
                            .zip(assoc_param_kinds.iter())
                            .enumerate()
                        {
                            if expected != actual {
                                invalid_assoc_defs = true;
                                self.messages.push(Message::error(
                                    ErrorCode::GenericTypeError,
                                    format!(
                                        "Type parameter {} of associated type `{}` has kind `{}`, expected `{}`",
                                        i + 1,
                                        aname,
                                        actual,
                                        expected
                                    ),
                                    m.0.into_range(),
                                ));
                            }
                        }
                        let rhs_kind = self.kind_of_ty(&resolved);
                        if rhs_kind != Kind::Type {
                            invalid_assoc_defs = true;
                            self.messages.push(Message::error(
                                ErrorCode::GenericTypeError,
                                format!(
                                    "Associated type `{}` in instance of `{}` must resolve to kind `*`, found `{}`",
                                    aname, class, rhs_kind
                                ),
                                ty.0.into_range(),
                            ));
                        }
                    }
                    if assoc_tys.contains_key(*aname) {
                        self.messages.push(Message::error(
                            ErrorCode::GenericTypeError,
                            format!(
                                "Duplicate associated type `{}` in instance of `{}`",
                                aname, class
                            ),
                            m.0.into_range(),
                        ));
                    } else {
                        assoc_names.push(aname.to_string());
                        let value = AssocTypeValue {
                            params: assoc_params
                                .iter()
                                .map(|tp| tp.name.to_string())
                                .collect(),
                            param_vars: assoc_param_vars,
                            param_kinds: assoc_param_kinds,
                            ty: resolved,
                        };
                        if let Some(idx) = stub_idx {
                            self.generics.instances[idx]
                                .assoc_tys
                                .insert(aname.to_string(), value.clone());
                        }
                        assoc_tys.insert(aname.to_string(), value);
                    }
                }
                _ => {
                    let maybe_fn = match m.1.as_ref() {
                        Expression::Function {
                            docs: _,
                            name,
                            type_params,
                            args,
                            returns,
                            where_constraints,
                            body,
                            is_coro,
                            ..
                        } => Some((
                            *name,
                            type_params.as_slice(),
                            args,
                            returns,
                            where_constraints.as_slice(),
                            body,
                            *is_coro,
                        )),
                        Expression::Method(_, body) => match body.1.as_ref() {
                            Expression::Function {
                                docs: _,
                                name,
                                type_params,
                                args,
                                returns,
                                where_constraints,
                                body,
                                is_coro,
                                ..
                            } => Some((
                                *name,
                                type_params.as_slice(),
                                args,
                                returns,
                                where_constraints.as_slice(),
                                body,
                                *is_coro,
                            )),
                            _ => None,
                        },
                        _ => None,
                    };
                    if let Some((mname, mparams, margs, returns, where_cs, body, is_coro)) =
                        maybe_fn
                    {
                        let fqn = format!(
                            "{}__{}__{}",
                            class,
                            arg_tys
                                .iter()
                                .map(|t| format!("{}", t))
                                .collect::<Vec<_>>()
                                .join("_"),
                            mname,
                        );
                        method_names.push(mname.to_string());
                        method_fqns.insert(mname.to_string(), fqn.clone());
                        self.infer_function(
                            mname,
                            mparams,
                            margs,
                            returns.as_ref(),
                            where_cs,
                            body.as_ref(),
                            &m.0.into_range(),
                            None,
                            is_coro,
                            None,
                            false,
                        );
                    } else {
                        let _ = self.infer(m);
                    }
                }
            }
        }
        let mut invalid_instance =
            class_def.is_none() || orphaned || (overlapping.is_some() && !same_decl);
        if let Some(class_def) = class_def.as_ref() {
            // Superclass instances must already exist for the same args.
            // `impl Ordered<int>` requires `Equal<int>`, transitively.
            let mut missing_supers = Vec::new();
            let mut seen_super = HashSet::new();
            let mut stack: Vec<String> = class_def.superclasses.clone();
            while let Some(super_name) = stack.pop() {
                if !seen_super.insert(super_name.clone()) {
                    continue;
                }
                if self.generics.find_instance(&super_name, &arg_tys).is_none() {
                    missing_supers.push(super_name.clone());
                }
                if let Some(super_def) = self.generics.typeclass(&super_name) {
                    stack.extend(super_def.superclasses.iter().cloned());
                }
            }
            for super_name in &missing_supers {
                let args_pretty = arg_tys
                    .iter()
                    .map(|ty| ty.to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                self.messages.push(Message::error(
                    ErrorCode::GenericTypeError,
                    format!(
                        "Instance of `{}` for `{}` requires superclass instance `{}<{}>`",
                        class, args_pretty, super_name, args_pretty
                    ),
                    range.clone(),
                ));
            }
            let unknown_methods = Generics::unknown_instance_methods(
                class_def,
                method_names.iter().map(|name| name.as_str()),
            );
            for method in &unknown_methods {
                self.messages.push(Message::error(
                    ErrorCode::GenericTypeError,
                    format!("Unknown method `{}` in instance of `{}`", method, class),
                    range.clone(),
                ));
            }
            let unknown_assoc = Generics::unknown_assoc_types(
                class_def,
                assoc_names.iter().map(|n| n.as_str()),
            );
            for aname in &unknown_assoc {
                self.messages.push(Message::error(
                    ErrorCode::GenericTypeError,
                    format!(
                        "Unknown associated type `{}` in instance of `{}`",
                        aname, class
                    ),
                    range.clone(),
                ));
            }
            let missing_assoc = Generics::missing_assoc_types(class_def, &assoc_tys);
            if !missing_assoc.is_empty() {
                let names = missing_assoc
                    .iter()
                    .map(|n| format!("`{}`", n))
                    .collect::<Vec<_>>()
                    .join(", ");
                let noun = if missing_assoc.len() == 1 {
                    "associated type"
                } else {
                    "associated types"
                };
                self.messages.push(Message::error(
                    ErrorCode::GenericTypeError,
                    format!(
                        "Instance of `{}` for `{}` is missing {} {}",
                        class,
                        arg_tys
                            .iter()
                            .map(|ty| ty.to_string())
                            .collect::<Vec<_>>()
                            .join(", "),
                        noun,
                        names
                    ),
                    range.clone(),
                ));
            }
            Generics::fill_default_method_fqns(class_def, &mut method_fqns);
            let missing_methods =
                Generics::missing_required_methods(class_def, &method_fqns);
            if !missing_methods.is_empty() {
                let methods = missing_methods
                    .iter()
                    .map(|method| format!("`{}`", method))
                    .collect::<Vec<_>>()
                    .join(", ");
                let noun = if missing_methods.len() == 1 {
                    "method"
                } else {
                    "methods"
                };
                self.messages.push(Message::error(
                    ErrorCode::GenericTypeError,
                    format!(
                        "Instance of `{}` for `{}` is missing {} {}",
                        class,
                        arg_tys
                            .iter()
                            .map(|ty| ty.to_string())
                            .collect::<Vec<_>>()
                            .join(", "),
                        noun,
                        methods
                    ),
                    range.clone(),
                ));
            }
            invalid_instance |= !unknown_methods.is_empty()
                || !missing_methods.is_empty()
                || !missing_supers.is_empty()
                || !unknown_assoc.is_empty()
                || !missing_assoc.is_empty()
                || invalid_assoc_defs;
        }
        // Finalize the stub (or push a fresh instance when no stub).
        // Omitted defaulted methods have been filled with class-default FQNs.
        // Never `remove(idx)` — that shifts later instance indices; clear
        // the stub in place when invalid instead.
        if let Some(idx) = stub_idx {
            if invalid_instance {
                self.generics.instances[idx].method_fqns.clear();
                self.generics.instances[idx].assoc_tys.clear();
                self.generics.instances[idx].args.clear();
                self.generics.instances[idx].class.clear();
            } else {
                self.generics.instances[idx].method_fqns = method_fqns;
                self.generics.instances[idx].assoc_tys = assoc_tys;
                self.generics.instances[idx].args = arg_tys.clone();
            }
        } else if !invalid_instance {
            self.generics.instances.push(InstanceDef {
                class: class.to_string(),
                defined_module: self.current_module.clone(),
                range: range.clone(),
                args: arg_tys,
                method_fqns,
                assoc_tys,
            });
        }
        unit_ty()
    }

    #[inline(never)]

    pub(super) fn infer_impl(
        &mut self,
        what: &str,
        owner: &str,
        type_params: &[parser::ast::TypeParam<'_>],
        methods: &[Output],
        range: &Range<usize>,
    ) {
        let pushed = self.push_type_params_for_type_parsing(type_params);
        let owner_key = self.qualify_module_name(owner);

        let owner_ty = if type_params.is_empty() {
            Ty::Con(owner_key.clone())
        } else {
            let frame = self
                .type_params_in_scope
                .last()
                .expect("type-param frame just pushed");
            let args: Vec<Ty> = type_params
                .iter()
                .map(|tp| Ty::Var(*frame.get(tp.name).expect("type param registered in frame")))
                .collect();
            Ty::App(Box::new(Ty::Con(owner_key.clone())), args)
        };

        let param_vars: Vec<TyVarId> = if pushed {
            let frame = self
                .type_params_in_scope
                .last()
                .expect("type-param frame just pushed");
            type_params
                .iter()
                .map(|tp| *frame.get(tp.name).expect("type param registered in frame"))
                .collect()
        } else {
            Vec::new()
        };

        let owner_is_class = self.classes.contains_key(&owner_key);
        if !owner_is_class {
            self.classes.insert(owner_key.clone(), Vec::new());
            self.env
                .insert_top(owner_key.clone(), Scheme::mono(Ty::Con(owner_key.clone())));
        }

        // `impl Foo<T: Eq + Hash>` bounds apply to every method body and to
        // the poly scheme / dictionary arity at call sites.
        let mut impl_constraints: Vec<Constraint> = Vec::new();
        if pushed {
            let bound_vars: Vec<(String, TyVarId)> = {
                let frame = self
                    .type_params_in_scope
                    .last()
                    .expect("type-param frame just pushed");
                type_params
                    .iter()
                    .map(|tp| {
                        (
                            tp.name.to_string(),
                            *frame
                                .get(tp.name)
                                .expect("type param registered in frame"),
                        )
                    })
                    .collect()
            };
            for (tp, (_name, var)) in type_params.iter().zip(bound_vars.iter()) {
                for bound in &tp.bounds {
                    if let Some(constraint) =
                        self.constraint_from_bound(bound, Ty::Var(*var), range)
                    {
                        impl_constraints.push(constraint);
                    }
                }
            }
        }
        let prev_constraints_len = self.active_constraints.len();
        self.active_constraints
            .extend(impl_constraints.iter().cloned());

        let prev_impl_owner = self.impl_owner.replace(owner_key.clone());
        // Register method schemes on the outer env (not a temporary frame).
        // Call-site dict emission looks them up by `Owner::method` FQN;
        // a push/pop around the loop used to drop those schemes.

        for method in methods {
            if let Expression::Method(vis, body) = method.1.as_ref() {
                if let Expression::Function {
                    docs: _,
                    name,
                    is_coro,
                    is_static,
                    args,
                    returns,
                    where_constraints,
                    body: func_body,
                    ..
                } = body.1.as_ref()
                {
                    if *name == "drop" {
                        self.check_drop_decl(
                            what,
                            &owner_key,
                            owner_is_class,
                            *is_static,
                            args,
                            &method.0.into_range(),
                        );
                    }
                    let self_ty = if *is_static { None } else { Some(&owner_ty) };
                    // Type params stay in the outer impl frame so `self`
                    // and method annotations share the same variables.
                    let fun_ty = self.infer_function(
                        name,
                        &[],
                        args,
                        returns.as_ref(),
                        where_constraints,
                        func_body.as_ref(),
                        &method.0.into_range(),
                        self_ty,
                        *is_coro,
                        Some(&owner_key),
                        *is_static,
                    );
                    if *name == "drop" {
                        let mut ret = apply_ty(&self.subst, &fun_ty);
                        while let Ty::Fun(_, r) = ret {
                            ret = *r;
                        }
                        match &ret {
                            Ty::Con(n) if n == "unit" => {}
                            Ty::Var(_) => {
                                self.unify(
                                    &ret,
                                    &unit_ty(),
                                    &method.0.into_range(),
                                    "drop return type",
                                );
                            }
                            _ => {
                                self.messages.push(Message::error(
                                    ErrorCode::InvalidDrop,
                                    "fn drop(self) must return unit".to_string(),
                                    method.0.into_range(),
                                ));
                            }
                        }
                    }
                    // Method calls resolve as `Owner::method`; mirror that
                    // key for named-arg reorder (self is never named).
                    let fqn = format!("{}::{}", owner_key, name);
                    if let Some(names) = self.fn_param_names.get(*name).cloned() {
                        self.fn_param_names.insert(fqn.clone(), names);
                    }
                    if let Some(has) = self.fn_has_rest.get(*name).copied() {
                        self.fn_has_rest.insert(fqn.clone(), has);
                    }
                    if let Some(tuple) = self.fn_tuple_rest.get(*name).copied() {
                        self.fn_tuple_rest.insert(fqn.clone(), tuple);
                    }
                    let scheme = if param_vars.is_empty() {
                        Scheme::mono(fun_ty.clone())
                    } else {
                        Scheme::poly(
                            param_vars.clone(),
                            impl_constraints.clone(),
                            fun_ty.clone(),
                        )
                    };
                    // Env lookup feeds `emit_call_site_dicts` at method CALL.
                    self.env.insert_top(fqn.clone(), scheme.clone());
                    if !impl_constraints.is_empty() {
                        let dict_n = impl_constraints.len();
                        // Bare name: method compile looks up dict arity by
                        // the function's short name; FQN: call sites / env.
                        self.fn_dict_arity.insert((*name).to_string(), dict_n);
                        self.fn_dict_arity.insert(fqn.clone(), dict_n);
                        self.generic_fns.insert((*name).to_string());
                        self.generic_fns.insert(fqn.clone());
                        self.generics.generic_fns.insert((*name).to_string());
                        self.generics.generic_fns.insert(fqn.clone());
                    }
                    // Arity overloads keyed by FQN — user-arg count only
                    // (`self` is packed separately at CALL sites).
                    let has_rest = self.fn_has_rest.get(*name).copied().unwrap_or(false);
                    let param_names = self.fn_param_names.get(*name).cloned().unwrap_or_default();
                    let fixed_arity = if has_rest {
                        param_names.len().saturating_sub(1)
                    } else {
                        param_names.len()
                    };
                    // Codegen looks up overload_decl_at on the Function span.
                    // Method.0 includes a leading pub, so using it misses and
                    // every overload falls back to id 0 (wrong selected body).
                    let fn_range = body.0.into_range();
                    let method_range = method.0.into_range();
                    self.register_overload_candidate(
                        &fqn,
                        OverloadCandidate {
                            id: 0,
                            fixed_arity,
                            is_rest: has_rest,
                            scheme: scheme.clone(),
                            param_names,
                        },
                        &fn_range,
                    );
                    // Alias the Method wrapper span (docs/attrs/pub) so a
                    // walker that still keys off method.0 hits the same id.
                    if let Some(id) = self
                        .overload_decl_by_span
                        .get(&(fn_range.start, fn_range.end))
                        .copied()
                    {
                        self.overload_decl_by_span
                            .insert((method_range.start, method_range.end), id);
                    }
                    if *is_static {
                        self.static_methods
                            .entry(owner_key.clone())
                            .or_default()
                            .insert(name.to_string());
                    }
                    self.methods
                        .entry(owner_key.clone())
                        .or_default()
                        .insert(name.to_string(), (*vis, scheme));
                } else {
                    self.messages.push(Message::error(
                        ErrorCode::GenericTypeError,
                        "Method body must be a function".to_string(),
                        method.0.into_range(),
                    ));
                }
            }
        }

        self.impl_owner = prev_impl_owner;
        self.active_constraints.truncate(prev_constraints_len);
        self.pop_type_params_for_type_parsing(pushed);
        let _ = range;
    }


}
