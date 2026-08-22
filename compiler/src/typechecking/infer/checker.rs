use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::ops::Range;

use parser::ast::{
    Expression, ExternFunction, FieldModifier, MatchArm, Output, Pattern, TypeParam, Visibility,
};
use reporting::{ErrorCode, Label, Message};

use crate::typechecking::env::{Env, TyVarCounter, instantiate_with_kinds};
use crate::typechecking::generics::{
    AssocTypeDecl, AssocTypeValue, Generics, InstanceDef, TypeClassDef, TypeClassMethodDef,
};
use crate::typechecking::id::{self, IdTable, NodeId};
use crate::typechecking::kind::Kind;
use crate::typechecking::subst::{Subst, apply_ty, apply_ty_prune, compose};
use crate::typechecking::ty::{AssocProjection, Constraint, Scheme};
use crate::typechecking::ty::{ArrayLength, array, array_fixed, tuple as tuple_ty};
use crate::typechecking::ty::{
    EnumVariantPayloadTy, Ty, TyVarId, boolean, float, int, is_option_ty, is_result_ty,
    list, never, option_app_ty, option_inner, option_ty, range_app, range_inclusive_ty, range_ty,
    readonly_ty, result_app_ty, result_ok_err, result_ty, schemaize_payload, schemaize_ty, string,
    strip_readonly, subst_payload_params, subst_ty_params, unit as unit_ty, vec_app_ty,
    vec_element_ty, RANGE, RANGE_INCLUSIVE,
};
use crate::typechecking::unify::{UnifyError, unify_with};
use crate::typechecking::virtual_modules::{
    BuiltinExport, FfiBuiltin, GcBuiltin, IoBuiltin, PreludeFn, StringBuiltin, ThreadBuiltin,
    VirtualModules,
};

use super::*;

/// Max native recursion depth for [`Checker::infer`]. Chosen well under what
/// a debug-build stack of a few MiB can hold even with `infer_inner`'s
/// current per-call frame size — see docs/internals/limitations.md.
const INFER_RECURSION_LIMIT: u32 = 2000;

/// Private unwind payload for [`Checker::infer`]'s recursion-limit panic.
/// Caught in [`Checker::check_program`]; never lets user input abort the
/// process the way a genuine native stack overflow does.
struct RecursionLimitExceeded;

impl Checker {
    pub fn new() -> Self {
        let mut env = Env::new();
        // Always start with one frame so callers can `register_native`
        // (and later `Compiler::register`) before `check_program` is
        // ever called. `check_program` pushes a second frame so the
        // first stays around for inspection.
        env.push();
        let mut checker = Self {
            env,
            counter: TyVarCounter::new(),
            subst: Subst::empty(),
            messages: Vec::new(),
            current_return_ty: None,
            current_module: String::new(),
            virtual_modules: VirtualModules::new(),
            scope_bindings: HashMap::new(),
            disk_imports: HashSet::new(),
            current_match_lhs: None,
            classes: std::collections::HashMap::new(),
            class_type_ids: std::collections::HashMap::new(),
            next_class_type_id: 1,
            classes_with_drop: std::collections::HashSet::new(),
            methods: std::collections::HashMap::new(),
            static_methods: std::collections::HashMap::new(),
            ids: IdTable::new(),
            next_id_idx: 0,
            infer_depth: 0,
            cache: std::collections::HashMap::new(),
            codegen_types_by_span: HashMap::new(),
            codegen_var_types: std::collections::HashMap::new(),
            polyfn_binding_spans: std::collections::HashSet::new(),
            codegen_var_types_scopes: Vec::new(),
            fn_codegen_baselines: Vec::new(),
            fn_param_names: std::collections::HashMap::new(),
            forward_free_fn_schemes: HashMap::new(),
            fn_has_rest: std::collections::HashMap::new(),
            fn_tuple_rest: std::collections::HashMap::new(),
            current_tuple_pack: None,
            spread_call_arity: HashMap::new(),
            spread_expanded_bases: std::collections::HashSet::new(),
            overload_sets: std::collections::HashMap::new(),
            selected_overloads_by_span: std::collections::HashMap::new(),
            overload_decl_by_span: std::collections::HashMap::new(),
            call_site_dicts: HashMap::new(),
            call_site_dicts_by_span: HashMap::new(),
            call_site_forward_dicts: HashMap::new(),
            call_site_forward_dicts_by_span: HashMap::new(),
            bound_method_calls: HashMap::new(),
            bound_method_calls_by_span: HashMap::new(),
            bound_operator_calls: HashMap::new(),
            bound_operator_calls_by_span: HashMap::new(),
            aggregate_arith: HashMap::new(),
            aggregate_arith_by_span: HashMap::new(),
            linear_algebra: HashMap::new(),
            linear_algebra_by_span: HashMap::new(),
            bound_display_calls: HashMap::new(),
            bound_display_calls_by_span: HashMap::new(),
            existential_packs_by_span: HashMap::new(),
            existential_method_calls: HashMap::new(),
            existential_method_calls_by_span: HashMap::new(),
            for_in_infos: HashMap::new(),
            for_in_infos_by_span: HashMap::new(),
            typeclass_method_schemes: HashMap::new(),
            current_expected: None,
            type_aliases: vec![HashMap::new()],
            generic_aliases: HashMap::new(),
            const_scopes: vec![HashSet::new()],
            const_fold_env: HashMap::new(),
            static_slots: HashMap::new(),
            static_slot_types: HashMap::new(),
            next_static_slot: 0,
            const_class_fields: HashMap::new(),
            impl_owner: None,
            enums: BTreeMap::new(),
            enum_tags: BTreeMap::new(),
            enum_payloads: BTreeMap::new(),
            enum_arities: BTreeMap::new(),
            pending_exhaustive: Vec::new(),
            async_functions: std::collections::HashSet::new(),
            async_depth: 0,
            current_yield_ty: None,
            current_send_ty: None,
            yield_receives_used: false,
            c_structs: Vec::new(),
            callback_sigs: Vec::new(),
            ffi_fn_ret_tys: HashMap::new(),
            ffi_fn_variadic: HashMap::new(),
            ffi_fn_nfixed: HashMap::new(),
            ffi_fn_ret_by_field: HashMap::new(),
            ffi_fn_variadic_by_field: HashMap::new(),
            ffi_fn_nfixed_by_field: HashMap::new(),
            ffi_fn_param_invoke_ret: HashMap::new(),
            current_function: None,
            extern_variadic: HashSet::new(),
            extern_variadic_nfixed: HashMap::new(),
            variadic_call_arg_tags: HashMap::new(),
            fn_result_mode: None,
            fn_option_mode: None,
            result_mode_fns: HashSet::new(),
            result_mode_ok_is_result: HashSet::new(),
            option_mode_fns: HashSet::new(),
            test_case_names: Vec::new(),
            main_decl_span: None,
            type_params_in_scope: Vec::new(),
            active_constraints: Vec::new(),
            abstract_constraint_bindings: Vec::new(),
            var_kinds: HashMap::new(),
            generics: crate::typechecking::generics::Generics::new(),
            generic_fns: HashSet::new(),
            fn_dict_arity: HashMap::new(),
            current_typeclass: None,
            registering_overloadable_fn: false,
            lambda_uncaptured_outer: None,
            partial_fills_by_span: std::collections::HashMap::new(),
            partial_filled_tys_by_span: std::collections::HashMap::new(),
            current_assoc_projections: None,
            open_assoc_projections: HashMap::new(),
        };
        checker.register_builtin_enums();
        checker.register_builtin_vec();
        checker.register_builtin_range();
        checker.register_builtin_call_sigs();
        checker
    }

    /// Pre-register compiler-built-in enums (`FFIType`, `Option`, `Result`).
    fn register_builtin_enums(&mut self) {
        self.register_builtin_ffi_type();
        self.register_builtin_option_result();
        // `IoError` is NOT registered here — it is not auto-imported.
        // Registering its variants (esp. `Other`) globally would collide
        // with user enums that use the same constructor names. Tags are
        // installed on first `use io::…` that binds `IoError` or an IO fn.
    }

    /// Synthetic `Vec<T>` class + inherent methods (host natives / method sugar).
    ///
    /// Idempotent: safe to call from [`Self::new`] and again from
    /// [`Self::check_program`] after `fn_param_names` is cleared.
    fn register_builtin_vec(&mut self) {
        use common::BUILTIN_VEC_TYPE;

        // Drop prior overload candidates so re-entry does not duplicate.
        self.overload_sets.retain(|k, _| !k.starts_with("Vec::"));

        self.classes
            .insert(BUILTIN_VEC_TYPE.to_string(), Vec::new());
        if self.env.lookup(BUILTIN_VEC_TYPE).is_none() {
            self.env.insert_top(
                BUILTIN_VEC_TYPE.to_string(),
                Scheme::mono(Ty::Con(BUILTIN_VEC_TYPE.into())),
            );
        }

        let fun = |params: &[Ty], ret: Ty| {
            params
                .iter()
                .rev()
                .fold(ret, |acc, p| Ty::Fun(Box::new(p.clone()), Box::new(acc)))
        };

        let unit = unit_ty();
        let dummy = 0..0;

        // Instance methods: scheme includes `self` as the first Fun param.
        // `fixed_arity` / `fn_param_names` count user args only (not self).
        let instance: &[(&str, usize, &[&str])] = &[
            ("push", 1, &["x"]),
            ("pop", 0, &[]),
            ("insert", 2, &["i", "x"]),
            ("remove", 1, &["i"]),
            ("clear", 0, &[]),
            ("reserve", 1, &["n"]),
            ("capacity", 0, &[]),
            ("len", 0, &[]),
        ];

        for &(name, arity, params) in instance {
            let t = self.counter.fresh();
            let vec_t = vec_app_ty(Ty::Var(t));
            let opt_t = option_app_ty(Ty::Var(t));
            let ty = match name {
                "push" => fun(&[vec_t, Ty::Var(t)], unit.clone()),
                "pop" => fun(&[vec_t], opt_t),
                "insert" => fun(&[vec_t, int(), Ty::Var(t)], unit.clone()),
                "remove" => fun(&[vec_t, int()], opt_t),
                "clear" => fun(&[vec_t], unit.clone()),
                "reserve" => fun(&[vec_t, int()], unit.clone()),
                "capacity" | "len" => fun(&[vec_t], int()),
                _ => unreachable!("unknown Vec instance method"),
            };
            let fqn = format!("{}::{}", BUILTIN_VEC_TYPE, name);
            let scheme = Scheme::poly(vec![t], vec![], ty);
            self.fn_param_names.insert(
                fqn.clone(),
                params.iter().map(|s| (*s).to_string()).collect(),
            );
            self.register_overload_candidate(
                &fqn,
                OverloadCandidate {
                    id: 0,
                    fixed_arity: arity,
                    is_rest: false,
                    scheme: scheme.clone(),
                    param_names: params.iter().map(|s| (*s).to_string()).collect(),
                },
                &dummy,
            );
            self.methods
                .entry(BUILTIN_VEC_TYPE.to_string())
                .or_default()
                .insert(name.to_string(), (Visibility::Public, scheme));
        }

        // Static methods.
        let statics: &[(&str, usize, &[&str])] = &[
            ("new", 0, &[]),
            ("with_capacity", 1, &["n"]),
            ("from", 1, &["arr"]),
        ];

        for &(name, arity, params) in statics {
            let t = self.counter.fresh();
            let vec_t = vec_app_ty(Ty::Var(t));
            let ty = match name {
                // Nullary sealed: call site `Vec::new()` applies unit.
                "new" => fun(&[unit.clone()], vec_t),
                "with_capacity" => fun(&[int()], vec_t),
                "from" => fun(&[array(Ty::Var(t))], vec_t),
                _ => unreachable!("unknown Vec static method"),
            };
            let fqn = format!("{}::{}", BUILTIN_VEC_TYPE, name);
            let scheme = Scheme::poly(vec![t], vec![], ty);
            self.fn_param_names.insert(
                fqn.clone(),
                params.iter().map(|s| (*s).to_string()).collect(),
            );
            self.register_overload_candidate(
                &fqn,
                OverloadCandidate {
                    id: 0,
                    fixed_arity: arity,
                    is_rest: false,
                    scheme: scheme.clone(),
                    param_names: params.iter().map(|s| (*s).to_string()).collect(),
                },
                &dummy,
            );
            self.static_methods
                .entry(BUILTIN_VEC_TYPE.to_string())
                .or_default()
                .insert(name.to_string());
            self.methods
                .entry(BUILTIN_VEC_TYPE.to_string())
                .or_default()
                .insert(name.to_string(), (Visibility::Public, scheme));
        }
    }

    /// Synthetic `Range<T>` / `RangeInclusive<T>` inherent `to_vec`.
    ///
    /// Idempotent: same re-entry contract as [`Self::register_builtin_vec`].
    /// Construction stays `T: Ord`; `.to_vec()` is rejected for non-numeric
    /// elements at the call site (see [`Self::constrain_range_to_vec`]).
    fn register_builtin_range(&mut self) {
        self.overload_sets
            .retain(|k, _| !k.starts_with("Range::") && !k.starts_with("RangeInclusive::"));

        let fun = |params: &[Ty], ret: Ty| {
            params
                .iter()
                .rev()
                .fold(ret, |acc, p| Ty::Fun(Box::new(p.clone()), Box::new(acc)))
        };
        let dummy = 0..0;

        for owner in [RANGE, RANGE_INCLUSIVE] {
            self.classes
                .entry(owner.to_string())
                .or_insert_with(Vec::new);
            let t = self.counter.fresh();
            let recv = if owner == RANGE {
                range_ty(Ty::Var(t))
            } else {
                range_inclusive_ty(Ty::Var(t))
            };
            let scheme = Scheme::poly(vec![t], vec![], fun(&[recv], vec_app_ty(Ty::Var(t))));
            let fqn = format!("{owner}::to_vec");
            self.fn_param_names.insert(fqn.clone(), Vec::new());
            self.register_overload_candidate(
                &fqn,
                OverloadCandidate {
                    id: 0,
                    fixed_arity: 0,
                    is_rest: false,
                    scheme: scheme.clone(),
                    param_names: Vec::new(),
                },
                &dummy,
            );
            self.methods
                .entry(owner.to_string())
                .or_default()
                .insert("to_vec".to_string(), (Visibility::Public, scheme));
        }
    }

    /// Parameter names for builtins that support named arguments at call sites.
    fn register_builtin_call_sigs(&mut self) {
        self.fn_param_names
            .insert("len".into(), vec!["value".into()]);
        self.fn_param_names
            .insert("string::format".into(), vec!["fmt".into(), "args".into()]);
        self.fn_param_names
            .insert("string::from_bytes".into(), vec!["bytes".into()]);
        self.fn_param_names
            .insert("string::to_bytes".into(), vec!["s".into()]);
        self.fn_param_names
            .insert("assert".into(), vec!["cond".into(), "msg".into()]);
        self.fn_param_names
            .insert("block_on".into(), vec!["handle".into()]);
        self.fn_param_names
            .insert("dload".into(), vec!["path".into()]);
        self.fn_param_names
            .insert("declare".into(), vec!["lib".into(), "name".into(), "sig".into()]);
        self.fn_param_names.insert(
            "invoke".into(),
            vec!["lib".into(), "name".into(), "args".into()],
        );
    }

    /// Reset scope bindings and inject the auto-prelude.
    pub fn inject_prelude_scope(&mut self) {
        self.scope_bindings.clear();
        for export in self.virtual_modules.prelude_exports() {
            let name = export.short_name().to_string();
            self.scope_bindings.insert(name, export);
        }
    }

    /// Bind a virtual export under `local` (and drop any previous short
    /// binding for the export's canonical short name when `local` differs).
    pub fn bind_virtual_export(&mut self, local: String, export: BuiltinExport) {
        let host_registry = match &export {
            BuiltinExport::HostFn { registry, .. } => Some(*registry),
            _ => None,
        };
        // Lazily register `IoError` tags when the virtual `io` module is
        // brought into scope (enum or any host fn whose scheme mentions it).
        let needs_io_error = matches!(
            &export,
            BuiltinExport::Enum {
                name: common::BUILTIN_IO_ERROR_ENUM
            } | BuiltinExport::IoFn { .. }
                | BuiltinExport::StringFn {
                    kind: StringBuiltin::FromBytes,
                }
        ) || host_registry.is_some_and(|r| r.starts_with("fs_"));
        if needs_io_error && !self.enums.contains_key(common::BUILTIN_IO_ERROR_ENUM) {
            self.register_builtin_io_error();
        }

        let needs_thread_error = matches!(
            &export,
            BuiltinExport::Enum {
                name: common::BUILTIN_THREAD_ERROR_ENUM
            } | BuiltinExport::ThreadFn { .. }
        );
        if needs_thread_error && !self.enums.contains_key(common::BUILTIN_THREAD_ERROR_ENUM) {
            self.register_builtin_thread_error();
        }

        let needs_time_error = matches!(
            &export,
            BuiltinExport::Enum {
                name: common::BUILTIN_TIME_ERROR_ENUM
            }
        ) || host_registry.is_some_and(|r| r.starts_with("time_"));
        if needs_time_error && !self.enums.contains_key(common::BUILTIN_TIME_ERROR_ENUM) {
            self.register_builtin_time_error();
        }

        let needs_env_error = matches!(
            &export,
            BuiltinExport::Enum {
                name: common::BUILTIN_ENV_ERROR_ENUM
            }
        ) || host_registry.is_some_and(|r| r.starts_with("env_"));
        if needs_env_error && !self.enums.contains_key(common::BUILTIN_ENV_ERROR_ENUM) {
            self.register_builtin_env_error();
        }

        let needs_crypto_error = matches!(
            &export,
            BuiltinExport::Enum {
                name: common::BUILTIN_CRYPTO_ERROR_ENUM
            }
        ) || host_registry.is_some_and(|r| r.starts_with("crypto_"));
        if needs_crypto_error && !self.enums.contains_key(common::BUILTIN_CRYPTO_ERROR_ENUM) {
            self.register_builtin_crypto_error();
        }


        // Lazily register `Error` / `ErrorKind` when the virtual `ffi`
        // module is brought into scope (enum or any FFI builtin).
        let needs_ffi_error = matches!(
            &export,
            BuiltinExport::Enum {
                name: common::BUILTIN_FFI_ERROR_ENUM
            } | BuiltinExport::Enum {
                name: common::BUILTIN_FFI_ERROR_KIND_ENUM
            } | BuiltinExport::FfiFn { .. }
        );
        if needs_ffi_error {
            if !self.enums.contains_key(common::BUILTIN_FFI_ERROR_KIND_ENUM) {
                self.register_builtin_ffi_error_kind();
            }
            if !self.enums.contains_key(common::BUILTIN_FFI_ERROR_ENUM) {
                self.register_builtin_ffi_error();
            }
        }

        let canonical = export.short_name().to_string();
        if local != canonical {
            // `use prelude::ops::Eq as PreludeEq` frees the short name.
            if self
                .scope_bindings
                .get(&canonical)
                .is_some_and(|e| e == &export)
            {
                self.scope_bindings.remove(&canonical);
            }
        }
        self.scope_bindings.insert(local, export);
    }

    /// Look up a short name in the virtual-module scope.
    pub fn scope_binding(&self, name: &str) -> Option<&BuiltinExport> {
        self.scope_bindings.get(name)
    }

    /// True when `name` is an in-scope FFI tag constructor (`Int`, …).
    pub fn ffi_tag_in_scope(&self, name: &str) -> bool {
        matches!(
            self.scope_bindings.get(name),
            Some(BuiltinExport::FfiTag { .. })
        )
    }

    /// Resolve an in-scope name to a userland FFI builtin, if any.
    pub fn ffi_fn_in_scope(&self, name: &str) -> Option<FfiBuiltin> {
        match self.scope_bindings.get(name)? {
            BuiltinExport::FfiFn { kind } => Some(*kind),
            _ => None,
        }
    }

    /// Resolve an in-scope name to a prelude/test callable (`assert`), if any.
    pub fn prelude_fn_in_scope(&self, name: &str) -> Option<PreludeFn> {
        match self.scope_bindings.get(name)? {
            BuiltinExport::Fn { kind } => Some(*kind),
            _ => None,
        }
    }

    /// Resolve an in-scope name to an IO host native (`open`, `read`, …).
    pub fn io_fn_in_scope(&self, name: &str) -> Option<IoBuiltin> {
        match self.scope_bindings.get(name)? {
            BuiltinExport::IoFn { kind } => Some(*kind),
            _ => None,
        }
    }

    /// Resolve an in-scope name to a string helper (`format`, `to_bytes`, …).
    pub fn string_fn_in_scope(&self, name: &str) -> Option<StringBuiltin> {
        match self.scope_bindings.get(name)? {
            BuiltinExport::StringFn { kind } => Some(*kind),
            _ => None,
        }
    }

    /// Resolve an in-scope name to a thread host native (`spawn`, `send`, …).
    pub fn thread_fn_in_scope(&self, name: &str) -> Option<ThreadBuiltin> {
        match self.scope_bindings.get(name)? {
            BuiltinExport::ThreadFn { kind } => Some(*kind),
            _ => None,
        }
    }

    /// Resolve an in-scope name to a GC host native (`root`, `weak`, …).
    pub fn gc_fn_in_scope(&self, name: &str) -> Option<GcBuiltin> {
        match self.scope_bindings.get(name)? {
            BuiltinExport::GcFn { kind } => Some(*kind),
            _ => None,
        }
    }

    /// Registry key for a generic host native (`time_*`, `env_*`, `fs_*`, `crypto_*`).
    pub fn host_fn_in_scope(&self, name: &str) -> Option<&'static str> {
        match self.scope_bindings.get(name)? {
            BuiltinExport::HostFn { registry, .. } => Some(registry),
            _ => None,
        }
    }

    /// True when a bare enum/trait name is allowed (prelude or explicit use).
    pub fn builtin_name_in_scope(&self, name: &str) -> bool {
        self.scope_bindings.contains_key(name)
    }

    pub fn virtual_modules(&self) -> &VirtualModules {
        &self.virtual_modules
    }

    /// Apply a `use` against virtual modules. Returns `true` when handled
    /// (caller should not treat it as a disk-module function import).
    ///
    /// Wildcard `use …::*` is rejected in `infer` (`E0124`) before this runs.
    pub fn apply_virtual_use(&mut self, path: &[String], name: &str, alias: Option<&str>) -> bool {
        let Some(export) = self.virtual_modules.resolve_item(path, name) else {
            return false;
        };
        let local = alias
            .map(|s| s.to_string())
            .unwrap_or_else(|| name.to_string());
        self.bind_virtual_export(local, export);
        true
    }

    /// Pre-register the compiler-built-in `FFIType` enum with fixed tags.
    fn register_builtin_ffi_type(&mut self) {
        use common::{BUILTIN_FFI_TYPE_ENUM, BUILTIN_FFI_TYPE_VARIANTS};
        let name = BUILTIN_FFI_TYPE_ENUM.to_string();
        let variant_names: Vec<String> = BUILTIN_FFI_TYPE_VARIANTS
            .iter()
            .map(|s| s.to_string())
            .collect();
        let arities = vec![0; variant_names.len()];
        let payloads = vec![EnumVariantPayloadTy::Unit; variant_names.len()];
        let mut tag_map = BTreeMap::new();
        for (i, vn) in variant_names.iter().enumerate() {
            tag_map.insert(vn.clone(), i as u32);
        }
        self.enums.insert(name.clone(), variant_names);
        self.enum_tags.insert(name.clone(), tag_map);
        self.enum_payloads.insert(name.clone(), payloads);
        self.enum_arities.insert(name, arities);
    }

    /// Pre-register polymorphic `Option` / `Result` tags and payload
    /// shapes. Type annotations use `Ty::App`; these registry entries
    /// remain for constructor tags, payload arities, and codegen.
    fn register_builtin_option_result(&mut self) {
        use common::{
            BUILTIN_OPTION_ENUM, BUILTIN_OPTION_VARIANTS, BUILTIN_RESULT_ENUM,
            BUILTIN_RESULT_VARIANTS,
        };

        // Option { None, Some(T) }
        {
            let name = BUILTIN_OPTION_ENUM.to_string();
            let variant_names: Vec<String> = BUILTIN_OPTION_VARIANTS
                .iter()
                .map(|s| s.to_string())
                .collect();
            let payloads = vec![
                EnumVariantPayloadTy::Unit,
                EnumVariantPayloadTy::Tuple(vec![Ty::Con("T".into())]),
            ];
            let arities = vec![0, 1];
            let mut tag_map = BTreeMap::new();
            for (i, vn) in variant_names.iter().enumerate() {
                tag_map.insert(vn.clone(), i as u32);
            }
            self.enums.insert(name.clone(), variant_names);
            self.enum_tags.insert(name.clone(), tag_map);
            self.enum_payloads.insert(name.clone(), payloads);
            self.enum_arities.insert(name, arities);
        }

        // Result { Ok(T), Err(E) }
        {
            let name = BUILTIN_RESULT_ENUM.to_string();
            let variant_names: Vec<String> = BUILTIN_RESULT_VARIANTS
                .iter()
                .map(|s| s.to_string())
                .collect();
            let payloads = vec![
                EnumVariantPayloadTy::Tuple(vec![Ty::Con("T".into())]),
                EnumVariantPayloadTy::Tuple(vec![Ty::Con("E".into())]),
            ];
            let arities = vec![1, 1];
            let mut tag_map = BTreeMap::new();
            for (i, vn) in variant_names.iter().enumerate() {
                tag_map.insert(vn.clone(), i as u32);
            }
            self.enums.insert(name.clone(), variant_names);
            self.enum_tags.insert(name.clone(), tag_map);
            self.enum_payloads.insert(name.clone(), payloads);
            self.enum_arities.insert(name, arities);
        }
    }

    /// Pre-register `IoError` unit variants for stream IO.
    fn register_builtin_io_error(&mut self) {
        use common::{BUILTIN_IO_ERROR_ENUM, BUILTIN_IO_ERROR_VARIANTS};
        let name = BUILTIN_IO_ERROR_ENUM.to_string();
        let variant_names: Vec<String> = BUILTIN_IO_ERROR_VARIANTS
            .iter()
            .map(|s| s.to_string())
            .collect();
        let payloads = variant_names
            .iter()
            .map(|_| EnumVariantPayloadTy::Unit)
            .collect();
        let arities = vec![0; variant_names.len()];
        let mut tag_map = BTreeMap::new();
        for (i, vn) in variant_names.iter().enumerate() {
            tag_map.insert(vn.clone(), i as u32);
        }
        self.enums.insert(name.clone(), variant_names);
        self.enum_tags.insert(name.clone(), tag_map);
        self.enum_payloads.insert(name.clone(), payloads);
        self.enum_arities.insert(name, arities);
    }

    fn register_builtin_unit_enum(&mut self, enum_name: &str, variants: &[&str]) {
        let name = enum_name.to_string();
        let variant_names: Vec<String> = variants.iter().map(|s| s.to_string()).collect();
        let payloads = variant_names
            .iter()
            .map(|_| EnumVariantPayloadTy::Unit)
            .collect();
        let arities = vec![0; variant_names.len()];
        let mut tag_map = BTreeMap::new();
        for (i, vn) in variant_names.iter().enumerate() {
            tag_map.insert(vn.clone(), i as u32);
        }
        self.enums.insert(name.clone(), variant_names);
        self.enum_tags.insert(name.clone(), tag_map);
        self.enum_payloads.insert(name.clone(), payloads);
        self.enum_arities.insert(name, arities);
    }

    fn register_builtin_time_error(&mut self) {
        self.register_builtin_unit_enum(
            common::BUILTIN_TIME_ERROR_ENUM,
            common::BUILTIN_TIME_ERROR_VARIANTS,
        );
    }

    fn register_builtin_env_error(&mut self) {
        self.register_builtin_unit_enum(
            common::BUILTIN_ENV_ERROR_ENUM,
            common::BUILTIN_ENV_ERROR_VARIANTS,
        );
    }

    fn register_builtin_crypto_error(&mut self) {
        self.register_builtin_unit_enum(
            common::BUILTIN_CRYPTO_ERROR_ENUM,
            common::BUILTIN_CRYPTO_ERROR_VARIANTS,
        );
    }


    /// Pre-register `ThreadError` unit variants for the virtual `thread` module.
    fn register_builtin_thread_error(&mut self) {
        use common::{BUILTIN_THREAD_ERROR_ENUM, BUILTIN_THREAD_ERROR_VARIANTS};
        let name = BUILTIN_THREAD_ERROR_ENUM.to_string();
        let variant_names: Vec<String> = BUILTIN_THREAD_ERROR_VARIANTS
            .iter()
            .map(|s| s.to_string())
            .collect();
        let payloads = variant_names
            .iter()
            .map(|_| EnumVariantPayloadTy::Unit)
            .collect();
        let arities = vec![0; variant_names.len()];
        let mut tag_map = BTreeMap::new();
        for (i, vn) in variant_names.iter().enumerate() {
            tag_map.insert(vn.clone(), i as u32);
        }
        self.enums.insert(name.clone(), variant_names);
        self.enum_tags.insert(name.clone(), tag_map);
        self.enum_payloads.insert(name.clone(), payloads);
        self.enum_arities.insert(name, arities);
    }

    /// Pre-register `ffi::ErrorKind` unit variants.
    fn register_builtin_ffi_error_kind(&mut self) {
        use common::{BUILTIN_FFI_ERROR_KIND_ENUM, BUILTIN_FFI_ERROR_KIND_VARIANTS};
        let name = BUILTIN_FFI_ERROR_KIND_ENUM.to_string();
        let variant_names: Vec<String> = BUILTIN_FFI_ERROR_KIND_VARIANTS
            .iter()
            .map(|s| s.to_string())
            .collect();
        let payloads = variant_names
            .iter()
            .map(|_| EnumVariantPayloadTy::Unit)
            .collect();
        let arities = vec![0; variant_names.len()];
        let mut tag_map = BTreeMap::new();
        for (i, vn) in variant_names.iter().enumerate() {
            tag_map.insert(vn.clone(), i as u32);
        }
        self.enums.insert(name.clone(), variant_names);
        self.enum_tags.insert(name.clone(), tag_map);
        self.enum_payloads.insert(name.clone(), payloads);
        self.enum_arities.insert(name, arities);
    }

    /// Pre-register `ffi::Error` as a single record variant
    /// `Error { kind: ErrorKind, message: string }`.
    fn register_builtin_ffi_error(&mut self) {
        use common::{
            BUILTIN_FFI_ERROR_ENUM, BUILTIN_FFI_ERROR_KIND_ENUM, BUILTIN_FFI_ERROR_VARIANT,
        };
        let name = BUILTIN_FFI_ERROR_ENUM.to_string();
        let variant_names = vec![BUILTIN_FFI_ERROR_VARIANT.to_string()];
        let payloads = vec![EnumVariantPayloadTy::Record(vec![
            ("kind".into(), Ty::Con(BUILTIN_FFI_ERROR_KIND_ENUM.into())),
            ("message".into(), string()),
        ])];
        let arities = vec![2];
        let mut tag_map = BTreeMap::new();
        tag_map.insert(BUILTIN_FFI_ERROR_VARIANT.to_string(), 0u32);
        self.enums.insert(name.clone(), variant_names);
        self.enum_tags.insert(name.clone(), tag_map);
        self.enum_payloads.insert(name.clone(), payloads);
        self.enum_arities.insert(name, arities);
    }

    /// Scheme for a virtual `io` host native (inserted on `use io::{…}`).
    pub fn io_fn_scheme(kind: IoBuiltin) -> Scheme {
        #[cfg(feature = "tls")]
        use crate::typechecking::ty::record;
        use crate::typechecking::ty::{boolean, byte, stream_ty, tuple};
        let stream = stream_ty();
        let bytes = vec_app_ty(byte());
        let io_err = Ty::Con(common::BUILTIN_IO_ERROR_ENUM.into());
        let opt_int = option_app_ty(int());
        let res_opt_int = result_app_ty(opt_int, io_err.clone());
        let res_int = result_app_ty(int(), io_err.clone());
        let res_unit = result_app_ty(unit_ty(), io_err.clone());
        let res_stream = result_app_ty(stream.clone(), io_err.clone());
        let res_string = result_app_ty(string(), io_err.clone());
        let addr_ty = tuple(vec![string(), int()]);
        let res_addr = result_app_ty(addr_ty, io_err.clone());
        // `(nbytes, peer_host, peer_port)` from `io::net::udp::recv_from`.
        let recv_from_ty = tuple(vec![int(), string(), int()]);
        let res_recv_from = result_app_ty(recv_from_ty, io_err);
        let fun = |params: &[Ty], ret: Ty| {
            params
                .iter()
                .rev()
                .fold(ret, |acc, p| Ty::Fun(Box::new(p.clone()), Box::new(acc)))
        };
        let ty = match kind {
            IoBuiltin::Stdin | IoBuiltin::Stdout | IoBuiltin::Stderr => stream,
            IoBuiltin::Open => fun(&[string(), string()], res_stream),
            IoBuiltin::Close => fun(&[stream], res_unit),
            IoBuiltin::Read => fun(&[stream, bytes], res_opt_int),
            IoBuiltin::Write => fun(&[stream, bytes], res_int),
            IoBuiltin::WriteFrom => fun(&[stream, bytes, int()], res_int),
            IoBuiltin::AwaitReadable | IoBuiltin::AwaitWritable => fun(&[stream], res_unit),
            IoBuiltin::Drive | IoBuiltin::WaitReady => fun(&[], int()),
            IoBuiltin::FromBytes => fun(&[bytes], res_string),
            IoBuiltin::ToBytes => fun(&[string()], bytes),
            IoBuiltin::TcpConnect | IoBuiltin::TcpListen => fun(&[string(), int()], res_stream),
            IoBuiltin::TcpConnectTimeout => fun(&[string(), int(), int()], res_stream),
            IoBuiltin::TcpAccept => fun(&[stream], res_stream),
            IoBuiltin::TcpPeerAddr | IoBuiltin::TcpLocalAddr => fun(&[stream], res_addr),
            IoBuiltin::TcpSetNodelay => fun(&[stream, boolean()], res_unit),
            IoBuiltin::TcpShutdown => fun(&[stream, int()], res_unit),
            #[cfg(feature = "tls")]
            IoBuiltin::TlsClientEnable => {
                let opt_string = option_app_ty(string());
                let opts = record(vec![
                    ("verify".into(), boolean()),
                    ("ca_pem".into(), opt_string.clone()),
                    ("ca_path".into(), opt_string),
                    ("timeout_ms".into(), int()),
                    ("alpn".into(), string()),
                ]);
                fun(&[stream, string(), opts], res_stream)
            }
            #[cfg(feature = "tls")]
            IoBuiltin::TlsClientDisable => fun(&[stream], res_stream),
            #[cfg(feature = "tls")]
            IoBuiltin::TlsServerEnable => {
                let opts = record(vec![
                    ("cert_pem".into(), string()),
                    ("key_pem".into(), string()),
                    ("timeout_ms".into(), int()),
                    ("client_ca_pem".into(), string()),
                    ("alpn".into(), string()),
                ]);
                fun(&[stream, opts], res_stream)
            }
            #[cfg(feature = "tls")]
            IoBuiltin::TlsServerDisable => fun(&[stream], res_stream),
            #[cfg(feature = "tls")]
            IoBuiltin::TlsAlpnProtocol => fun(&[stream], res_string),
            IoBuiltin::UdpBind | IoBuiltin::UdpConnect => fun(&[string(), int()], res_stream),
            IoBuiltin::UdpSendTo => fun(&[stream, bytes, string(), int()], res_int),
            IoBuiltin::UdpRecvFrom => {
                fun(&[stream, bytes], res_recv_from)
            }
            IoBuiltin::UdpLocalPort => fun(&[stream], res_int),
        };
        Scheme::mono(ty)
    }

    /// Scheme for virtual `string` helpers.
    ///
    /// `format` is checked by the call-special path because its arity and
    /// argument types are determined by the literal specifiers.
    pub fn string_fn_scheme(kind: StringBuiltin) -> Scheme {
        use crate::typechecking::ty::byte;
        let bytes = vec_app_ty(byte());
        let io_err = Ty::Con(common::BUILTIN_IO_ERROR_ENUM.into());
        let fun = |params: &[Ty], ret: Ty| {
            params
                .iter()
                .rev()
                .fold(ret, |acc, p| Ty::Fun(Box::new(p.clone()), Box::new(acc)))
        };
        let ty = match kind {
            StringBuiltin::Format => fun(&[string()], string()),
            StringBuiltin::FromBytes => fun(&[bytes], result_app_ty(string(), io_err)),
            StringBuiltin::ToBytes => fun(&[string()], bytes),
        };
        Scheme::mono(ty)
    }

    fn virtual_callable_scheme(
        &mut self,
        export: BuiltinExport,
        range: Range<usize>,
    ) -> Option<Scheme> {
        match export {
            BuiltinExport::IoFn { kind } => Some(Self::io_fn_scheme(kind)),
            BuiltinExport::StringFn { kind } => Some(Self::string_fn_scheme(kind)),
            BuiltinExport::ThreadFn { kind } => Some(self.thread_fn_scheme(kind)),
            BuiltinExport::GcFn { kind } => Some(self.gc_fn_scheme(kind)),
            BuiltinExport::HostFn { registry, .. } => Some(self.host_fn_scheme(registry, range)),
            BuiltinExport::FfiFn { .. } => Some(Scheme::mono(Ty::Var(self.counter.fresh()))),
            _ => None,
        }
    }

    /// Scheme for a virtual `thread` host native (inserted on `use thread::{…}`).
    pub fn thread_fn_scheme(&mut self, kind: ThreadBuiltin) -> Scheme {
        use crate::typechecking::ty::{
            mutex_ty, receiver_ty, rwlock_ty, sender_ty, thread_ty, tuple,
        };
        let thread_err = Ty::Con(common::BUILTIN_THREAD_ERROR_ENUM.into());
        let res_thread = result_app_ty(thread_ty(), thread_err.clone());
        let res_unit = result_app_ty(unit_ty(), thread_err.clone());
        let fun = |params: &[Ty], ret: Ty| {
            params
                .iter()
                .rev()
                .fold(ret, |acc, p| Ty::Fun(Box::new(p.clone()), Box::new(acc)))
        };
        let fn_ty = |param: Ty, ret: Ty| Ty::Fun(Box::new(param), Box::new(ret));
        match kind {
            ThreadBuiltin::Spawn => {
                let t = self.counter.fresh();
                let a = self.counter.fresh();
                let fn_a_t = fn_ty(Ty::Var(a), Ty::Var(t));
                Scheme::poly(vec![t, a], vec![], fun(&[fn_a_t, Ty::Var(a)], res_thread))
            }
            ThreadBuiltin::Join => {
                let t = self.counter.fresh();
                Scheme::poly(
                    vec![t],
                    vec![],
                    fun(
                        &[thread_ty()],
                        result_app_ty(Ty::Var(t), thread_err.clone()),
                    ),
                )
            }
            ThreadBuiltin::Detach => Scheme::mono(fun(&[thread_ty()], res_unit)),
            ThreadBuiltin::Channel => {
                let t = self.counter.fresh();
                Scheme::poly(
                    vec![t],
                    vec![],
                    fun(
                        &[],
                        result_app_ty(tuple(vec![sender_ty(), receiver_ty()]), thread_err),
                    ),
                )
            }
            ThreadBuiltin::Send | ThreadBuiltin::TrySend => {
                let t = self.counter.fresh();
                Scheme::poly(
                    vec![t],
                    vec![],
                    fun(&[sender_ty(), Ty::Var(t)], res_unit.clone()),
                )
            }
            ThreadBuiltin::Recv | ThreadBuiltin::TryRecv => {
                let t = self.counter.fresh();
                Scheme::poly(
                    vec![t],
                    vec![],
                    fun(
                        &[receiver_ty()],
                        result_app_ty(Ty::Var(t), thread_err.clone()),
                    ),
                )
            }
            ThreadBuiltin::Close => Scheme::mono(fun(&[sender_ty()], res_unit.clone())),
            ThreadBuiltin::Mutex => {
                let t = self.counter.fresh();
                Scheme::poly(
                    vec![t],
                    vec![],
                    fun(&[Ty::Var(t)], result_app_ty(mutex_ty(), thread_err.clone())),
                )
            }
            ThreadBuiltin::Rwlock => {
                let t = self.counter.fresh();
                Scheme::poly(
                    vec![t],
                    vec![],
                    fun(
                        &[Ty::Var(t)],
                        result_app_ty(rwlock_ty(), thread_err.clone()),
                    ),
                )
            }
            ThreadBuiltin::WithLock => {
                let t = self.counter.fresh();
                let r = self.counter.fresh();
                let callback = fn_ty(Ty::Var(t), tuple(vec![Ty::Var(t), Ty::Var(r)]));
                Scheme::poly(
                    vec![t, r],
                    vec![],
                    fun(
                        &[mutex_ty(), callback],
                        result_app_ty(Ty::Var(r), thread_err.clone()),
                    ),
                )
            }
            ThreadBuiltin::WithWrite | ThreadBuiltin::TryWrite => {
                let t = self.counter.fresh();
                let r = self.counter.fresh();
                let callback = fn_ty(Ty::Var(t), tuple(vec![Ty::Var(t), Ty::Var(r)]));
                Scheme::poly(
                    vec![t, r],
                    vec![],
                    fun(
                        &[rwlock_ty(), callback],
                        result_app_ty(Ty::Var(r), thread_err.clone()),
                    ),
                )
            }
            ThreadBuiltin::WithRead | ThreadBuiltin::TryRead => {
                let t = self.counter.fresh();
                let r = self.counter.fresh();
                let callback = fn_ty(Ty::Var(t), Ty::Var(r));
                Scheme::poly(
                    vec![t, r],
                    vec![],
                    fun(
                        &[rwlock_ty(), callback],
                        result_app_ty(Ty::Var(r), thread_err.clone()),
                    ),
                )
            }
            ThreadBuiltin::Lock | ThreadBuiltin::TryLock | ThreadBuiltin::Unlock => {
                Scheme::mono(fun(&[mutex_ty()], res_unit))
            }
        }
    }

    /// Scheme for a virtual `gc` host native (inserted on `use gc::{…}`).
    pub fn gc_fn_scheme(&mut self, kind: GcBuiltin) -> Scheme {
        use crate::typechecking::ty::{root_app_ty, weak_app_ty};
        let fun = |params: &[Ty], ret: Ty| {
            params
                .iter()
                .rev()
                .fold(ret, |acc, p| Ty::Fun(Box::new(p.clone()), Box::new(acc)))
        };
        match kind {
            GcBuiltin::Root => {
                let t = self.counter.fresh();
                Scheme::poly(vec![t], vec![], fun(&[Ty::Var(t)], root_app_ty(Ty::Var(t))))
            }
            GcBuiltin::Unroot | GcBuiltin::Get => {
                let t = self.counter.fresh();
                Scheme::poly(
                    vec![t],
                    vec![],
                    fun(&[root_app_ty(Ty::Var(t))], Ty::Var(t)),
                )
            }
            GcBuiltin::Weak => {
                let t = self.counter.fresh();
                Scheme::poly(vec![t], vec![], fun(&[Ty::Var(t)], weak_app_ty(Ty::Var(t))))
            }
            GcBuiltin::Upgrade => {
                let t = self.counter.fresh();
                Scheme::poly(
                    vec![t],
                    vec![],
                    fun(&[weak_app_ty(Ty::Var(t))], option_app_ty(Ty::Var(t))),
                )
            }
            GcBuiltin::HeapBytes => Scheme::mono(fun(&[], int())),
            GcBuiltin::Collect => Scheme::mono(fun(&[], int())),
        }
    }

    /// Scheme for `fs_*` / `time_*` / `env_*` / `crypto_*` pipeline host natives.
    pub fn host_fn_scheme(&mut self, registry: &str, range: Range<usize>) -> Scheme {
        #[cfg(feature = "crypto")]
        use crate::typechecking::ty::byte;
        #[cfg(feature = "crypto")]
        use crate::typechecking::ty::tuple;
        use crate::typechecking::ty::{boolean, record};
        #[cfg(feature = "crypto")]
        use common::BUILTIN_CRYPTO_ERROR_ENUM;
        #[cfg(feature = "time")]
        use common::BUILTIN_TIME_ERROR_ENUM;
        use common::{BUILTIN_ENV_ERROR_ENUM, BUILTIN_IO_ERROR_ENUM};

        let fun = |params: &[Ty], ret: Ty| {
            params
                .iter()
                .rev()
                .fold(ret, |acc, p| Ty::Fun(Box::new(p.clone()), Box::new(acc)))
        };
        let io_err = Ty::Con(BUILTIN_IO_ERROR_ENUM.into());
        #[cfg(feature = "time")]
        let time_err = Ty::Con(BUILTIN_TIME_ERROR_ENUM.into());
        let env_err = Ty::Con(BUILTIN_ENV_ERROR_ENUM.into());
        #[cfg(feature = "crypto")]
        let crypto_err = Ty::Con(BUILTIN_CRYPTO_ERROR_ENUM.into());

        let res_bool_io = result_app_ty(boolean(), io_err.clone());
        let res_unit_io = result_app_ty(unit_ty(), io_err.clone());
        let res_string_io = result_app_ty(string(), io_err.clone());
        let res_strs_io = result_app_ty(vec_app_ty(string()), io_err.clone());
        let res_meta_io = result_app_ty(
            record(vec![
                ("size".into(), int()),
                ("is_file".into(), boolean()),
                ("is_dir".into(), boolean()),
                ("is_symlink".into(), boolean()),
                ("modified_unix".into(), int()),
            ]),
            io_err,
        );

        #[cfg(feature = "time")]
        let res_int_time = result_app_ty(int(), time_err.clone());
        #[cfg(feature = "time")]
        let res_string_time = result_app_ty(string(), time_err.clone());
        #[cfg(feature = "time")]
        let res_unit_time = result_app_ty(unit_ty(), time_err);

        let res_string_env = result_app_ty(string(), env_err.clone());
        let res_strs_env = result_app_ty(vec_app_ty(string()), env_err.clone());
        let res_unit_env = result_app_ty(unit_ty(), env_err.clone());
        let res_int_env = result_app_ty(int(), env_err);

        #[cfg(feature = "crypto")]
        let bytes = vec_app_ty(byte());
        #[cfg(feature = "crypto")]
        let res_bytes_crypto = result_app_ty(bytes.clone(), crypto_err.clone());
        #[cfg(feature = "crypto")]
        let res_bool_crypto = result_app_ty(boolean(), crypto_err.clone());
        #[cfg(feature = "crypto")]
        let res_int_crypto = result_app_ty(int(), crypto_err.clone());
        #[cfg(feature = "crypto")]
        let res_unit_crypto = result_app_ty(unit_ty(), crypto_err.clone());
        #[cfg(feature = "crypto")]
        let keypair = tuple(vec![bytes.clone(), bytes.clone()]);

        let ty = match registry {
            "fs_exists" | "fs_is_file" | "fs_is_dir" | "fs_is_symlink" => {
                fun(&[string()], res_bool_io)
            }
            "fs_metadata" => fun(&[string()], res_meta_io),
            "fs_create_dir" | "fs_create_dir_all" | "fs_remove_file" | "fs_remove_dir"
            | "fs_remove_dir_all" => fun(&[string()], res_unit_io.clone()),
            "fs_rename" | "fs_copy" | "fs_symlink" => {
                fun(&[string(), string()], res_unit_io.clone())
            }
            "fs_read_link" | "fs_realpath" => fun(&[string()], res_string_io.clone()),
            "fs_list_dir" => fun(&[string()], res_strs_io),

            #[cfg(feature = "time")]
            "time_timestamp" | "time_instant_now" | "time_epoch" => fun(&[], res_int_time.clone()),
            #[cfg(feature = "time")]
            "time_sleep_ms" => fun(&[int()], res_unit_time),
            #[cfg(feature = "time")]
            "time_elapsed_nanos"
            | "time_elapsed_millis"
            | "time_date_from_period"
            | "time_date_from_epoch_period" => fun(&[int()], res_int_time.clone()),
            #[cfg(feature = "time")]
            "time_add" | "time_sub" | "time_period_add" | "time_period_sub" => {
                fun(&[int(), int()], res_int_time.clone())
            }
            #[cfg(feature = "time")]
            "time_format" => fun(&[int(), string()], res_string_time.clone()),
            #[cfg(feature = "time")]
            "time_parse" => fun(&[string(), string()], res_int_time.clone()),
            #[cfg(feature = "time")]
            "time_date" => fun(&[], res_int_time.clone()),
            #[cfg(feature = "time")]
            "time_period" => {
                let params: Vec<Ty> = std::iter::repeat_with(int).take(9).collect();
                fun(&params, res_int_time)
            }

            "env_args" => fun(&[], res_strs_env),
            "env_var" => fun(&[string()], res_string_env.clone()),
            "env_cwd" => fun(&[], res_string_env),
            "env_remove_var" | "env_set_cwd" => fun(&[string()], res_unit_env.clone()),
            "env_set_var" => fun(&[string(), string()], res_unit_env.clone()),
            "env_exec" => fun(&[string(), vec_app_ty(string())], res_int_env),
            "env_exit" => fun(&[int()], unit_ty()),

            #[cfg(feature = "crypto")]
            "crypto_sha256" | "crypto_sha512" | "crypto_blake3" => {
                fun(&[bytes.clone()], res_bytes_crypto.clone())
            }
            #[cfg(feature = "crypto")]
            "crypto_hasher_init" => fun(&[string()], res_int_crypto.clone()),
            #[cfg(feature = "crypto")]
            "crypto_hasher_update" => fun(&[int(), bytes.clone()], res_unit_crypto.clone()),
            #[cfg(feature = "crypto")]
            "crypto_hasher_finalize" => fun(&[int()], res_bytes_crypto.clone()),
            #[cfg(feature = "crypto")]
            "crypto_hmac_sha256" | "crypto_hmac_sha512" => {
                fun(&[bytes.clone(), bytes.clone()], res_bytes_crypto.clone())
            }
            #[cfg(feature = "crypto")]
            "crypto_hmac_verify_sha256" => fun(
                &[bytes.clone(), bytes.clone(), bytes.clone()],
                res_bool_crypto.clone(),
            ),
            #[cfg(feature = "crypto")]
            "crypto_random_bytes" => fun(&[int()], res_bytes_crypto.clone()),
            #[cfg(feature = "crypto")]
            "crypto_random_u64" => fun(&[], res_int_crypto),
            #[cfg(feature = "crypto")]
            "crypto_chacha20_poly1305_encrypt"
            | "crypto_chacha20_poly1305_decrypt"
            | "crypto_aes_256_gcm_encrypt"
            | "crypto_aes_256_gcm_decrypt" => fun(
                &[bytes.clone(), bytes.clone(), bytes.clone(), bytes.clone()],
                res_bytes_crypto.clone(),
            ),
            #[cfg(feature = "crypto")]
            "crypto_ed25519_generate" | "crypto_x25519_generate" => {
                fun(&[], result_app_ty(keypair.clone(), crypto_err.clone()))
            }
            #[cfg(feature = "crypto")]
            "crypto_ed25519_sign" | "crypto_x25519_shared_secret" => {
                fun(&[bytes.clone(), bytes.clone()], res_bytes_crypto.clone())
            }
            #[cfg(feature = "crypto")]
            "crypto_ed25519_verify" => fun(
                &[bytes.clone(), bytes.clone(), bytes.clone()],
                res_bool_crypto.clone(),
            ),
            #[cfg(feature = "crypto")]
            "crypto_argon2id_hash" | "crypto_argon2id_verify" => {
                fun(&[bytes.clone(), bytes.clone()], res_unit_crypto)
            }
            #[cfg(feature = "crypto")]
            "crypto_ct_eq" => fun(&[bytes.clone(), bytes.clone()], res_bool_crypto),


            _ => {
                let mut msg = Message::error(
                    ErrorCode::GenericTypeError,
                    format!("unknown host native `{}`", registry),
                    range,
                );
                msg.with_help(
                    "every HostFn registry key must have a host_fn_scheme arm".to_string(),
                );
                self.messages.push(msg);
                Ty::Var(self.counter.fresh())
            }
        };
        Scheme::mono(ty)
    }

    /// Zero-argument functions are nullary at the call site (`f()`), but their
    /// value is still a `unit -> R` function suitable for `spawn(f)` / `MakeFn`.
    fn seal_nullary_fun_ty(fun_ty: Ty, arg_count: usize, has_self_receiver: bool) -> Ty {
        if arg_count == 0 && !has_self_receiver {
            Ty::Fun(Box::new(unit_ty()), Box::new(fun_ty))
        } else {
            fun_ty
        }
    }

    fn infer_thread_spawn_call(
        &mut self,
        arg_tys: &[Ty],
        arg_exprs: Option<&[Output]>,
        range: Range<usize>,
    ) -> Ty {
        use crate::typechecking::ty::thread_ty;
        let thread_err = Ty::Con(common::BUILTIN_THREAD_ERROR_ENUM.into());
        let res_thread = result_app_ty(thread_ty(), thread_err.clone());
        let fn_ty = |param: Ty, ret: Ty| Ty::Fun(Box::new(param), Box::new(ret));
        match arg_tys.len() {
            0 => self.error_with_help(
                ErrorCode::GenericTypeError,
                "spawn expects a function (and optional sendable argument)".to_string(),
                range,
                Some("call `spawn(work)` or `spawn(work, arg)`".to_string()),
            ),
            1 => {
                let t = self.counter.fresh();
                let expected = fn_ty(unit_ty(), Ty::Var(t));
                self.coerce_or_unify(
                    &expected,
                    &arg_tys[0],
                    arg_exprs.and_then(|a| a.first()),
                    &range,
                    "spawn function",
                );
                res_thread
            }
            2 => {
                let t = self.counter.fresh();
                let a = self.counter.fresh();
                let expected_fn = fn_ty(Ty::Var(a), Ty::Var(t));
                self.coerce_or_unify(
                    &expected_fn,
                    &arg_tys[0],
                    arg_exprs.and_then(|a| a.first()),
                    &range,
                    "spawn function",
                );
                self.coerce_or_unify(
                    &Ty::Var(a),
                    &arg_tys[1],
                    arg_exprs.and_then(|a| a.get(1)),
                    &range,
                    "spawn argument",
                );
                let arg_resolved = apply_ty_prune(&self.subst, &arg_tys[1]);
                if !self.is_thread_sendable_ty(&arg_resolved) {
                    self.error_with_help(
                        ErrorCode::GenericTypeError,
                        format!(
                            "spawn argument type `{}` is not sendable across threads",
                            arg_resolved.to_string()
                        ),
                        range.clone(),
                        Some(
                            "only immediates, strings, and aggregates of sendable values may cross threads"
                                .to_string(),
                        ),
                    );
                }
                res_thread
            }
            n => self.error_with_help(
                ErrorCode::GenericTypeError,
                format!("spawn was called with too many arguments (expected 1 or 2, got {n})"),
                range,
                None,
            ),
        }
    }

    /// Whether `ty` may be deep-copied across OS thread boundaries (best-effort).
    pub fn is_thread_sendable_ty(&self, ty: &Ty) -> bool {
        match ty {
            Ty::Var(_) => true, // re-checked on concrete spawn arg after unify
            Ty::Con(name) => {
                let n = name.to_ascii_lowercase();
                if matches!(
                    n.as_str(),
                    "stream" | "thread" | "coroutine" | "library" | "fn" | "polyfn"
                        | "root" | "weak"
                ) {
                    return false;
                }
                if matches!(
                    n.as_str(),
                    "int"
                        | "float"
                        | "string"
                        | "bool"
                        | "byte"
                        | "void"
                        | "unit"
                        | "int8"
                        | "int16"
                        | "int32"
                        | "uint8"
                        | "uint16"
                        | "uint32"
                        | "uint64"
                ) || name == crate::typechecking::ty::SENDER
                    || name == crate::typechecking::ty::RECEIVER
                    || name == crate::typechecking::ty::MUTEX
                    || name == crate::typechecking::ty::RWLOCK
                {
                    return true;
                }
                // User class: sendable when every field type is sendable
                // (so `spawn(f, mailbox)` works for a class of channel ends).
                if let Some(fields) = self.classes.get(name) {
                    return fields
                        .iter()
                        .all(|(_, _, field_ty)| self.is_thread_sendable_ty(field_ty));
                }
                false
            }
            Ty::App(head, args) => {
                if matches!(head.as_ref(), Ty::Con(n) if n == "coroutine") {
                    return false;
                }
                if matches!(
                    head.as_ref(),
                    Ty::Con(n) if n == common::BUILTIN_ROOT_TYPE || n == common::BUILTIN_WEAK_TYPE
                ) {
                    return false;
                }
                args.iter().all(|t| self.is_thread_sendable_ty(t))
            }
            Ty::Fun(_, _) => false,
            Ty::Array { element, .. } => self.is_thread_sendable_ty(element),
            Ty::List(inner) => self.is_thread_sendable_ty(inner),
            Ty::Readonly(inner) => self.is_thread_sendable_ty(inner),
            Ty::Tuple(elems) => elems.iter().all(|t| self.is_thread_sendable_ty(t)),
            Ty::Record { fields } => fields.iter().all(|(_, t)| self.is_thread_sendable_ty(t)),
            Ty::Sum { variants, .. } => variants.iter().all(|(_, payload)| {
                payload
                    .field_types()
                    .iter()
                    .all(|t| self.is_thread_sendable_ty(t))
            }),
            Ty::Existential { .. } | Ty::Constructor { .. } | Ty::Forall { .. } | Ty::Never => {
                false
            }
        }
    }

    /// Set the free-function name used when resolving bare param `invoke`
    /// fn-ids (call-site `declare` flow). Codegen must restore this while
    /// emitting a function body; returns the previous value.
    pub fn set_current_function(&mut self, name: Option<String>) -> Option<String> {
        std::mem::replace(&mut self.current_function, name)
    }

    /// Set the module path used for ownership checks while typechecking.
    pub fn set_current_module(&mut self, module: impl Into<String>) {
        self.current_module = module.into();
    }

    /// Run inference over `ast`. Returns the inferred type of the root
    /// expression under the final substitution. Diagnostic messages are
    /// accumulated and can be retrieved with [`take_messages`].
    ///
    /// The top frame is left on the env stack after this call so that
    /// callers (and tests) can inspect declared bindings. Use
    /// [`env_mut`](Self::env_mut) and [`Env::pop`] if you need to drop
    /// it.
    pub fn check_program(&mut self, ast: &Output) -> Ty {
        // Reset per-program state. The pre-pass, the main infer
        // pass, and the post-pass all share the same checker; only
        // the per-program tables and caches get cleared.
        self.ids = IdTable::new();
        self.next_id_idx = 0;
        self.infer_depth = 0;
        self.cache.clear();
        self.codegen_types_by_span.clear();
        self.codegen_var_types.clear();
        self.polyfn_binding_spans.clear();
        self.codegen_var_types_scopes.clear();
        self.fn_codegen_baselines.clear();
        self.fn_param_names.clear();
        self.forward_free_fn_schemes.clear();
        self.fn_has_rest.clear();
        self.fn_tuple_rest.clear();
        self.current_tuple_pack = None;
        self.spread_call_arity.clear();
        self.spread_expanded_bases.clear();
        // Keep module-qualified overload families across multi-file
        // `check_program` calls so importers can type-dispatch after deps
        // were checked. Drop bare keys (entry registrations + prior `use`
        // aliases) so they do not leak into the next module.
        self.overload_sets.retain(|k, _| k.contains("::"));
        // Declaration/call span tables are per-module.
        self.selected_overloads_by_span.clear();
        self.overload_decl_by_span.clear();
        self.partial_fills_by_span.clear();
        self.partial_filled_tys_by_span.clear();
        self.lambda_uncaptured_outer = None;
        self.call_site_dicts.clear();
        self.call_site_dicts_by_span.clear();
        self.call_site_forward_dicts.clear();
        self.call_site_forward_dicts_by_span.clear();
        self.bound_method_calls.clear();
        self.bound_method_calls_by_span.clear();
        self.bound_operator_calls.clear();
        self.bound_operator_calls_by_span.clear();
        self.aggregate_arith.clear();
        self.aggregate_arith_by_span.clear();
        self.linear_algebra.clear();
        self.linear_algebra_by_span.clear();
        self.bound_display_calls.clear();
        self.bound_display_calls_by_span.clear();
        self.existential_packs_by_span.clear();
        self.existential_method_calls.clear();
        self.existential_method_calls_by_span.clear();
        self.for_in_infos.clear();
        self.for_in_infos_by_span.clear();
        self.typeclass_method_schemes.clear();
        self.current_expected = None;
        self.type_aliases.clear();
        self.type_aliases.push(HashMap::new());
        self.generic_aliases.clear();
        self.const_scopes.clear();
        self.const_scopes.push(HashSet::new());
        self.abstract_constraint_bindings.clear();
        self.enums.retain(|k, _| k.contains("::"));
        self.enum_tags.retain(|k, _| k.contains("::"));
        self.enum_payloads.retain(|k, _| k.contains("::"));
        self.enum_arities.retain(|k, _| k.contains("::"));
        self.c_structs.clear();
        self.callback_sigs.clear();
        self.ffi_fn_ret_tys.clear();
        self.ffi_fn_variadic.clear();
        self.ffi_fn_nfixed.clear();
        self.ffi_fn_ret_by_field.clear();
        self.ffi_fn_variadic_by_field.clear();
        self.ffi_fn_nfixed_by_field.clear();
        self.ffi_fn_param_invoke_ret.clear();
        self.current_function = None;
        self.extern_variadic.clear();
        self.extern_variadic_nfixed.clear();
        self.variadic_call_arg_tags.clear();
        self.pending_exhaustive.clear();
        self.async_functions.clear();
        self.async_depth = 0;
        self.current_yield_ty = None;
        self.current_send_ty = None;
        self.yield_receives_used = false;
        self.fn_result_mode = None;
        self.fn_option_mode = None;
        self.result_mode_fns.clear();
        self.result_mode_ok_is_result.clear();
        self.option_mode_fns.clear();
        self.test_case_names.clear();
        self.main_decl_span = None;
        // Keep FQN class tables / generic ctors so later files can `use`
        // `module::Class` (and `Class<T>`) after the defining module was
        // checked. Bare entry-module keys are dropped like overload sets.
        self.classes.retain(|k, _| k.contains("::"));
        self.methods.retain(|k, _| k.contains("::"));
        self.static_methods.retain(|k, _| k.contains("::"));
        self.const_class_fields.retain(|k, _| k.contains("::"));
        self.generics.generic_type_ctors.retain(|k, _| k.contains("::"));
        self.generics.register_builtin_type_ctors();
        // Keep module-qualified generic names so importers still see
        // `num::min` as generic after `num` was checked (dict-passing ABI).
        self.generics.generic_fns.retain(|k| k.contains("::"));
        self.fn_dict_arity.retain(|k, _| k.contains("::"));
        self.var_kinds.clear();
        self.current_typeclass = None;
        self.current_assoc_projections = None;
        self.open_assoc_projections.clear();
        self.register_builtin_typeclass_method_schemes();

        // Built-in enums survive the per-program enum reset.
        self.register_builtin_enums();
        // `fn_param_names` was cleared above — reinstall Vec / Range method ABI.
        self.register_builtin_vec();
        self.register_builtin_range();
        self.register_builtin_call_sigs();

        // Implicit `use prelude::*; use prelude::ops::*;` — FFI stays out.
        self.inject_prelude_scope();
        self.disk_imports.clear();

        // Mint NodeIds for every AST node (pre-walk). The visit order
        // matches `infer`'s recursion, so the IDs line up.
        id::pre_walk(ast, &mut self.ids);

        // Forward-declaration pre-pass: walk the AST once and
        // register every `enum` declaration's shape. This must run
        // before the main infer pass so constructor / match uses
        // that appear textually before their enum declaration still
        // resolve correctly.
        if let Err(msgs) = self.pre_register_enums(ast) {
            self.messages.extend(msgs);
        }
        self.pre_collect_free_function_param_names(ast);
        self.pre_register_inherent_methods(ast);
        self.pre_process_top_level_uses(ast);
        self.pre_pass_ffi_invoke_param_flow(ast);

        // Top frame for natives/globals; left on stack after check_program.
        self.push_scope();

        // Forward-declare module-level function signatures so `impl`
        // methods that appear earlier in the file can call them.
        self.pre_register_free_functions(ast);

        let ty = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.infer(ast))) {
            Ok(ty) => ty,
            Err(payload) => {
                // Only swallow our own recursion-limit signal (message
                // already recorded in `infer`) — any other panic is a real
                // bug and must keep crashing loudly.
                if payload.downcast_ref::<RecursionLimitExceeded>().is_none() {
                    std::panic::resume_unwind(payload);
                }
                // The AST wasn't fully walked: NodeId / exhaustiveness state
                // is inconsistent, so skip the post-passes below and bail
                // out with the error already on `self.messages`.
                return Ty::Var(self.counter.fresh());
            }
        };
        // NOTE: the frame is intentionally NOT popped — see the
        // doc-comment above.

        // Post-pass: run deferred exhaustiveness checks now that
        // the substitution is closed and every scrutinee type can
        // be fully resolved.
        self.run_pending_exhaustiveness();

        // `test("…") { … }` cases provide a virtual main — reject a
        // user-written `fn main` in the same file.
        if !self.test_case_names.is_empty()
            && let Some(span) = self.main_decl_span.clone()
        {
            let mut msg = Message::error(
                ErrorCode::GenericTypeError,
                "test files with `test(...)` cases must not define `main`".into(),
                span,
            );
            msg.with_help("remove `fn main`; the test harness provides a virtual main".into());
            self.messages.push(msg);
        }

        // Return the fully-resolved type so callers see e.g. `Foo`
        // rather than `Var(0)` even when the type was inferred
        // through let-binding + unify.
        apply_ty_prune(&self.subst, &ty)
    }

    /// Take all accumulated diagnostic messages, leaving the checker
    /// with an empty message buffer.
    pub fn take_messages(&mut self) -> Vec<Message> {
        std::mem::take(&mut self.messages)
    }

    /// Borrow the accumulated messages without consuming them.
    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    pub fn env(&self) -> &Env {
        &self.env
    }

    pub fn env_mut(&mut self) -> &mut Env {
        &mut self.env
    }

    /// Borrow the running substitution (useful for diagnostics).
    pub fn subst(&self) -> &Subst {
        &self.subst
    }

    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) fn cache(&self) -> impl Iterator<Item = (NodeId, &Ty)> {
        self.cache.iter().map(|(k, v)| (*k, v))
    }

    fn push_scope(&mut self) {
        self.env.push();
        self.const_scopes.push(HashSet::new());
        self.type_aliases.push(HashMap::new());
    }

    fn pop_scope(&mut self) {
        let _ = self.env.pop();
        let _ = self.const_scopes.pop();
        if self.type_aliases.len() > 1 {
            let _ = self.type_aliases.pop();
        } else if let Some(frame) = self.type_aliases.last_mut() {
            frame.clear();
        } else {
            self.type_aliases.push(HashMap::new());
        }
    }

    /// Begin a `{ … }` block overlay for codegen var types. Function
    /// frames must NOT use this — restoring across function `push_scope`
    /// would revive an earlier function's parameter type over a later
    /// `let` of the same name (breaks escaped PolyFn typing).
    fn push_block_codegen_scope(&mut self) {
        self.codegen_var_types_scopes.push(HashMap::new());
    }

    fn pop_block_codegen_scope(&mut self) {
        if let Some(frame) = self.codegen_var_types_scopes.pop() {
            for (name, prev) in frame {
                match prev {
                    Some(ty) => {
                        self.codegen_var_types.insert(name, ty);
                    }
                    None => {
                        // Binding introduced in this block — keep for
                        // post-check Access / LoadField.
                    }
                }
            }
        }
    }

    /// Record a binding's type for codegen. Inside a `{ … }` block overlay,
    /// remember the previous flat-map entry so [`pop_block_codegen_scope`]
    /// can restore shadows when:
    /// - an *outer block in this nesting* already introduced the name, or
    /// - the name is a parameter/`self` in the current function baseline.
    ///
    /// Flat-map leftovers from other functions must not be restored (that
    /// would revive `apply_id`'s `f` over `let f = id` in `main`).
    fn record_codegen_var_type(&mut self, name: String, ty: Ty) {
        if !self.codegen_var_types_scopes.is_empty() {
            let save = {
                let scopes = &self.codegen_var_types_scopes;
                let outer_introduced = scopes
                    .iter()
                    .rev()
                    .skip(1)
                    .any(|frame| frame.contains_key(&name));
                let in_fn_baseline = self
                    .fn_codegen_baselines
                    .last()
                    .is_some_and(|b| b.contains(&name));
                if outer_introduced || in_fn_baseline {
                    self.codegen_var_types.get(&name).cloned()
                } else {
                    None
                }
            };
            self.codegen_var_types_scopes
                .last_mut()
                .expect("non-empty scopes")
                .entry(name.clone())
                .or_insert(save);
        }
        self.codegen_var_types.insert(name, ty);
    }

    /// Whether any type-parameter hole appears in `ty` (including nested
    /// tuple / array / record / app argument positions).
    fn ty_contains_open_var(ty: &Ty) -> bool {
        match ty {
            Ty::Var(_) => true,
            Ty::Forall { body, .. } => Self::ty_contains_open_var(body),
            Ty::Fun(arg, ret) => Self::ty_contains_open_var(arg) || Self::ty_contains_open_var(ret),
            Ty::App(ctor, args) => {
                Self::ty_contains_open_var(ctor) || args.iter().any(Self::ty_contains_open_var)
            }
            Ty::List(inner) | Ty::Array { element: inner, .. } => Self::ty_contains_open_var(inner),
            Ty::Tuple(elems) => elems.iter().any(Self::ty_contains_open_var),
            Ty::Record { fields } => fields.iter().any(|(_, t)| Self::ty_contains_open_var(t)),
            Ty::Constructor { owner, .. } => Self::ty_contains_open_var(owner),
            Ty::Readonly(inner) => Self::ty_contains_open_var(inner),
            Ty::Con(_) | Ty::Sum { .. } | Ty::Existential { .. } | Ty::Never => false,
        }
    }

    /// True when any parameter position in a (possibly quantified) function
    /// type still contains an open type variable — the runtime value is a
    /// PolyFn that expects boxed args.
    fn fn_args_contain_type_param(ty: &Ty) -> bool {
        match ty {
            Ty::Forall { body, .. } => Self::fn_args_contain_type_param(body),
            Ty::Fun(arg, ret) => {
                Self::ty_contains_open_var(arg) || Self::fn_args_contain_type_param(ret)
            }
            _ => false,
        }
    }

    fn maybe_record_polyfn_binding(&mut self, span: (usize, usize), ty: &Ty) {
        if matches!(ty, Ty::Forall { .. }) || Self::fn_args_contain_type_param(ty) {
            self.polyfn_binding_spans.insert(span);
        }
    }

    /// Whether the let/const binder at `span` was PolyFn-shaped. Used by
    /// codegen to seed per-scope `polyfn_vars` (not consulted at call sites).
    pub fn is_polyfn_binding_at(&self, start: usize, end: usize) -> bool {
        self.polyfn_binding_spans.contains(&(start, end))
    }

    fn register_type_alias(&mut self, name: &str, alias_ty: Ty, range: Range<usize>) {
        if self.type_aliases.is_empty() {
            self.type_aliases.push(HashMap::new());
        }

        let duplicate = self
            .type_aliases
            .last()
            .map(|frame| frame.contains_key(name))
            .unwrap_or(false)
            || self.generic_aliases.contains_key(name);

        if duplicate {
            let mut msg = Message::error(
                ErrorCode::GenericTypeError,
                format!("Duplicate type alias `{}`", name),
                range,
            );
            msg.with_help("type aliases may shadow names only from an outer scope".to_string());
            self.messages.push(msg);
            return;
        }

        self.type_aliases
            .last_mut()
            .expect("type alias scope should exist")
            .insert(name.to_string(), alias_ty);
        self.generics
            .register_nominal_type(name, &self.current_module);
    }

    fn register_generic_alias(
        &mut self,
        name: &str,
        params: Vec<String>,
        param_vars: Vec<TyVarId>,
        body: Ty,
        range: Range<usize>,
    ) {
        let duplicate = self.generic_aliases.contains_key(name)
            || self
                .type_aliases
                .last()
                .map(|frame| frame.contains_key(name))
                .unwrap_or(false);
        if duplicate {
            let mut msg = Message::error(
                ErrorCode::GenericTypeError,
                format!("Duplicate type alias `{}`", name),
                range,
            );
            msg.with_help("type aliases may shadow names only from an outer scope".to_string());
            self.messages.push(msg);
            return;
        }
        self.generic_aliases.insert(
            name.to_string(),
            GenericAliasDef {
                params,
                param_vars,
                body,
            },
        );
        self.generics
            .register_nominal_type(name, &self.current_module);
    }

    /// Expand a generic alias by substituting concrete type arguments.
    fn expand_generic_alias(&self, def: &GenericAliasDef, arg_tys: &[Ty]) -> Ty {
        let mut subst = Subst::empty();
        for (var, arg) in def.param_vars.iter().zip(arg_tys.iter()) {
            subst.insert(*var, arg.clone());
        }
        apply_ty(&subst, &def.body)
    }

    fn projection_arg_key(&self, args: &[Ty]) -> Vec<String> {
        args.iter()
            .map(|arg| apply_ty_prune(&self.subst, arg).to_string())
            .collect()
    }

    fn record_current_assoc_projection(&mut self, var: TyVarId, name: &str, args: &[Ty]) {
        let Some(projections) = self.current_assoc_projections.as_mut() else {
            return;
        };
        if projections.iter().any(|p| p.var == var) {
            return;
        }
        projections.push(AssocProjection {
            var,
            name: name.to_string(),
            args: args.to_vec(),
        });
    }

    fn instantiate_assoc_value(&self, value: &AssocTypeValue, args: &[Ty]) -> Ty {
        let mut subst = Subst::empty();
        for (var, arg) in value.param_vars.iter().zip(args.iter()) {
            subst.insert(*var, apply_ty_prune(&self.subst, arg));
        }
        apply_ty(&subst, &value.ty)
    }

    fn kind_of_ty(&self, ty: &Ty) -> Kind {
        match ty {
            Ty::Var(v) => self.kind_of_var(*v),
            Ty::Con(name) => self.bare_constructor_kind(name).unwrap_or(Kind::Type),
            Ty::App(..)
            | Ty::Fun(..)
            | Ty::List(..)
            | Ty::Sum { .. }
            | Ty::Constructor { .. }
            | Ty::Tuple(_)
            | Ty::Array { .. }
            | Ty::Record { .. }
            | Ty::Existential { .. }
            | Ty::Forall { .. }
            | Ty::Readonly(_)
            | Ty::Never => Kind::Type,
        }
    }

    fn validate_assoc_projection_args(
        &mut self,
        class: &str,
        decl: &AssocTypeDecl,
        args: &[Ty],
        range: &Range<usize>,
    ) {
        if decl.params.len() != args.len() {
            self.messages.push(Message::error(
                ErrorCode::GenericTypeError,
                format!(
                    "Associated type `{}::{}` expects {} type argument{}, got {}",
                    class,
                    decl.name,
                    decl.params.len(),
                    if decl.params.len() == 1 { "" } else { "s" },
                    args.len()
                ),
                range.clone(),
            ));
            return;
        }
        for (i, (arg, expected)) in args.iter().zip(decl.param_kinds.iter()).enumerate() {
            let actual = self.kind_of_ty(arg);
            if &actual != expected {
                self.messages.push(Message::error(
                    ErrorCode::GenericTypeError,
                    format!(
                        "Type argument {} to associated type `{}::{}` has kind `{}`, expected `{}`",
                        i + 1,
                        class,
                        decl.name,
                        actual,
                        expected
                    ),
                    range.clone(),
                ));
            }
        }
    }

    fn register_generic_type_ctor(
        &mut self,
        name: &str,
        type_params: &[parser::ast::TypeParam<'_>],
    ) -> Option<Vec<String>> {
        let previous = self.generics.generic_type_ctors.get(name).cloned();
        if type_params.is_empty() {
            return previous;
        }
        self.generics.generic_type_ctors.insert(
            name.to_string(),
            type_params.iter().map(|tp| tp.name.to_string()).collect(),
        );
        previous
    }

    fn restore_generic_type_ctor(&mut self, name: &str, previous: Option<Vec<String>>) {
        match previous {
            Some(params) => {
                self.generics
                    .generic_type_ctors
                    .insert(name.to_string(), params);
            }
            None => {
                self.generics.generic_type_ctors.remove(name);
            }
        }
    }

    fn push_type_params_for_type_parsing(
        &mut self,
        type_params: &[parser::ast::TypeParam<'_>],
    ) -> bool {
        if type_params.is_empty() {
            return false;
        }
        let mut frame = HashMap::new();
        for tp in type_params {
            frame.insert(tp.name.to_string(), self.counter.fresh());
        }
        self.type_params_in_scope.push(frame);
        true
    }

    fn pop_type_params_for_type_parsing(&mut self, pushed: bool) {
        if pushed {
            let _ = self.type_params_in_scope.pop();
        }
    }

    fn insert_const_binding(&mut self, name: impl Into<String>) {
        if self.const_scopes.is_empty() {
            self.const_scopes.push(HashSet::new());
        }
        self.const_scopes
            .last_mut()
            .expect("const scope should exist")
            .insert(name.into());
    }

    fn is_const_binding(&self, name: &str) -> bool {
        self.const_scopes
            .iter()
            .rev()
            .any(|scope| scope.contains(name))
    }

    fn coroutine_type(&self, yield_ty: Ty, send_ty: Ty) -> Ty {
        Ty::App(
            Box::new(Ty::Con("coroutine".to_string())),
            vec![yield_ty, send_ty],
        )
    }

    fn infer(&mut self, expr: &Output) -> Ty {
        self.infer_depth += 1;
        if self.infer_depth > INFER_RECURSION_LIMIT {
            let _ = self.error_with_help(
                ErrorCode::ExpressionNestingTooDeep,
                format!(
                    "expression nested too deeply (over {INFER_RECURSION_LIMIT} levels) for the typechecker"
                ),
                expr.0.into_range(),
                Some("split the expression into smaller named bindings".to_string()),
            );
            std::panic::panic_any(RecursionLimitExceeded);
        }

        // Pull the next ID from the pre-walk's minting order. Both
        // `infer` and the pre-walk visit in pre-order, so the `n`-th
        // call here consumes the `n`-th ID.
        let id = self.ids.ids()[self.next_id_idx];
        self.next_id_idx += 1;

        let ty = self.infer_inner(expr, Some(id));
        self.cache.insert(id, ty.clone());
        self.codegen_types_by_span
            .entry((expr.0.start, expr.0.end))
            .or_insert_with(|| ty.clone());
        self.infer_depth -= 1;
        ty
    }

    /// Register the compiler-owned signatures for the primitive classes.
    ///
    /// Their instances are emitted as bytecode thunks, but they participate in
    /// lookup exactly like source-declared classes so method/UFCS and operator
    /// dispatch share one dictionary ABI.
    fn register_builtin_typeclass_method_schemes(&mut self) {
        for (class, methods, returns_bool) in [
            ("Add", &["add"][..], false),
            ("Sub", &["sub"][..], false),
            ("Mul", &["mul"][..], false),
            ("Div", &["div"][..], false),
            ("Lt", &["lt"][..], true),
            ("Le", &["le"][..], true),
            ("Gt", &["gt"][..], true),
            ("Ge", &["ge"][..], true),
            ("Eq", &["eq", "ne"][..], true),
        ] {
            for method in methods {
                let var = self.counter.fresh();
                let ty = Ty::Fun(
                    Box::new(Ty::Var(var)),
                    Box::new(Ty::Fun(
                        Box::new(Ty::Var(var)),
                        Box::new(if returns_bool {
                            boolean()
                        } else {
                            Ty::Var(var)
                        }),
                    )),
                );
                self.typeclass_method_schemes.insert(
                    (class.to_string(), (*method).to_string()),
                    Scheme::poly(vec![var], vec![Constraint::unary(class, var)], ty),
                );
            }
        }

        let var = self.counter.fresh();
        self.typeclass_method_schemes.insert(
            ("Show".to_string(), "show".to_string()),
            Scheme::poly(
                vec![var],
                vec![Constraint::unary("Show", var)],
                Ty::Fun(Box::new(Ty::Var(var)), Box::new(string())),
            ),
        );

        // Length::len : ∀T. Length T => T → int
        {
            let var = self.counter.fresh();
            self.typeclass_method_schemes.insert(
                ("Length".to_string(), "len".to_string()),
                Scheme::poly(
                    vec![var],
                    vec![Constraint::unary("Length", var)],
                    Ty::Fun(Box::new(Ty::Var(var)), Box::new(int())),
                ),
            );
        }

        // Into::into : ∀Self T. Into<Self, T> => Self → T
        {
            let self_v = self.counter.fresh();
            let t_v = self.counter.fresh();
            self.typeclass_method_schemes.insert(
                ("Into".to_string(), "into".to_string()),
                Scheme::poly(
                    vec![self_v, t_v],
                    vec![Constraint {
                        class: "Into".into(),
                        args: vec![Ty::Var(self_v), Ty::Var(t_v)],
                    }],
                    Ty::Fun(Box::new(Ty::Var(self_v)), Box::new(Ty::Var(t_v))),
                ),
            );
        }

        // Read::read / Write::write — stream IO groundwork.
        {
            use crate::typechecking::ty::byte;
            let var = self.counter.fresh();
            let io_err = Ty::Con(common::BUILTIN_IO_ERROR_ENUM.into());
            let res_opt_int = result_app_ty(option_app_ty(int()), io_err.clone());
            let res_int = result_app_ty(int(), io_err);
            let bytes = vec_app_ty(byte());
            self.typeclass_method_schemes.insert(
                ("Read".to_string(), "read".to_string()),
                Scheme::poly(
                    vec![var],
                    vec![Constraint::unary("Read", var)],
                    Ty::Fun(
                        Box::new(Ty::Var(var)),
                        Box::new(Ty::Fun(Box::new(bytes.clone()), Box::new(res_opt_int))),
                    ),
                ),
            );
            let var = self.counter.fresh();
            self.typeclass_method_schemes.insert(
                ("Write".to_string(), "write".to_string()),
                Scheme::poly(
                    vec![var],
                    vec![Constraint::unary("Write", var)],
                    Ty::Fun(
                        Box::new(Ty::Var(var)),
                        Box::new(Ty::Fun(Box::new(bytes), Box::new(res_int))),
                    ),
                ),
            );
        }

        // Iterator::next : ∀I Item. Iterator<I> => I → Option<Item>
        {
            let i_var = self.counter.fresh();
            let item_var = self.counter.fresh();
            self.typeclass_method_schemes.insert(
                ("Iterator".to_string(), "next".to_string()),
                Scheme::poly_with_kinds_and_assoc(
                    vec![i_var, item_var],
                    vec![Kind::Type, Kind::Type],
                    vec![Constraint::unary("Iterator", i_var)],
                    vec![AssocProjection {
                        var: item_var,
                        name: "Item".into(),
                        args: vec![],
                    }],
                    Ty::Fun(
                        Box::new(Ty::Var(i_var)),
                        Box::new(option_app_ty(Ty::Var(item_var))),
                    ),
                ),
            );
        }
        // IntoIterator::into_iter : ∀T Item IntoIter. IntoIterator<T> => T → IntoIter
        {
            let t_var = self.counter.fresh();
            let item_var = self.counter.fresh();
            let into_iter_var = self.counter.fresh();
            self.typeclass_method_schemes.insert(
                ("IntoIterator".to_string(), "into_iter".to_string()),
                Scheme::poly_with_kinds_and_assoc(
                    vec![t_var, item_var, into_iter_var],
                    vec![Kind::Type, Kind::Type, Kind::Type],
                    vec![Constraint::unary("IntoIterator", t_var)],
                    vec![
                        AssocProjection {
                            var: item_var,
                            name: "Item".into(),
                            args: vec![],
                        },
                        AssocProjection {
                            var: into_iter_var,
                            name: "IntoIter".into(),
                            args: vec![],
                        },
                    ],
                    Ty::Fun(Box::new(Ty::Var(t_var)), Box::new(Ty::Var(into_iter_var))),
                ),
            );
        }
        // Serialize / Deserialize / Default / Hash / String
        {
            use crate::typechecking::ty::byte;
            let var = self.counter.fresh();
            let bytes = vec_app_ty(byte());
            self.typeclass_method_schemes.insert(
                ("Serialize".to_string(), "serialize".to_string()),
                Scheme::poly(
                    vec![var],
                    vec![Constraint::unary("Serialize", var)],
                    Ty::Fun(Box::new(Ty::Var(var)), Box::new(bytes.clone())),
                ),
            );
            let var = self.counter.fresh();
            self.typeclass_method_schemes.insert(
                ("Deserialize".to_string(), "deserialize".to_string()),
                Scheme::poly(
                    vec![var],
                    vec![Constraint::unary("Deserialize", var)],
                    Ty::Fun(Box::new(bytes), Box::new(Ty::Var(var))),
                ),
            );
            let var = self.counter.fresh();
            self.typeclass_method_schemes.insert(
                ("Default".to_string(), "default".to_string()),
                Scheme::poly(
                    vec![var],
                    vec![Constraint::unary("Default", var)],
                    Ty::Fun(Box::new(unit_ty()), Box::new(Ty::Var(var))),
                ),
            );
            let var = self.counter.fresh();
            self.typeclass_method_schemes.insert(
                ("Hash".to_string(), "hash".to_string()),
                Scheme::poly(
                    vec![var],
                    vec![Constraint::unary("Hash", var)],
                    Ty::Fun(Box::new(Ty::Var(var)), Box::new(int())),
                ),
            );
            let var = self.counter.fresh();
            self.typeclass_method_schemes.insert(
                ("String".to_string(), "to_string".to_string()),
                Scheme::poly(
                    vec![var],
                    vec![Constraint::unary("String", var)],
                    Ty::Fun(Box::new(Ty::Var(var)), Box::new(string())),
                ),
            );
        }
    }

    /// Inner inference — does the actual dispatch but no caching.
    /// Every recursive call into a child still goes through
    /// [`infer`](Self::infer), so each child also gets cached.
    fn infer_inner(&mut self, expr: &Output, id: Option<NodeId>) -> Ty {
        let range = expr.0.into_range();
        let child = expr.1.as_ref();

        match child {
            // ---- Literals ----
            Expression::Integer(n) => {
                // Under an expected `byte`, in-range integer literals type as
                // `byte` so arithmetic like `return 1 + 1;` (expected byte)
                // unifies without falling back to `int` + post-hoc coerce.
                if let Some(exp) = self.current_expected.clone() {
                    let exp = apply_ty_prune(&self.subst, &exp);
                    if Self::is_byte_ty(&exp) {
                        if (0..=255).contains(n) {
                            return crate::typechecking::ty::byte();
                        }
                        return self.error_with_help(
                            ErrorCode::TypeMismatch,
                            format!("byte literal out of range: `{n}` is not in 0..=255"),
                            range,
                            Some("a `byte` must be an integer between 0 and 255".to_string()),
                        );
                    }
                }
                int()
            }
            Expression::Float(_) => float(),
            Expression::String(s) => {
                // Under an expected `byte`, a string literal whose UTF-8
                // encoding is exactly one byte types as `byte`. Under an
                // expected `[byte]` / `[byte; N]`, the whole literal becomes
                // that byte array (length must match for fixed `N`).
                if let Some(exp) = self.current_expected.clone() {
                    let exp = apply_ty_prune(&self.subst, &exp);
                    if Self::is_byte_ty(&exp) {
                        return self.coerce_string_literal_to_byte(s, &range);
                    }
                    if Self::is_byte_array_ty(&exp).is_some() {
                        return self.coerce_string_literal_to_bytes(s, &exp, &range);
                    }
                }
                string()
            }
            Expression::Bool(_) => boolean(),

            // ---- Names ----
            Expression::Identifier(name) => self.infer_identifier(name, range),

            // A bare type name (only valid as an annotation, but be
            // permissive).
            Expression::Type(name) => self.parse_type_name_str(name),
            Expression::TypeFun(arg, ret) => {
                let arg_ty = self.infer(arg);
                let ret_ty = self.infer(ret);
                Ty::Fun(Box::new(arg_ty), Box::new(ret_ty))
            }

            // ---- Wrappers / no-ops ----
            Expression::Noop(_)
            | Expression::Comment(_)
            | Expression::Break
            | Expression::Continue => unit_ty(),
            // Named call-site arg wrapper — type is the value's type.
            Expression::NamedArg(_, value) => self.infer(value),
            // `use` — virtual modules first, else disk-module function alias
            Expression::Use { path, name, alias } => self.infer_use_decl(path, name, alias, range),
            Expression::Module(_, _) => unit_ty(),
            // FFI declaration block — register each function
            // signature in the top frame (so subsequent calls
            // can type-check) and return unit. The body is
            // empty (FFI symbols are resolved at VM startup,
            // not at compile time).
            Expression::ExternBlock {
                library: _,
                declarations,
            } => self.infer_extern_block(declarations),

            Expression::Expr(e) | Expression::Group(e) | Expression::Statement(e) => self.infer(e),
            // Semicolon form discards the value (same as a Rust statement).
            Expression::ExprStatement(e) => {
                let _ = self.infer(e);
                unit_ty()
            }

            // ---- Blocks ----
            // Program runs in the current frame (the global frame from
            // check_program). This is what makes top-level `let`
            // bindings visible after inference. Block introduces its
            // own scope.
            Expression::Block(children) => {
                self.push_scope();
                self.push_block_codegen_scope();
                let mut last_ty = unit_ty();
                for child in children {
                    last_ty = self.infer(child);
                }
                self.pop_block_codegen_scope();
                self.pop_scope();
                last_ty
            }
            Expression::Program(children) => {
                let mut last_ty = unit_ty();
                for child in children {
                    last_ty = self.infer(child);
                }
                last_ty
            }

            // ---- Fragments (from `let x = expr`) ----
            Expression::Fragment(children) => self.infer_fragment(children),

            // ---- `let (a, b) = expr` / `let { x, y } = expr` ----
            Expression::LetDestructure { pattern, rhs } => {
                let rhs_ty = self.infer(rhs);
                let _ = self.infer_let_pattern(pattern, &rhs_ty, &rhs.0.into_range());
                unit_ty()
            }

            // ---- `let` / `const` ----
            Expression::Variable(name, ty_opt) => {
                let var_ty = match ty_opt {
                    Some(ann) => self.parse_type_name(ann),
                    None => Ty::Var(self.counter.fresh()),
                };
                self.env.insert_top(name.to_string(), Scheme::mono(var_ty));
                unit_ty()
            }

            Expression::Constant(name, ty_opt) => {
                let var_ty = match ty_opt {
                    Some(ann) => self.parse_type_name(ann),
                    None => Ty::Var(self.counter.fresh()),
                };
                let ident = match name.1.as_ref() {
                    Expression::Identifier(n) => n.to_string(),
                    _ => {
                        return self.error_with_help(
                            ErrorCode::GenericTypeError,
                            "Invalid constant name".to_string(),
                            range,
                            Some("a constant name must be an identifier".to_string()),
                        );
                    }
                };
                self.env.insert_top(ident.clone(), Scheme::mono(var_ty));
                self.insert_const_binding(ident);
                unit_ty()
            }

            // ---- Assignment / compound assignment / adjust ----
            Expression::CompoundAssign(target, op, value) => self.infer_compound_assign(target, op, value, id, range),

            Expression::Assignment(name, value) => {
                if let Expression::Index(arr, None) = name.1.as_ref() {
                    return self.infer_array_append_assign(arr, value, range);
                }
                // `x = resume x` overwrites the coroutine handle with the yield value.
                if let (Expression::Identifier(var_name), Expression::Resume(target, None)) =
                    (name.1.as_ref(), value.1.as_ref())
                {
                    if let Expression::Identifier(target_name) = target.1.as_ref() {
                        if var_name == target_name {
                            let val_ty = self.infer(value);
                            if self.env.lookup(var_name).is_some() {
                                self.env
                                    .insert_top(var_name.to_string(), Scheme::mono(val_ty.clone()));
                                self.record_codegen_var_type(var_name.to_string(), val_ty.clone());
                            }
                            return val_ty;
                        }
                    }
                }

                if is_yield_expression(value) {
                    self.yield_receives_used = true;
                }
                let val_ty = self.infer(value);
                let target_ty = self.infer_mutable_lvalue(name, range.clone());
                self.coerce_or_unify(&target_ty, &val_ty, Some(value), &range, "assignment");
                self.maybe_record_ffi_declare_for_field_assignment(name, value);
                apply_ty_prune(&self.subst, &val_ty)
            }

            // ---- Arithmetic / bitwise ----
            Expression::Range {
                start,
                end,
                inclusive,
            } => self.infer_range(start, end, *inclusive, range),
            Expression::Add(lhs, rhs) => self.infer_arith(lhs, rhs, id, range, "+"),
            Expression::Sub(lhs, rhs) => self.infer_arith(lhs, rhs, id, range, "-"),
            Expression::Mul(lhs, rhs) => self.infer_arith(lhs, rhs, id, range, "*"),
            Expression::Div(lhs, rhs) => self.infer_arith(lhs, rhs, id, range, "/"),
            Expression::Mod(lhs, rhs) => self.infer_arith(lhs, rhs, id, range, "%"),
            Expression::Pow(lhs, rhs) => self.infer_arith(lhs, rhs, id, range, "**"),
            Expression::Shl(lhs, rhs) => self.infer_arith(lhs, rhs, id, range, "<<"),
            Expression::Shr(lhs, rhs) => self.infer_arith(lhs, rhs, id, range, ">>"),
            Expression::Xor(lhs, rhs) => self.infer_arith(lhs, rhs, id, range, "^"),
            Expression::BitAnd(lhs, rhs) => self.infer_arith(lhs, rhs, id, range, "&"),
            Expression::BitOr(lhs, rhs) => self.infer_arith(lhs, rhs, id, range, "|"),

            // ---- Logical ----
            Expression::And(lhs, rhs) | Expression::Or(lhs, rhs) => {
                let lt = self.infer(lhs);
                let rt = self.infer(rhs);
                self.unify(&lt, &boolean(), &lhs.0.into_range(), "left of logical");
                self.unify(&rt, &boolean(), &rhs.0.into_range(), "right of logical");
                boolean()
            }

            // ---- Comparison ----
            Expression::Eq(lhs, rhs) => self.infer_comparison(lhs, rhs, id, range, "Eq", "eq"),
            Expression::Neq(lhs, rhs) => self.infer_comparison(lhs, rhs, id, range, "Eq", "ne"),
            Expression::Le(lhs, rhs) => self.infer_comparison(lhs, rhs, id, range, "Lt", "lt"),
            Expression::Gt(lhs, rhs) => self.infer_comparison(lhs, rhs, id, range, "Gt", "gt"),
            Expression::Leq(lhs, rhs) => self.infer_comparison(lhs, rhs, id, range, "Le", "le"),
            Expression::Geq(lhs, rhs) => self.infer_comparison(lhs, rhs, id, range, "Ge", "ge"),

            // ---- Prefix / postfix ----
            Expression::Negate(e) => {
                let inner = self.infer(e);
                let pruned = apply_ty_prune(&self.subst, &inner);
                if crate::typechecking::aggregate_arith::is_matrix_ty(&pruned) {
                    return self.infer_matrix_neg(pruned, id, range);
                }
                if matches!(&pruned, Ty::Tuple(_) | Ty::Array { .. }) {
                    self.infer_aggregate_neg(pruned, id, range)
                } else {
                    pruned
                }
            }
            Expression::Positive(e) => self.infer(e),
            Expression::Not(e) => {
                let t = self.infer(e);
                self.unify(&t, &int(), &e.0.into_range(), "operand of `~`");
                int()
            }
            Expression::LogicalNot(e) => {
                let t = self.infer(e);
                let pruned = apply_ty_prune(&self.subst, &t);
                match pruned {
                    Ty::Con(name) if name == "bool" || name == "int" => boolean(),
                    _ => {
                        let _ = self.error_with_help(
                            ErrorCode::GenericTypeError,
                            "Logical NOT requires a `bool` or `int` operand".to_string(),
                            e.0.into_range(),
                            Some(format!(
                                "found `{pruned}`; use `~` for bitwise negation on integers"
                            )),
                        );
                        boolean()
                    }
                }
            }
            Expression::Adjust { target, .. } => {
                let ty = self.infer_mutable_lvalue(target, range.clone());
                let pruned = apply_ty_prune(&self.subst, &ty);
                if !matches!(pruned, Ty::Con(ref n) if n == "int" || n == "float") {
                    let _ = self.error_with_help(
                        ErrorCode::GenericTypeError,
                        "Increment/decrement requires a numeric lvalue".to_string(),
                        range,
                        Some(
                            "only `int` and `float` variables, fields, and indices support ++/--"
                                .to_string(),
                        ),
                    );
                }
                pruned
            }
            Expression::Call { name, args } => self.infer_call_expr(name, args, id, range),

            // ---- Match / loop / if ----
            Expression::If(branches) => self.infer_if(branches),
            Expression::Branch(cond, body) => {
                if let Some(c) = cond {
                    let ct = self.infer(c);
                    self.unify(&ct, &boolean(), &c.0.into_range(), "branch condition");
                }
                self.infer(body)
            }
            Expression::Match { scrutinee, arms } => self.infer_match(scrutinee, arms, range),
            Expression::Loop {
                identifier,
                iterable,
                body,
            } => {
                if let Some(binding) = identifier {
                    // `for x in expr { body }` — IntoIterator / Iterator protocol
                    // (builtin arrays, homogeneous tuples/dicts, coroutines, or
                    // user `impl`s). Bind `x : Item`.
                    let it = self.infer(iterable);
                    let resolved = apply_ty_prune(&self.subst, &it);
                    let elem_ty = self
                        .resolve_for_in_iterable(&resolved, id, &iterable.0.into_range(), &range)
                        .unwrap_or_else(|| Ty::Var(self.counter.fresh()));
                    self.env.push();
                    if let Expression::Identifier(name) = binding.1.as_ref() {
                        self.env
                            .insert_top(name.to_string(), Scheme::mono(elem_ty.clone()));
                        self.record_codegen_var_type(name.to_string(), elem_ty.clone());
                    }
                    // Consume the binding node's ID (pre-walk order) now that
                    // the name is in scope.
                    let _ = self.infer(binding);
                    let _ = self.infer(body);
                    self.env.pop();
                    unit_ty()
                } else {
                    let it = self.infer(iterable);
                    self.unify(&it, &boolean(), &iterable.0.into_range(), "while condition");
                    let _ = self.infer(body);
                    let lookup = |name: &str| self.const_fold_env.get(name).copied();
                    if crate::typechecking::control_flow::is_infinite_loop(expr, &lookup) {
                        never()
                    } else {
                        unit_ty()
                    }
                }
            }
            Expression::For {
                init,
                cond,
                step,
                body,
            } => {
                if let Some(init) = init {
                    let _ = self.infer(init);
                }
                let cond_ty = self.infer(cond);
                self.unify(&cond_ty, &boolean(), &cond.0.into_range(), "for condition");
                let _ = self.infer(body);
                if let Some(step) = step {
                    let _ = self.infer(step);
                }
                let lookup = |name: &str| self.const_fold_env.get(name).copied();
                if crate::typechecking::control_flow::is_infinite_loop(expr, &lookup) {
                    never()
                } else {
                    unit_ty()
                }
            }

            // ---- Return ----
            Expression::Return(e) | Expression::ImplicitReturn(e) => {
                // Push the declared return type as expected so ground trait
                // calls like `return c.into();` can pin `Into`'s target `T`
                // before constraint discharge (same as annotated `let`).
                // In result mode, bare `return v` expects the Ok payload. An
                // explicit `return Result::Ok(v)` / `Result::Err(e)` expects
                // the full Result when the Ok payload is not itself a Result
                // (nested `Result<Result<…>, …>` still uses Ok as payload + wrap).
                let prev_expected = self.current_expected.take();
                let flat_explicit_result = self.fn_result_mode.as_ref().is_some_and(|(ok, _)| {
                    result_ok_err(ok).is_none() && Self::expr_is_explicit_result_construct(e)
                });
                if flat_explicit_result {
                    if let Some((ok, err)) = self.fn_result_mode.clone() {
                        self.current_expected = Some(result_ty(ok, err));
                    }
                } else if let Some(ret) = self.current_return_ty.clone() {
                    self.current_expected = Some(ret);
                }
                let ty = self.infer(e);
                self.current_expected = prev_expected;
                if flat_explicit_result {
                    if let Some((ok, err)) = self.fn_result_mode.clone() {
                        let full = result_ty(ok, err);
                        self.coerce_or_unify(
                            &full,
                            &ty,
                            Some(e),
                            &e.0.into_range(),
                            "return value",
                        );
                    }
                } else if let Some(ret) = self.current_return_ty.clone() {
                    self.coerce_or_unify(&ret, &ty, Some(e), &e.0.into_range(), "return value");
                }
                never()
            }

            // ---- raise / ? / ?? / ?. ----
            Expression::Raise(e) => {
                // `raise err?` parses as `raise (err?)` (postfix `?` binds
                // tighter than the `raise` keyword). Point users at the
                // bare-`raise` early-return idiom instead of a bare InvalidTry.
                if matches!(
                    unwrap_expr_wrappers(e).1.as_ref(),
                    Expression::Try(_)
                ) {
                    return self.error_with_help(
                        ErrorCode::InvalidTry,
                        "`?` after `raise` applies to the error expression, not to `raise`"
                            .to_string(),
                        range,
                        Some(
                            "write `raise err;` — `raise` already early-returns `Err`; do not write `raise err?`"
                                .to_string(),
                        ),
                    );
                }
                let err_ty = self.infer(e);
                let _ok_ty = self.ensure_result_mode(&err_ty, &e.0.into_range());
                never()
            }

            Expression::Panic(e) => {
                let msg_ty = self.infer(e);
                self.unify(&msg_ty, &string(), &e.0.into_range(), "panic message");
                never()
            }

            Expression::Try(inner) => {
                // `(raise err)?` — `raise` already diverges as Err.
                // Parens often wrap as `Group(Fragment([Raise]))`.
                if Self::expr_is_raise(inner) {
                    return self.error_with_help(
                        ErrorCode::InvalidTry,
                        "`?` cannot follow `raise`".to_string(),
                        range,
                        Some(
                            "use `raise err;` — `raise` already early-returns `Err`"
                                .to_string(),
                        ),
                    );
                }
                let inner_ty = self.infer(inner);
                let resolved = apply_ty_prune(&self.subst, &inner_ty);
                if let Some((ok, err)) = result_ok_err(&resolved) {
                    let _ = self.ensure_result_mode(&err, &range);
                    ok
                } else if is_option_ty(&resolved) {
                    let inner =
                        option_inner(&resolved).unwrap_or_else(|| Ty::Var(self.counter.fresh()));
                    self.ensure_option_mode(&inner, &range);
                    inner
                } else if matches!(resolved, Ty::Var(_)) {
                    // Not yet pinned — assume Result and let later
                    // unifications fill Ok/Err (or fail).
                    let ok = Ty::Var(self.counter.fresh());
                    let err = Ty::Var(self.counter.fresh());
                    let result = result_ty(ok.clone(), err.clone());
                    self.unify(&inner_ty, &result, &range, "try operand");
                    let _ = self.ensure_result_mode(&err, &range);
                    ok
                } else {
                    self.error(
                        ErrorCode::InvalidTry,
                        format!("`?` requires Option or Result, found `{}`", resolved),
                        range,
                    )
                }
            }

            Expression::Cast(expr, ty_ann) => self.infer_cast(expr, ty_ann),

            Expression::TypeOf(inner) => {
                let inner_ty = self.infer(inner);
                let resolved = apply_ty_prune(&self.subst, &inner_ty);
                match crate::typechecking::pretty::format_ty_fqn(
                    &resolved,
                    &self.generics.nominal_type_modules,
                ) {
                    Some(_) => string(),
                    None => self.error_with_help(
                        ErrorCode::GenericTypeError,
                        format!(
                            "`typeof` requires a ground type, found `{}`",
                            crate::typechecking::pretty::format_ty_for_diag(&self.subst, &resolved)
                        ),
                        inner.0.into_range(),
                        Some(
                            "specialize generic parameters or annotate the expression so its type is fully known"
                                .to_string(),
                        ),
                    ),
                }
            }

            Expression::Coalesce(lhs, rhs) => {
                let lhs_ty = self.infer(lhs);
                let resolved = apply_ty_prune(&self.subst, &lhs_ty);
                let payload = if let Some((ok, _)) = result_ok_err(&resolved) {
                    ok
                } else if is_option_ty(&resolved) {
                    option_inner(&resolved).unwrap_or_else(|| Ty::Var(self.counter.fresh()))
                } else if matches!(resolved, Ty::Var(_)) {
                    // Prefer Option for free vars under `??`.
                    let inner = Ty::Var(self.counter.fresh());
                    self.unify(
                        &lhs_ty,
                        &option_ty(inner.clone()),
                        &lhs.0.into_range(),
                        "coalesce lhs",
                    );
                    inner
                } else {
                    return self.error(
                        ErrorCode::InvalidCoalesce,
                        format!("`??` requires Option or Result, found `{}`", resolved),
                        range,
                    );
                };
                let rhs_ty = self.infer(rhs);
                self.unify(&payload, &rhs_ty, &rhs.0.into_range(), "coalesce rhs");
                payload
            }

            Expression::OptionalAccess(receiver, field) => {
                let recv_ty = self.infer(receiver);
                let resolved = apply_ty_prune(&self.subst, &recv_ty);
                let inner = if is_option_ty(&resolved) {
                    option_inner(&resolved).unwrap_or_else(|| Ty::Var(self.counter.fresh()))
                } else if matches!(resolved, Ty::Var(_)) {
                    let inner = Ty::Var(self.counter.fresh());
                    self.unify(
                        &recv_ty,
                        &option_ty(inner.clone()),
                        &receiver.0.into_range(),
                        "optional access receiver",
                    );
                    inner
                } else {
                    return self.error(
                        ErrorCode::InvalidOptionalAccess,
                        format!("`?.` requires Option, found `{}`", resolved),
                        range,
                    );
                };
                // Resolve field on the inner type (enum record / dict).
                let field_ty = self.field_type_from_ty(&inner, field, &range);
                option_ty(field_ty)
            }

            Expression::TypeApp { name, args } => {
                // Appears in type-annotation positions; treat like Type.
                self.parse_type_app(name, args, range)
            }
            Expression::TypeProjection { owner, name, args } => {
                let arg_tys: Vec<Ty> = args.iter().map(|a| self.parse_type_name(a)).collect();
                self.resolve_type_projection(owner, name, &arg_tys, &range)
            }

            // ---- Userland FFI builtins ----
            //
            // Legacy AST form (tests / older parsers). Prefer Call + `use ffi::{…}`.
            Expression::Dload(path) => self.infer_ffi_dload(std::slice::from_ref(path), range),
            // `done(h)` — true when coroutine handle `h` is Done.
            Expression::Done(handle) => {
                let handle_ty = self.infer(handle);
                let y_var = Ty::Var(self.counter.fresh());
                let s_var = Ty::Var(self.counter.fresh());
                let coro_ty = self.coroutine_type(y_var, s_var);
                self.unify(&handle_ty, &coro_ty, &range, "done argument");
                boolean()
            }
            // Tuple literal
            Expression::Tuple(items) => {
                let mut elem_tys = Vec::with_capacity(items.len());
                for item in items {
                    let t = self.infer(item);
                    elem_tys.push(apply_ty_prune(&self.subst, &t));
                }
                tuple_ty(elem_tys)
            }
            // Array literal (static length from item count)
            Expression::Array(items) => self.infer_array_literal(items, range),
            // Index: static-length OOB check for literal indices
            Expression::Index(target, index_expr) => self.infer_index_expr(target, index_expr, range),
            // ---- Dict literals ----
            Expression::Dict(fields) => {
                // Check for duplicate field names — diagnostic
                // is raised BEFORE we proceed (recovery: keep
                // all fields, but emit once).
                let mut seen: HashMap<String, ()> = HashMap::new();
                for f in fields {
                    if seen.insert(f.name.to_string(), ()).is_some() {
                        let _ = self.error_with_help(
                            ErrorCode::DuplicateField,
                            format!("Duplicate field `{}` in record literal", f.name),
                            range.clone(),
                            Some("record literals must have unique field names".to_string()),
                        );
                    }
                }
                // Build the record type in source order.
                let mut record_fields: Vec<(String, Ty)> = Vec::with_capacity(fields.len());
                for f in fields {
                    let fty = self.infer(&f.value);
                    let fty_pruned = apply_ty_prune(&self.subst, &fty);
                    record_fields.push((f.name.to_string(), fty_pruned));
                }
                // Sort canonically by name for unification
                // determinism (mirrors the existing record-
                // variant treatment in `Ty::Sum`).
                record_fields.sort_by(|a, b| a.0.cmp(&b.0));
                crate::typechecking::ty::record(record_fields)
            }
            // — registers a signature in the library and returns
            // a function id (an `int`). We verify that each
            // arg/ret position is an `FFIType::X` constructor
            // application (otherwise the codegen won't know how
            // to encode the type). Returns `int`.
            Expression::Declare(args) => self.infer_ffi_declare(args, range),
            Expression::Invoke(args) => self.infer_ffi_invoke(args, range),

            // ---- Defer / coroutines / list ----
            Expression::Defer { captures, body } => {
                // Same explicit-capture isolation as lambdas: outer locals
                // are invisible unless listed in `use (…)`.
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
                let mut uncaptured = self.env.all_names();
                for (n, _) in &cap_bindings {
                    uncaptured.remove(n);
                }

                let import_rebinds =
                    self.snapshot_file_level_imports(&mut uncaptured, range.clone());

                let saved_frames = self.env.take_and_isolate();
                let prev_uncaptured = self.lambda_uncaptured_outer.replace(uncaptured);
                self.rebind_file_level_imports(import_rebinds);
                for (n, ty) in &cap_bindings {
                    self.env.insert_top(n.clone(), Scheme::mono(ty.clone()));
                    self.record_codegen_var_type(n.clone(), ty.clone());
                }
                let _ = self.infer(body);
                self.lambda_uncaptured_outer = prev_uncaptured;
                self.env.restore_frames(saved_frames);
                unit_ty()
            }
            Expression::Yield(e) => {
                if self.async_depth == 0 {
                    return self.error_with_help(
                        ErrorCode::YieldOutsideAsync,
                        "yield outside async function".to_string(),
                        range,
                        Some("yield may only appear inside an async fn body".to_string()),
                    );
                }
                let ty = self.infer(e);
                if let Some(yield_ty) = self.current_yield_ty.clone() {
                    self.unify(&yield_ty, &ty, &e.0.into_range(), "yield value");
                }
                if let Some(send_ty) = self.current_send_ty.clone() {
                    apply_ty_prune(&self.subst, &send_ty)
                } else {
                    unit_ty()
                }
            }
            Expression::YieldFrom(e) => {
                if self.async_depth == 0 {
                    return self.error_with_help(
                        ErrorCode::YieldOutsideAsync,
                        "yield from outside async function".to_string(),
                        range,
                        Some("yield from may only appear inside an async fn body".to_string()),
                    );
                }
                let inner_ty = self.infer(e);
                let (y_var, s_var) = (Ty::Var(self.counter.fresh()), Ty::Var(self.counter.fresh()));
                let expected = self.coroutine_type(y_var.clone(), s_var.clone());
                self.unify(&inner_ty, &expected, &range, "yield from target");
                if let Some(yield_ty) = self.current_yield_ty.clone() {
                    self.unify(&yield_ty, &y_var, &range, "yield from yield type");
                }
                if let Some(send_ty) = self.current_send_ty.clone() {
                    self.unify(&send_ty, &s_var, &range, "yield from send type");
                }
                unit_ty()
            }
            Expression::Resume(target, arg) => {
                let target_ty = self.infer(target);
                let y_var = Ty::Var(self.counter.fresh());
                let s_var = Ty::Var(self.counter.fresh());
                let coro_ty = self.coroutine_type(y_var.clone(), s_var.clone());
                self.unify(&target_ty, &coro_ty, &range, "resume target");
                if let Some(a) = arg {
                    let v_ty = self.infer(a);
                    self.unify(&v_ty, &s_var, &a.0.into_range(), "resume send value");
                }
                apply_ty_prune(&self.subst, &y_var)
            }
            Expression::List(elements) => self.infer_list(elements, range),

            // ---- Default arm ----
            Expression::Default(_) => self
                .current_match_lhs
                .clone()
                .unwrap_or_else(|| Ty::Var(self.counter.fresh())),

            // ---- Function declarations ----
            Expression::Function {
                docs: _,
                attrs,
                name,
                is_coro,
                is_static,
                type_params,
                args,
                returns,
                where_constraints,
                body,
            } => self.infer_function_expr(
                attrs,
                name,
                *is_coro,
                *is_static,
                type_params,
                args,
                returns,
                where_constraints,
                body,
                range,
            ),

            // ---- Anonymous lambdas ----
            Expression::Lambda {
                args,
                captures,
                body,
            } => self.infer_lambda(args, captures, body, range),

            // ---- `test("…") { … }` harness cases ----
            Expression::TestCase { name, body } => self.infer_test_case(name, body, &range),
            Expression::Implementation {
                what,
                owner,
                methods,
                type_params,
                ..
            } => {
                self.infer_impl(what, owner, type_params, methods, &range);
                unit_ty()
            }
            Expression::Class {
                docs: _,
                name,
                type_params,
                fields,
                ..
            } => {
                let key = self.qualify_module_name(name);
                let _ = self.register_generic_type_ctor(&key, type_params);
                let pushed = self.push_type_params_for_type_parsing(type_params);
                self.register_class(name, fields, &range);
                self.pop_type_params_for_type_parsing(pushed);
                unit_ty()
            }
            Expression::Argument { ty, is_rest, .. } => {
                if *is_rest {
                    match ty {
                        None => Ty::Var(self.counter.fresh()),
                        Some(t) => array(self.parse_type_name(t)),
                    }
                } else {
                    self.parse_type_name(ty.as_ref().expect("fixed param type"))
                }
            }
            Expression::Spread(inner) => {
                let _ = self.infer(inner);
                self.error_with_help(
                    ErrorCode::GenericTypeError,
                    "spread expression is only valid in call argument lists".to_string(),
                    range,
                    None,
                )
            }
            Expression::AttrDecl { .. } => unit_ty(),
            Expression::Method(_vis, body) => self.infer(body),
            Expression::Member(_) => unit_ty(),
            Expression::Access(receiver, field) => self.infer_access_expr(receiver, field, range),
            Expression::Instantiate(class_expr, args) => self.infer_instantiate(class_expr, args, range),
            Expression::Field { .. } => unit_ty(),

            // ---- Enums / constructors / type aliases ----
            Expression::EnumDecl {
                docs: _,
                name,
                type_params,
                variants,
                ..
            } => {
                let _ = self.register_generic_type_ctor(name, type_params);
                self.infer_enum_decl(name, variants, &range);
                unit_ty()
            }
            Expression::TypeAlias {
                docs: _,
                name,
                type_params,
                ty,
            } => {
                let _ = self.register_generic_type_ctor(name, type_params);
                let pushed = self.push_type_params_for_type_parsing(type_params);
                let alias_ty = self.parse_type_name(ty);
                // Capture param → var mapping before popping the frame.
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
                self.pop_type_params_for_type_parsing(pushed);
                if type_params.is_empty() {
                    self.register_type_alias(name, alias_ty, range);
                } else {
                    let params = type_params.iter().map(|tp| tp.name.to_string()).collect();
                    self.register_generic_alias(name, params, param_vars, alias_ty, range);
                }
                let _ = self.infer(ty); // ID alignment
                unit_ty()
            }
            Expression::ExternStruct(decl) => {
                use common::encode_tag_operand;
                let span = expr.0.into_range();
                if self.c_structs.iter().any(|s| s.name == decl.name) {
                    self.messages.push(Message::error(
                        ErrorCode::GenericTypeError,
                        format!("Duplicate extern struct `{}`", decl.name),
                        span.clone(),
                    ));
                } else {
                    let mut fields = Vec::new();
                    for (fname, fty) in &decl.fields {
                        self.require_ffi_type_expr(fty);
                        if let Some((tag, aux)) = self.ffi_type_tag_from_output(fty) {
                            fields.push((fname.clone(), encode_tag_operand(tag, aux)));
                        } else {
                            fields.push((fname.clone(), 0));
                        }
                        let _ = self.infer(fty);
                    }
                    self.c_structs.push(CStructDef {
                        name: decl.name.to_string(),
                        fields,
                    });
                }
                unit_ty()
            }
            Expression::EnumVariant { payload, .. } => {
                use parser::ast::EnumVariantPayload;
                // The pre-walk mints an ID for every payload
                // element. Recurse so this arm's ID consumption
                // stays in lockstep. The actual payload parsing
                // happens in `infer_enum_decl`, which knows the
                // parent variant name and target arity.
                match payload {
                    EnumVariantPayload::Unit => {}
                    EnumVariantPayload::Tuple(parts) => {
                        for p in parts {
                            let _ = self.infer(p);
                        }
                    }
                    EnumVariantPayload::Record(fields) => {
                        for f in fields {
                            let _ = self.infer(&f.value);
                        }
                    }
                }
                unit_ty()
            }
            Expression::Construct {
                enum_name,
                variant_name,
                fields,
            } => self.infer_construct(enum_name, variant_name, fields, range, id),

            // ---- Generics ----
            Expression::TypeClass {
                docs: _,
                name,
                type_params,
                methods,
            } => self.infer_typeclass_decl(name, type_params, methods, range),

            Expression::TypeClassImpl {
                class,
                args,
                methods,
            } => self.infer_typeclass_impl(class, args, methods, range),

            Expression::AssocTypeDecl { .. } => unit_ty(),
            Expression::AssocTypeDef { ty, .. } => {
                let _ = self.parse_type_name(ty);
                unit_ty()
            }

            Expression::Readonly(inner) => {
                let inner_ty = self.infer(inner);
                readonly_ty(apply_ty_prune(&self.subst, &inner_ty))
            }
            Expression::QualifiedAccess { owner, member } => {
                let fqn = self.class_member_fqn(owner, member);
                if let Some(ty) = self.static_slot_types.get(&fqn).cloned() {
                    return apply_ty_prune(&self.subst, &ty);
                }
                self.error_with_help(
                    ErrorCode::UnknownValue,
                    format!("Cannot find static member `{}`", fqn),
                    range,
                    Some("declare it with `static` at module or class scope".to_string()),
                )
            }
            Expression::StaticDecl {
                is_const,
                name,
                ty,
                init,
            } => {
                let fqn = self.qualify_module_name(name);
                let declared = ty.as_ref().map(|t| self.parse_type_name(t));
                let init_ty = self.infer(init);
                let slot_ty = if let Some(d) = declared {
                    self.coerce_or_unify(&d, &init_ty, Some(init), &range, "static initializer");
                    apply_ty_prune(&self.subst, &d)
                } else {
                    apply_ty_prune(&self.subst, &init_ty)
                };
                self.register_static_slot(fqn, *is_const, slot_ty.clone(), range.clone());
                self.env
                    .insert_top(name.to_string(), Scheme::mono(slot_ty.clone()));
                self.record_codegen_var_type(name.to_string(), slot_ty.clone());
                if *is_const {
                    self.insert_const_binding(name.to_string());
                    self.warn_shallow_const_binding(name, &slot_ty, range);
                }
                unit_ty()
            }

            Expression::Forall { params, ty } => {
                self.forall_type(params, |checker| checker.infer(ty))
            }

            // ---- Fallback ----
            //
            // `unreachable!` because the match above is exhaustive over every
            // `Expression` variant. The arm is here so that adding a new variant
            // produces a non-exhaustive match error here, instead of silently
            // ignoring the new node. If you add a variant, handle it in the match
            // above and remove this arm.
            #[allow(unreachable_patterns)]
            _ => unreachable!("all Expression variants must be handled above"),
        }
    }

    #[inline(never)]
    fn infer_compound_assign(
        &mut self,
        target: &Output,
        op: &parser::ast::AssignOp,
        value: &Output,
        id: Option<NodeId>,
        range: Range<usize>,
    ) -> Ty {
        let target_ty = self.infer_mutable_lvalue(target, range.clone());
        let val_ty = self.infer(value);
        let op_name = Self::compound_op_name(*op);
        if matches!(
            op,
            parser::ast::AssignOp::Shl
                | parser::ast::AssignOp::Shr
                | parser::ast::AssignOp::BitAnd
                | parser::ast::AssignOp::BitOr
                | parser::ast::AssignOp::BitXor
        ) {
            let _ = unify_with(&self.subst, &target_ty, &int());
            let _ = unify_with(&self.subst, &val_ty, &int());
        } else {
            let tp = apply_ty_prune(&self.subst, &target_ty);
            let vp = apply_ty_prune(&self.subst, &val_ty);
            if crate::typechecking::aggregate_arith::is_matrix_ty(&tp)
                || crate::typechecking::aggregate_arith::is_matrix_ty(&vp)
            {
                let result =
                    self.infer_matrix_arith(tp.clone(), vp, id, range.clone(), op_name);
                let _ = self.unify(
                    &target_ty,
                    &result,
                    &range,
                    &format!("operands of `{}=`", op_name),
                );
            } else if matches!(&tp, Ty::Tuple(_) | Ty::Array { .. })
                || matches!(&vp, Ty::Tuple(_) | Ty::Array { .. })
            {
                // Resolve as aggregate arith; result must match LHS shape.
                let result =
                    self.infer_aggregate_arith(tp.clone(), vp, id, range.clone(), op_name);
                let _ = self.unify(
                    &target_ty,
                    &result,
                    &range,
                    &format!("operands of `{}=`", op_name),
                );
            } else {
                self.unify(
                    &target_ty,
                    &val_ty,
                    &range,
                    &format!("operands of `{}=`", op_name),
                );
            }
        }
        apply_ty_prune(&self.subst, &target_ty)
    }

    #[inline(never)]
    #[allow(clippy::too_many_arguments)]
    fn infer_function_expr(
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
        let prev_test_result_mode = if let Some(desc) = &test_desc {
            self.test_case_names.push(desc.clone());
            let prev = self.fn_result_mode.take();
            self.fn_result_mode = Some((unit_ty(), string()));
            prev
        } else {
            None
        };

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

        if test_desc.is_some() {
            self.result_mode_fns.insert(name.to_string());
            self.fn_result_mode = prev_test_result_mode;
        }

        self.registering_overloadable_fn = prev_overloadable;
        unit_ty()
    }

    #[inline(never)]
    fn infer_lambda(
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

    #[inline(never)]
    fn infer_extern_block(&mut self, declarations: &[ExternFunction]) -> Ty {
        for decl in declarations {
            let arg_tys: Vec<Ty> = if let Expression::Fragment(items) = decl.args.1.as_ref()
            {
                items
                    .iter()
                    .filter_map(|item| {
                        if let Expression::Argument { ty, .. } = item.1.as_ref() {
                            ty.as_ref().map(|t| self.parse_type_name(t))
                        } else {
                            None
                        }
                    })
                    .collect()
            } else {
                Vec::new()
            };
            let nfixed = arg_tys.len();
            let ret_ty = decl
                .returns
                .as_ref()
                .map(|r| self.parse_type_name(r))
                .unwrap_or_else(unit_ty);
            // Register the fixed-prefix function type (extra `...` args
            // are accepted at call sites via `extern_variadic`).
            let fn_ty = arg_tys
                .iter()
                .rev()
                .fold(ret_ty, |acc, p| Ty::Fun(Box::new(p.clone()), Box::new(acc)));
            self.env
                .insert_top(decl.name.to_string(), Scheme::mono(fn_ty.clone()));
            if decl.variadic {
                self.extern_variadic.insert(decl.name.to_string());
                self.extern_variadic_nfixed
                    .insert(decl.name.to_string(), nfixed);
            } else {
                // Fixed-arity extern overloads (C `...` is not a member).
                let param_names: Vec<String> =
                    if let Expression::Fragment(items) = decl.args.1.as_ref() {
                        items
                            .iter()
                            .filter_map(|item| {
                                if let Expression::Argument { name, .. } = item.1.as_ref() {
                                    Some(name.to_string())
                                } else {
                                    None
                                }
                            })
                            .collect()
                    } else {
                        Vec::new()
                    };
                self.register_overload_candidate(
                    decl.name,
                    OverloadCandidate {
            id: 0,
                        fixed_arity: nfixed,
                        is_rest: false,
                        scheme: Scheme::mono(fn_ty),
                        param_names,
                    },
                    &decl.args.0.into_range(),
                );
            }
        }
        unit_ty()
    }

    #[inline(never)]
    fn infer_use_decl(
        &mut self,
        path: &[String],
        name: &str,
        alias: &Option<String>,
        range: Range<usize>,
    ) -> Ty {
        let module_ns = path.join("::");
        if name == "*" {
            // Prelude is injected automatically; every other module —
            // virtual or userland — requires explicit imports.
            let mod_label = if module_ns.is_empty() {
                "<entry>".to_string()
            } else {
                module_ns.clone()
            };
            return self.error_with_help(
                ErrorCode::WildcardImport,
                format!("wildcard import `use {}::*` is not allowed", mod_label),
                range,
                Some(format!(
                    "list names explicitly, e.g. `use {}::{{name1, name2}}`; prelude is auto-imported",
                    mod_label
                )),
            );
        }
        if self.apply_virtual_use(path, name, alias.as_deref()) {
            // Bind FFI callables into the value env so Call sites
            // resolve; enums/traits/tags are scope-only.
            let locals: Vec<(String, BuiltinExport)> = self
                .scope_bindings
                .iter()
                .filter(|(_, e)| {
                    matches!(
                        e,
                        BuiltinExport::FfiFn { .. }
                            | BuiltinExport::IoFn { .. }
                            | BuiltinExport::StringFn { .. }
                            | BuiltinExport::ThreadFn { .. }
                            | BuiltinExport::GcFn { .. }
                            | BuiltinExport::HostFn { .. }
                    )
                })
                .map(|(k, e)| (k.clone(), e.clone()))
                .collect();
            for (local, export) in locals {
                if self.env.lookup(&local).is_some() {
                    continue;
                }
                if let Some(scheme) = self.virtual_callable_scheme(export, range.clone()) {
                    self.env.insert_top(local, scheme);
                }
            }
            return unit_ty();
        }
        let local = alias.clone().unwrap_or_else(|| name.to_string());
        let fqn = if module_ns.is_empty() {
            name.to_string()
        } else {
            format!("{module_ns}::{name}")
        };
        // Prefer the defining module's real scheme (incl. generics with
        // bounds) over a dummy Var so call sites get dict-passing ABI.
        self.reexport_module_item(&fqn, &local);
        // Re-export overload families under the local alias so
        // `use num::{abs}` can still type-dispatch.
        if let Some(cands) = self.overload_sets.get(&fqn).cloned() {
            if cands.len() > 1 {
                self.overload_sets.insert(local.clone(), cands);
            }
        }
        // Disk imports are file-level globals — track for lambda/defer
        // rebind after `take_and_isolate`.
        if name != "*" {
            self.disk_imports.insert(local);
        }
        unit_ty()
    }

    #[inline(never)]
    fn infer_instantiate(
        &mut self,
        class_expr: &Output,
        args: &Option<Vec<Output>>,
        range: Range<usize>,
    ) -> Ty {
        let class_name = if let Expression::Identifier(name) = class_expr.1.as_ref()
            && let Some(key) = self.resolve_class_key(name)
        {
            key
        } else {
            let class_ty = self.infer(class_expr);
            let resolved = apply_ty_prune(&self.subst, &class_ty);
            match &resolved {
                Ty::Con(n) => n.clone(),
                _ => {
                    return self.error(
                        ErrorCode::NotAFunction,
                        "Cannot instantiate non-class type".to_string(),
                        range,
                    );
                }
            }
        };
        if let Some(fields) = self.classes.get(&class_name).cloned() {
            let param_names = self
                .generics
                .generic_type_ctors
                .get(&class_name)
                .cloned()
                .unwrap_or_default();
            // Freshen field types at each `new` site so independent
            // instantiations don't share type variables.
            let (field_tys, result_ty) = if param_names.is_empty() {
                (
                    fields.iter().map(|(_, _, t)| t.clone()).collect::<Vec<_>>(),
                    Ty::Con(class_name.clone()),
                )
            } else {
                let mut map = HashMap::new();
                let mut app_args = Vec::with_capacity(param_names.len());
                for p in &param_names {
                    let v = Ty::Var(self.counter.fresh());
                    app_args.push(v.clone());
                    map.insert(p.clone(), v);
                }
                let field_tys = fields
                    .iter()
                    .map(|(_, _, t)| subst_ty_params(t, &map))
                    .collect();
                (
                    field_tys,
                    Ty::App(Box::new(Ty::Con(class_name.clone())), app_args),
                )
            };
            let provided = args.as_ref().map(|a| a.as_slice()).unwrap_or(&[]);
            if provided.len() != fields.len() {
                let _ = self.error_with_help(
                    ErrorCode::ConstructorArity,
                    format!(
                        "Constructor `{}` expects {} arguments, got {}",
                        class_name,
                        fields.len(),
                        provided.len()
                    ),
                    range,
                    Some(
                        "pass one argument per class field, in declaration order"
                            .to_string(),
                    ),
                );
            } else {
                for (arg, fty) in provided.iter().zip(field_tys.iter()) {
                    let aty = self.infer(arg);
                    self.unify(&aty, fty, &arg.0.into_range(), "constructor argument");
                }
            }
            return apply_ty_prune(&self.subst, &result_ty);
        }
        Ty::Con(class_name)
    }

    #[inline(never)]
    fn infer_access_expr(
        &mut self,
        receiver: &Output,
        field: &str,
        range: Range<usize>,
    ) -> Ty {
        let receiver_ty = self.infer(receiver);
        let resolved = apply_ty_prune(&self.subst, &receiver_ty);
        match strip_readonly(&resolved) {
            Ty::Sum { name, variants } => {
                self.access_field_in_sum(name, variants, None, field, range)
            }
            Ty::Constructor { tag, owner, .. } => {
                // Resolve the owner to its variants.
                match owner.as_ref() {
                    Ty::Sum { name, variants } => {
                        self.access_field_in_sum(name, variants, Some(*tag), field, range)
                    }
                    _ => self.error_with_help(
                        ErrorCode::GenericTypeError,
                        format!("Cannot access field `{}` on non-record type", field),
                        range,
                        Some(
                            "only values of record-shaped enum types expose fields"
                                .to_string(),
                        ),
                    ),
                }
            }
            Ty::App(head, args) if matches!(head.as_ref(), Ty::Con(n) if self.classes.contains_key(n)) =>
            {
                let name = match head.as_ref() {
                    Ty::Con(n) => n.clone(),
                    _ => unreachable!(),
                };
                self.access_class_field(&name, field, args, range)
            }
            Ty::Con(name) => {
                // Class instance field access.
                if self.classes.contains_key(name) {
                    return self.access_class_field(name, field, &[], range);
                }
                // Bare type name — resolve via the
                // checker's enum registry.
                let variant_names = self.enums.get(name).cloned().unwrap_or_default();
                let payloads = self.enum_payloads.get(name).cloned().unwrap_or_default();
                if variant_names.is_empty() {
                    return self.error_with_help(
                        ErrorCode::GenericTypeError,
                        format!("Cannot access field `{}` on non-record type", field),
                        range,
                        Some(format!("type `{}` is not a record-shaped enum", name)),
                    );
                }
                let variants: Vec<(String, EnumVariantPayloadTy)> =
                    variant_names.into_iter().zip(payloads).collect();
                self.access_field_in_sum(name, &variants, None, field, range)
            }
            Ty::Record { fields } => match fields.iter().find(|(n, _)| n == field) {
                Some((_, fty)) => fty.clone(),
                None => {
                    let known: Vec<&str> = fields.iter().map(|(n, _)| n.as_str()).collect();
                    let msg = format!(
                        "Cannot find field `{}` on record `{{ {} }}`",
                        field,
                        fields
                            .iter()
                            .map(|(n, t)| format!("{}: {}", n, t))
                            .collect::<Vec<_>>()
                            .join(", ")
                    );
                    let help = if known.is_empty() {
                        Some("the record has no fields".to_string())
                    } else {
                        Some(format!("the record has fields: {}", known.join(", ")))
                    };
                    self.error_with_help(ErrorCode::GenericTypeError, msg, range, help)
                }
            },
            _ => self.error_with_help(
                ErrorCode::GenericTypeError,
                format!("Cannot access field `{}` on non-record type", field),
                range,
                Some("only values of record-shaped enum types expose fields".to_string()),
            ),
        }
    }

    #[inline(never)]
    fn infer_cast(&mut self, expr: &Output, ty_ann: &Output) -> Ty {
        let dst_ty = self.parse_type_name(ty_ann);
        let dst_ty = apply_ty_prune(&self.subst, &dst_ty);
        // Pin expected type so `"/" as byte`, `"hi" as [byte]`, and
        // in-range `65 as byte` type the operand as the target when it
        // is a coercible literal.
        let prev_expected = self.current_expected.take();
        if Self::is_byte_ty(&dst_ty) || Self::is_byte_array_ty(&dst_ty).is_some() {
            self.current_expected = Some(dst_ty.clone());
        }
        let src_ty = self.infer(expr);
        self.current_expected = prev_expected;
        let src_ty = apply_ty_prune(&self.subst, &src_ty);
        let range = expr.0.into_range();
        if Self::is_byte_array_ty(&dst_ty).is_some() {
            if Self::byte_array_tys_compatible(&src_ty, &dst_ty) {
                return dst_ty;
            }
            if Self::is_string_ty(&src_ty) {
                if Self::is_vec_byte_ty(&dst_ty) {
                    return self.error_with_help(
                        ErrorCode::TypeMismatch,
                        "cannot cast `string` to `Vec<byte>`".to_string(),
                        range,
                        Some(
                            "use `to_bytes(s)` to encode a string as UTF-8 bytes (a cast does not UTF-8-encode)"
                                .to_string(),
                        ),
                    );
                }
                // Non-literal `s as [byte]` → `to_bytes(s)`. Fixed
                // `[byte; N]` still requires a literal (length known).
                if matches!(
                    Self::is_byte_slice_ty(&dst_ty),
                    Some(crate::typechecking::ty::ArrayLength::Dynamic)
                ) {
                    return dst_ty;
                }
                return self.error_with_help(
                    ErrorCode::TypeMismatch,
                    "cannot cast `string` to fixed-length `[byte; N]`".to_string(),
                    range,
                    Some(
                        "use a string literal of length N, or `to_bytes(s)` for a dynamic `[byte]` / `Vec<byte>`"
                            .to_string(),
                    ),
                );
            }
        }
        match (
            Self::primitive_cast_name(&src_ty),
            Self::primitive_cast_name(&dst_ty),
        ) {
            (Some(from), Some(to)) if from == to || Self::primitive_cast_allowed(from, to) => {
                // Expected-byte inference can type `(-1)` as `byte` before
                // this match (`from == to`); still reject out-of-range literals.
                if to == "byte"
                    && (from == "int" || from == "byte")
                    && let Err(Some(n)) = Self::byte_literal_coercion(expr)
                {
                    return self.error_with_help(
                        ErrorCode::TypeMismatch,
                        format!("byte literal out of range: `{n}` is not in 0..=255"),
                        range,
                        Some(
                            "literal `int as byte` must be in 0..=255; non-literal ints wrap at runtime"
                                .to_string(),
                        ),
                    );
                }
                dst_ty
            }
            (Some(from), Some(to)) => self.error_with_help(
                ErrorCode::TypeMismatch,
                format!("cannot cast `{from}` to `{to}`"),
                range,
                Some("allowed casts: int↔float, int↔byte, int↔bool; single-byte string literals → byte; string literals → `[byte]` / `[byte; N]`".to_string()),
            ),
            (None, Some("byte")) if Self::is_string_ty(&src_ty) => self.error_with_help(
                ErrorCode::TypeMismatch,
                "cannot cast `string` to `byte`".to_string(),
                range,
                Some(
                    "only a string literal whose UTF-8 encoding is exactly one byte coerces to `byte` (e.g. `\"/\"`, `\"\\n\"`)"
                        .to_string(),
                ),
            ),
            _ => self.error_with_help(
                ErrorCode::TypeMismatch,
                "cast target must be a primitive type (`int`, `float`, `byte`, or `bool`) or a byte array (`[byte]` / `[byte; N]`)"
                    .to_string(),
                ty_ann.0.into_range(),
                None,
            ),
        }
    }

    #[inline(never)]
    fn infer_index_expr(
        &mut self,
        target: &Output,
        index_expr: &Option<Output>,
        range: Range<usize>,
    ) -> Ty {
        let target_ty = self.infer(target);
        let target_ty = apply_ty_prune(&self.subst, &target_ty);
        let Some(index_expr) = index_expr else {
            return self.error_with_help(
                ErrorCode::CannotIndex,
                "empty index `arr[]` is not valid".to_string(),
                range,
                Some("use `vec.push(value)` to append to a `Vec`".to_string()),
            );
        };
        let index_ty = self.infer(index_expr);
        let index_ty_pruned = apply_ty_prune(&self.subst, &index_ty);
        // Constrain the index to be an `int` (the VM only
        // supports integer indices).
        let _ = unify_with(&self.subst, &index_ty_pruned, &int());
        let resolved = apply_ty_prune(&self.subst, &target_ty);
        // Peel `Matrix<Data>` so `m[i][j]` indexes the nested rows.
        let resolved =
            if let Some(data) = crate::typechecking::aggregate_arith::unwrap_matrix_ty(&resolved) {
                data.clone()
            } else {
                resolved
            };
        match &resolved {
            Ty::Array { element, length } => {
                // Out-of-bounds check: only fires when the
                // target is a *static-length* array and the
                // index is a literal integer.
                if let ArrayLength::Static(n) = length
                    && let Expression::Integer(idx) = index_expr.1.as_ref()
                {
                    let i = *idx;
                    if i < 0 || (i as usize) >= *n {
                        let _ = self.error_with_help(
                            ErrorCode::IndexOutOfBounds,
                            format!(
                                "array index {} out of bounds for array of length {}",
                                i, n
                            ),
                            range.clone(),
                            Some(format!(
                                "indices are valid in [0..{}); the array has length {}",
                                n, n
                            )),
                        );
                    }
                }
                (**element).clone()
            }
            other if vec_element_ty(other).is_some() => {
                vec_element_ty(other).expect("checked").clone()
            }
            Ty::Tuple(tys) => {
                // Tuple indexing: same diagnostic on constant
                // out-of-bounds; dynamic fallback returns a
                // fresh ty var (the runtime pushes -1i64 for
                // OOB).
                if let Expression::Integer(idx) = index_expr.1.as_ref() {
                    let i = *idx;
                    if i < 0 || (i as usize) >= tys.len() {
                        let _ = self.error_with_help(
                            ErrorCode::IndexOutOfBounds,
                            format!(
                                "tuple index {} out of bounds for tuple of length {}",
                                i,
                                tys.len()
                            ),
                            range.clone(),
                            Some(format!(
                                "indices are valid in [0..{}); the tuple has length {}",
                                tys.len(),
                                tys.len()
                            )),
                        );
                    } else {
                        return tys[i as usize].clone();
                    }
                }
                Ty::Var(self.counter.fresh())
            }
            _ => {
                // Non-aggregate target: emit a diagnostic.
                let _ = self.error_with_help(
                    ErrorCode::CannotIndex,
                    "cannot index non-aggregate type".to_string(),
                    range.clone(),
                    Some(format!("type `{}` does not support indexing", resolved)),
                );
                Ty::Var(self.counter.fresh())
            }
        }
    }

    #[inline(never)]
    fn infer_array_literal(&mut self, items: &[Output], range: Range<usize>) -> Ty {
        let expected_elem = self.current_expected.clone().and_then(|exp| {
            let exp = apply_ty_prune(&self.subst, &exp);
            if let Ty::Array { element, .. } = &exp {
                return Some(element.as_ref().clone());
            }
            vec_element_ty(&exp).cloned()
        });
        let mut elem_ty: Option<Ty> = None;
        for item in items {
            let prev_expected = self.current_expected.take();
            if let Some(ref e) = expected_elem {
                self.current_expected = Some(e.clone());
            }
            let t = self.infer(item);
            self.current_expected = prev_expected;
            // Peel constructor tags so `[Rank::Low, Rank::Mid]` is
            // `[Rank; 2]`, not a stuck `::v0` element type.
            let t_pruned =
                crate::typechecking::ty::peel_constructor_refinement(apply_ty_prune(
                    &self.subst, &t,
                ));
            match &elem_ty {
                None => elem_ty = Some(t_pruned),
                Some(prev) => {
                    let prev_pruned = apply_ty_prune(&self.subst, prev);
                    match unify_with(&self.subst, &prev_pruned, &t_pruned) {
                        Ok(s) => {
                            self.subst = compose(&s, &self.subst);
                            elem_ty = Some(apply_ty_prune(
                                &self.subst,
                                &crate::typechecking::ty::peel_constructor_refinement(
                                    prev_pruned,
                                ),
                            ));
                        }
                        Err(_) => {
                            let _ = self.error_with_help(
                                ErrorCode::TypeMismatch,
                                format!(
                                    "array element type mismatch: expected `{}`, found `{}`",
                                    prev_pruned, t_pruned
                                ),
                                range.clone(),
                                Some(
                                    "an array literal requires every element to have the same type"
                                        .to_string(),
                                ),
                            );
                        }
                    }
                }
            }
        }
        let element = elem_ty.unwrap_or_else(|| Ty::Var(self.counter.fresh()));
        let len = items.len();
        if len == 0 {
            if let Some(exp) = self.current_expected.clone() {
                let exp = apply_ty_prune(&self.subst, &exp);
                if let Some(vec_elem) = vec_element_ty(&exp) {
                    let _ = unify_with(&self.subst, vec_elem, &element);
                    return apply_ty_prune(&self.subst, &exp);
                }
                if let Ty::Array {
                    element: arr_elem,
                    length,
                } = &exp
                {
                    match length {
                        ArrayLength::Static(0) => {
                            let _ = unify_with(&self.subst, arr_elem.as_ref(), &element);
                            return array_fixed(
                                apply_ty_prune(&self.subst, arr_elem.as_ref()),
                                0,
                            );
                        }
                        ArrayLength::Static(n) => {
                            return self.error_with_help(
                                ErrorCode::TypeMismatch,
                                format!(
                                    "empty array literal `[]` cannot satisfy `[_; {}]`",
                                    n
                                ),
                                range,
                                Some(format!(
                                    "expected {} element{}, or annotate as `Vec<T>` / `[T; 0]`",
                                    n,
                                    if *n == 1 { "" } else { "s" }
                                )),
                            );
                        }
                        ArrayLength::Dynamic => {}
                    }
                }
            }
            return self.error_with_help(
                ErrorCode::GenericTypeError,
                "empty array literal `[]` requires a type annotation".to_string(),
                range,
                Some(
                    "annotate as `Vec<T>` (growable) or `[T; 0]` (fixed empty array)"
                        .to_string(),
                ),
            );
        }
        array_fixed(element, len)
    }

    #[inline(never)]
    fn infer_identifier(&mut self, name: &str, range: Range<usize>) -> Ty {
        // When `name` has multiple overload candidates and appears in
        // value position, try to narrow using `current_expected`.
        if self.is_overloaded(name) {
            let candidates: Vec<OverloadCandidate> = self
                .overload_candidates(name)
                .map(|c| c.to_vec())
                .unwrap_or_default();
            // If exactly one candidate matches current_expected, pick it.
            let expected = self.current_expected.clone();
            let matching: Vec<&OverloadCandidate> = if let Some(ref exp) = expected {
                // Prune so a solved `Ty::Var` expected type doesn't look
                // open; unify under `self.subst` (not empty) so existing
                // bindings are visible without mutating the running subst.
                let exp = apply_ty_prune(&self.subst, exp);
                candidates
                    .iter()
                    .filter(|c| {
                        let (fun_ty, _, _) = self.instantiate_scheme_mapped(&c.scheme);
                        crate::typechecking::unify::unify_with(&self.subst, &fun_ty, &exp)
                            .is_ok()
                    })
                    .collect()
            } else {
                Vec::new()
            };

            if matching.len() == 1 {
                // Unique match — record and return its type.
                let candidate = matching[0].clone();
                self.selected_overloads_by_span.insert(
                    (range.start, range.end),
                    (candidate.fixed_arity, candidate.is_rest, candidate.id),
                );
                return self.instantiate_ty(&candidate.scheme);
            } else if matching.len() > 1 || expected.is_none() {
                // Multiple matches or no expected type — ambiguous.
                // For the single-candidate case there is no ambiguity even
                // without context, but we already checked `is_overloaded`
                // (len > 1), so ambiguous.
                let arities: Vec<String> = candidates
                    .iter()
                    .map(|c| Self::overload_sig_label(c))
                    .collect();
                return self.error_with_help(
                    ErrorCode::AmbiguousOverload,
                    format!(
                        "Ambiguous overload: `{}` has multiple candidates in value position",
                        name
                    ),
                    range,
                    Some(format!(
                        "available overloads: {}; annotate the expected type to disambiguate",
                        arities.join(", ")
                    )),
                );
            }
            // matching.len() == 0 with an expected type — no candidate
            // unifies. Emit a dedicated diagnostic rather than falling
            // through to the last-registered scheme (wrong codegen key /
            // confusing TypeMismatch downstream).
            let arities: Vec<String> = candidates
                .iter()
                .map(|c| Self::overload_sig_label(c))
                .collect();
            let expected_pretty = expected
                .as_ref()
                .map(|e| apply_ty_prune(&self.subst, e).to_string())
                .unwrap_or_else(|| "?".into());
            return self.error_with_help(
                ErrorCode::TypeMismatch,
                format!(
                    "No overload of `{}` matches expected type `{}`",
                    name, expected_pretty
                ),
                range,
                Some(format!("available overloads: {}", arities.join(", "))),
            );
        }

        let scheme = self.env.lookup(name).cloned();
        match scheme {
            Some(s) => self.instantiate_ty(&s),
            None => {
                if self
                    .lambda_uncaptured_outer
                    .as_ref()
                    .is_some_and(|s| s.contains(name))
                {
                    return self.error_with_help(
                        ErrorCode::UnknownValue,
                        format!("cannot capture `{}` without `use ({})`", name, name),
                        range,
                        Some(format!(
                            "list `{}` in the enclosing `use (…)` capture list",
                            name
                        )),
                    );
                }
                self.error(
                    ErrorCode::UnknownValue,
                    format!("Cannot find value `{}` in this scope", name),
                    range,
                )
            }
        }
    }

    #[inline(never)]
    fn infer_typeclass_decl(
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
    fn infer_typeclass_impl(
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
        if let Some(existing) = overlapping.as_ref() {
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
        let stub_idx = if class_def.is_some() && !orphaned && overlapping.is_none() {
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
        let mut invalid_instance = class_def.is_none() || orphaned || overlapping.is_some();
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
    fn lookup_fn_scheme(&self, ident: &str) -> Option<Scheme> {
        self.env
            .lookup(ident)
            .cloned()
            .or_else(|| self.forward_free_fn_schemes.get(ident).cloned())
    }

    fn infer_call_expr(
        &mut self,
        name: &Output,
        args: &Option<Vec<Output>>,
        id: Option<NodeId>,
        range: Range<usize>,
    ) -> Ty {
        if let Expression::Identifier(callee) = name.1.as_ref() {
            if let Some(arg_list) = args.as_deref() {
                if arg_list.len() == 1 {
                    if let Expression::Spread(pack) = arg_list[0].1.as_ref() {
                        if self.next_id_idx < self.ids.ids().len() {
                            self.next_id_idx += 1;
                        }
                        if let Some(ty) =
                            self.try_infer_spread_call_target(callee, pack, &range, id)
                        {
                            return ty;
                        }
                    }
                }
            }
        }
        // Method call: `recv.method(args)` — Access callee.
        if let Expression::Access(recv, method) = name.1.as_ref() {
            let method_args = args.as_deref().unwrap_or(&[]);
            let method_has_named = method_args
                .iter()
                .any(|a| matches!(a.1.as_ref(), Expression::NamedArg(..)));

            let recv_ty = self.infer(recv);
            let resolved = apply_ty_prune(&self.subst, &recv_ty);

            // Named args on methods: only inherent class methods.
            if method_has_named {
                let class_owner = self.class_owner_from_ty(&resolved);
                if let Some(owner) = class_owner.as_ref()
                    && self
                        .methods
                        .get(owner)
                        .and_then(|m| m.get(*method))
                        .is_some()
                {
                    if *method == "to_vec" {
                        self.constrain_range_to_vec(owner, &resolved, &range);
                    }
                    let fqn = format!("{}::{}", owner, method);
                    let user_argc = method_args.len();
                    let scheme = if self.is_overloaded(&fqn) {
                        let prelim_tys: Vec<Ty> = method_args
                            .iter()
                            .map(|a| {
                                let value = match a.1.as_ref() {
                                    Expression::NamedArg(_, v) => v,
                                    _ => a,
                                };
                                let ty = self.infer(value);
                                apply_ty_prune(&self.subst, &ty)
                            })
                            .collect();
                        match self.select_overload_for_args(&fqn, user_argc, &prelim_tys) {
                            OverloadSelect::Selected(c) => {
                                let c = c.clone();
                                self.selected_overloads_by_span.insert(
                                    (range.start, range.end),
                                    (c.fixed_arity, c.is_rest, c.id),
                                );
                                c.scheme
                            }
                            OverloadSelect::Ambiguous => {
                                return self.error_with_help(
                                    ErrorCode::AmbiguousOverload,
                                    format!(
                                        "Ambiguous overload: call to `{}` matches multiple candidates",
                                        fqn
                                    ),
                                    range,
                                    Some(self.ambiguous_overload_help(&fqn)),
                                );
                            }
                            OverloadSelect::NoMatch => {
                                return self.error(
                                    ErrorCode::WrongArity,
                                    format!(
                                        "No overload of `{}` accepts {} argument{}",
                                        fqn,
                                        user_argc,
                                        if user_argc == 1 { "" } else { "s" }
                                    ),
                                    range,
                                );
                            }
                        }
                    } else {
                        self.methods
                            .get(owner)
                            .and_then(|m| m.get(*method))
                            .map(|(_, s)| s.clone())
                            .expect("method present")
                    };
                    let (fun_ty, constraints, _mapping) =
                        self.instantiate_scheme_mapped(&scheme);
                    let mut arg_tys = vec![recv_ty];
                    let (tys, ordered_exprs) =
                        self.infer_and_reorder_call_args(&fqn, method_args, &range);
                    arg_tys.extend(tys);
                    // Align exprs with `[self, …args]` for coerce_or_unify
                    // (byte / string literal coercion on method args).
                    let mut arg_exprs = Vec::with_capacity(1 + ordered_exprs.len());
                    arg_exprs.push(recv.clone());
                    arg_exprs.extend(ordered_exprs);
                    let result = self.apply_function(
                        Some(&fqn),
                        &fun_ty,
                        &arg_tys,
                        Some(&arg_exprs),
                        id,
                        range.clone(),
                    );
                    if !constraints.is_empty() {
                        self.discharge_constraints(id, &constraints, &range);
                    }
                    return result;
                }
                return self.error_with_help(
                    ErrorCode::GenericTypeError,
                    format!(
                        "Named arguments are not supported on this call to `{}`",
                        method
                    ),
                    range,
                    Some(
                        "named arguments are supported on ordinary functions and inherent methods"
                            .to_string(),
                    ),
                );
            }

            if let Ty::Existential { class } = &resolved
                && let Some((owner, method_slot, scheme)) =
                    self.existential_method_candidate(class, method)
            {
                let mut arg_tys = vec![recv_ty];
                if let Some(a) = args {
                    for arg in a {
                        arg_tys.push(self.infer(arg));
                    }
                }
                let hint = ExistentialMethodCall {
                    method_slot,
                    arity: arg_tys.len(),
                    has_receiver: true,
                };
                if let Some(call_id) = id {
                    self.existential_method_calls.insert(call_id, hint.clone());
                }
                self.existential_method_calls_by_span
                    .insert((range.start, range.end), hint);
                return self.apply_existential_method(
                    &owner,
                    method,
                    &scheme,
                    &arg_tys,
                    args.as_deref(),
                    id,
                    range,
                );
            }
            if let Some(receiver_var) = Self::constraint_var_of_ty(&resolved) {
                let candidates = self.bound_method_candidates(method, Some(receiver_var));
                if let Some((dict_index, dict_class, class, method_slot, scheme)) =
                    self.select_bound_method(candidates, method, &range)
                {
                    self.bind_matching_abstract_constraints(
                        Some(receiver_var),
                        &dict_class,
                    );
                    let (fun_ty, constraints, mapping) =
                        self.instantiate_scheme_mapped(&scheme);
                    let mut arg_tys = vec![recv_ty];
                    if let Some(a) = args {
                        for arg in a {
                            arg_tys.push(self.infer(arg));
                        }
                    }
                    if let Some(call_id) = id {
                        self.bound_method_calls.insert(
                            call_id,
                            BoundMethodCall {
                                dict_index,
                                method_slot,
                                arity: arg_tys.len(),
                                has_receiver: true,
                            },
                        );
                    }
                    self.bound_method_calls_by_span.insert(
                        (range.start, range.end),
                        BoundMethodCall {
                            dict_index,
                            method_slot,
                            arity: arg_tys.len(),
                            has_receiver: true,
                        },
                    );
                    let result = self.apply_function(
                        Some(&format!("{}::{}", class, method)),
                        &fun_ty,
                        &arg_tys,
                        None,
                        id,
                        range.clone(),
                    );
                    if !constraints.is_empty() {
                        self.discharge_constraints(id, &constraints, &range);
                        self.pin_assoc_after_discharge(
                            &class,
                            &constraints,
                            Some(&scheme),
                            &mapping,
                            &range,
                        );
                    }
                    return result;
                }
            }
            // Inherent class methods win over ground trait methods
            // (Rust-style): `impl Point { fn show() ... }` must not be
            // shadowed by prelude `Show::show` when no Show instance
            // exists for Point.
            let class_owner = self.class_owner_from_ty(&resolved);
            if let Some(owner) = class_owner.as_ref()
                && self
                    .methods
                    .get(owner)
                    .and_then(|m| m.get(*method))
                    .is_some()
            {
                if *method == "to_vec" {
                    self.constrain_range_to_vec(owner, &resolved, &range);
                }
                if self.is_static_method(owner, method) {
                    let fqn = format!("{}::{}", owner, method);
                    return self.error_with_help(
                        ErrorCode::GenericTypeError,
                        format!(
                            "`{}` is a static method; call it as `{}(...)`",
                            method, fqn
                        ),
                        range,
                        Some("static methods have no `self` receiver".to_string()),
                    );
                }
                let fqn = format!("{}::{}", owner, method);
                let user_argc = method_args.len();
                let (scheme, selected) = if self.is_overloaded(&fqn) {
                    let prelim_tys: Vec<Ty> = method_args
                        .iter()
                        .map(|a| {
                            let value = match a.1.as_ref() {
                                Expression::NamedArg(_, v) => v,
                                _ => a,
                            };
                            let ty = self.infer(value);
                            apply_ty_prune(&self.subst, &ty)
                        })
                        .collect();
                    match self.select_overload_for_args(&fqn, user_argc, &prelim_tys) {
                        OverloadSelect::Selected(c) => {
                            let c = c.clone();
                            self.selected_overloads_by_span.insert(
                                (range.start, range.end),
                                (c.fixed_arity, c.is_rest, c.id),
                            );
                            (c.scheme, true)
                        }
                        OverloadSelect::Ambiguous => {
                            return self.error_with_help(
                                ErrorCode::AmbiguousOverload,
                                format!(
                                    "Ambiguous overload: call to `{}` matches multiple candidates",
                                    fqn
                                ),
                                range,
                                Some(self.ambiguous_overload_help(&fqn)),
                            );
                        }
                        OverloadSelect::NoMatch => {
                            let available: Vec<String> = self
                                .overload_sets
                                .get(&fqn)
                                .map(|cs| {
                                    cs.iter()
                                        .map(|c| {
                                            if c.is_rest {
                                                format!("{}+ args (rest)", c.fixed_arity)
                                            } else {
                                                format!("{} args", c.fixed_arity)
                                            }
                                        })
                                        .collect()
                                })
                                .unwrap_or_default();
                            return self.error_with_help(
                                ErrorCode::WrongArity,
                                format!(
                                    "No overload of `{}` accepts {} argument{}",
                                    fqn,
                                    user_argc,
                                    if user_argc == 1 { "" } else { "s" }
                                ),
                                range,
                                Some(format!(
                                    "available arities: {}",
                                    available.join(", ")
                                )),
                            );
                        }
                    }
                } else {
                    let scheme = self
                        .methods
                        .get(owner)
                        .and_then(|m| m.get(*method))
                        .map(|(_, s)| s.clone())
                        .expect("method present");
                    (scheme, false)
                };
                let _ = selected;
                let (fun_ty, constraints, _mapping) =
                    self.instantiate_scheme_mapped(&scheme);
                let mut arg_tys = vec![recv_ty];
                let mut arg_exprs = Vec::with_capacity(1 + method_args.len());
                arg_exprs.push(recv.clone());
                if self.fn_has_rest(&fqn) {
                    let (tys, ordered_exprs) =
                        self.infer_and_reorder_call_args(&fqn, method_args, &range);
                    arg_tys.extend(tys);
                    arg_exprs.extend(ordered_exprs);
                } else if let Some(a) = args {
                    for arg in a {
                        arg_tys.push(self.infer(arg));
                        arg_exprs.push(arg.clone());
                    }
                }
                let result = self.apply_function(
                    Some(&fqn),
                    &fun_ty,
                    &arg_tys,
                    Some(&arg_exprs),
                    id,
                    range.clone(),
                );
                if !constraints.is_empty() {
                    self.discharge_constraints(id, &constraints, &range);
                }
                return result;
            }

            // Ground trait method: `recv.into()` / `recv.show()` via a
            // concrete instance (no open bound). Pin the return type from
            // `current_expected` when present so `let y: T = x.into();`
            // (or `return x.into();` under `-> T`) can select among
            // multiple `Into` targets.
            if let Some((class, scheme)) =
                self.ground_trait_method_for_receiver(method, &recv_ty)
            {
                let (fun_ty, constraints, mapping) =
                    self.instantiate_scheme_mapped(&scheme);
                let mut arg_tys = vec![recv_ty];
                if let Some(a) = args {
                    for arg in a {
                        arg_tys.push(self.infer(arg));
                    }
                }
                let result = self.apply_function(
                    Some(&format!("{}::{}", class, method)),
                    &fun_ty,
                    &arg_tys,
                    None,
                    id,
                    range.clone(),
                );
                if let Some(expected) = self.current_expected.clone() {
                    self.unify(&result, &expected, &range, "expected type");
                }
                if !constraints.is_empty() {
                    self.discharge_constraints(id, &constraints, &range);
                    self.pin_assoc_after_discharge(
                        &class,
                        &constraints,
                        Some(&scheme),
                        &mapping,
                        &range,
                    );
                }
                return apply_ty_prune(&self.subst, &result);
            }

            if let Some(owner) = class_owner {
                return self.error(
                    ErrorCode::UnknownFunction,
                    format!("Cannot find method `{}` on class `{}`", method, owner),
                    range,
                );
            }
            return self.error_with_help(
                ErrorCode::NotAFunction,
                format!("Cannot call method `{}` on non-class type", method),
                range,
                Some("method calls require a class instance receiver".to_string()),
            );
        }

        // First-class callee (`lambda(...)`, nested fn value, etc.).
        if !matches!(
            name.1.as_ref(),
            Expression::Identifier(_)
                | Expression::Access(_, _)
                | Expression::QualifiedAccess { .. }
        ) {
            let callee_ty = self.infer(name);
            let flat_args = self.flatten_spread_call_args(args.as_deref().unwrap_or(&[]));
            let arg_tys: Vec<Ty> = flat_args
                .iter()
                .map(|arg| self.infer_call_arg(arg))
                .collect();
            return self.apply_function(
                None,
                &callee_ty,
                &arg_tys,
                args.as_deref(),
                id,
                range,
            );
        }

        let ident = match name.1.as_ref() {
            Expression::Identifier(n) => n.to_string(),
            Expression::QualifiedAccess { owner, member } => {
                let fqn = self.class_member_fqn(owner, member);
                if self.static_slot_types.contains_key(&fqn) {
                    return self.error_with_help(
                        ErrorCode::GenericTypeError,
                        format!("`{}` is a static field, not a function", fqn),
                        range,
                        Some(
                            "read it as a value or assign with `Class::field = expr`"
                                .to_string(),
                        ),
                    );
                }
                // Parser may emit Call(QualifiedAccess) for module paths;
                // Class::static_method stays Construct, but accept Call too.
                if let Some(ty) = self.try_infer_static_method_call(
                    owner,
                    member,
                    &parser::ast::EnumConstructPayload::Tuple(
                        args.as_deref().unwrap_or(&[]).to_vec(),
                    ),
                    range.clone(),
                    id,
                ) {
                    return ty;
                }
                if self.has_method(owner, member) {
                    return self.error_with_help(
                        ErrorCode::GenericTypeError,
                        format!(
                            "`{}` is an instance method; call it on a value (`obj.{}(...)`)",
                            fqn, member
                        ),
                        range,
                        Some(format!(
                            "or declare `static fn {}` to call it as `{}`",
                            member, fqn
                        )),
                    );
                }
                fqn
            }
            _ => {
                return self.error(
                    ErrorCode::UnknownFunction,
                    "Invalid call target".to_string(),
                    range,
                );
            }
        };

        let raw_args = args.as_deref().unwrap_or(&[]);
        let has_named = raw_args
            .iter()
            .any(|a| matches!(a.1.as_ref(), Expression::NamedArg(..)));

        if ident == "len" {
            if has_named {
                let (tys, reordered) =
                    self.infer_and_reorder_call_args("len", raw_args, &range);
                return self.infer_len_call_from_tys(&tys, &reordered, id, range);
            }
            return self.infer_len_call(args.as_deref(), id, range);
        }
        if let Some(kind) = self.string_fn_for_call(&ident) {
            let arg_slice = if has_named {
                let (_, reordered) =
                    self.infer_and_reorder_call_args(&ident, raw_args, &range);
                return match kind {
                    StringBuiltin::Format => {
                        self.infer_string_format_call(reordered.as_slice(), range)
                    }
                    StringBuiltin::FromBytes | StringBuiltin::ToBytes => {
                        if kind == StringBuiltin::FromBytes
                            && !self.enums.contains_key(common::BUILTIN_IO_ERROR_ENUM)
                        {
                            self.register_builtin_io_error();
                        }
                        let fun_ty = self.instantiate_ty(&Self::string_fn_scheme(kind));
                        let flat_args = self.flatten_spread_call_args(reordered.as_slice());
                        let arg_tys: Vec<Ty> = flat_args
                            .iter()
                            .map(|arg| self.infer_call_arg(arg))
                            .collect();
                        self.apply_function(
                            Some(&ident),
                            &fun_ty,
                            &arg_tys,
                            if flat_args.is_empty() {
                                None
                            } else {
                                Some(&flat_args)
                            },
                            id,
                            range,
                        )
                    }
                };
            } else {
                args.as_deref().unwrap_or(&[])
            };
            return match kind {
                StringBuiltin::Format => self.infer_string_format_call(arg_slice, range),
                StringBuiltin::FromBytes | StringBuiltin::ToBytes => {
                    if kind == StringBuiltin::FromBytes
                        && !self.enums.contains_key(common::BUILTIN_IO_ERROR_ENUM)
                    {
                        self.register_builtin_io_error();
                    }
                    let fun_ty = self.instantiate_ty(&Self::string_fn_scheme(kind));
                    let flat_args = self.flatten_spread_call_args(arg_slice);
                    let arg_tys: Vec<Ty> = flat_args
                        .iter()
                        .map(|arg| self.infer_call_arg(arg))
                        .collect();
                    self.apply_function(
                        Some(&ident),
                        &fun_ty,
                        &arg_tys,
                        if flat_args.is_empty() {
                            None
                        } else {
                            Some(&flat_args)
                        },
                        id,
                        range,
                    )
                }
            };
        }
        // `assert` from `prelude::test` (auto-imported or via `use`).
        if let Some(kind) = self.prelude_fn_in_scope(&ident) {
            if has_named {
                let (_, reordered) =
                    self.infer_and_reorder_call_args(&ident, raw_args, &range);
                return match kind {
                    PreludeFn::Assert => self.infer_assert(&reordered, range),
                    PreludeFn::BlockOn => self.infer_block_on(&reordered, range),
                    PreludeFn::Dot => self.infer_dot(&reordered, id, range),
                    PreludeFn::MatMul => self.infer_matmul(&reordered, id, range),
                    PreludeFn::Cross => self.infer_cross(&reordered, id, range),
                    PreludeFn::Matrix => self.infer_matrix_ctor(&reordered, id, range),
                    PreludeFn::Ord => self.infer_ord(&reordered, range),
                    PreludeFn::Char => self.infer_char(&reordered, range),
                    PreludeFn::Sin
                    | PreludeFn::Cos
                    | PreludeFn::Tan
                    | PreludeFn::Sqrt
                    | PreludeFn::Floor
                    | PreludeFn::Ceil
                    | PreludeFn::Exp
                    | PreludeFn::Ln
                    | PreludeFn::Pow => self.infer_math(kind, &reordered, range),
                };
            }
            let arg_slice = args.as_deref().unwrap_or(&[]);
            return match kind {
                PreludeFn::Assert => self.infer_assert(arg_slice, range),
                PreludeFn::BlockOn => self.infer_block_on(arg_slice, range),
                PreludeFn::Dot => self.infer_dot(arg_slice, id, range),
                PreludeFn::MatMul => self.infer_matmul(arg_slice, id, range),
                PreludeFn::Cross => self.infer_cross(arg_slice, id, range),
                PreludeFn::Matrix => self.infer_matrix_ctor(arg_slice, id, range),
                PreludeFn::Ord => self.infer_ord(arg_slice, range),
                PreludeFn::Char => self.infer_char(arg_slice, range),
                PreludeFn::Sin
                | PreludeFn::Cos
                | PreludeFn::Tan
                | PreludeFn::Sqrt
                | PreludeFn::Floor
                | PreludeFn::Ceil
                | PreludeFn::Exp
                | PreludeFn::Ln
                | PreludeFn::Pow => self.infer_math(kind, arg_slice, range),
            };
        }
        // `dload` / `declare` / `invoke` after `use ffi::{…}`.
        if let Some(kind) = self.ffi_fn_in_scope(&ident) {
            if has_named {
                let (_, reordered) =
                    self.infer_and_reorder_call_args(&ident, raw_args, &range);
                return match kind {
                    FfiBuiltin::Dload => self.infer_ffi_dload(&reordered, range),
                    FfiBuiltin::Declare => self.infer_ffi_declare(&reordered, range),
                    FfiBuiltin::Invoke => self.infer_ffi_invoke(&reordered, range),
                };
            }
            let arg_slice = args.as_deref().unwrap_or(&[]);
            return match kind {
                FfiBuiltin::Dload => self.infer_ffi_dload(arg_slice, range),
                FfiBuiltin::Declare => self.infer_ffi_declare(arg_slice, range),
                FfiBuiltin::Invoke => self.infer_ffi_invoke(arg_slice, range),
            };
        }
        if matches!(ident.as_str(), "dload" | "declare" | "invoke") {
            return self.error_with_help(
                ErrorCode::UnknownValue,
                format!("Cannot find value `{}` in this scope", ident),
                range,
                Some(
                    "import it with `use ffi::{dload, declare, invoke}`".to_string(),
                ),
            );
        }

        // ── Overload-dispatch: select by argc + argument types ─────
        // Must happen before `has_named` and `fn_has_rest` branches so
        // the correct candidate's param_names / is_rest are used.
        if self.is_overloaded(&ident) {
            let argc = raw_args.len();
            // Preliminary arg types for same-arity disambiguation.
            // Named args contribute their value type in source order;
            // reordering happens after the candidate is chosen.
            let prelim_tys: Vec<Ty> = raw_args
                .iter()
                .map(|a| {
                    let value = match a.1.as_ref() {
                        Expression::NamedArg(_, v) => v,
                        _ => a,
                    };
                    let ty = self.infer(value);
                    apply_ty_prune(&self.subst, &ty)
                })
                .collect();
            let candidate_opt = self
                .select_overload_for_args(&ident, argc, &prelim_tys);
            match candidate_opt {
                OverloadSelect::NoMatch => {
                    // No candidate accepts this arity/types — emit a
                    // "no overload" error listing the available arities.
                    let available: Vec<String> = self
                        .overload_candidates(&ident)
                        .map(|cs| {
                            cs.iter().map(|c| Self::overload_sig_label(c)).collect()
                        })
                        .unwrap_or_default();
                    return self.error_with_help(
                        ErrorCode::WrongArity,
                        format!(
                            "No overload of `{}` accepts {} argument{}",
                            ident,
                            argc,
                            if argc == 1 { "" } else { "s" }
                        ),
                        range,
                        Some(format!("available overloads: {}", available.join(", "))),
                    );
                }
                OverloadSelect::Ambiguous => {
                    return self.error_with_help(
                        ErrorCode::AmbiguousOverload,
                        format!(
                            "Ambiguous overload: call to `{}` matches multiple candidates",
                            ident
                        ),
                        range,
                        Some(self.ambiguous_overload_help(&ident)),
                    );
                }
                OverloadSelect::Selected(candidate) => {
                    let candidate = candidate.clone();
                    // Record the selection for codegen.
                    self.selected_overloads_by_span.insert(
                        (range.start, range.end),
                        (candidate.fixed_arity, candidate.is_rest, candidate.id),
                    );
                    let (fun_ty, fresh_constraints, fresh_mapping, original_scheme) = {
                        let (fun_ty, constraints, mapping) =
                            self.instantiate_scheme_mapped(&candidate.scheme);
                        (fun_ty, constraints, mapping, Some(candidate.scheme.clone()))
                    };
                    let (arg_tys, ordered_args) = self
                        .infer_and_reorder_call_args_with_candidate(
                            &ident, &candidate, raw_args, &range,
                        );
                    let result = self.apply_function(
                        Some(&ident),
                        &fun_ty,
                        &arg_tys,
                        if ordered_args.is_empty() {
                            None
                        } else {
                            Some(&ordered_args)
                        },
                        id,
                        range.clone(),
                    );
                    if !fresh_constraints.is_empty() {
                        self.discharge_constraints(id, &fresh_constraints, &range);
                        if let Some(scheme) = original_scheme.as_ref() {
                            self.pin_assoc_after_discharge(
                                "",
                                &fresh_constraints,
                                Some(scheme),
                                &fresh_mapping,
                                &range,
                            );
                        }
                    }
                    return result;
                }
            }
        }

        // Named call-site args: skip trait UFCS and resolve an ordinary
        // function (partial application is allowed — residual Fun is OK).
        if has_named {
            let scheme = self.lookup_fn_scheme(&ident);
            let (fun_ty, fresh_constraints, fresh_mapping, original_scheme) = match scheme {
                Some(s) => {
                    let (fun_ty, constraints, mapping) = self.instantiate_scheme_mapped(&s);
                    (fun_ty, constraints, mapping, Some(s))
                }
                None => {
                    return self.error(
                        ErrorCode::UnknownFunction,
                        format!("Cannot find function `{}`", ident),
                        range,
                    );
                }
            };
            let (arg_tys, ordered_args) =
                self.infer_and_reorder_call_args(&ident, raw_args, &range);
            let result = if let Some(filled) = self
                .partial_filled_tys_by_span
                .get(&(range.start, range.end))
                .cloned()
            {
                let mask = self
                    .partial_fills_by_span
                    .get(&(range.start, range.end))
                    .copied()
                    .unwrap_or(0);
                let n = mask.count_ones();
                if !Self::is_prefix_fill_mask(mask, n) {
                    self.apply_partial_with_mask(&fun_ty, &filled, &range)
                } else {
                    self.apply_function(
                        Some(&ident),
                        &fun_ty,
                        &arg_tys,
                        if ordered_args.is_empty() {
                            None
                        } else {
                            Some(&ordered_args)
                        },
                        id,
                        range.clone(),
                    )
                }
            } else {
                self.apply_function(
                    Some(&ident),
                    &fun_ty,
                    &arg_tys,
                    if ordered_args.is_empty() {
                        None
                    } else {
                        Some(&ordered_args)
                    },
                    id,
                    range.clone(),
                )
            };
            if !fresh_constraints.is_empty() {
                self.discharge_constraints(id, &fresh_constraints, &range);
                if let Some(scheme) = original_scheme.as_ref() {
                    self.pin_assoc_after_discharge(
                        "",
                        &fresh_constraints,
                        Some(scheme),
                        &fresh_mapping,
                        &range,
                    );
                }
            }
            // Named under-apply is now allowed (residual Fun is returned
            // for partial application). Error only on unknown/duplicate
            // named args (handled inside infer_and_reorder_call_args).
            return result;
        }

        // Rest-parameter calls pack trailing args; skip UFCS trait
        // resolution so we don't double-infer (NodeId alignment).
        if self.fn_has_rest(&ident) {
            let scheme = self.lookup_fn_scheme(&ident);
            let (fun_ty, fresh_constraints, fresh_mapping, original_scheme) = match scheme {
                Some(s) => {
                    let (fun_ty, constraints, mapping) = self.instantiate_scheme_mapped(&s);
                    (fun_ty, constraints, mapping, Some(s))
                }
                None => {
                    return self.error(
                        ErrorCode::UnknownFunction,
                        format!("Cannot find function `{}`", ident),
                        range,
                    );
                }
            };
            let (call_arg_tys, ordered_args) =
                self.infer_and_reorder_call_args(&ident, raw_args, &range);
            let result = self.apply_function(
                Some(&ident),
                &fun_ty,
                &call_arg_tys,
                if ordered_args.is_empty() {
                    None
                } else {
                    Some(&ordered_args)
                },
                id,
                range.clone(),
            );
            if !fresh_constraints.is_empty() {
                self.discharge_constraints(id, &fresh_constraints, &range);
                if let Some(scheme) = original_scheme.as_ref() {
                    self.pin_assoc_after_discharge(
                        "",
                        &fresh_constraints,
                        Some(scheme),
                        &fresh_mapping,
                        &range,
                    );
                }
            }
            return result;
        }

        // Bare/UFCS trait method call: `method(x)`.
        // Resolve it before ordinary environment lookup because class
        // methods are selected by the active bound, not by a global FQN.
        let flat_args = self.flatten_spread_call_args(args.as_deref().unwrap_or(&[]));
        let arg_tys: Vec<Ty> = flat_args
            .iter()
            .map(|arg| self.infer_call_arg(arg))
            .collect();
        if let Some(Ty::Existential { class }) =
            arg_tys.first().map(|ty| apply_ty_prune(&self.subst, ty))
            && let Some((owner, method_slot, scheme)) =
                self.existential_method_candidate(&class, &ident)
        {
            let hint = ExistentialMethodCall {
                method_slot,
                arity: arg_tys.len(),
                has_receiver: false,
            };
            if let Some(call_id) = id {
                self.existential_method_calls.insert(call_id, hint.clone());
            }
            self.existential_method_calls_by_span
                .insert((range.start, range.end), hint);
            return self.apply_existential_method(
                &owner,
                &ident,
                &scheme,
                &arg_tys,
                args.as_deref(),
                id,
                range,
            );
        }
        let candidates = self.bound_method_candidates(&ident, None);
        if !candidates.is_empty() {
            let receiver_var = arg_tys.first().and_then(|ty| {
                Self::constraint_var_of_ty(&apply_ty_prune(&self.subst, ty))
            });
            let candidates = receiver_var
                .map(|v| self.bound_method_candidates(&ident, Some(v)))
                .unwrap_or_else(|| self.bound_method_candidates(&ident, None));
            if let Some((dict_index, dict_class, class, method_slot, scheme)) =
                self.select_bound_method(candidates, &ident, &range)
            {
                self.bind_matching_abstract_constraints(receiver_var, &dict_class);
                let (fun_ty, constraints, mapping) =
                    self.instantiate_scheme_mapped(&scheme);
                if let Some(call_id) = id {
                    self.bound_method_calls.insert(
                        call_id,
                        BoundMethodCall {
                            dict_index,
                            method_slot,
                            arity: arg_tys.len(),
                            has_receiver: false,
                        },
                    );
                }
                self.bound_method_calls_by_span.insert(
                    (range.start, range.end),
                    BoundMethodCall {
                        dict_index,
                        method_slot,
                        arity: arg_tys.len(),
                        has_receiver: false,
                    },
                );
                let result = self.apply_function(
                    Some(&format!("{}::{}", class, ident)),
                    &fun_ty,
                    &arg_tys,
                    args.as_deref(),
                    id,
                    range.clone(),
                );
                if !constraints.is_empty() {
                    self.discharge_constraints(id, &constraints, &range);
                    self.pin_assoc_after_discharge(
                        &class,
                        &constraints,
                        Some(&scheme),
                        &mapping,
                        &range,
                    );
                }
                return result;
            }
        }

        let scheme = self.lookup_fn_scheme(&ident);
        let (fun_ty, fresh_constraints, fresh_mapping, original_scheme) = match scheme {
            Some(s) => {
                let (fun_ty, constraints, mapping) = self.instantiate_scheme_mapped(&s);
                (fun_ty, constraints, mapping, Some(s))
            }
            None => {
                return self.error(
                    ErrorCode::UnknownFunction,
                    format!("Cannot find function `{}`", ident),
                    range,
                );
            }
        };

        // C-varargs extern: accept `>= nfixed` args; only unify the fixed prefix.
        if self.extern_variadic.contains(ident.as_str()) {
            return self.apply_extern_variadic_call(
                &ident,
                &fun_ty,
                &arg_tys,
                args.as_deref(),
                range,
            );
        }

        if self.thread_fn_in_scope(&ident) == Some(ThreadBuiltin::Spawn) {
            return self.infer_thread_spawn_call(&arg_tys, args.as_deref(), range);
        }

        self.maybe_record_ffi_param_invoke_flow_for_call(&ident, &flat_args);

        let result = self.apply_function(
            Some(&ident),
            &fun_ty,
            &arg_tys,
            if flat_args.is_empty() {
                None
            } else {
                Some(&flat_args)
            },
            id,
            range.clone(),
        );
        // Discharge trait constraints from the instantiated scheme.
        // This verifies that each concrete type argument satisfies the
        // required bound, or propagates the constraint if the caller is
        // itself generic with the same bound.
        if !fresh_constraints.is_empty() {
            self.discharge_constraints(id, &fresh_constraints, &range);
            if let Some(scheme) = original_scheme.as_ref() {
                self.pin_assoc_after_discharge(
                    "",
                    &fresh_constraints,
                    Some(scheme),
                    &fresh_mapping,
                    &range,
                );
            }
        }
        result
    }

    // ============================================================
    //  Type cache and lookup
    // ============================================================

    /// Look up the inferred type of a node by [`NodeId`].
    pub fn lookup_at(&self, id: NodeId) -> Option<Ty> {
        self.cache.get(&id).map(|t| apply_ty_prune(&self.subst, t))
    }

    /// Look up the original HM result by source span without re-running inference.
    pub fn lookup_for_codegen_span(&self, start: usize, end: usize) -> Option<Ty> {
        self.codegen_types_by_span
            .get(&(start, end))
            .map(|ty| apply_ty_prune(&self.subst, ty))
    }

    /// Concrete trait instances selected while discharging a call's bounds.
    pub fn call_dicts_at(&self, id: NodeId) -> Option<&[InstanceDef]> {
        self.call_site_dicts.get(&id).map(Vec::as_slice)
    }

    /// Span fallback for [`call_dicts_at`] when NodeIds are misaligned.
    pub fn call_dicts_for_span(&self, start: usize, end: usize) -> Option<&[InstanceDef]> {
        self.call_site_dicts_by_span
            .get(&(start, end))
            .map(Vec::as_slice)
    }

    fn record_call_site_dict(
        &mut self,
        call_id: Option<NodeId>,
        range: &Range<usize>,
        instance: InstanceDef,
    ) {
        if let Some(call_id) = call_id {
            self.call_site_dicts
                .entry(call_id)
                .or_default()
                .push(instance.clone());
        }
        self.call_site_dicts_by_span
            .entry((range.start, range.end))
            .or_default()
            .push(instance);
    }

    pub fn forwarded_dicts_at(&self, id: NodeId) -> Option<&[usize]> {
        self.call_site_forward_dicts.get(&id).map(Vec::as_slice)
    }

    pub fn forwarded_dicts_for_span(&self, start: usize, end: usize) -> Option<&[usize]> {
        self.call_site_forward_dicts_by_span
            .get(&(start, end))
            .map(Vec::as_slice)
    }

    pub fn bound_method_call_at(&self, id: NodeId) -> Option<&BoundMethodCall> {
        self.bound_method_calls.get(&id)
    }

    pub fn bound_method_call_for_span(&self, start: usize, end: usize) -> Option<&BoundMethodCall> {
        self.bound_method_calls_by_span.get(&(start, end))
    }

    pub fn bound_operator_call_at(&self, id: NodeId) -> Option<&BoundOperatorCall> {
        self.bound_operator_calls.get(&id)
    }

    pub fn bound_operator_call_for_span(
        &self,
        start: usize,
        end: usize,
    ) -> Option<&BoundOperatorCall> {
        self.bound_operator_calls_by_span.get(&(start, end))
    }

    pub fn bound_display_call_at(&self, id: NodeId) -> Option<&BoundDisplayCall> {
        self.bound_display_calls.get(&id)
    }

    pub fn bound_display_call_for_span(
        &self,
        start: usize,
        end: usize,
    ) -> Option<&BoundDisplayCall> {
        self.bound_display_calls_by_span.get(&(start, end))
    }

    pub fn existential_pack_for_span(&self, start: usize, end: usize) -> Option<&ExistentialPack> {
        self.existential_packs_by_span.get(&(start, end))
    }

    pub fn existential_method_call_at(&self, id: NodeId) -> Option<&ExistentialMethodCall> {
        self.existential_method_calls.get(&id)
    }

    pub fn existential_method_call_for_span(
        &self,
        start: usize,
        end: usize,
    ) -> Option<&ExistentialMethodCall> {
        self.existential_method_calls_by_span.get(&(start, end))
    }

    pub fn typeclass_method_scheme(&self, class: &str, method: &str) -> Option<&Scheme> {
        self.typeclass_method_schemes
            .get(&(class.to_string(), method.to_string()))
    }

    /// All call-site dicts (for debugging / testing).
    #[cfg(test)]
    pub fn all_call_site_dicts(&self) -> &HashMap<NodeId, Vec<InstanceDef>> {
        &self.call_site_dicts
    }

    /// Borrow the pre-walk [`IdTable`].
    pub fn id_table(&self) -> &IdTable {
        &self.ids
    }

    /// Number of nodes that have a cached inferred type. Useful in
    /// tests that want to assert the cache is fully populated.
    #[cfg(test)]
    pub(crate) fn cache_len(&self) -> usize {
        self.cache.len()
    }

    // ============================================================
    //  Helpers
    // ============================================================

    /// File-level imports (virtual + disk) are globals, not closure captures.
    /// Snapshot their schemes, drop them from `uncaptured`, then rebind after
    /// `take_and_isolate`.
    fn snapshot_file_level_imports(
        &mut self,
        uncaptured: &mut HashSet<String>,
        range: Range<usize>,
    ) -> Vec<(String, Scheme)> {
        let mut rebinds = Vec::new();
        let virtual_callables: Vec<(String, BuiltinExport)> = self
            .scope_bindings
            .iter()
            .filter(|(_, e)| {
                matches!(
                    e,
                    BuiltinExport::FfiFn { .. }
                        | BuiltinExport::IoFn { .. }
                        | BuiltinExport::StringFn { .. }
                        | BuiltinExport::ThreadFn { .. }
                        | BuiltinExport::GcFn { .. }
                        | BuiltinExport::HostFn { .. }
                )
            })
            .map(|(k, e)| (k.clone(), e.clone()))
            .collect();
        for (local, export) in virtual_callables {
            uncaptured.remove(&local);
            if let Some(scheme) = self.virtual_callable_scheme(export, range.clone()) {
                rebinds.push((local, scheme));
            }
        }
        for name in self.disk_imports.clone() {
            uncaptured.remove(&name);
            if let Some(scheme) = self.env.lookup(&name).cloned() {
                rebinds.push((name, scheme));
            }
        }
        rebinds
    }

    fn rebind_file_level_imports(&mut self, rebinds: Vec<(String, Scheme)>) {
        for (local, scheme) in rebinds {
            self.env.insert_top(local, scheme);
        }
    }

    /// Process a [`Expression::Fragment`] (the body of a `let x = expr`
    /// declaration, or the body of a Block that consists entirely of
    /// declarations).
    ///
    /// Each `Variable` / `Constant` declaration binds in the
    /// environment. If the immediate next sibling is a value-producing
    /// expression (anything that's not another declaration, comment, or
    /// `use`), it is treated as the initializer and unified with the
    /// declared type.
    ///
    /// No new frame is pushed: `let` bindings live in the surrounding
    /// scope (block, function body, or program) so they're visible to
    /// subsequent statements.
    fn infer_fragment(&mut self, children: &[Output]) -> Ty {
        let mut last_ty = unit_ty();
        let mut i = 0;
        while i < children.len() {
            let child = &children[i];
            match child.1.as_ref() {
                Expression::Variable(name, ty_opt) => {
                    let var_ty = match ty_opt {
                        Some(ann) => self.parse_type_name(ann),
                        None => Ty::Var(self.counter.fresh()),
                    };
                    self.env
                        .insert_top(name.to_string(), Scheme::mono(var_ty.clone()));
                    self.record_codegen_var_type(name.to_string(), var_ty.clone());
                    last_ty = unit_ty();

                    // Try to consume the next sibling as the initializer.
                    if i + 1 < children.len() {
                        let next = &children[i + 1];
                        if !is_declaration_like(next) {
                            if is_yield_expression(next) {
                                self.yield_receives_used = true;
                            }
                            if let Expression::Identifier(source) =
                                unwrap_expr_wrappers(next).1.as_ref()
                                && let Some(source_scheme) = self.env.lookup(source).cloned()
                                && !source_scheme.bounds.is_empty()
                            {
                                let _ = self.infer(next);
                                self.env.insert_top(name.to_string(), source_scheme.clone());
                                self.record_codegen_var_type(
                                    name.to_string(),
                                    source_scheme.ty.clone(),
                                );
                                self.maybe_record_polyfn_binding(
                                    (child.0.start, child.0.end),
                                    &source_scheme.ty,
                                );
                                last_ty = source_scheme.ty;
                                i += 2;
                                continue;
                            }
                            // Annotated lets push an expected type so ground
                            // trait calls (`x.into()`) can pin conversion targets.
                            let prev_expected = self.current_expected.take();
                            if ty_opt.is_some() {
                                self.current_expected = Some(var_ty.clone());
                            }
                            let val_ty = self.infer(next);
                            self.current_expected = prev_expected;
                            self.coerce_or_unify(
                                &var_ty,
                                &val_ty,
                                Some(next),
                                &child.0.into_range(),
                                "let binding",
                            );
                            // Keep the side-table in sync with the unified type
                            // so Access codegen sees Record/enum types, not the
                            // pre-unify fresh variable.
                            let pruned = apply_ty_prune(&self.subst, &var_ty);
                            self.record_codegen_var_type(name.to_string(), pruned.clone());
                            self.maybe_record_polyfn_binding((child.0.start, child.0.end), &pruned);
                            // `let id = declare(...)` may wrap Declare/Call in
                            // ExprStatement/Statement/`?` — unwrap before matching.
                            let init = unwrap_expr_wrappers(next);
                            let init = match init.1.as_ref() {
                                Expression::Try(inner) => unwrap_expr_wrappers(inner),
                                _ => init,
                            };
                            self.maybe_record_ffi_declare_for_let_init(name, init);
                            i += 1;
                        }
                    }
                }
                Expression::Constant(name, ty_opt) => {
                    let var_ty = match ty_opt {
                        Some(ann) => self.parse_type_name(ann),
                        None => Ty::Var(self.counter.fresh()),
                    };
                    if let Expression::Identifier(n) = name.1.as_ref() {
                        self.env
                            .insert_top(n.to_string(), Scheme::mono(var_ty.clone()));
                        self.record_codegen_var_type(n.to_string(), var_ty.clone());
                        self.insert_const_binding(n.to_string());
                        if i + 1 < children.len() {
                            let next = &children[i + 1];
                            if !is_declaration_like(next) {
                                let prev_expected = self.current_expected.take();
                                if ty_opt.is_some() {
                                    self.current_expected = Some(var_ty.clone());
                                }
                                let val_ty = self.infer(next);
                                self.current_expected = prev_expected;
                                self.coerce_or_unify(
                                    &var_ty,
                                    &val_ty,
                                    Some(next),
                                    &child.0.into_range(),
                                    "const binding",
                                );
                                let pruned = apply_ty_prune(&self.subst, &var_ty);
                                self.record_codegen_var_type(n.to_string(), pruned.clone());
                                self.warn_shallow_const_binding(n, &pruned, child.0.into_range());
                                if let Some(cv) = crate::typechecking::const_eval::eval_const(next, &|name| {
                                    self.const_fold_env.get(name).copied()
                                }) {
                                    self.const_fold_env.insert(n.to_string(), cv);
                                }
                                self.maybe_record_polyfn_binding(
                                    (child.0.start, child.0.end),
                                    &pruned,
                                );
                                i += 1;
                            }
                        }
                    }
                    last_ty = unit_ty();
                }
                _ => {
                    last_ty = self.infer(child);
                }
            }
            i += 1;
        }
        last_ty
    }

    fn record_bound_operator(
        &mut self,
        id: Option<NodeId>,
        range: &Range<usize>,
        var: TyVarId,
        class: &str,
        method: &str,
    ) {
        let Some((dict_index, dict_class)) = self.user_dict_index_and_class(var, class) else {
            return;
        };
        let Some(class_def) = self.generics.typeclass(&dict_class) else {
            return;
        };
        let Some(method_slot) = class_def
            .flattened_methods(&self.generics)
            .iter()
            .position(|(_, candidate)| candidate.name == method)
        else {
            return;
        };
        let hint = BoundOperatorCall {
            dict_index,
            method_slot,
        };
        if let Some(id) = id {
            self.bound_operator_calls.insert(id, hint.clone());
        }
        self.bound_operator_calls_by_span
            .insert((range.start, range.end), hint);
    }

    fn record_bound_display(&mut self, range: &Range<usize>, var: TyVarId) {
        let Some((dict_index, dict_class)) = self.user_dict_index_and_class(var, "Show") else {
            return;
        };
        let Some(class_def) = self.generics.typeclass(&dict_class) else {
            return;
        };
        let Some(method_slot) = class_def
            .flattened_methods(&self.generics)
            .iter()
            .position(|(_, candidate)| candidate.name == "show")
        else {
            return;
        };
        let hint = BoundDisplayCall {
            dict_index,
            method_slot,
        };
        self.bound_display_calls_by_span
            .insert((range.start, range.end), hint);
    }

    /// Peel `Constructor` / structural `Sum` down to a nominal head so
    /// `Color::Red == Color::Blue` unifies as `Color` rather than as two
    /// incompatible constructor refinements.
    fn peel_comparison_ty(ty: &Ty) -> Ty {
        match ty {
            Ty::Constructor { owner, .. } => Self::peel_comparison_ty(owner),
            Ty::Sum { name, .. } => Ty::Con(name.clone()),
            other => other.clone(),
        }
    }

    fn infer_comparison(
        &mut self,
        lhs: &Output,
        rhs: &Output,
        id: Option<NodeId>,
        range: Range<usize>,
        class: &str,
        method: &str,
    ) -> Ty {
        let lt = Self::peel_comparison_ty(&self.infer(lhs));
        let rt = Self::peel_comparison_ty(&self.infer(rhs));
        let lt = apply_ty_prune(&self.subst, &lt);
        let rt = apply_ty_prune(&self.subst, &rt);
        // `b == "/"` — single-byte string literal compares as `byte`.
        if Self::is_byte_ty(&lt) && Self::is_string_ty(&rt) {
            if self.try_mark_string_literal_as_byte(rhs) {
                return boolean();
            }
        } else if Self::is_byte_ty(&rt) && Self::is_string_ty(&lt) {
            if self.try_mark_string_literal_as_byte(lhs) {
                return boolean();
            }
        }
        let unified = self.unify(&lt, &rt, &range, "comparison operands");
        if let Ty::Var(var) = apply_ty_prune(&self.subst, &unified) {
            if self.user_dict_index(var, class).is_none() {
                self.bind_matching_abstract_constraints(Some(var), class);
            }
            if self.user_dict_index(var, class).is_some() {
                self.record_bound_operator(id, &range, var, class, method);
            } else if self
                .type_params_in_scope
                .iter()
                .any(|frame| frame.values().any(|&candidate| candidate == var))
            {
                self.messages.push(Message::error(
                    ErrorCode::GenericTypeError,
                    format!("Cannot compare generic type without bound `{}`", class),
                    range,
                ));
            }
        }
        boolean()
    }

    /// Lazy `start..end` / `start..=end` — bounds unify to `T` with `T: Ord`.
    fn infer_range(
        &mut self,
        start: &Output,
        end: &Output,
        inclusive: bool,
        range: Range<usize>,
    ) -> Ty {
        let st = self.infer(start);
        let et = self.infer(end);
        let sp = apply_ty_prune(&self.subst, &st);
        let ep = apply_ty_prune(&self.subst, &et);
        // Coerce int literals under a `byte` peer (same as annotated `byte` lets).
        if Self::is_byte_ty(&sp) && Self::byte_literal_coercion(end).is_ok() {
            let _ = self.unify(
                &et,
                &crate::typechecking::ty::byte(),
                &end.0.into_range(),
                "range end",
            );
        } else if Self::is_byte_ty(&ep) && Self::byte_literal_coercion(start).is_ok() {
            let _ = self.unify(
                &st,
                &crate::typechecking::ty::byte(),
                &start.0.into_range(),
                "range start",
            );
        }
        let elem = self.unify(&st, &et, &range, "range bounds");
        if !self.require_ord_for_range(&elem, &range) {
            return if inclusive {
                range_inclusive_ty(Ty::Var(self.counter.fresh()))
            } else {
                range_ty(Ty::Var(self.counter.fresh()))
            };
        }
        let elem = apply_ty_prune(&self.subst, &elem);
        if inclusive {
            range_inclusive_ty(elem)
        } else {
            range_ty(elem)
        }
    }

    /// Ensure range element type satisfies `Ord` (instance or active bound).
    /// Unconstrained free type variables default to `int` (has `Ord`).
    fn require_ord_for_range(&mut self, elem: &Ty, range: &Range<usize>) -> bool {
        let pruned = apply_ty_prune(&self.subst, elem);
        match &pruned {
            Ty::Var(v) => {
                if self.user_dict_index(*v, "Ord").is_none() {
                    self.bind_matching_abstract_constraints(Some(*v), "Ord");
                }
                if self.user_dict_index(*v, "Ord").is_some() {
                    return true;
                }
                let in_scope = self
                    .type_params_in_scope
                    .iter()
                    .any(|frame| frame.values().any(|&id| id == *v));
                if in_scope {
                    let _ = self.error_with_help(
                        ErrorCode::GenericTypeError,
                        "range bounds require bound `Ord`".to_string(),
                        range.clone(),
                        Some(
                            "add a `T: Ord` bound, or use a concrete ordered type (`int`, `byte`, `float`)"
                                .to_string(),
                        ),
                    );
                    return false;
                }
                // Free var — pin to int (literals / unconstrained inference).
                let _ = self.unify(elem, &int(), range, "range element type");
                true
            }
            other => {
                if self.generics.has_instance("Ord", other) {
                    true
                } else {
                    let _ = self.error_with_help(
                        ErrorCode::GenericTypeError,
                        format!("range bounds require `T: Ord`, found `{}`", other),
                        range.clone(),
                        Some(
                            "both ends of `a..b` / `a..=b` must share a type that implements `Ord`"
                                .to_string(),
                        ),
                    );
                    false
                }
            }
        }
    }

    fn infer_arith(
        &mut self,
        lhs: &Output,
        rhs: &Output,
        id: Option<NodeId>,
        range: Range<usize>,
        op: &str,
    ) -> Ty {
        let lt = self.infer(lhs);
        let rt = self.infer(rhs);
        if op == "+" {
            let lp = apply_ty_prune(&self.subst, &lt);
            let rp = apply_ty_prune(&self.subst, &rt);
            let left_string = is_string_ty(&lp);
            let right_string = is_string_ty(&rp);
            if left_string && right_string {
                return string();
            }
            if left_string || right_string {
                return self.unify(&lt, &rt, &range, "operands of `+`");
            }
        }

        let lp = apply_ty_prune(&self.subst, &lt);
        let rp = apply_ty_prune(&self.subst, &rt);
        // Nominal `Matrix` — `*` is matmul (Mul), `+`/`-` are element-wise.
        // Must run before aggregate zip so nested-array data inside Matrix
        // is not treated as Hadamard product.
        if crate::typechecking::aggregate_arith::is_matrix_ty(&lp) || crate::typechecking::aggregate_arith::is_matrix_ty(&rp) {
            return self.infer_matrix_arith(lp, rp, id, range, op);
        }
        if matches!(&lp, Ty::Tuple(_) | Ty::Array { .. })
            || matches!(&rp, Ty::Tuple(_) | Ty::Array { .. })
        {
            return self.infer_aggregate_arith(lp, rp, id, range, op);
        }

        let result = self.unify(&lt, &rt, &range, &format!("operands of `{}`", op));
        // Open type variables need the matching op trait (`Add` for `+`, …).
        // `T: Num` also covers these via superclass implication.
        let pruned = apply_ty_prune(&self.subst, &result);
        if let Ty::Var(v) = &pruned {
            let (class, method) = match op {
                "+" => ("Add", "add"),
                "-" => ("Sub", "sub"),
                "*" => ("Mul", "mul"),
                "/" => ("Div", "div"),
                _ => {
                    let in_scope = self
                        .type_params_in_scope
                        .iter()
                        .any(|frame| frame.values().any(|&id| id == *v));
                    if in_scope {
                        self.messages.push(Message::error(
                            ErrorCode::GenericTypeError,
                            format!(
                                "Operator `{}` is not available through an arithmetic trait",
                                op
                            ),
                            range,
                        ));
                    }
                    return result;
                }
            };
            if self.user_dict_index(*v, class).is_none() {
                self.bind_matching_abstract_constraints(Some(*v), class);
            }
            if self.user_dict_index(*v, class).is_some() {
                self.record_bound_operator(id, &range, *v, class, method);
            } else {
                let in_scope = self
                    .type_params_in_scope
                    .iter()
                    .any(|frame| frame.values().any(|&id| id == *v));
                if in_scope {
                    self.messages.push(Message::error(
                        ErrorCode::GenericTypeError,
                        format!(
                            "Cannot apply `{}` to value of generic type without bound `{}`",
                            op, class
                        ),
                        range,
                    ));
                }
            }
        }
        result
    }

    /// Element-wise / broadcast arithmetic on homogeneous tuples and arrays.
    fn infer_aggregate_arith(
        &mut self,
        lp: Ty,
        rp: Ty,
        id: Option<NodeId>,
        range: Range<usize>,
        op: &str,
    ) -> Ty {
        use crate::typechecking::aggregate_arith::*;

        let Some(agg_op) = AggregateOp::from_str(op) else {
            return self.error_with_help(
                ErrorCode::GenericTypeError,
                format!("operator `{}` is not supported on aggregates", op),
                range,
                None,
            );
        };

        // Homogeneity for tuples.
        if let Ty::Tuple(elems) = &lp
            && elems.len() > 1
            && self.homogeneous_types(elems, &range, "tuple").is_none()
        {
            return Ty::Var(self.counter.fresh());
        }
        if let Ty::Tuple(elems) = &rp
            && elems.len() > 1
            && self.homogeneous_types(elems, &range, "tuple").is_none()
        {
            return Ty::Var(self.counter.fresh());
        }

        let left = classify_arith(&apply_ty_prune(&self.subst, &lp));
        let right = classify_arith(&apply_ty_prune(&self.subst, &rp));

        // Resolve shapes.
        let resolved: Result<(ArithShape, ZipMode), String> = match (&left, &right) {
            (
                ArithShape::Tuple {
                    elem: e1,
                    arity: n1,
                },
                ArithShape::Tuple {
                    elem: e2,
                    arity: n2,
                },
            ) => {
                if n1 != n2 {
                    Err(format!(
                        "cannot zip tuples of length {} and {} with `{}`",
                        n1, n2, op
                    ))
                } else {
                    let _ = self.unify(e1, e2, &range, &format!("element types of `{}`", op));
                    let elem = apply_ty_prune(&self.subst, e1);
                    Ok((ArithShape::Tuple { elem, arity: *n1 }, ZipMode::Zip))
                }
            }
            (
                ArithShape::Array {
                    elem: e1,
                    length: ArrayLength::Static(n1),
                },
                ArithShape::Array {
                    elem: e2,
                    length: ArrayLength::Static(n2),
                },
            ) => {
                if n1 != n2 {
                    Err(format!(
                        "cannot zip arrays of length {} and {} with `{}`",
                        n1, n2, op
                    ))
                } else {
                    let _ = self.unify(e1, e2, &range, &format!("element types of `{}`", op));
                    let elem = apply_ty_prune(&self.subst, e1);
                    Ok((
                        ArithShape::Array {
                            elem,
                            length: ArrayLength::Static(*n1),
                        },
                        ZipMode::Zip,
                    ))
                }
            }
            (
                ArithShape::Array {
                    length: ArrayLength::Dynamic,
                    ..
                },
                ArithShape::Array { .. },
            )
            | (
                ArithShape::Array { .. },
                ArithShape::Array {
                    length: ArrayLength::Dynamic,
                    ..
                },
            ) => Err(format!(
                "cannot zip dynamic-length arrays with `{}`; use fixed-length `[T; N]` or broadcast a scalar",
                op
            )),
            (ArithShape::Tuple { elem, arity }, ArithShape::Scalar(s)) => {
                let _ = self.unify(elem, s, &range, &format!("operands of `{}`", op));
                let elem = apply_ty_prune(&self.subst, elem);
                Ok((
                    ArithShape::Tuple {
                        elem,
                        arity: *arity,
                    },
                    ZipMode::BroadcastRight,
                ))
            }
            (ArithShape::Scalar(s), ArithShape::Tuple { elem, arity }) => {
                let _ = self.unify(s, elem, &range, &format!("operands of `{}`", op));
                let elem = apply_ty_prune(&self.subst, elem);
                Ok((
                    ArithShape::Tuple {
                        elem,
                        arity: *arity,
                    },
                    ZipMode::BroadcastLeft,
                ))
            }
            (ArithShape::Array { elem, length }, ArithShape::Scalar(s)) => {
                let _ = self.unify(elem, s, &range, &format!("operands of `{}`", op));
                let elem = apply_ty_prune(&self.subst, elem);
                Ok((
                    ArithShape::Array {
                        elem,
                        length: *length,
                    },
                    ZipMode::BroadcastRight,
                ))
            }
            (ArithShape::Scalar(s), ArithShape::Array { elem, length }) => {
                let _ = self.unify(s, elem, &range, &format!("operands of `{}`", op));
                let elem = apply_ty_prune(&self.subst, elem);
                Ok((
                    ArithShape::Array {
                        elem,
                        length: *length,
                    },
                    ZipMode::BroadcastLeft,
                ))
            }
            (ArithShape::Tuple { .. }, ArithShape::Array { .. })
            | (ArithShape::Array { .. }, ArithShape::Tuple { .. }) => Err(format!(
                "cannot apply `{}` between a tuple and an array",
                op
            )),
            _ => Err(format!("cannot apply `{}` to these operand shapes", op)),
        };

        let (result_shape, mode) = match resolved {
            Ok(v) => v,
            Err(msg) => {
                return self.error_with_help(
                    ErrorCode::GenericTypeError,
                    msg,
                    range,
                    Some(
                        "element-wise arithmetic requires equal static lengths, or a scalar broadcast"
                            .to_string(),
                    ),
                );
            }
        };

        let elem = match &result_shape {
            ArithShape::Tuple { elem, .. } | ArithShape::Array { elem, .. } => elem.clone(),
            ArithShape::Scalar(e) => e.clone(),
        };
        let elem = apply_ty_prune(&self.subst, &elem);
        if !is_numeric_elem(&elem) {
            return self.error_with_help(
                ErrorCode::GenericTypeError,
                format!(
                    "element type `{}` does not support `{}`",
                    elem, op
                ),
                range,
                Some("element-wise arithmetic requires numeric elements (`int`, `float`, or a `Num`-bounded type parameter)".to_string()),
            );
        }

        // Open element → bind op trait (Tier B).
        // `%` / `**` intentionally skip trait binding — same as scalar
        // `infer_arith` (no Mod/Pow trait dictionaries yet). Concrete
        // `int`/`float` elements still typecheck via `is_numeric_elem`.
        if let Ty::Var(v) = &elem {
            let (class, method) = match op {
                "+" => ("Add", "add"),
                "-" => ("Sub", "sub"),
                "*" => ("Mul", "mul"),
                "/" => ("Div", "div"),
                _ => ("", ""),
            };
            if !class.is_empty() {
                if self.user_dict_index(*v, class).is_none() {
                    self.bind_matching_abstract_constraints(Some(*v), class);
                }
                if self.user_dict_index(*v, class).is_some() {
                    self.record_bound_operator(id, &range, *v, class, method);
                }
            }
        }

        let float = elem_is_float(&elem);
        let kind = match (&result_shape, mode) {
            (ArithShape::Tuple { arity, .. }, ZipMode::Zip) => AggregateArithKind::ZipTuple {
                arity: *arity,
                elem_is_float: float,
            },
            (
                ArithShape::Array {
                    length: ArrayLength::Static(n),
                    ..
                },
                ZipMode::Zip,
            ) => AggregateArithKind::ZipArray {
                length: *n,
                elem_is_float: float,
            },
            (ArithShape::Tuple { arity, .. }, ZipMode::BroadcastRight) => {
                AggregateArithKind::BroadcastTuple {
                    arity: *arity,
                    scalar_on: ScalarSide::Right,
                    elem_is_float: float,
                }
            }
            (ArithShape::Tuple { arity, .. }, ZipMode::BroadcastLeft) => {
                AggregateArithKind::BroadcastTuple {
                    arity: *arity,
                    scalar_on: ScalarSide::Left,
                    elem_is_float: float,
                }
            }
            (ArithShape::Array { length, .. }, ZipMode::BroadcastRight) => {
                AggregateArithKind::BroadcastArray {
                    length: match length {
                        ArrayLength::Static(n) => Some(*n),
                        ArrayLength::Dynamic => None,
                    },
                    scalar_on: ScalarSide::Right,
                    elem_is_float: float,
                }
            }
            (ArithShape::Array { length, .. }, ZipMode::BroadcastLeft) => {
                AggregateArithKind::BroadcastArray {
                    length: match length {
                        ArrayLength::Static(n) => Some(*n),
                        ArrayLength::Dynamic => None,
                    },
                    scalar_on: ScalarSide::Left,
                    elem_is_float: float,
                }
            }
            _ => {
                return self.error_with_help(
                    ErrorCode::GenericTypeError,
                    format!("internal error resolving aggregate `{}`", op),
                    range,
                    None,
                );
            }
        };

        let info = AggregateArithInfo { kind, op: agg_op };
        if let Some(id) = id {
            self.aggregate_arith.insert(id, info.clone());
        }
        self.aggregate_arith_by_span
            .insert((range.start, range.end), info);

        result_ty_for(&result_shape)
    }

    fn infer_aggregate_neg(&mut self, inner: Ty, id: Option<NodeId>, range: Range<usize>) -> Ty {
        use crate::typechecking::aggregate_arith::*;

        let pruned = apply_ty_prune(&self.subst, &inner);
        if let Ty::Tuple(elems) = &pruned {
            if elems.is_empty() {
                return self.error_with_help(
                    ErrorCode::GenericTypeError,
                    "cannot negate an empty tuple".to_string(),
                    range,
                    None,
                );
            }
            if elems.len() > 1 && self.homogeneous_types(elems, &range, "tuple").is_none() {
                return Ty::Var(self.counter.fresh());
            }
            let elem = apply_ty_prune(&self.subst, &elems[0]);
            if !is_numeric_elem(&elem) {
                return self.error_with_help(
                    ErrorCode::GenericTypeError,
                    format!("element type `{}` does not support unary `-`", elem),
                    range,
                    None,
                );
            }
            let info = AggregateArithInfo {
                kind: AggregateArithKind::NegTuple {
                    arity: elems.len(),
                    elem_is_float: elem_is_float(&elem),
                },
                op: AggregateOp::Neg,
            };
            if let Some(id) = id {
                self.aggregate_arith.insert(id, info.clone());
            }
            self.aggregate_arith_by_span
                .insert((range.start, range.end), info);
            return Ty::Tuple(vec![elem; elems.len()]);
        }
        if let Ty::Array { element, length } = &pruned {
            let elem = apply_ty_prune(&self.subst, element);
            if !is_numeric_elem(&elem) {
                return self.error_with_help(
                    ErrorCode::GenericTypeError,
                    format!("element type `{}` does not support unary `-`", elem),
                    range,
                    None,
                );
            }
            let info = AggregateArithInfo {
                kind: AggregateArithKind::NegArray {
                    length: match length {
                        ArrayLength::Static(n) => Some(*n),
                        ArrayLength::Dynamic => None,
                    },
                    elem_is_float: elem_is_float(&elem),
                },
                op: AggregateOp::Neg,
            };
            if let Some(id) = id {
                self.aggregate_arith.insert(id, info.clone());
            }
            self.aggregate_arith_by_span
                .insert((range.start, range.end), info);
            return Ty::Array {
                element: Box::new(elem),
                length: *length,
            };
        }
        // Scalar path — leave to caller.
        pruned
    }

    pub fn aggregate_arith_at(
        &self,
        id: NodeId,
    ) -> Option<&crate::typechecking::aggregate_arith::AggregateArithInfo> {
        self.aggregate_arith.get(&id)
    }

    pub fn aggregate_arith_for_span(
        &self,
        start: usize,
        end: usize,
    ) -> Option<&crate::typechecking::aggregate_arith::AggregateArithInfo> {
        self.aggregate_arith_by_span.get(&(start, end))
    }

    pub fn linear_algebra_at(
        &self,
        id: NodeId,
    ) -> Option<&crate::typechecking::aggregate_arith::LinearAlgebraInfo> {
        self.linear_algebra.get(&id)
    }

    pub fn linear_algebra_for_span(
        &self,
        start: usize,
        end: usize,
    ) -> Option<&crate::typechecking::aggregate_arith::LinearAlgebraInfo> {
        self.linear_algebra_by_span.get(&(start, end))
    }

    fn record_linear_algebra(
        &mut self,
        id: Option<NodeId>,
        range: &Range<usize>,
        info: crate::typechecking::aggregate_arith::LinearAlgebraInfo,
    ) {
        self.warn_packed_la_dim_limit(&info.kind, range);
        if let Some(id) = id {
            self.linear_algebra.insert(id, info.clone());
        }
        self.linear_algebra_by_span
            .insert((range.start, range.end), info);
    }

    /// Warn when static dims exceed Approach A packed-opcode packing
    /// (`u8` for matrix dims, `u16` for `dot` length). Codegen still falls
    /// back to scalar unroll — this is advisory, not a hard error.
    fn warn_packed_la_dim_limit(
        &mut self,
        kind: &crate::typechecking::aggregate_arith::LinearAlgebraKind,
        range: &Range<usize>,
    ) {
        use crate::typechecking::aggregate_arith::LinearAlgebraKind;
        let (what, limit, dims): (&str, usize, String) = match kind {
            LinearAlgebraKind::Dot { length, .. } if *length > u16::MAX as usize => {
                ("dot length", u16::MAX as usize, format!("{length}"))
            }
            LinearAlgebraKind::MatMul { m, k, n, .. }
                if *m > u8::MAX as usize || *k > u8::MAX as usize || *n > u8::MAX as usize =>
            {
                (
                    "matrix multiply dimensions",
                    u8::MAX as usize,
                    format!("{m}×{k}×{n}"),
                )
            }
            LinearAlgebraKind::MatrixZip { m, n, .. }
                if *m > u8::MAX as usize || *n > u8::MAX as usize =>
            {
                ("matrix dimensions", u8::MAX as usize, format!("{m}×{n}"))
            }
            LinearAlgebraKind::MatrixNeg { m, n, .. }
                if *m > u8::MAX as usize || *n > u8::MAX as usize =>
            {
                ("matrix dimensions", u8::MAX as usize, format!("{m}×{n}"))
            }
            _ => return,
        };
        let mut msg = Message::warn(
            ErrorCode::GenericTypeError,
            format!("{what} `{dims}` exceed the packed kernel meta limit ({limit})",),
            range.clone(),
        );
        msg.with_help(
            "codegen will fall back to scalar unroll; prefer smaller static shapes for the HostInvoke packed kernel"
                .to_string(),
        );
        self.messages.push(msg);
    }

    fn compound_op_name(op: parser::ast::AssignOp) -> &'static str {
        use parser::ast::AssignOp;
        match op {
            AssignOp::Add => "+",
            AssignOp::Sub => "-",
            AssignOp::Mul => "*",
            AssignOp::Div => "/",
            AssignOp::Mod => "%",
            AssignOp::Pow => "**",
            AssignOp::Shl => "<<",
            AssignOp::Shr => ">>",
            AssignOp::BitAnd => "&",
            AssignOp::BitOr => "|",
            AssignOp::BitXor => "^",
        }
    }

    fn infer_mutable_lvalue(&mut self, target: &Output, range: Range<usize>) -> Ty {
        match target.1.as_ref() {
            Expression::Identifier(n) => {
                let ident = n.to_string();
                let module_fqn = self.qualify_module_name(&ident);
                if self.static_slots.contains_key(&module_fqn) {
                    if self.is_static_const_fqn(&module_fqn) {
                        let mut msg = Message::error(
                            ErrorCode::InvalidAssignment,
                            format!("Cannot assign to constant `{}`", ident),
                            range,
                        );
                        msg.with_help(
                            "`static const` bindings are immutable after initialization"
                                .to_string(),
                        );
                        self.messages.push(msg);
                    }
                    return self
                        .static_slot_types
                        .get(&module_fqn)
                        .cloned()
                        .unwrap_or_else(|| Ty::Var(self.counter.fresh()));
                }
                match self.env.lookup(&ident).cloned() {
                    Some(s) => {
                        let ty = self.instantiate_ty(&s);
                        if self.is_const_binding(&ident) {
                            let mut msg = Message::error(
                                ErrorCode::InvalidAssignment,
                                format!("Cannot assign to constant `{}`", ident),
                                range,
                            );
                            msg.with_help(
                                "constants are immutable after their initializer".to_string(),
                            );
                            self.messages.push(msg);
                        }
                        ty
                    }
                    None => self.error_with_help(
                        ErrorCode::UndeclaredAssignment,
                        format!("Cannot assign to undeclared variable `{}`", ident),
                        range,
                        Some(format!("try declaring it first with `let {};`", ident)),
                    ),
                }
            }
            Expression::Access(receiver, field) => {
                let is_self_field = matches!(
                    receiver.1.as_ref(),
                    Expression::Identifier(n) if *n == "self" && self.impl_owner.is_some()
                );
                let receiver_ty = self.infer(receiver);
                if !is_self_field {
                    self.check_readonly_external_mutation(&receiver_ty, range.clone());
                }
                let resolved = apply_ty_prune(&self.subst, &receiver_ty);
                let class_name = self.class_owner_from_ty(&resolved);
                if let Some(class) = class_name.as_ref()
                    && self.is_const_class_field(class, field)
                {
                    let _ = self.error_with_help(
                        ErrorCode::InvalidAssignment,
                        format!("Cannot assign to const field `{}` of `{}`", field, class),
                        range.clone(),
                        Some("const fields are immutable after construction".to_string()),
                    );
                }
                match &resolved {
                    Ty::Record { fields } => match fields.iter().find(|(n, _)| n == field) {
                        Some((_, fty)) => fty.clone(),
                        None => {
                            let known: Vec<&str> =
                                fields.iter().map(|(n, _)| n.as_str()).collect();
                            self.error_with_help(
                                ErrorCode::UnknownField, format!("Cannot find field `{}` on record", field),
                                range,
                                Some(format!("the record has fields: {}", known.join(", "))),
                            )
                        }
                    },
                    Ty::App(head, args)
                        if matches!(head.as_ref(), Ty::Con(n) if self.classes.contains_key(n)) =>
                    {
                        let name = match head.as_ref() {
                            Ty::Con(n) => n.clone(),
                            _ => unreachable!(),
                        };
                        self.access_class_field(&name, field, args, range)
                    }
                    Ty::Con(name) if self.classes.contains_key(name) => {
                        self.access_class_field(name, field, &[], range)
                    }
                    _ => self.error_with_help(
                        ErrorCode::InvalidAssignment, "Invalid assignment target".to_string(),
                        range,
                        Some(
                            "only variables, dict fields, class fields, and array elements may be assigned"
                                .to_string(),
                        ),
                    ),
                }
            }
            Expression::QualifiedAccess { owner, member } => {
                let fqn = self.class_member_fqn(owner, member);
                if let Some(ty) = self.static_slot_types.get(&fqn).cloned() {
                    if self.is_static_const_fqn(&fqn) {
                        let _ = self.error_with_help(
                            ErrorCode::InvalidAssignment,
                            format!("Cannot assign to constant `{}`", fqn),
                            range,
                            Some(
                                "`static const` bindings are immutable after initialization"
                                    .to_string(),
                            ),
                        );
                    }
                    return ty;
                }
                self.error_with_help(
                    ErrorCode::UndeclaredAssignment,
                    format!("Cannot assign to undeclared static `{}`", fqn),
                    range,
                    None,
                )
            }
            Expression::Construct {
                enum_name,
                variant_name,
                fields,
            } if matches!(fields, parser::ast::EnumConstructPayload::Unit) => {
                let fqn = self.class_member_fqn(enum_name, variant_name);
                if let Some(ty) = self.static_slot_types.get(&fqn).cloned() {
                    if self.is_static_const_fqn(&fqn) {
                        let _ = self.error_with_help(
                            ErrorCode::InvalidAssignment,
                            format!("Cannot assign to constant `{}`", fqn),
                            range,
                            Some(
                                "`static const` bindings are immutable after initialization"
                                    .to_string(),
                            ),
                        );
                    }
                    return ty;
                }
                self.error_with_help(
                    ErrorCode::UndeclaredAssignment,
                    format!("Cannot assign to undeclared static `{}`", fqn),
                    range,
                    None,
                )
            }
            Expression::Index(arr, idx) => {
                let target_ty = self.infer(arr);
                let target_ty = apply_ty_prune(&self.subst, &target_ty);
                let Some(idx) = idx else {
                    return self.error_with_help(
                        ErrorCode::InvalidAssignment,
                        "empty index `arr[]` is not a valid assignment target".to_string(),
                        range,
                        Some("use `vec.push(value)` to append to a `Vec`".to_string()),
                    );
                };
                self.check_readonly_external_mutation(&target_ty, range.clone());
                let index_ty = self.infer(idx);
                let _ = unify_with(&self.subst, &apply_ty_prune(&self.subst, &index_ty), &int());
                match &target_ty {
                    Ty::Array { element, length } => {
                        if let ArrayLength::Static(n) = length {
                            if let Expression::Integer(i) = idx.1.as_ref() {
                                if *i < 0 || (*i as usize) >= *n {
                                    let _ = self.error_with_help(
                                        ErrorCode::IndexOutOfBounds,
                                        format!(
                                            "array index {} out of bounds for array of length {}",
                                            i, n
                                        ),
                                        range.clone(),
                                        None,
                                    );
                                }
                            }
                        }
                        (**element).clone()
                    }
                    other if vec_element_ty(other).is_some() => {
                        vec_element_ty(other).expect("checked").clone()
                    }
                    Ty::Tuple(_) => self.error_with_help(
                        ErrorCode::InvalidAssignment,
                        "Invalid assignment target".to_string(),
                        range,
                        Some("tuple elements are immutable".to_string()),
                    ),
                    _ => self.error_with_help(
                        ErrorCode::InvalidAssignment,
                        "Invalid assignment target".to_string(),
                        range,
                        Some(
                            "only array or `Vec` elements may be indexed for assignment"
                                .to_string(),
                        ),
                    ),
                }
            }
            _ => self.error_with_help(
                ErrorCode::InvalidAssignment,
                "Invalid assignment target".to_string(),
                range,
                Some(
                    "the left-hand side must be a variable, dict field, or array index".to_string(),
                ),
            ),
        }
    }

    fn infer_if(&mut self, branches: &[Output]) -> Ty {
        let mut result_ty = Ty::Var(self.counter.fresh());
        let mut first = true;
        for branch in branches {
            if let Expression::Branch(cond, body) = branch.1.as_ref() {
                if let Some(c) = cond {
                    let ct = self.infer(c);
                    self.unify(&ct, &boolean(), &c.0.into_range(), "if condition");
                }
                let body_ty = self.infer(body);
                if first {
                    result_ty = body_ty;
                    first = false;
                } else {
                    result_ty =
                        self.join_ty(&result_ty, &body_ty, &body.0.into_range(), "if branch");
                }
            }
        }
        result_ty
    }

    fn infer_list(&mut self, elements: &[Output], _range: Range<usize>) -> Ty {
        if elements.is_empty() {
            return list(Ty::Var(self.counter.fresh()));
        }
        let first_ty = self.infer(&elements[0]);
        for elem in &elements[1..] {
            let t = self.infer(elem);
            self.unify(&first_ty, &t, &elem.0.into_range(), "list element");
        }
        list(first_ty)
    }

    /// `len(x)` — structural length for arrays/tuples/dicts/strings, otherwise
    /// `Length::len` typeclass dispatch (active bound or concrete instance).
    fn infer_len_call(
        &mut self,
        args: Option<&[Output]>,
        id: Option<NodeId>,
        range: Range<usize>,
    ) -> Ty {
        let args = args.unwrap_or(&[]);
        if args.len() != 1 {
            for arg in args {
                let _ = self.infer(arg);
            }
            return self.error_with_help(
                ErrorCode::ConstructorArity,
                format!("len expects 1 argument, got {}", args.len()),
                range,
                Some("use `len(value)`".to_string()),
            );
        }

        let target_ty = self.infer(&args[0]);
        self.finish_len_call(target_ty, args, id, range)
    }

    /// Named-arg path: arguments were already inferred during reorder.
    fn infer_len_call_from_tys(
        &mut self,
        tys: &[Ty],
        args: &[Output],
        id: Option<NodeId>,
        range: Range<usize>,
    ) -> Ty {
        if tys.len() != 1 || args.len() != 1 {
            return self.error_with_help(
                ErrorCode::ConstructorArity,
                format!("len expects 1 argument, got {}", args.len()),
                range,
                Some("use `len(value: …)`".to_string()),
            );
        }
        self.finish_len_call(tys[0].clone(), args, id, range)
    }

    fn finish_len_call(
        &mut self,
        target_ty: Ty,
        args: &[Output],
        id: Option<NodeId>,
        range: Range<usize>,
    ) -> Ty {
        let resolved = apply_ty_prune(&self.subst, &target_ty);
        if Self::is_structural_len_ty(&resolved) {
            return int();
        }
        // Open vars: prefer Length (custom / generic) over forcing an array.
        if matches!(&resolved, Ty::Var(_)) {
            return self.infer_length_method_call(&target_ty, args, id, range);
        }
        self.infer_length_method_call(&target_ty, args, id, range)
    }

    fn is_structural_len_ty(ty: &Ty) -> bool {
        match strip_readonly(ty) {
            Ty::Array { .. } | Ty::Tuple(_) | Ty::Record { .. } => true,
            Ty::Con(name) if name == "string" || name == crate::typechecking::ty::STRING => true,
            // `Vec<T>` shares the array runtime carrier — same ArrayLen path.
            other if vec_element_ty(other).is_some() => true,
            _ => false,
        }
    }

    /// Codegen helper: same structural `len` shapes as typechecking.
    pub fn is_structural_len_ty_for_codegen(ty: &Ty) -> bool {
        Self::is_structural_len_ty(ty)
    }

    /// Resolve `len(x)` via the `Length` typeclass (bound or ground instance).
    fn infer_length_method_call(
        &mut self,
        target_ty: &Ty,
        args: &[Output],
        id: Option<NodeId>,
        range: Range<usize>,
    ) -> Ty {
        let resolved = apply_ty_prune(&self.subst, target_ty);
        let receiver_var = Self::constraint_var_of_ty(&resolved);
        let candidates = self.bound_method_candidates("len", receiver_var);
        if !candidates.is_empty() {
            if let Some((dict_index, dict_class, class, method_slot, scheme)) =
                self.select_bound_method(candidates, "len", &range)
            {
                self.bind_matching_abstract_constraints(receiver_var, &dict_class);
                let (fun_ty, constraints, mapping) = self.instantiate_scheme_mapped(&scheme);
                let hint = BoundMethodCall {
                    dict_index,
                    method_slot,
                    arity: 1,
                    has_receiver: false,
                };
                if let Some(call_id) = id {
                    self.bound_method_calls.insert(call_id, hint.clone());
                }
                self.bound_method_calls_by_span
                    .insert((range.start, range.end), hint);
                let arg_tys = vec![target_ty.clone()];
                let result = self.apply_function(
                    Some(&format!("{}::len", class)),
                    &fun_ty,
                    &arg_tys,
                    Some(args),
                    id,
                    range.clone(),
                );
                if !constraints.is_empty() {
                    self.discharge_constraints(id, &constraints, &range);
                    self.pin_assoc_after_discharge(
                        &class,
                        &constraints,
                        Some(&scheme),
                        &mapping,
                        &range,
                    );
                }
                return result;
            }
        }

        let Some(scheme) = self
            .typeclass_method_schemes
            .get(&("Length".to_string(), "len".to_string()))
            .cloned()
        else {
            return self.error_with_help(
                ErrorCode::GenericTypeError,
                format!(
                    "`len` requires a `Length` instance, found `{}`",
                    crate::typechecking::pretty::format_ty_for_diag(&self.subst, &resolved)
                ),
                args[0].0.into_range(),
                Some("implement `impl Length for T { fn len(T x) -> int { ... } }`".to_string()),
            );
        };

        let (fun_ty, constraints, mapping) = self.instantiate_scheme_mapped(&scheme);
        let arg_tys = vec![target_ty.clone()];
        let result = self.apply_function(
            Some("Length::len"),
            &fun_ty,
            &arg_tys,
            Some(args),
            id,
            range.clone(),
        );
        if !constraints.is_empty() {
            self.discharge_constraints(id, &constraints, &range);
            self.pin_assoc_after_discharge("Length", &constraints, Some(&scheme), &mapping, &range);
        }
        // If discharge failed (no instance), a diagnostic is already recorded.
        apply_ty_prune(&self.subst, &result)
    }

    /// `assert(bool)` / `assert(bool, string)` → `Result<(), string>`.
    ///
    /// Does not enter result-mode; callers use `?` / `match` / `raise`.
    fn infer_assert(&mut self, args: &[Output], range: Range<usize>) -> Ty {
        if !(1..=2).contains(&args.len()) {
            for arg in args {
                let _ = self.infer(arg);
            }
            return self.error_with_help(
                ErrorCode::ConstructorArity,
                format!("assert expects 1 or 2 arguments, got {}", args.len()),
                range,
                Some("use `assert(cond)` or `assert(cond, message)`".to_string()),
            );
        }

        let cond_ty = self.infer(&args[0]);
        self.unify(
            &cond_ty,
            &boolean(),
            &args[0].0.into_range(),
            "assert condition",
        );
        if let Some(msg) = args.get(1) {
            let msg_ty = self.infer(msg);
            self.unify(&msg_ty, &string(), &msg.0.into_range(), "assert message");
        }
        result_app_ty(unit_ty(), string())
    }

    /// `block_on(coro)` — drive `coroutine<Y>` / `coroutine<Y, unit>` to completion → `Y`.
    fn infer_block_on(&mut self, args: &[Output], range: Range<usize>) -> Ty {
        if args.len() != 1 {
            for arg in args {
                let _ = self.infer(arg);
            }
            return self.error_with_help(
                ErrorCode::ConstructorArity,
                format!("block_on expects 1 argument, got {}", args.len()),
                range,
                Some("use `block_on(async_fn_call())`".to_string()),
            );
        }
        let handle_ty = self.infer(&args[0]);
        let y_var = Ty::Var(self.counter.fresh());
        let s_var = Ty::Var(self.counter.fresh());
        let coro_ty = self.coroutine_type(y_var.clone(), s_var.clone());
        self.unify(
            &handle_ty,
            &coro_ty,
            &args[0].0.into_range(),
            "block_on argument",
        );
        // Prefer unit send; free send vars unify with unit.
        let unit = unit_ty();
        let _ = self.unify(&s_var, &unit, &range, "block_on coroutine send type");
        apply_ty_prune(&self.subst, &y_var)
    }

    fn primitive_cast_name(ty: &Ty) -> Option<&'static str> {
        use crate::typechecking::ty::{BOOL, BYTE, FLOAT, INT};
        match ty {
            Ty::Con(name) => match name.as_str() {
                INT => Some("int"),
                FLOAT => Some("float"),
                BYTE => Some("byte"),
                BOOL => Some("bool"),
                _ => None,
            },
            _ => None,
        }
    }

    fn primitive_cast_allowed(from: &str, to: &str) -> bool {
        matches!(
            (from, to),
            ("int", "float")
                | ("float", "int")
                | ("int", "byte")
                | ("byte", "int")
                | ("int", "bool")
                | ("bool", "int")
        )
    }

    /// `ord(string) -> Result<byte, string>`.
    fn infer_ord(&mut self, args: &[Output], range: Range<usize>) -> Ty {
        use crate::typechecking::ty::byte;
        if args.len() != 1 {
            for arg in args {
                let _ = self.infer(arg);
            }
            return self.error_with_help(
                ErrorCode::ConstructorArity,
                format!("ord expects 1 argument, got {}", args.len()),
                range,
                Some("use `ord(s)` with a single-character string".to_string()),
            );
        }
        let s_ty = self.infer(&args[0]);
        self.unify(&s_ty, &string(), &args[0].0.into_range(), "ord argument");
        result_app_ty(byte(), string())
    }

    /// `char(byte) -> Result<string, string>`.
    fn infer_char(&mut self, args: &[Output], range: Range<usize>) -> Ty {
        use crate::typechecking::ty::byte;
        if args.len() != 1 {
            for arg in args {
                let _ = self.infer(arg);
            }
            return self.error_with_help(
                ErrorCode::ConstructorArity,
                format!("char expects 1 argument, got {}", args.len()),
                range,
                Some("use `char(b)` with a `byte` value".to_string()),
            );
        }
        let b_ty = self.infer(&args[0]);
        self.unify(&b_ty, &byte(), &args[0].0.into_range(), "char argument");
        result_app_ty(string(), string())
    }

    /// Scalar `prelude::math` natives: unary `float -> float`, or `pow(float, float) -> float`.
    fn infer_math(&mut self, kind: PreludeFn, args: &[Output], range: Range<usize>) -> Ty {
        let expected_arity = if kind == PreludeFn::Pow { 2 } else { 1 };
        if args.len() != expected_arity {
            for arg in args {
                let _ = self.infer(arg);
            }
            return self.error_with_help(
                ErrorCode::ConstructorArity,
                format!(
                    "{} expects {} argument{}, got {}",
                    kind.as_str(),
                    expected_arity,
                    if expected_arity == 1 { "" } else { "s" },
                    args.len()
                ),
                range,
                Some(format!(
                    "use `{}({})` with float arguments",
                    kind.as_str(),
                    if expected_arity == 1 { "x" } else { "base, exponent" }
                )),
            );
        }

        for arg in args {
            let arg_ty = self.infer(arg);
            self.unify(
                &arg_ty,
                &float(),
                &arg.0.into_range(),
                &format!("{} argument", kind.as_str()),
            );
        }
        float()
    }

    /// `dot(a, b)` — equal-length homogeneous numeric vectors → scalar.
    fn infer_dot(&mut self, args: &[Output], id: Option<NodeId>, range: Range<usize>) -> Ty {
        use crate::typechecking::aggregate_arith::{
            LinearAlgebraInfo, LinearAlgebraKind, classify_vector, elem_is_float, is_numeric_elem,
        };

        if args.len() != 2 {
            for arg in args {
                let _ = self.infer(arg);
            }
            return self.error_with_help(
                ErrorCode::ConstructorArity,
                format!("dot expects 2 arguments, got {}", args.len()),
                range,
                Some("use `dot(a, b)` with equal-length numeric vectors".to_string()),
            );
        }

        let lt = self.infer(&args[0]);
        let rt = self.infer(&args[1]);
        let lp = apply_ty_prune(&self.subst, &lt);
        let rp = apply_ty_prune(&self.subst, &rt);

        let Some((le, ln, left_is_tuple)) = classify_vector(&lp) else {
            return self.error_with_help(
                ErrorCode::GenericTypeError,
                format!("dot expects a homogeneous numeric vector, found `{}`", lp),
                args[0].0.into_range(),
                Some("use a tuple `(T,…,T)` or fixed-length `[T; N]`".to_string()),
            );
        };
        let Some((re, rn, right_is_tuple)) = classify_vector(&rp) else {
            return self.error_with_help(
                ErrorCode::GenericTypeError,
                format!("dot expects a homogeneous numeric vector, found `{}`", rp),
                args[1].0.into_range(),
                Some("use a tuple `(T,…,T)` or fixed-length `[T; N]`".to_string()),
            );
        };
        if left_is_tuple != right_is_tuple {
            return self.error_with_help(
                ErrorCode::GenericTypeError,
                "cannot mix tuple and array operands in `dot`".to_string(),
                range,
                Some(
                    "both arguments must be tuples or both must be fixed-length arrays".to_string(),
                ),
            );
        }
        if ln != rn {
            return self.error_with_help(
                ErrorCode::GenericTypeError,
                format!("cannot take `dot` of vectors of length {} and {}", ln, rn),
                range,
                Some("vector lengths must be equal and known at compile time".to_string()),
            );
        }
        let _ = self.unify(&le, &re, &range, "element types of `dot`");
        let elem = apply_ty_prune(&self.subst, &le);
        if !is_numeric_elem(&elem) {
            return self.error_with_help(
                ErrorCode::GenericTypeError,
                format!("element type `{}` does not support `dot`", elem),
                range,
                Some("dot requires numeric elements (`int`, `float`, or `byte`)".to_string()),
            );
        }

        self.record_linear_algebra(
            id,
            &range,
            LinearAlgebraInfo {
                kind: LinearAlgebraKind::Dot {
                    length: ln,
                    left_is_tuple,
                    elem_is_float: elem_is_float(&elem),
                },
            },
        );
        elem
    }

    /// `cross(a, b)` — length-3 vectors → length-3 vector.
    fn infer_cross(&mut self, args: &[Output], id: Option<NodeId>, range: Range<usize>) -> Ty {
        use crate::typechecking::aggregate_arith::{
            LinearAlgebraInfo, LinearAlgebraKind, classify_vector, elem_is_float, is_numeric_elem,
        };

        if args.len() != 2 {
            for arg in args {
                let _ = self.infer(arg);
            }
            return self.error_with_help(
                ErrorCode::ConstructorArity,
                format!("cross expects 2 arguments, got {}", args.len()),
                range,
                Some("use `cross(a, b)` with length-3 numeric vectors".to_string()),
            );
        }

        let lt = self.infer(&args[0]);
        let rt = self.infer(&args[1]);
        let lp = apply_ty_prune(&self.subst, &lt);
        let rp = apply_ty_prune(&self.subst, &rt);

        let Some((le, ln, left_is_tuple)) = classify_vector(&lp) else {
            return self.error_with_help(
                ErrorCode::GenericTypeError,
                format!("cross expects a length-3 numeric vector, found `{}`", lp),
                args[0].0.into_range(),
                None,
            );
        };
        let Some((re, rn, right_is_tuple)) = classify_vector(&rp) else {
            return self.error_with_help(
                ErrorCode::GenericTypeError,
                format!("cross expects a length-3 numeric vector, found `{}`", rp),
                args[1].0.into_range(),
                None,
            );
        };
        if ln != 3 || rn != 3 {
            return self.error_with_help(
                ErrorCode::GenericTypeError,
                format!(
                    "`cross` requires length-3 vectors, found lengths {} and {}",
                    ln, rn
                ),
                range,
                None,
            );
        }
        if left_is_tuple != right_is_tuple {
            return self.error_with_help(
                ErrorCode::GenericTypeError,
                "cannot mix tuple and array operands in `cross`".to_string(),
                range,
                None,
            );
        }
        let _ = self.unify(&le, &re, &range, "element types of `cross`");
        let elem = apply_ty_prune(&self.subst, &le);
        if !is_numeric_elem(&elem) {
            return self.error_with_help(
                ErrorCode::GenericTypeError,
                format!("element type `{}` does not support `cross`", elem),
                range,
                None,
            );
        }

        self.record_linear_algebra(
            id,
            &range,
            LinearAlgebraInfo {
                kind: LinearAlgebraKind::Cross {
                    left_is_tuple,
                    elem_is_float: elem_is_float(&elem),
                },
            },
        );
        if left_is_tuple {
            Ty::Tuple(vec![elem.clone(), elem.clone(), elem])
        } else {
            Ty::Array {
                element: Box::new(elem),
                length: ArrayLength::Static(3),
            }
        }
    }

    /// `matmul(A, B)` — nested static matrices `(m×k) × (k×n) → (m×n)`.
    fn infer_matmul(&mut self, args: &[Output], id: Option<NodeId>, range: Range<usize>) -> Ty {
        use crate::typechecking::aggregate_arith::{
            LinearAlgebraInfo, LinearAlgebraKind, classify_matrix, elem_is_float, is_numeric_elem,
        };

        if args.len() != 2 {
            for arg in args {
                let _ = self.infer(arg);
            }
            return self.error_with_help(
                ErrorCode::ConstructorArity,
                format!("matmul expects 2 arguments, got {}", args.len()),
                range,
                Some(
                    "use `matmul(A, B)` with nested fixed-length matrices (`[[T; K]; M]` × `[[T; N]; K]`)"
                        .to_string(),
                ),
            );
        }

        let lt = self.infer(&args[0]);
        let rt = self.infer(&args[1]);
        let lp = apply_ty_prune(&self.subst, &lt);
        let rp = apply_ty_prune(&self.subst, &rt);

        let Some((le, m, k1, outer_is_tuple, row_is_tuple)) = classify_matrix(&lp) else {
            return self.error_with_help(
                ErrorCode::GenericTypeError,
                format!(
                    "matmul expects a nested fixed-length matrix, found `{}`",
                    lp
                ),
                args[0].0.into_range(),
                Some(
                    "use `[[T; K]; M]` (or a tuple of equal-length row tuples/arrays)".to_string(),
                ),
            );
        };
        let Some((re, k2, n, right_outer_tuple, right_row_tuple)) = classify_matrix(&rp) else {
            return self.error_with_help(
                ErrorCode::GenericTypeError,
                format!(
                    "matmul expects a nested fixed-length matrix, found `{}`",
                    rp
                ),
                args[1].0.into_range(),
                Some(
                    "use `[[T; N]; K]` (or a tuple of equal-length row tuples/arrays)".to_string(),
                ),
            );
        };
        if outer_is_tuple != right_outer_tuple || row_is_tuple != right_row_tuple {
            return self.error_with_help(
                ErrorCode::GenericTypeError,
                "matmul operands must use the same matrix container shape".to_string(),
                range,
                Some("both matrices must be array-of-arrays or both tuple-of-rows".to_string()),
            );
        }
        if k1 != k2 {
            return self.error_with_help(
                ErrorCode::GenericTypeError,
                format!(
                    "matmul inner dimensions mismatch: {}×{} and {}×{}",
                    m, k1, k2, n
                ),
                range,
                Some("left columns must equal right rows".to_string()),
            );
        }
        let _ = self.unify(&le, &re, &range, "element types of `matmul`");
        let elem = apply_ty_prune(&self.subst, &le);
        if !is_numeric_elem(&elem) {
            return self.error_with_help(
                ErrorCode::GenericTypeError,
                format!("element type `{}` does not support `matmul`", elem),
                range,
                None,
            );
        }

        self.record_linear_algebra(
            id,
            &range,
            LinearAlgebraInfo {
                kind: LinearAlgebraKind::MatMul {
                    m,
                    k: k1,
                    n,
                    outer_is_tuple,
                    row_is_tuple,
                    elem_is_float: elem_is_float(&elem),
                },
            },
        );

        let row_ty = if row_is_tuple {
            Ty::Tuple(vec![elem.clone(); n])
        } else {
            Ty::Array {
                element: Box::new(elem.clone()),
                length: ArrayLength::Static(n),
            }
        };
        if outer_is_tuple {
            Ty::Tuple(vec![row_ty; m])
        } else {
            Ty::Array {
                element: Box::new(row_ty),
                length: ArrayLength::Static(m),
            }
        }
    }

    /// `matrix(rows)` — wrap nested static matrix data as `Matrix<Data>`.
    fn infer_matrix_ctor(
        &mut self,
        args: &[Output],
        _id: Option<NodeId>,
        range: Range<usize>,
    ) -> Ty {
        use crate::typechecking::aggregate_arith::{classify_matrix, is_numeric_elem, wrap_matrix_ty};

        if args.len() != 1 {
            for arg in args {
                let _ = self.infer(arg);
            }
            return self.error_with_help(
                ErrorCode::ConstructorArity,
                format!("matrix expects 1 argument, got {}", args.len()),
                range,
                Some("use `matrix([[…], …])` with nested fixed-length rows".to_string()),
            );
        }

        let data_ty = self.infer(&args[0]);
        let pruned = apply_ty_prune(&self.subst, &data_ty);
        let Some((elem, _m, _n, _, _)) = classify_matrix(&pruned) else {
            return self.error_with_help(
                ErrorCode::GenericTypeError,
                format!(
                    "`matrix` expects a nested fixed-length matrix, found `{}`",
                    pruned
                ),
                args[0].0.into_range(),
                Some(
                    "use `[[T; N]; M]` (or a tuple of equal-length row tuples/arrays)".to_string(),
                ),
            );
        };
        if !is_numeric_elem(&elem) {
            return self.error_with_help(
                ErrorCode::GenericTypeError,
                format!("element type `{}` does not support `matrix`", elem),
                range,
                Some("matrix elements must be numeric (`int`, `float`, or `byte`)".to_string()),
            );
        }
        wrap_matrix_ty(pruned)
    }

    /// Arithmetic on nominal `Matrix` values.
    ///
    /// - `*` → matmul (Mul), recording [`LinearAlgebraInfo`]
    /// - `+` / `-` → element-wise zip of the nested data (Add / Sub)
    /// - unary handled via [`Self::infer_aggregate_neg`]
    /// - `/`, `%`, `**` rejected (Matrix is not `Num`)
    fn infer_matrix_arith(
        &mut self,
        lp: Ty,
        rp: Ty,
        id: Option<NodeId>,
        range: Range<usize>,
        op: &str,
    ) -> Ty {
        use crate::typechecking::aggregate_arith::{
            LinearAlgebraInfo, LinearAlgebraKind, classify_matrix, elem_is_float, is_numeric_elem,
            unwrap_matrix_ty, wrap_matrix_ty,
        };

        let Some(ld) = unwrap_matrix_ty(&lp) else {
            return self.error_with_help(
                ErrorCode::GenericTypeError,
                format!("cannot apply `{}` between `{}` and `{}`", op, lp, rp),
                range,
                Some("both operands of matrix arithmetic must be `Matrix` values".to_string()),
            );
        };
        let Some(rd) = unwrap_matrix_ty(&rp) else {
            return self.error_with_help(
                ErrorCode::GenericTypeError,
                format!("cannot apply `{}` between `{}` and `{}`", op, lp, rp),
                range,
                Some("both operands of matrix arithmetic must be `Matrix` values".to_string()),
            );
        };

        match op {
            "*" => {
                let Some((le, m, k1, outer_is_tuple, row_is_tuple)) = classify_matrix(ld) else {
                    return self.error_with_help(
                        ErrorCode::GenericTypeError,
                        format!("invalid matrix layout `{}`", ld),
                        range.clone(),
                        None,
                    );
                };
                let Some((re, k2, n, right_outer, right_row)) = classify_matrix(rd) else {
                    return self.error_with_help(
                        ErrorCode::GenericTypeError,
                        format!("invalid matrix layout `{}`", rd),
                        range.clone(),
                        None,
                    );
                };
                if outer_is_tuple != right_outer || row_is_tuple != right_row {
                    return self.error_with_help(
                        ErrorCode::GenericTypeError,
                        "matrix operands must use the same container shape".to_string(),
                        range,
                        Some(
                            "both matrices must be array-of-arrays or both tuple-of-rows"
                                .to_string(),
                        ),
                    );
                }
                if k1 != k2 {
                    return self.error_with_help(
                        ErrorCode::GenericTypeError,
                        format!(
                            "matrix multiply inner dimensions mismatch: {}×{} and {}×{}",
                            m, k1, k2, n
                        ),
                        range,
                        Some("left columns must equal right rows".to_string()),
                    );
                }
                let _ = self.unify(&le, &re, &range, "element types of matrix `*`");
                let elem = apply_ty_prune(&self.subst, &le);
                if !is_numeric_elem(&elem) {
                    return self.error_with_help(
                        ErrorCode::GenericTypeError,
                        format!("element type `{}` does not support matrix `*`", elem),
                        range,
                        None,
                    );
                }
                self.record_linear_algebra(
                    id,
                    &range,
                    LinearAlgebraInfo {
                        kind: LinearAlgebraKind::MatMul {
                            m,
                            k: k1,
                            n,
                            outer_is_tuple,
                            row_is_tuple,
                            elem_is_float: elem_is_float(&elem),
                        },
                    },
                );
                let row_ty = if row_is_tuple {
                    Ty::Tuple(vec![elem.clone(); n])
                } else {
                    Ty::Array {
                        element: Box::new(elem.clone()),
                        length: ArrayLength::Static(n),
                    }
                };
                let data = if outer_is_tuple {
                    Ty::Tuple(vec![row_ty; m])
                } else {
                    Ty::Array {
                        element: Box::new(row_ty),
                        length: ArrayLength::Static(m),
                    }
                };
                wrap_matrix_ty(data)
            }
            "+" | "-" => {
                use crate::typechecking::aggregate_arith::AggregateOp;
                let Some((le, m, n, outer_is_tuple, row_is_tuple)) = classify_matrix(ld) else {
                    return self.error_with_help(
                        ErrorCode::GenericTypeError,
                        format!("invalid matrix layout `{}`", ld),
                        range.clone(),
                        None,
                    );
                };
                let Some((re, m2, n2, right_outer, right_row)) = classify_matrix(rd) else {
                    return self.error_with_help(
                        ErrorCode::GenericTypeError,
                        format!("invalid matrix layout `{}`", rd),
                        range.clone(),
                        None,
                    );
                };
                if m != m2
                    || n != n2
                    || outer_is_tuple != right_outer
                    || row_is_tuple != right_row
                {
                    return self.error_with_help(
                        ErrorCode::GenericTypeError,
                        format!(
                            "cannot apply `{}` to matrices of different shapes",
                            op
                        ),
                        range,
                        Some("element-wise matrix ops require equal dimensions".to_string()),
                    );
                }
                let _ = self.unify(&le, &re, &range, &format!("element types of matrix `{}`", op));
                let elem = apply_ty_prune(&self.subst, &le);
                if !is_numeric_elem(&elem) {
                    return self.error_with_help(
                        ErrorCode::GenericTypeError,
                        format!("element type `{}` does not support matrix `{}`", elem, op),
                        range,
                        None,
                    );
                }
                let agg_op = if op == "+" {
                    AggregateOp::Add
                } else {
                    AggregateOp::Sub
                };
                self.record_linear_algebra(
                    id,
                    &range,
                    LinearAlgebraInfo {
                        kind: LinearAlgebraKind::MatrixZip {
                            m,
                            n,
                            op: agg_op,
                            outer_is_tuple,
                            row_is_tuple,
                            elem_is_float: elem_is_float(&elem),
                        },
                    },
                );
                wrap_matrix_ty(ld.clone())
            }
            _ => self.error_with_help(
                ErrorCode::GenericTypeError,
                format!(
                    "operator `{}` is not supported on `Matrix` (use `*` for matmul, `+`/`-` for element-wise)",
                    op
                ),
                range,
                Some(
                    "`Matrix` implements Mul/Add/Sub only — not Num (no `/`, `%`, or `**`)"
                        .to_string(),
                ),
            ),
        }
    }

    /// Unary `-` on a `Matrix` — element-wise negate of every cell.
    fn infer_matrix_neg(&mut self, matrix_ty: Ty, id: Option<NodeId>, range: Range<usize>) -> Ty {
        use crate::typechecking::aggregate_arith::{
            LinearAlgebraInfo, LinearAlgebraKind, classify_matrix, elem_is_float, is_numeric_elem,
            unwrap_matrix_ty, wrap_matrix_ty,
        };

        let Some(data) = unwrap_matrix_ty(&matrix_ty) else {
            return matrix_ty;
        };
        let Some((elem, m, n, outer_is_tuple, row_is_tuple)) = classify_matrix(data) else {
            return self.error_with_help(
                ErrorCode::GenericTypeError,
                format!("invalid matrix layout `{}`", data),
                range,
                None,
            );
        };
        if !is_numeric_elem(&elem) {
            return self.error_with_help(
                ErrorCode::GenericTypeError,
                format!("element type `{}` does not support unary `-`", elem),
                range,
                None,
            );
        }
        self.record_linear_algebra(
            id,
            &range,
            LinearAlgebraInfo {
                kind: LinearAlgebraKind::MatrixNeg {
                    m,
                    n,
                    outer_is_tuple,
                    row_is_tuple,
                    elem_is_float: elem_is_float(&elem),
                },
            },
        );
        wrap_matrix_ty(data.clone())
    }

    /// Thread a curried function type through a list of argument types,
    /// unifying each. Returns the final return type.
    ///
    /// If at any point the type doesn't look like a function and isn't a
    /// variable, the call is rejected.
    fn apply_function(
        &mut self,
        name: Option<&str>,
        fun_ty: &Ty,
        arg_tys: &[Ty],
        arg_exprs: Option<&[Output]>,
        call_id: Option<NodeId>,
        range: Range<usize>,
    ) -> Ty {
        let mut current = fun_ty.clone();
        // Nullary `f()` calls apply the implicit `unit` parameter (see
        // `seal_nullary_fun_ty` on function declarations).
        if arg_tys.is_empty() {
            loop {
                let pruned = apply_ty_prune(&self.subst, &current);
                match pruned {
                    Ty::Forall { .. } => {
                        let (body, constraints) = self.instantiate_forall_ty(&pruned);
                        if !constraints.is_empty() {
                            self.discharge_constraints(call_id, &constraints, &range);
                        }
                        current = body;
                    }
                    Ty::Fun(param, ret) => {
                        self.coerce_or_unify(
                            param.as_ref(),
                            &unit_ty(),
                            None,
                            &range,
                            "function argument",
                        );
                        return apply_ty_prune(&self.subst, ret.as_ref());
                    }
                    _ => break,
                }
            }
        }
        for (i, arg) in arg_tys.iter().enumerate() {
            let mut pending_constraints = Vec::new();
            loop {
                let pruned = apply_ty(&self.subst, &current);
                match pruned {
                    forall @ Ty::Forall { .. } => {
                        let (body, constraints) = self.instantiate_forall_ty(&forall);
                        pending_constraints.extend(constraints);
                        current = body;
                    }
                    Ty::Fun(param, ret) => {
                        if matches!(param.as_ref(), Ty::Forall { .. }) {
                            self.check_rank_n_argument(
                                param.as_ref(),
                                arg,
                                arg_exprs.and_then(|args| args.get(i)),
                                &range,
                            );
                        } else {
                            self.coerce_or_unify(
                                param.as_ref(),
                                arg,
                                arg_exprs.and_then(|args| args.get(i)),
                                &range,
                                "function argument",
                            );
                        }
                        if !pending_constraints.is_empty() {
                            self.discharge_constraints(call_id, &pending_constraints, &range);
                        }
                        current = *ret;
                        break;
                    }
                    Ty::Var(v) => {
                        let ret_ty = Ty::Var(self.counter.fresh());
                        let new_fun = Ty::Fun(Box::new(arg.clone()), Box::new(ret_ty.clone()));
                        self.unify(&Ty::Var(v), &new_fun, &range, "function type");
                        current = ret_ty;
                        break;
                    }
                    _ => {
                        // We've run out of function parameters — the call
                        // had more arguments than the function accepts.
                        let actual = format!("{}", apply_ty_prune(&self.subst, &pruned));
                        return self.error_with_help(
                            ErrorCode::GenericTypeError,
                            match name {
                                Some(n) => format!(
                                    "Function `{}` was called with too many arguments \
                                     (it accepts {}, but argument #{} was given)",
                                    n,
                                    i,
                                    i + 1,
                                ),
                                None => format!(
                                    "Cannot call value of type `{}` as a function \
                                     (it accepts {} argument{})",
                                    actual,
                                    i,
                                    if i == 1 { "" } else { "s" },
                                ),
                            },
                            range,
                            Some(
                                "check the function signature or the number of arguments"
                                    .to_string(),
                            ),
                        );
                    }
                }
            }
        }
        current
    }

    /// Apply a partially-filled call where named holes may skip parameters.
    ///
    /// `filled` is `(slot_index, arg_ty)` for each provided argument. Unfilled
    /// slots become residual `Fun` parameters (in declaration order).
    fn apply_partial_with_mask(
        &mut self,
        fun_ty: &Ty,
        filled: &[(usize, Ty)],
        range: &Range<usize>,
    ) -> Ty {
        let mut params: Vec<Ty> = Vec::new();
        let mut current = fun_ty.clone();
        loop {
            let pruned = apply_ty_prune(&self.subst, &current);
            match pruned {
                Ty::Forall { .. } => {
                    let (body, _) = self.instantiate_forall_ty(&pruned);
                    current = body;
                }
                Ty::Fun(param, ret) => {
                    params.push(*param);
                    current = *ret;
                }
                other => {
                    current = other;
                    break;
                }
            }
        }
        let mut residual: Vec<Ty> = Vec::new();
        for (i, param) in params.iter().enumerate() {
            if let Some((_, arg_ty)) = filled.iter().find(|(s, _)| *s == i) {
                self.unify(param, arg_ty, range, "function argument");
            } else {
                residual.push(param.clone());
            }
        }
        let mut out = current;
        for p in residual.into_iter().rev() {
            out = Ty::Fun(Box::new(p), Box::new(out));
        }
        out
    }

    /// True when `mask` fills exactly the first `n` bits (positional prefix).
    fn is_prefix_fill_mask(mask: u32, n_filled: u32) -> bool {
        n_filled > 0 && mask == (1u32 << n_filled).wrapping_sub(1)
    }

    /// Apply a C-varargs extern call: unify the fixed prefix, accept extra
    /// FFI-marshallable args, and record per-arg tags for codegen.
    fn apply_extern_variadic_call(
        &mut self,
        name: &str,
        fun_ty: &Ty,
        arg_tys: &[Ty],
        arg_exprs: Option<&[Output]>,
        range: Range<usize>,
    ) -> Ty {
        let nfixed = self
            .extern_variadic_nfixed
            .get(name)
            .copied()
            .unwrap_or_else(|| Self::fun_arity(fun_ty));
        if arg_tys.len() < nfixed {
            return self.error_with_help(
                ErrorCode::GenericTypeError,
                format!(
                    "Function `{}` was called with too few arguments \
                     (variadic; expects at least {}, got {})",
                    name,
                    nfixed,
                    arg_tys.len()
                ),
                range,
                Some("provide the fixed parameters before any `...` arguments".to_string()),
            );
        }
        for (i, ty) in arg_tys.iter().enumerate().skip(nfixed) {
            if !Self::is_ffi_marshallable_ty(ty) {
                let span = arg_exprs
                    .and_then(|a| a.get(i))
                    .map(|e| e.0.into_range())
                    .unwrap_or_else(|| range.clone());
                let mut m = Message::error(
                    ErrorCode::GenericTypeError,
                    format!(
                        "variadic FFI argument #{} has non-marshallable type `{}`",
                        i + 1,
                        apply_ty_prune(&self.subst, ty)
                    ),
                    span.clone(),
                );
                m.push(Label::new(
                    "expected int, float, string, bool, or pointer-like".to_string(),
                    span,
                ));
                self.messages.push(m);
            }
        }
        let fixed = &arg_tys[..nfixed];
        let fixed_exprs = arg_exprs.map(|a| &a[..nfixed.min(a.len())]);
        let ret = self.apply_function(Some(name), fun_ty, fixed, fixed_exprs, None, range.clone());
        let tags: Vec<(u32, u32)> = arg_tys
            .iter()
            .map(|ty| Self::ffi_tag_from_ty_static(&apply_ty_prune(&self.subst, ty)))
            .collect();
        self.variadic_call_arg_tags
            .insert((range.start, range.end), tags);
        ret
    }

    fn fun_arity(ty: &Ty) -> usize {
        let mut n = 0;
        let mut cur = ty;
        while let Ty::Fun(_, ret) = cur {
            n += 1;
            cur = ret.as_ref();
        }
        n
    }

    fn is_ffi_marshallable_ty(ty: &Ty) -> bool {
        match ty {
            Ty::Con(n) => matches!(
                n.to_ascii_lowercase().as_str(),
                "int"
                    | "float"
                    | "string"
                    | "bool"
                    | "byte"
                    | "void"
                    | "int8"
                    | "int16"
                    | "int32"
                    | "uint8"
                    | "uint16"
                    | "uint32"
                    | "uint64"
                    | "ptr"
            ),
            Ty::Array { .. } | Ty::Tuple(_) | Ty::Record { .. } => true,
            _ => false,
        }
    }

    fn ffi_tag_from_ty_static(ty: &Ty) -> (u32, u32) {
        use common::tag;
        match ty {
            Ty::Con(n) => match n.to_ascii_lowercase().as_str() {
                "float" => (tag::FLOAT, 0),
                "string" => (tag::STRING, 0),
                "bool" => (tag::BOOL, 0),
                "byte" | "uint8" => (tag::UINT8, 0),
                "int8" => (tag::INT8, 0),
                "int16" => (tag::INT16, 0),
                "int32" => (tag::INT32, 0),
                "uint16" => (tag::UINT16, 0),
                "uint32" => (tag::UINT32, 0),
                "uint64" => (tag::UINT64, 0),
                "ptr" => (tag::PTR, 0),
                "void" => (tag::VOID, 0),
                _ => (tag::INT, 0),
            },
            Ty::Array { .. } | Ty::Tuple(_) => (tag::PTR, 0),
            _ => (tag::INT, 0),
        }
    }

    /// Whether `name` is an `extern` function declared with C `...`.
    pub fn is_extern_variadic(&self, name: &str) -> bool {
        self.extern_variadic.contains(name)
    }

    /// Whether a `declare` binding was marked variadic.
    pub fn is_ffi_declare_variadic(&self, binding: &str) -> bool {
        self.ffi_fn_variadic.get(binding).copied().unwrap_or(false)
    }

    /// Whether the fn-id expression passed to `invoke` came from a variadic `declare`.
    pub fn is_ffi_declare_variadic_for_fn_id(&self, expr: &Output) -> bool {
        self.ffi_invoke_fn_id_metadata(expr)
            .map(|(_, variadic, _)| variadic)
            .unwrap_or(false)
    }

    /// Per-arg FFI tags recorded for a variadic call/invoke at `span`.
    pub fn variadic_arg_tags_at(&self, span: (usize, usize)) -> Option<&[(u32, u32)]> {
        self.variadic_call_arg_tags.get(&span).map(|v| v.as_slice())
    }

    fn apply_existential_method(
        &mut self,
        class: &str,
        method: &str,
        scheme: &Scheme,
        arg_tys: &[Ty],
        arg_exprs: Option<&[Output]>,
        call_id: Option<NodeId>,
        range: Range<usize>,
    ) -> Ty {
        let (fun_ty, constraints, _mapping) = self.instantiate_scheme_mapped(scheme);
        let result = self.apply_function(
            Some(&format!("{}::{}", class, method)),
            &fun_ty,
            arg_tys,
            arg_exprs,
            call_id,
            range.clone(),
        );
        let remaining: Vec<_> = constraints
            .into_iter()
            .filter(|constraint| constraint.class != class)
            .collect();
        if !remaining.is_empty() {
            self.discharge_constraints(call_id, &remaining, &range);
        }
        result
    }

    fn coerce_or_unify(
        &mut self,
        expected: &Ty,
        actual: &Ty,
        expr: Option<&Output>,
        range: &Range<usize>,
        context: &str,
    ) -> Ty {
        let expected = apply_ty_prune(&self.subst, expected);
        let actual = apply_ty_prune(&self.subst, actual);
        // Integer literals may coerce to `byte` when in range 0..=255.
        if Self::is_byte_ty(&expected)
            && Self::is_int_ty(&actual)
            && let Some(expr) = expr
        {
            match Self::byte_literal_coercion(expr) {
                Ok(()) => return expected,
                Err(Some(n)) => {
                    return self.error_with_help(
                        ErrorCode::TypeMismatch,
                        format!("byte literal out of range: `{n}` is not in 0..=255"),
                        range.clone(),
                        Some("a `byte` must be an integer between 0 and 255".to_string()),
                    );
                }
                Err(None) => {}
            }
        }
        // Single UTF-8-byte string literals coerce to `byte`.
        if Self::is_byte_ty(&expected)
            && Self::is_string_ty(&actual)
            && let Some(expr) = expr
        {
            if self.try_mark_string_literal_as_byte(expr) {
                return expected;
            }
            if let Expression::String(s) = unwrap_expr_wrappers(expr).1.as_ref() {
                return self.string_literal_byte_mismatch(s, range);
            }
        }
        // String literals coerce to `[byte]` / `[byte; N]` (UTF-8 bytes).
        if Self::is_byte_array_ty(&expected).is_some()
            && Self::is_string_ty(&actual)
            && let Some(expr) = expr
        {
            if self.try_mark_string_literal_as_bytes(expr, &expected) {
                return expected.clone();
            }
            if let Expression::String(s) = unwrap_expr_wrappers(expr).1.as_ref() {
                return self.coerce_string_literal_to_bytes(s, &expected, range);
            }
        }
        // Array literals of in-range integer / single-byte string literals coerce to `[byte]`.
        if let (
            Ty::Array {
                element: exp_elem,
                length: exp_len,
            },
            Ty::Array {
                element: act_elem,
                length: act_len,
            },
        ) = (&expected, &actual)
            && Self::is_byte_ty(exp_elem)
            && (Self::is_int_ty(act_elem) || Self::is_string_ty(act_elem))
            && (exp_len == act_len
                || matches!(exp_len, ArrayLength::Dynamic)
                || matches!(act_len, ArrayLength::Dynamic))
            && let Some(expr) = expr
            && let Expression::Array(items) = unwrap_expr_wrappers(expr).1.as_ref()
        {
            let mut ok = true;
            for item in items {
                if Self::byte_literal_coercion(item).is_ok() {
                    continue;
                }
                if self.try_mark_string_literal_as_byte(item) {
                    continue;
                }
                match Self::byte_literal_coercion(item) {
                    Err(Some(n)) => {
                        let _ = self.error_with_help(
                            ErrorCode::TypeMismatch,
                            format!("byte literal out of range: `{n}` is not in 0..=255"),
                            item.0.into_range(),
                            Some("a `byte` must be an integer between 0 and 255".to_string()),
                        );
                        ok = false;
                    }
                    _ => {
                        if let Expression::String(s) = unwrap_expr_wrappers(item).1.as_ref() {
                            let _ = self.string_literal_byte_mismatch(s, &item.0.into_range());
                            ok = false;
                        } else {
                            ok = false;
                            break;
                        }
                    }
                }
            }
            if ok {
                return expected;
            }
        }
        match (&expected, &actual) {
            (
                Ty::Existential {
                    class: expected_class,
                },
                Ty::Existential {
                    class: actual_class,
                },
            ) if expected_class == actual_class => expected,
            (Ty::Existential { class }, _) => {
                if matches!(actual, Ty::Var(_)) {
                    return self.error_with_help(
                        ErrorCode::GenericTypeError,
                        format!("Cannot pack open generic value as `{}`", class),
                        range.clone(),
                        Some("bare-class existentials require a concrete value type at the pack site".to_string()),
                    );
                }
                let lookup_ty = Self::existential_lookup_ty(&actual);
                match self.find_unique_instance(class, std::slice::from_ref(&lookup_ty), range) {
                    Ok(Some(_)) => {
                        if let Some(expr) = expr {
                            self.existential_packs_by_span.insert(
                                (expr.0.start, expr.0.end),
                                ExistentialPack {
                                    class: class.clone(),
                                    value_ty: lookup_ty,
                                },
                            );
                        }
                        expected
                    }
                    Ok(None) => {
                        let pretty = Constraint {
                            class: class.clone(),
                            args: vec![lookup_ty],
                        };
                        self.messages.push(Message::error(
                            ErrorCode::GenericTypeError,
                            format!("No instance for `{}`", pretty),
                            range.clone(),
                        ));
                        expected
                    }
                    Err(()) => expected,
                }
            }
            _ => self.unify(&expected, &actual, range, context),
        }
    }

    fn is_byte_ty(ty: &Ty) -> bool {
        matches!(ty, Ty::Con(n) if n == crate::typechecking::ty::BYTE)
    }

    fn is_string_ty(ty: &Ty) -> bool {
        matches!(ty, Ty::Con(n) if n == crate::typechecking::ty::STRING)
    }

    /// True when `node` is `raise …`, including a parenthesized
    /// `Group(Fragment([Raise]))` form from `(raise err)`.
    fn expr_is_raise(node: &Output) -> bool {
        let node = unwrap_expr_wrappers(node);
        match node.1.as_ref() {
            Expression::Raise(_) => true,
            Expression::Fragment(items) if items.len() == 1 => {
                matches!(
                    unwrap_expr_wrappers(&items[0]).1.as_ref(),
                    Expression::Raise(_)
                )
            }
            _ => false,
        }
    }

    /// `Result::Ok(...)` / `Result::Err(...)` (also `Ok`/`Err` if prelude-bound).
    fn expr_is_explicit_result_construct(node: &Output) -> bool {
        let node = unwrap_expr_wrappers(node);
        match node.1.as_ref() {
            Expression::Construct {
                enum_name,
                variant_name,
                ..
            } => {
                let is_result = *enum_name == common::BUILTIN_RESULT_ENUM
                    || enum_name.ends_with("::Result");
                is_result && (*variant_name == "Ok" || *variant_name == "Err")
            }
            Expression::Fragment(items) if items.len() == 1 => {
                Self::expr_is_explicit_result_construct(&items[0])
            }
            _ => false,
        }
    }

    fn is_int_ty(ty: &Ty) -> bool {
        matches!(ty, Ty::Con(n) if n == crate::typechecking::ty::INT)
    }

    /// `[byte]`, `[byte; N]`, or `Vec<byte>` — returns the length constraint when so.
    /// `Vec<byte>` is treated as dynamic length.
    fn is_byte_array_ty(ty: &Ty) -> Option<ArrayLength> {
        Self::is_byte_slice_ty(ty).or_else(|| {
            if Self::is_vec_byte_ty(ty) {
                Some(ArrayLength::Dynamic)
            } else {
                None
            }
        })
    }

    fn is_byte_slice_ty(ty: &Ty) -> Option<ArrayLength> {
        match ty {
            Ty::Array { element, length } if Self::is_byte_ty(element) => Some(*length),
            _ => None,
        }
    }

    fn is_vec_byte_ty(ty: &Ty) -> bool {
        vec_element_ty(ty).is_some_and(|e| Self::is_byte_ty(e))
    }

    fn byte_array_tys_compatible(src: &Ty, dst: &Ty) -> bool {
        match (Self::is_byte_array_ty(src), Self::is_byte_array_ty(dst)) {
            (Some(src_len), Some(dst_len)) => match (src_len, dst_len) {
                (ArrayLength::Dynamic, _) | (_, ArrayLength::Dynamic) => true,
                (ArrayLength::Static(a), ArrayLength::Static(b)) => a == b,
            },
            _ => false,
        }
    }

    /// Type a string literal as `byte` when its UTF-8 encoding is one byte.
    fn coerce_string_literal_to_byte(&mut self, raw: &str, range: &Range<usize>) -> Ty {
        match crate::codegen::string_literal_as_single_byte(raw) {
            Ok(_) => crate::typechecking::ty::byte(),
            Err(err) => self.string_literal_byte_error(err, range),
        }
    }

    /// Type a string literal as `[byte]` / `[byte; N]` from its UTF-8 bytes.
    fn coerce_string_literal_to_bytes(
        &mut self,
        raw: &str,
        expected: &Ty,
        range: &Range<usize>,
    ) -> Ty {
        let bytes = crate::codegen::string_literal_as_bytes(raw);
        let Some(length) = Self::is_byte_array_ty(expected) else {
            return string();
        };
        match length {
            ArrayLength::Static(n) if n != bytes.len() => self.error_with_help(
                ErrorCode::TypeMismatch,
                format!(
                    "string literal has {} byte{}, but expected `[byte; {}]`",
                    bytes.len(),
                    if bytes.len() == 1 { "" } else { "s" },
                    n
                ),
                range.clone(),
                Some("fixed-length `[byte; N]` requires a string literal with exactly N UTF-8 bytes".to_string()),
            ),
            ArrayLength::Static(_) => expected.clone(),
            ArrayLength::Dynamic => {
                // Prefer a fixed length for the literal; assignment to `[byte]` /
                // `Vec<byte>` accepts `[byte; N]` via array length flexibility.
                crate::typechecking::ty::array_fixed(crate::typechecking::ty::byte(), bytes.len())
            }
        }
    }

    /// If `expr` is a single-byte string literal, retarget its codegen span to `byte`.
    fn try_mark_string_literal_as_byte(&mut self, expr: &Output) -> bool {
        let node = unwrap_expr_wrappers(expr);
        let Expression::String(s) = node.1.as_ref() else {
            return false;
        };
        if crate::codegen::string_literal_as_single_byte(s).is_err() {
            return false;
        }
        self.codegen_types_by_span.insert(
            (node.0.start, node.0.end),
            crate::typechecking::ty::byte(),
        );
        true
    }

    /// If `expr` is a string literal compatible with `expected` byte-array type,
    /// retarget its codegen span to that array type.
    fn try_mark_string_literal_as_bytes(&mut self, expr: &Output, expected: &Ty) -> bool {
        let node = unwrap_expr_wrappers(expr);
        let Expression::String(s) = node.1.as_ref() else {
            return false;
        };
        let Some(length) = Self::is_byte_array_ty(expected) else {
            return false;
        };
        let bytes = crate::codegen::string_literal_as_bytes(s);
        match length {
            ArrayLength::Static(n) if n != bytes.len() => return false,
            _ => {}
        }
        let ty = match length {
            ArrayLength::Static(_) => expected.clone(),
            ArrayLength::Dynamic => {
                crate::typechecking::ty::array_fixed(crate::typechecking::ty::byte(), bytes.len())
            }
        };
        self.codegen_types_by_span
            .insert((node.0.start, node.0.end), ty);
        true
    }

    fn string_literal_byte_mismatch(&mut self, raw: &str, range: &Range<usize>) -> Ty {
        match crate::codegen::string_literal_as_single_byte(raw) {
            Ok(_) => crate::typechecking::ty::byte(),
            Err(err) => self.string_literal_byte_error(err, range),
        }
    }

    fn string_literal_byte_error(
        &mut self,
        err: crate::codegen::StringLiteralByteError,
        range: &Range<usize>,
    ) -> Ty {
        use crate::codegen::StringLiteralByteError;
        match err {
            StringLiteralByteError::Empty => self.error_with_help(
                ErrorCode::TypeMismatch,
                "empty string literal cannot coerce to `byte`".to_string(),
                range.clone(),
                Some("use a single-byte literal such as `\"/\"` or `\"\\n\"`".to_string()),
            ),
            StringLiteralByteError::NotSingleByte => self.error_with_help(
                ErrorCode::TypeMismatch,
                "string literal must be exactly one UTF-8 byte to coerce to `byte`".to_string(),
                range.clone(),
                Some(
                    "multi-byte characters (e.g. `\"é\"`) are not a single `byte`; compare bytes from `to_bytes` instead"
                        .to_string(),
                ),
            ),
        }
    }

    /// `Ok(())` if `expr` is an integer literal in `0..=255`.
    /// `Err(Some(n))` if literal but out of range; `Err(None)` if not a literal.
    fn byte_literal_coercion(expr: &Output) -> Result<(), Option<i64>> {
        let expr = Self::peel_literal_expr(expr);
        match expr.1.as_ref() {
            Expression::Integer(n) => {
                if (0..=255).contains(n) {
                    Ok(())
                } else {
                    Err(Some(*n))
                }
            }
            Expression::Negate(inner) => match Self::peel_literal_expr(inner).1.as_ref() {
                Expression::Integer(n) => Err(Some(-n)),
                _ => Err(None),
            },
            _ => Err(None),
        }
    }

    /// Peel wrappers around a literal operand, including `(…)` as
    /// `Group(Fragment([inner]))`.
    fn peel_literal_expr<'a>(expr: &'a Output<'a>) -> &'a Output<'a> {
        let expr = unwrap_expr_wrappers(expr);
        match expr.1.as_ref() {
            Expression::Fragment(items) if items.len() == 1 => {
                Self::peel_literal_expr(&items[0])
            }
            _ => expr,
        }
    }

    fn existential_lookup_ty(ty: &Ty) -> Ty {
        match ty {
            Ty::Sum { name, .. } => Ty::Con(name.clone()),
            Ty::Constructor { owner, .. } => Self::existential_lookup_ty(owner),
            other => other.clone(),
        }
    }

    fn instantiate_forall_ty(&mut self, ty: &Ty) -> (Ty, Vec<Constraint>) {
        let Ty::Forall {
            bounds,
            constraints,
            body,
        } = ty
        else {
            return (ty.clone(), Vec::new());
        };

        let mapping: HashMap<TyVarId, TyVarId> =
            bounds.iter().map(|&v| (v, self.counter.fresh())).collect();
        let body = crate::typechecking::env::substitute_vars(body, &mapping);
        let constraints = constraints
            .iter()
            .map(|c| Constraint {
                class: c.class.clone(),
                args: c
                    .args
                    .iter()
                    .map(|a| crate::typechecking::env::substitute_vars(a, &mapping))
                    .collect(),
            })
            .collect();
        (body, constraints)
    }

    fn skolemize_forall_ty(&mut self, ty: &Ty) -> (Ty, Vec<Constraint>) {
        let Ty::Forall {
            bounds,
            constraints,
            body,
        } = ty
        else {
            return (ty.clone(), Vec::new());
        };

        let mut subst = Subst::empty();
        for bound in bounds {
            let fresh = self.counter.fresh();
            let name = format!("$forall{}", fresh.raw());
            subst.insert(*bound, Ty::Con(name));
        }
        let body = apply_ty(&subst, body);
        let constraints = constraints
            .iter()
            .map(|c| Constraint {
                class: c.class.clone(),
                args: c.args.iter().map(|a| apply_ty(&subst, a)).collect(),
            })
            .collect();
        (body, constraints)
    }

    fn check_rank_n_argument(
        &mut self,
        expected: &Ty,
        inferred_arg: &Ty,
        arg_expr: Option<&Output>,
        range: &Range<usize>,
    ) {
        let (expected_body, expected_constraints) = self.skolemize_forall_ty(expected);
        let (candidate, candidate_constraints) = match arg_expr.and_then(identifier_name) {
            Some(name) => match self.env.lookup(name).cloned() {
                Some(scheme) => self.instantiate_scheme(&scheme),
                None => (inferred_arg.clone(), Vec::new()),
            },
            None => (inferred_arg.clone(), Vec::new()),
        };
        let candidate = apply_ty_prune(&self.subst, &candidate);

        let local = match unify_with(&Subst::empty(), &candidate, &expected_body) {
            Ok(s) => s,
            Err(_) => {
                let expected_pretty =
                    crate::typechecking::pretty::format_ty_for_diag(&self.subst, expected);
                let found_pretty =
                    crate::typechecking::pretty::format_ty_for_diag(&self.subst, &candidate);
                self.messages.push(Message::error(
                    ErrorCode::TypeMismatch,
                    format!(
                        "Type mismatch: expected `{}`, found `{}`",
                        expected_pretty, found_pretty
                    ),
                    arg_expr
                        .map(|arg| arg.0.into_range())
                        .unwrap_or_else(|| range.clone()),
                ));
                return;
            }
        };

        for constraint in candidate_constraints {
            let resolved_args: Vec<Ty> = constraint
                .args
                .iter()
                .map(|a| apply_ty_prune(&local, a))
                .collect();
            let all_skolems = resolved_args
                .iter()
                .all(|a| matches!(a, Ty::Con(name) if name.starts_with("$forall")));
            let all_open = resolved_args.iter().all(|a| matches!(a, Ty::Var(_)));
            let any_open = resolved_args.iter().any(|a| matches!(a, Ty::Var(_)));

            if all_skolems {
                let covered = expected_constraints.iter().any(|ec| {
                    ec.class == constraint.class
                        && ec.args.len() == resolved_args.len()
                        && ec
                            .args
                            .iter()
                            .zip(resolved_args.iter())
                            .all(|(a, b)| a == b)
                });
                if !covered {
                    self.messages.push(Message::error(
                        ErrorCode::GenericTypeError,
                        format!(
                            "Cannot pass constrained polymorphic value where unconstrained `forall` is expected"
                        ),
                        arg_expr
                            .map(|arg| arg.0.into_range())
                            .unwrap_or_else(|| range.clone()),
                    ));
                }
            } else if any_open {
                let needed = Constraint {
                    class: constraint.class.clone(),
                    args: resolved_args,
                };
                if !self.constraint_is_covered(&needed) {
                    self.messages.push(Message::error(
                        ErrorCode::GenericTypeError,
                        format!("Cannot satisfy constraint `{}`", constraint.class),
                        arg_expr
                            .map(|arg| arg.0.into_range())
                            .unwrap_or_else(|| range.clone()),
                    ));
                }
                let _ = all_open;
            } else {
                let lookup = self.instance_lookup_args(&constraint.class, &resolved_args);
                if self
                    .generics
                    .find_instance(&constraint.class, &lookup)
                    .is_none()
                {
                    let pretty = resolved_args
                        .iter()
                        .map(|t| t.to_string())
                        .collect::<Vec<_>>()
                        .join(", ");
                    self.messages.push(Message::error(
                        ErrorCode::GenericTypeError,
                        format!("No instance for `{}<{}>`", constraint.class, pretty),
                        arg_expr
                            .map(|arg| arg.0.into_range())
                            .unwrap_or_else(|| range.clone()),
                    ));
                }
            }
        }
    }

    fn forall_type<F>(&mut self, params: &[parser::ast::TypeParam], body: F) -> Ty
    where
        F: FnOnce(&mut Self) -> Ty,
    {
        let mut frame = HashMap::new();
        let mut bounds = Vec::new();
        let mut kinds = Vec::new();
        for tp in params {
            let var = self.counter.fresh();
            let kind = self.resolve_type_param_kind(tp);
            self.set_var_kind(var, kind.clone());
            frame.insert(tp.name.to_string(), var);
            bounds.push(var);
            kinds.push(kind);
        }

        self.type_params_in_scope.push(frame);
        let mut constraints = Vec::new();
        let synthetic_range = 0..0;
        for (tp, var) in params.iter().zip(bounds.iter()) {
            for bound in &tp.bounds {
                if let Some(constraint) =
                    self.constraint_from_bound(bound, Ty::Var(*var), &synthetic_range)
                {
                    constraints.push(constraint);
                }
            }
        }
        let inner_ty = body(self);
        self.type_params_in_scope.pop();

        Ty::Forall {
            bounds,
            constraints,
            body: Box::new(inner_ty),
        }
    }

    /// Join two branch/arm types, absorbing [`Ty::Never`].
    fn join_ty(&mut self, a: &Ty, b: &Ty, range: &Range<usize>, ctx: &str) -> Ty {
        let a = apply_ty_prune(&self.subst, a);
        let b = apply_ty_prune(&self.subst, b);
        match (&a, &b) {
            (Ty::Never, _) => b,
            (_, Ty::Never) => a,
            _ => self.unify(&a, &b, range, ctx),
        }
    }

    /// Unify two types under the current substitution, updating
    /// `self.subst` on success. On failure, record a message and return
    /// a fresh variable so inference can continue.
    fn unify(&mut self, t1: &Ty, t2: &Ty, range: &Range<usize>, ctx: &str) -> Ty {
        match unify_with(&self.subst, t1, t2) {
            Ok(s) => {
                self.subst = compose(&s, &self.subst);
                apply_ty(&self.subst, t1)
            }
            Err(UnifyError::Mismatch { left, right }) => {
                let left_s = crate::typechecking::pretty::format_ty_for_diag(&self.subst, &left);
                let right_s = crate::typechecking::pretty::format_ty_for_diag(&self.subst, &right);
                self.error_with_help(
                    ErrorCode::TypeMismatch,
                    format!("Type mismatch: expected `{}`, found `{}`", left_s, right_s),
                    range.clone(),
                    Some(format!("while checking `{}`", ctx)),
                )
            }
            Err(UnifyError::Occurs { var, ty }) => {
                let ty_s = crate::typechecking::pretty::format_ty_for_diag(&self.subst, &ty);
                let var_s =
                    crate::typechecking::pretty::format_ty_for_diag(&self.subst, &Ty::Var(var));
                self.error_with_help(
                    ErrorCode::InfiniteType,
                    format!("Cannot construct infinite type `{}`", ty_s),
                    range.clone(),
                    Some(format!(
                        "the type variable `{}` would occur in its own definition",
                        var_s
                    )),
                )
            }
        }
    }

    /// Record an error message and return a fresh variable.
    ///
    /// This is the simplest form: a single message with a primary
    /// label at `range`. No help hint, no secondary labels. For richer
    /// diagnostics use [`error_with_help`] or [`error_with_labels`].
    fn error(&mut self, code: ErrorCode, message: String, range: Range<usize>) -> Ty {
        self.messages.push(Message::error(code, message, range));
        Ty::Var(self.counter.fresh())
    }

    /// Record an error message with a help hint.
    ///
    /// The hint is shown beneath the underline by ariadne's renderer.
    fn error_with_help(
        &mut self,
        code: ErrorCode,
        message: String,
        range: Range<usize>,
        help: Option<String>,
    ) -> Ty {
        let mut msg = Message::error(code, message, range);
        if let Some(h) = help {
            msg.with_help(h);
        }
        self.messages.push(msg);
        Ty::Var(self.counter.fresh())
    }

    /// Record an error with a primary label and one or more secondary
    /// labels. Each secondary label is rendered by ariadne below the
    /// primary underline; use them to point at related source positions
    /// (e.g., "expected type comes from here", "found type comes from
    /// here").
    #[allow(dead_code)]
    fn error_with_labels(
        &mut self,
        code: ErrorCode,
        primary_message: String,
        primary_range: Range<usize>,
        secondary: Vec<(String, Range<usize>)>,
        help: Option<String>,
    ) -> Ty {
        let mut msg = Message::error(code, primary_message, primary_range);
        for (label_text, range) in secondary {
            msg.push(Label::new(label_text, range));
        }
        if let Some(h) = help {
            msg.with_help(h);
        }
        self.messages.push(msg);
        Ty::Var(self.counter.fresh())
    }

    /// Discharge freshened trait constraints from a generic call site.
    ///
    /// For each freshened constraint `c` (returned by instantiate):
    ///
    /// 1. Resolve every argument under the current substitution.
    /// 2. If any arg is still open, check whether an active constraint covers
    ///    the whole predicate (same class + args) — if so, forward the dict.
    /// 3. When all args are concrete, look up `find_instance` with the N-ary
    ///    arg list (HKT heads rewritten via [`instance_lookup_args`]).
    ///
    /// Matched instances are stored by call-site [`NodeId`] for codegen.
    fn discharge_constraints(
        &mut self,
        call_id: Option<NodeId>,
        constraints: &[Constraint],
        range: &Range<usize>,
    ) {
        for c in constraints {
            let resolved_args: Vec<Ty> = c
                .args
                .iter()
                .map(|a| apply_ty_prune(&self.subst, a))
                .collect();
            let any_open = resolved_args.iter().any(|a| matches!(a, Ty::Var(_)));
            if any_open {
                let needed = Constraint {
                    class: c.class.clone(),
                    args: resolved_args.clone(),
                };
                if self.constraint_is_covered(&needed) {
                    if let Some(call_id) = call_id
                        && let Some(dict_index) = self.dict_index_for(&needed)
                    {
                        self.call_site_forward_dicts
                            .entry(call_id)
                            .or_default()
                            .push(dict_index);
                        self.call_site_forward_dicts_by_span
                            .entry((range.start, range.end))
                            .or_default()
                            .push(dict_index);
                    }
                    continue;
                }
                // Partially open (e.g. `Convert<int, β>`): unify against
                // registered instances so free args get pinned by the match.
                match self.find_unique_instance(&c.class, &resolved_args, range) {
                    Ok(Some(instance)) => {
                        self.record_call_site_dict(call_id, range, instance.clone());
                        self.pin_assoc_types_for_instance(&c.class, &instance, None, range);
                    }
                    Ok(None) => {
                        self.messages.push(Message::error(
                            ErrorCode::GenericTypeError,
                            format!("Cannot satisfy constraint `{}`", needed),
                            range.clone(),
                        ));
                    }
                    Err(()) => {}
                }
            } else {
                match self.find_unique_instance(&c.class, &resolved_args, range) {
                    Ok(Some(instance)) => {
                        self.record_call_site_dict(call_id, range, instance.clone());
                        self.pin_assoc_types_for_instance(&c.class, &instance, None, range);
                    }
                    Ok(None) => {
                        if self.try_lift_aggregate_constraint(&c.class, &resolved_args, range) {
                            // Satisfied via element-instance lift (NT-5). Ground
                            // calls monomorphize to zip lowering; do not record a
                            // scalar dict for the aggregate head.
                            continue;
                        }
                        let pretty = Constraint {
                            class: c.class.clone(),
                            args: resolved_args,
                        };
                        self.messages.push(Message::error(
                            ErrorCode::GenericTypeError,
                            format!("No instance for `{}`", pretty),
                            range.clone(),
                        ));
                    }
                    Err(()) => {}
                }
            }
        }
    }

    /// After discharging constraints for a trait method call, pin
    /// freshened associated-type variables from the scheme mapping.
    fn pin_assoc_after_discharge(
        &mut self,
        class: &str,
        constraints: &[Constraint],
        scheme: Option<&Scheme>,
        mapping: &HashMap<TyVarId, TyVarId>,
        range: &Range<usize>,
    ) {
        for c in constraints {
            if !class.is_empty() && c.class != class {
                continue;
            }
            let resolved_args: Vec<Ty> = c
                .args
                .iter()
                .map(|a| apply_ty_prune(&self.subst, a))
                .collect();
            let any_open = resolved_args.iter().any(|a| matches!(a, Ty::Var(_)));
            if any_open {
                // Open bound: leave assoc vars free (open projection).
                continue;
            }
            if let Ok(Some(instance)) = self.find_unique_instance(&c.class, &resolved_args, range) {
                if let Some(scheme) = scheme {
                    self.pin_assoc_vars_from_mapping(&c.class, &instance, scheme, mapping, range);
                }
                self.pin_assoc_types_for_instance(&c.class, &instance, scheme, range);
            }
        }
    }

    /// Find exactly one instance of `class` whose args unify with `wanted`
    /// NT-5: discharge `Add`/`Num`/… for a homogeneous aggregate when the
    /// element type already has that instance (constraint lifting).
    fn try_lift_aggregate_constraint(
        &mut self,
        class: &str,
        wanted: &[Ty],
        range: &Range<usize>,
    ) -> bool {
        use crate::typechecking::aggregate_arith::{homogeneous_aggregate_elem, is_liftable_arith_trait};
        if !is_liftable_arith_trait(class) || wanted.len() != 1 {
            return false;
        }
        let Some(elem) = homogeneous_aggregate_elem(&wanted[0]) else {
            return false;
        };
        // Recurse through nested aggregates: `((int,int),(int,int))` lifts if
        // `(int,int)` lifts if `int` has the instance.
        match self.find_unique_instance(class, std::slice::from_ref(&elem), range) {
            Ok(Some(_)) => true,
            Ok(None) => {
                self.try_lift_aggregate_constraint(class, std::slice::from_ref(&elem), range)
            }
            Err(()) => false,
        }
    }

    /// (open vars in `wanted` may be bound by the match). Ambiguous matches
    /// are diagnosed instead of silently selecting declaration order.
    fn find_unique_instance(
        &mut self,
        class: &str,
        wanted: &[Ty],
        range: &Range<usize>,
    ) -> Result<Option<InstanceDef>, ()> {
        let wanted_lookup = self.instance_lookup_args(class, wanted);
        let candidates: Vec<_> = self
            .generics
            .instances
            .iter()
            .filter(|inst| inst.class == class && inst.args.len() == wanted_lookup.len())
            .cloned()
            .collect();
        let mut matches: Vec<(InstanceDef, Subst)> = Vec::new();
        for inst in candidates {
            let mut ok = true;
            let mut local = self.subst.clone();
            for (have, need) in inst.args.iter().zip(wanted_lookup.iter()) {
                // Bind open vars in `need` to the concrete instance arg.
                match unify_with(&local, need, have) {
                    Ok(s) => local = s,
                    Err(_) => {
                        ok = false;
                        break;
                    }
                }
            }
            if ok {
                matches.push((inst, local));
            }
        }
        match matches.len() {
            0 => Ok(None),
            1 => {
                let (inst, local) = matches.pop().expect("one match");
                self.subst = compose(&local, &self.subst);
                Ok(Some(inst))
            }
            _ => {
                let first = &matches[0].0;
                let second = &matches[1].0;
                self.report_ambiguous_instance(class, wanted, first, second, range);
                Err(())
            }
        }
    }

    fn report_ambiguous_instance(
        &mut self,
        class: &str,
        wanted: &[Ty],
        first: &InstanceDef,
        second: &InstanceDef,
        range: &Range<usize>,
    ) {
        let pretty = self.instance_signature(class, wanted);
        let mut msg = Message::error(
            ErrorCode::GenericTypeError,
            format!("Ambiguous instance for `{}`", pretty),
            range.clone(),
        );
        msg.with_help(format!(
            "both `{}` from module `{}` and `{}` from module `{}` match",
            self.instance_signature(&first.class, &first.args),
            first.defined_module,
            self.instance_signature(&second.class, &second.args),
            second.defined_module
        ));
        if first.defined_module == self.current_module {
            msg.push(Label::new(
                "matching instance declared here".to_string(),
                first.range.clone(),
            ));
        }
        if second.defined_module == self.current_module {
            msg.push(Label::new(
                "another matching instance declared here".to_string(),
                second.range.clone(),
            ));
        }
        self.messages.push(msg);
    }

    /// True when some active constraint matches `needed` under the current subst,
    /// or implies it via a superclass (Phase 5: `Ordered<T>` covers `Equal<T>`).
    fn constraint_is_covered(&self, needed: &Constraint) -> bool {
        let needed_args: Vec<Ty> = needed
            .args
            .iter()
            .map(|a| apply_ty_prune(&self.subst, a))
            .collect();
        self.active_constraints.iter().any(|ac| {
            if ac.args.len() != needed_args.len() {
                return false;
            }
            let args_match = ac
                .args
                .iter()
                .zip(needed_args.iter())
                .all(|(a, b)| apply_ty_prune(&self.subst, a) == *b);
            if !args_match {
                return false;
            }
            let ac_class = self
                .abstract_constraint_binding(&ac.class)
                .unwrap_or(ac.class.as_str());
            if ac_class == needed.class {
                return true;
            }
            // Implied bound: active subclass covers a superclass constraint.
            self.generics
                .typeclass(ac_class)
                .is_some_and(|def| def.has_superclass(&needed.class, &self.generics))
        })
    }

    fn abstract_constraint_binding(&self, name: &str) -> Option<&str> {
        self.abstract_constraint_bindings
            .iter()
            .rev()
            .find_map(|frame| frame.get(name).map(String::as_str))
    }

    fn bind_abstract_constraint(&mut self, abstract_name: &str, concrete_class: &str) {
        if self.constraint_param_kind(abstract_name).is_none() {
            return;
        }
        if let Some(frame) = self.abstract_constraint_bindings.last_mut() {
            frame
                .entry(abstract_name.to_string())
                .or_insert_with(|| concrete_class.to_string());
        }
    }

    fn bind_matching_abstract_constraints(
        &mut self,
        receiver_var: Option<TyVarId>,
        concrete_class: &str,
    ) {
        let names: Vec<String> = self
            .active_constraints
            .iter()
            .filter(|constraint| self.constraint_param_kind(&constraint.class).is_some())
            .filter(|constraint| {
                receiver_var.is_none_or(|var| {
                    constraint.primary_var() == Some(var)
                        || constraint
                            .args
                            .iter()
                            .any(|a| matches!(a, Ty::Var(v) if *v == var))
                })
            })
            .map(|constraint| constraint.class.clone())
            .collect();
        for name in names {
            self.bind_abstract_constraint(&name, concrete_class);
        }
    }

    fn class_own_method_slot(&self, class_name: &str, method: &str) -> Option<usize> {
        let class_def = self.generics.typeclass(class_name)?;
        class_def.methods.iter().position(|m| m.name == method)
    }

    fn possible_classes_for_constraint_method(
        &self,
        constraint: &Constraint,
        method: &str,
    ) -> Vec<String> {
        if let Some(bound) = self.abstract_constraint_binding(&constraint.class) {
            return vec![bound.to_string()];
        }
        if self.constraint_param_kind(&constraint.class).is_none() {
            return vec![constraint.class.clone()];
        }

        self.generics
            .typeclasses
            .iter()
            .filter_map(|(name, class_def)| {
                if class_def.type_params.len() != constraint.args.len() {
                    return None;
                }
                if self.class_own_method_slot(name, method).is_none() {
                    return None;
                }
                let kinds_match = constraint
                    .args
                    .iter()
                    .enumerate()
                    .all(|(i, arg)| class_def.kind_at(i) == self.kind_of_type_argument(arg));
                kinds_match.then(|| name.clone())
            })
            .collect()
    }

    fn dict_index_for(&self, needed: &Constraint) -> Option<usize> {
        let needed_args: Vec<Ty> = needed
            .args
            .iter()
            .map(|a| apply_ty_prune(&self.subst, a))
            .collect();
        // Prefer an exact class match; fall back to a covering subclass dict
        // (flattened layout holds superclass methods at trailing slots).
        self.active_constraints
            .iter()
            .position(|ac| {
                let ac_class = self
                    .abstract_constraint_binding(&ac.class)
                    .unwrap_or(ac.class.as_str());
                ac_class == needed.class
                    && ac.args.len() == needed_args.len()
                    && ac
                        .args
                        .iter()
                        .zip(needed_args.iter())
                        .all(|(a, b)| apply_ty_prune(&self.subst, a) == *b)
            })
            .or_else(|| {
                self.active_constraints.iter().position(|ac| {
                    let ac_class = self
                        .abstract_constraint_binding(&ac.class)
                        .unwrap_or(ac.class.as_str());
                    ac.args.len() == needed_args.len()
                        && ac
                            .args
                            .iter()
                            .zip(needed_args.iter())
                            .all(|(a, b)| apply_ty_prune(&self.subst, a) == *b)
                        && self
                            .generics
                            .typeclass(ac_class)
                            .is_some_and(|def| def.has_superclass(&needed.class, &self.generics))
                })
            })
    }

    fn user_dict_index(&self, var: TyVarId, class: &str) -> Option<usize> {
        self.user_dict_index_and_class(var, class)
            .map(|(idx, _)| idx)
    }

    fn user_dict_index_and_class(&self, var: TyVarId, class: &str) -> Option<(usize, String)> {
        self.active_constraints
            .iter()
            .enumerate()
            .find_map(|(idx, constraint)| {
                let concrete = self
                    .abstract_constraint_binding(&constraint.class)
                    .unwrap_or(constraint.class.as_str());
                let covers = concrete == class
                    || self
                        .generics
                        .typeclass(concrete)
                        .is_some_and(|def| def.has_superclass(class, &self.generics));
                (covers && (constraint.is_unary_on(var) || constraint.primary_var() == Some(var)))
                    .then(|| (idx, concrete.to_string()))
            })
    }

    fn bound_method_candidates(
        &self,
        method: &str,
        receiver_var: Option<TyVarId>,
    ) -> Vec<(usize, String, String, usize, Scheme)> {
        self.active_constraints
            .iter()
            .enumerate()
            .filter(|(_, constraint)| {
                receiver_var.is_none_or(|var| {
                    constraint.primary_var() == Some(var)
                        || constraint
                            .args
                            .iter()
                            .any(|a| matches!(a, Ty::Var(v) if *v == var))
                })
            })
            .flat_map(|(dict_index, constraint)| {
                self.possible_classes_for_constraint_method(constraint, method)
                    .into_iter()
                    .filter_map(move |dict_class| {
                        let class_def = self.generics.typeclass(&dict_class)?;
                        // Flattened dict: own methods then superclass methods. A call
                        // to a superclass method under `T: Ordered` resolves here with
                        // the trailing slot index (implied Equal).
                        let flat = class_def.flattened_methods(&self.generics);
                        let (method_slot, owner) =
                            flat.iter().enumerate().find_map(|(slot, (owner, m))| {
                                if m.name == method {
                                    Some((slot, (*owner).to_string()))
                                } else {
                                    None
                                }
                            })?;
                        let scheme = self
                            .typeclass_method_schemes
                            .get(&(owner.clone(), method.to_string()))?
                            .clone();
                        Some((dict_index, dict_class, owner, method_slot, scheme))
                    })
            })
            .collect()
    }

    fn existential_method_candidate(
        &self,
        class: &str,
        method: &str,
    ) -> Option<(String, usize, Scheme)> {
        let class_def = self.generics.typeclass(class)?;
        let flat = class_def.flattened_methods(&self.generics);
        let (method_slot, owner) = flat.iter().enumerate().find_map(|(slot, (owner, m))| {
            (m.name == method).then(|| (slot, (*owner).to_string()))
        })?;
        let scheme = self
            .typeclass_method_schemes
            .get(&(owner.clone(), method.to_string()))?
            .clone();
        Some((owner, method_slot, scheme))
    }

    fn select_bound_method(
        &mut self,
        candidates: Vec<(usize, String, String, usize, Scheme)>,
        method: &str,
        range: &Range<usize>,
    ) -> Option<(usize, String, String, usize, Scheme)> {
        if candidates.len() > 1 {
            self.messages.push(Message::error(
                ErrorCode::GenericTypeError,
                format!("Ambiguous trait method `{}`", method),
                range.clone(),
            ));
            return None;
        }
        candidates.into_iter().next()
    }

    /// Resolve a ground trait method call on a concrete receiver
    /// (`recv.into()`, `recv.show()`, …) when no open bound is active.
    ///
    /// Returns `(class, scheme)` when exactly one registered method scheme
    /// named `method` has a first parameter that unifies with `recv_ty`.
    fn ground_trait_method_for_receiver(
        &mut self,
        method: &str,
        recv_ty: &Ty,
    ) -> Option<(String, Scheme)> {
        let recv = apply_ty_prune(&self.subst, recv_ty);
        let schemes: Vec<(String, Scheme)> = self
            .typeclass_method_schemes
            .iter()
            .filter(|((_, mname), _)| mname.as_str() == method)
            .map(|((class, _), scheme)| (class.clone(), scheme.clone()))
            .collect();
        let mut matches: Vec<(String, Scheme)> = Vec::new();
        for (class, scheme) in schemes {
            // Freshen to probe the first parameter; trial unify does not
            // commit into `self.subst`.
            let (fun_ty, _constraints, _kinds) = instantiate_with_kinds(&scheme, &mut self.counter);
            let Some(first_param) = Self::first_fun_param(&fun_ty) else {
                continue;
            };
            if unify_with(&self.subst, &first_param, &recv).is_ok() {
                matches.push((class, scheme));
            }
        }
        if matches.len() == 1 {
            matches.pop()
        } else {
            None
        }
    }

    fn first_fun_param(ty: &Ty) -> Option<Ty> {
        match ty {
            Ty::Fun(param, _) => Some(param.as_ref().clone()),
            _ => None,
        }
    }

    /// Instantiate a scheme and record freshened variable kinds (Phase 5).
    fn instantiate_ty(&mut self, scheme: &Scheme) -> Ty {
        let (ty, _, kinds) = instantiate_with_kinds(scheme, &mut self.counter);
        self.var_kinds.extend(kinds);
        ty
    }

    /// Instantiate a scheme with constraints, recording freshened kinds.
    fn instantiate_scheme(&mut self, scheme: &Scheme) -> (Ty, Vec<Constraint>) {
        let (ty, constraints, kinds) = instantiate_with_kinds(scheme, &mut self.counter);
        self.var_kinds.extend(kinds);
        (ty, constraints)
    }

    /// Kind of a type variable, defaulting to `*`.
    fn kind_of_var(&self, var: TyVarId) -> Kind {
        self.var_kinds.get(&var).cloned().unwrap_or(Kind::Type)
    }

    /// Record a type variable's kind (overwrites).
    fn set_var_kind(&mut self, var: TyVarId, kind: Kind) {
        self.var_kinds.insert(var, kind);
    }

    fn constraint_param_kind(&self, name: &str) -> Option<Kind> {
        self.type_param_kind(name)
            .filter(Kind::is_constraint_constructor_kind)
    }

    fn type_param_kind(&self, name: &str) -> Option<Kind> {
        self.type_params_in_scope
            .iter()
            .rev()
            .find_map(|frame| frame.get(name).copied())
            .map(|var| self.kind_of_var(var))
    }

    fn expected_constraint_kind_for_arg(&self, arg: &Ty) -> Kind {
        Kind::arrow(self.kind_of_type_argument(arg), Kind::Constraint)
    }

    fn constraint_from_bound(
        &mut self,
        bound: &str,
        arg: Ty,
        range: &Range<usize>,
    ) -> Option<Constraint> {
        if let Some(kind) = self.constraint_param_kind(bound) {
            let expected = self.expected_constraint_kind_for_arg(&arg);
            if kind != expected {
                self.messages.push(Message::error(
                    ErrorCode::GenericTypeError,
                    format!(
                        "Constraint parameter `{}` has kind `{}`, expected `{}`",
                        bound, kind, expected
                    ),
                    range.clone(),
                ));
                return None;
            }
            return Some(Constraint {
                class: bound.to_string(),
                args: vec![arg],
            });
        }

        if let Some(kind) = self.type_param_kind(bound) {
            let expected = self.expected_constraint_kind_for_arg(&arg);
            self.messages.push(Message::error(
                ErrorCode::GenericTypeError,
                format!(
                    "Constraint parameter `{}` has kind `{}`, expected `{}`",
                    bound, kind, expected
                ),
                range.clone(),
            ));
            return None;
        }

        if self.generics.typeclass(bound).is_none() {
            self.messages.push(Message::error(
                ErrorCode::GenericTypeError,
                format!("Cannot find trait or constraint parameter `{}`", bound),
                range.clone(),
            ));
            return None;
        }

        Some(Constraint {
            class: bound.to_string(),
            args: vec![arg],
        })
    }

    /// Resolve the kind of a type parameter from its AST annotation and/or
    /// class bounds. Explicit annotations win; otherwise a single-parameter
    /// bound whose class parameter is constructor-kinded upgrades the variable
    /// to that constructor kind.
    fn resolve_type_param_kind(&self, tp: &parser::ast::TypeParam<'_>) -> Kind {
        if tp.kind != parser::ast::Kind::Type {
            return Kind::from(tp.kind.clone());
        }
        for bound in &tp.bounds {
            if let Some(class_def) = self.generics.typeclass(bound) {
                if class_def.type_params.len() == 1 && class_def.is_constructor_kind_at(0) {
                    return class_def.kind_at(0);
                }
            }
        }
        Kind::from(tp.kind.clone())
    }

    fn bare_constructor_kind(&self, name: &str) -> Option<Kind> {
        let canon = Self::canonical_ctor_name(name);
        self.generics
            .generic_type_ctors
            .get(&canon)
            .map(|params| Kind::constructor(params.len()))
            .or_else(|| match canon.as_str() {
                common::BUILTIN_OPTION_ENUM => Some(Kind::constructor(1)),
                common::BUILTIN_RESULT_ENUM => Some(Kind::constructor(2)),
                _ => None,
            })
    }

    fn kind_of_type_argument(&self, ty: &Ty) -> Kind {
        match ty {
            Ty::Var(v) => self.kind_of_var(*v),
            Ty::Con(name) => self.bare_constructor_kind(name).unwrap_or(Kind::Type),
            _ => Kind::Type,
        }
    }

    fn check_type_app_kind(
        &mut self,
        name: &str,
        head_kind: &Kind,
        arg_tys: &[Ty],
        range: &Range<usize>,
    ) {
        if !head_kind.is_arrow() {
            self.messages.push(Message::error(
                ErrorCode::GenericTypeError,
                format!(
                    "Type parameter `{}` has kind `{}`, but is applied as a type constructor",
                    name, head_kind
                ),
                range.clone(),
            ));
            return;
        }

        let expected_args = head_kind.argument_kinds();
        if expected_args.len() != arg_tys.len() {
            self.messages.push(Message::error(
                ErrorCode::GenericTypeError,
                format!(
                    "Type constructor `{}` expects {} type arguments, got {}",
                    name,
                    expected_args.len(),
                    arg_tys.len()
                ),
                range.clone(),
            ));
            return;
        }

        for (i, (arg_ty, expected_kind)) in arg_tys.iter().zip(expected_args.iter()).enumerate() {
            let actual_kind = self.kind_of_type_argument(arg_ty);
            if &actual_kind != expected_kind {
                self.messages.push(Message::error(
                    ErrorCode::GenericTypeError,
                    format!(
                        "Type argument {} to `{}` has kind `{}`, expected `{}`",
                        i + 1,
                        name,
                        actual_kind,
                        expected_kind
                    ),
                    range.clone(),
                ));
            }
        }

        if head_kind.result_kind() != &Kind::Type {
            self.messages.push(Message::error(
                ErrorCode::GenericTypeError,
                format!(
                    "Type application `{}` has kind `{}`, expected `*`",
                    name,
                    head_kind.result_kind()
                ),
                range.clone(),
            ));
        }
    }

    fn validate_instance_head_kinds(
        &mut self,
        class_def: &TypeClassDef,
        arg_tys: &[Ty],
        range: &Range<usize>,
    ) {
        for (i, ty) in arg_tys.iter().enumerate() {
            let expected_kind = class_def.kind_at(i);
            if !expected_kind.is_constructor_kind() {
                continue;
            }

            if matches!(ty, Ty::App(_, _)) {
                self.messages.push(Message::error(
                    ErrorCode::GenericTypeError,
                    format!(
                        "Instance of constructor-kinded class `{}` expects a type constructor \
                         (kind `{}`) as argument {}, found applied type `{}`",
                        class_def.name,
                        expected_kind,
                        i + 1,
                        ty
                    ),
                    range.clone(),
                ));
                continue;
            }

            let actual_kind = match ty {
                Ty::Con(name) => self.bare_constructor_kind(name).unwrap_or(Kind::Type),
                Ty::Var(v) => self.kind_of_var(*v),
                _ => Kind::Type,
            };
            if actual_kind != expected_kind {
                self.messages.push(Message::error(
                    ErrorCode::GenericTypeError,
                    format!(
                        "Instance of constructor-kinded class `{}` expects argument {} \
                         to have kind `{}`, found kind `{}`",
                        class_def.name,
                        i + 1,
                        expected_kind,
                        actual_kind
                    ),
                    range.clone(),
                ));
            }
        }
    }

    /// Canonical name for a bare type constructor used as an instance head.
    fn canonical_ctor_name(name: &str) -> String {
        match name.to_ascii_lowercase().as_str() {
            "int" => "int".into(),
            "float" => "float".into(),
            "string" => "string".into(),
            "bool" => "bool".into(),
            "void" | "unit" => "unit".into(),
            "option" => common::BUILTIN_OPTION_ENUM.into(),
            "result" => common::BUILTIN_RESULT_ENUM.into(),
            _ => name.to_string(),
        }
    }

    /// Parse a trait instance argument. Bare registered constructors
    /// (`Option`, `Result`, user generics) become `Ty::Con` heads for HKT
    /// instances rather than applied `Option<_>` placeholders.
    fn parse_instance_head(&mut self, arg: &Output) -> Ty {
        match arg.1.as_ref() {
            Expression::Type(name) | Expression::Identifier(name) => {
                let canon = Self::canonical_ctor_name(name);
                if self.generics.generic_type_ctors.contains_key(&canon)
                    || matches!(name.to_ascii_lowercase().as_str(), "option" | "result")
                {
                    return Ty::Con(canon);
                }
                if let Some(key) = self.resolve_class_key(name) {
                    return Ty::Con(key);
                }
                // First-order instance heads (`int`, `MyType`, …).
                self.parse_type_name_str_with_range(name, Some(arg.0.into_range()))
            }
            Expression::TypeApp { name, args } => {
                // Applied heads are first-order (`impl Foo<Option<int>>`).
                // Constructor-kinded classes diagnose this later.
                self.parse_type_app(name, args, arg.0.into_range())
            }
            _ => self.parse_type_name(arg),
        }
    }

    /// Extract the type variable that a bound method should dispatch on.
    /// First-order: bare `T`. HKT: the constructor head of `F<A>`.
    fn constraint_var_of_ty(ty: &Ty) -> Option<TyVarId> {
        match ty {
            Ty::Var(v) => Some(*v),
            Ty::App(head, _) => match head.as_ref() {
                Ty::Var(v) => Some(*v),
                _ => None,
            },
            _ => None,
        }
    }

    /// For constructor-kinded class parameters, look up instances by
    /// constructor head (`Option`, `Result`), not by applied types
    /// (`Option<int>`, `Result<int, string>`).
    fn instance_lookup_args(&self, class: &str, args: &[Ty]) -> Vec<Ty> {
        if let Some(class_def) = self.generics.typeclass(class) {
            args.iter()
                .enumerate()
                .map(
                    |(i, concrete)| match (class_def.is_constructor_kind_at(i), concrete) {
                        (true, Ty::App(head, _)) => head.as_ref().clone(),
                        _ => concrete.clone(),
                    },
                )
                .collect()
        } else {
            args.to_vec()
        }
    }

    fn instance_signature(&self, class: &str, args: &[Ty]) -> String {
        if args.is_empty() {
            class.to_string()
        } else {
            format!(
                "{}<{}>",
                class,
                args.iter()
                    .map(|ty| ty.to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        }
    }

    fn instance_satisfies_orphan_rule(
        &self,
        class_def: &TypeClassDef,
        arg_exprs: &[Output],
        arg_tys: &[Ty],
    ) -> bool {
        if class_def.defined_module == self.current_module {
            return true;
        }

        arg_exprs
            .iter()
            .zip(arg_tys.iter())
            .filter(|(_, ty)| !matches!(apply_ty_prune(&self.subst, ty), Ty::Var(_)))
            .all(|(expr, ty)| {
                let head = self
                    .nominal_head_from_instance_arg(expr)
                    .or_else(|| self.nominal_head_from_ty(ty));
                head.as_deref().is_some_and(|name| {
                    let mod_of = |n: &str| self.generics.nominal_type_module(n);
                    mod_of(name) == Some(self.current_module.as_str())
                        // COI-110 registers enums/classes under `module::Name`;
                        // synth `Show`/`String` still pass the short head.
                        || mod_of(&self.qualify_module_name(name))
                            == Some(self.current_module.as_str())
                })
            })
    }

    fn nominal_head_from_instance_arg(&self, arg: &Output) -> Option<String> {
        match arg.1.as_ref() {
            Expression::Type(name) | Expression::Identifier(name) => Some(
                self.resolve_class_key(name)
                    .unwrap_or_else(|| Self::canonical_ctor_name(name)),
            ),
            Expression::TypeApp { name, .. } => Some(
                self.resolve_class_key(name)
                    .unwrap_or_else(|| Self::canonical_ctor_name(name)),
            ),
            _ => None,
        }
    }

    fn nominal_head_from_ty(&self, ty: &Ty) -> Option<String> {
        match apply_ty_prune(&self.subst, ty) {
            Ty::Var(_) => None,
            Ty::Con(name) => Some(Self::canonical_ctor_name(&name)),
            Ty::App(head, _) => self.nominal_head_from_ty(head.as_ref()),
            Ty::Sum { name, .. } => Some(name),
            Ty::Constructor { owner, .. } => self.nominal_head_from_ty(owner.as_ref()),
            Ty::List(_)
            | Ty::Array { .. }
            | Ty::Tuple(_)
            | Ty::Record { .. }
            | Ty::Existential { .. }
            | Ty::Fun(_, _)
            | Ty::Forall { .. }
            | Ty::Readonly(_)
            | Ty::Never => None,
        }
    }

    /// Force-cache `ty` at `expr`'s NodeId (and walk TypeApp children) so
    /// codegen FQNs for instance methods see the same head types.
    fn cache_forced_ty(&mut self, expr: &Output, ty: Ty) {
        let id = self.ids.ids()[self.next_id_idx];
        self.next_id_idx += 1;
        self.cache.insert(id, ty);
        if let Expression::TypeApp { args, .. } = expr.1.as_ref() {
            for arg in args {
                // Child annotations still need IDs consumed; infer normally.
                let _ = self.infer(arg);
            }
        }
    }

    fn parse_type_name(&mut self, ann: &Output) -> Ty {
        self.parse_type_name_inner(ann, false)
    }

    fn parse_return_type_name(&mut self, ann: &Output) -> Ty {
        self.parse_type_name_inner(ann, true)
    }

    fn parse_type_name_inner(&mut self, ann: &Output, allow_dynamic_slice: bool) -> Ty {
        match ann.1.as_ref() {
            Expression::Identifier(name) | Expression::Type(name) => {
                let range = ann.0.into_range();
                if let Some(class) = self.current_typeclass.clone()
                    && self
                        .generics
                        .typeclass(&class)
                        .is_some_and(|cdef| cdef.assoc_type(name).is_some())
                {
                    return self.resolve_type_projection(&class, name, &[], &range);
                }
                self.parse_type_name_str(name)
            }
            Expression::TypeApp { name, args } => {
                self.parse_type_app(name, args, ann.0.into_range())
            }
            Expression::TypeProjection { owner, name, args } => {
                let arg_tys: Vec<Ty> = args.iter().map(|a| self.parse_type_name(a)).collect();
                self.resolve_type_projection(owner, name, &arg_tys, &ann.0.into_range())
            }
            Expression::TypeFun(arg, ret) => Ty::Fun(
                Box::new(self.parse_type_name(arg)),
                Box::new(self.parse_type_name(ret)),
            ),
            Expression::TypeFnSig { params, ret } => self.parse_fn_sig_type(params, ret),
            Expression::Forall { params, ty } => {
                self.forall_type(params, |checker| checker.parse_type_name(ty))
            }
            Expression::Array(items) => {
                // `[T; N]` — parser emits `[Type(T), Integer(N)]` (or the
                // legacy single-`Integer(N)` shape, which always meant
                // `[int; N]`).
                if items.len() == 2
                    && let Expression::Integer(n) = items[1].1.as_ref()
                    && *n >= 0
                {
                    let elem_ty = self.parse_type_name(&items[0]);
                    return crate::typechecking::ty::array_fixed(elem_ty, *n as usize);
                }
                if items.len() == 1
                    && let Expression::Integer(n) = items[0].1.as_ref()
                    && *n >= 0
                {
                    return crate::typechecking::ty::array_fixed(
                        self.parse_type_name_str("int"),
                        *n as usize,
                    );
                }
                if items.len() == 1 {
                    let elem_ty = self.parse_type_name_inner(&items[0], allow_dynamic_slice);
                    if matches!(&elem_ty, Ty::Con(name) if name == "byte") || allow_dynamic_slice {
                        return crate::typechecking::ty::array(elem_ty);
                    }
                    let _ = self.error_with_help(
                        ErrorCode::GenericTypeError,
                        "dynamic array type `[T]` is not allowed".to_string(),
                        ann.0.into_range(),
                        Some("use a fixed-length `[T; N]` or growable `Vec<T>`".to_string()),
                    );
                    return Ty::Var(self.counter.fresh());
                }
                Ty::Var(self.counter.fresh())
            }
            Expression::Tuple(items) => {
                let mut tys = Vec::with_capacity(items.len());
                for item in items {
                    tys.push(self.parse_type_name(item));
                }
                crate::typechecking::ty::tuple(tys)
            }
            _ => Ty::Var(self.counter.fresh()),
        }
    }

    fn parse_type_app(&mut self, name: &str, args: &[Output], range: Range<usize>) -> Ty {
        let arg_tys: Vec<Ty> = args.iter().map(|a| self.parse_type_name(a)).collect();

        if let Some(class) = self.current_typeclass.clone()
            && self
                .generics
                .typeclass(&class)
                .is_some_and(|cdef| cdef.assoc_type(name).is_some())
        {
            return self.resolve_type_projection(&class, name, &arg_tys, &range);
        }

        // In-scope constructor-kinded type parameter as application head
        // (`F<A>`, `F<A, B>`, `F<G>`).
        for frame in self.type_params_in_scope.iter().rev() {
            if let Some(&var) = frame.get(name) {
                let kind = self.kind_of_var(var);
                self.check_type_app_kind(name, &kind, &arg_tys, &range);
                return Ty::App(Box::new(Ty::Var(var)), arg_tys);
            }
        }

        // Generic type aliases expand to their RHS (Phase 1).
        if let Some(def) = self.generic_aliases.get(name).cloned() {
            if def.params.len() != arg_tys.len() {
                self.messages.push(Message::error(
                    ErrorCode::GenericTypeError,
                    format!(
                        "Type constructor `{}` expects {} type arguments, got {}",
                        name,
                        def.params.len(),
                        arg_tys.len()
                    ),
                    range,
                ));
            }
            return self.expand_generic_alias(&def, &arg_tys);
        }

        let ctor = self
            .resolve_class_key(name)
            .unwrap_or_else(|| name.to_string());
        if let Some(expected_arity) = self
            .generics
            .generic_type_ctors
            .get(&ctor)
            .map(|params| params.len())
        {
            if expected_arity != arg_tys.len() {
                self.messages.push(Message::error(
                    ErrorCode::GenericTypeError,
                    format!(
                        "Type constructor `{}` expects {} type arguments, got {}",
                        name,
                        expected_arity,
                        arg_tys.len()
                    ),
                    range,
                ));
            }
            return Ty::App(Box::new(Ty::Con(ctor)), arg_tys);
        }

        self.messages.push(Message::error(
            ErrorCode::GenericTypeError,
            format!("Cannot find type constructor `{}`", name),
            range,
        ));
        Ty::App(Box::new(Ty::Con(name.to_string())), arg_tys)
    }

    fn parse_type_name_str(&mut self, name: &str) -> Ty {
        self.parse_type_name_str_with_range(name, None)
    }

    fn parse_type_name_str_with_range(&mut self, name: &str, range: Option<Range<usize>>) -> Ty {
        // Type parameters in scope take highest priority.
        for frame in self.type_params_in_scope.iter().rev() {
            if let Some(&var) = frame.get(name) {
                return Ty::Var(var);
            }
        }
        for frame in self.type_aliases.iter().rev() {
            if let Some(alias_ty) = frame.get(name) {
                return alias_ty.clone();
            }
        }
        // Built-in type names are matched case-insensitively so the
        // user can write `String`, `STRING`, etc.
        match name.to_ascii_lowercase().as_str() {
            "int" => int(),
            "float" => float(),
            "bool" => boolean(),
            "byte" => crate::typechecking::ty::byte(),
            "string" => string(),
            "void" => unit_ty(),
            "stream" => crate::typechecking::ty::stream_ty(),
            "thread" => crate::typechecking::ty::thread_ty(),
            "sender" => crate::typechecking::ty::sender_ty(),
            "receiver" => crate::typechecking::ty::receiver_ty(),
            "mutex" => crate::typechecking::ty::mutex_ty(),
            "rwlock" => crate::typechecking::ty::rwlock_ty(),
            "option" => option_app_ty(Ty::Var(self.counter.fresh())),
            "result" => result_app_ty(Ty::Var(self.counter.fresh()), Ty::Var(self.counter.fresh())),
            "ioerror" => Ty::Con(common::BUILTIN_IO_ERROR_ENUM.into()),
            "threaderror" => Ty::Con(common::BUILTIN_THREAD_ERROR_ENUM.into()),
            "error" => Ty::Con(common::BUILTIN_FFI_ERROR_ENUM.into()),
            "errorkind" => Ty::Con(common::BUILTIN_FFI_ERROR_KIND_ENUM.into()),
            _ => {
                if let Some(key) = self.resolve_class_key(name) {
                    return Ty::Con(key);
                }
                // COI-110: enums live under `module::Name`; annotations must use
                // that key so they unify with `Enum::Variant` constructors.
                if let Some(key) = self.resolve_enum_key(name) {
                    return Ty::Con(key);
                }
                // Prefer concrete type constructors over bare-class existentials
                // when a name collision exists.
                if self.enums.contains_key(name)
                    || self.generics.generic_type_ctors.contains_key(name)
                    || self.generics.nominal_type_module(name).is_some()
                {
                    return Ty::Con(name.to_string());
                }
                if let Some(class_def) = self.generics.typeclass(name) {
                    if class_def.type_params.len() == 1 && class_def.kind_at(0) == Kind::Type {
                        return Ty::Existential {
                            class: name.to_string(),
                        };
                    }
                    self.messages.push(Message::error(
                        ErrorCode::GenericTypeError,
                        format!("Typeclass `{}` cannot be used as a bare value type", name),
                        range.unwrap_or(0..0),
                    ));
                }
                Ty::Con(name.to_string())
            }
        }
    }

    /// Resolve `Owner::Assoc` in a type annotation (Phase 6).
    ///
    /// - Inside `trait Owner { … }`, `Owner::Elem` / bare scope lookup
    ///   resolves to the quantified assoc var.
    /// - `T::Elem` when `T` is a type param with an active class constraint
    ///   that declares `Elem` → fresh (or cached) open projection var,
    ///   pinned when a ground instance is later discharged.
    /// - Ground-only fallback: if `Owner` names a class and exactly one
    ///   registered instance defines the assoc type, use that concrete type.
    fn resolve_type_projection(
        &mut self,
        owner: &str,
        assoc: &str,
        args: &[Ty],
        range: &Range<usize>,
    ) -> Ty {
        // 1. Current typeclass: `Collect::Elem` while defining Collect.
        if self.current_typeclass.as_deref() == Some(owner) {
            let decl = self
                .generics
                .typeclass(owner)
                .and_then(|cdef| cdef.assoc_type(assoc))
                .cloned();
            if let Some(decl) = decl {
                self.validate_assoc_projection_args(owner, &decl, args, range);
                if let Some(existing) = self.current_assoc_projections.as_ref().and_then(|ps| {
                    ps.iter()
                        .find(|p| p.name == assoc && p.args == args)
                        .map(|p| p.var)
                }) {
                    return Ty::Var(existing);
                }
                let fresh = self.counter.fresh();
                self.set_var_kind(fresh, Kind::Type);
                self.record_current_assoc_projection(fresh, assoc, args);
                return Ty::Var(fresh);
            }
            self.messages.push(Message::error(
                ErrorCode::GenericTypeError,
                format!(
                    "Cannot find associated type `{}` on trait `{}`",
                    assoc, owner
                ),
                range.clone(),
            ));
            return Ty::Var(self.counter.fresh());
        }

        // 2. Type parameter owner: `T::Elem` with `T: Collect`.
        for frame in self.type_params_in_scope.iter().rev() {
            if let Some(&owner_var) = frame.get(owner) {
                // Find an active constraint on this var whose class declares `assoc`.
                let mut matching_decl: Option<(String, AssocTypeDecl)> = None;
                for c in &self.active_constraints {
                    let covers = c.args.iter().any(
                        |a| matches!(apply_ty_prune(&self.subst, a), Ty::Var(v) if v == owner_var),
                    );
                    if !covers {
                        continue;
                    }
                    if let Some(cdef) = self.generics.typeclass(&c.class) {
                        if let Some(decl) = cdef.assoc_type(assoc) {
                            matching_decl = Some((c.class.clone(), decl.clone()));
                            break;
                        }
                        // Superclass assoc types (rare; check flattened supers).
                        for super_name in &cdef.superclasses {
                            if let Some(sdef) = self.generics.typeclass(super_name) {
                                if let Some(decl) = sdef.assoc_type(assoc) {
                                    matching_decl = Some((super_name.clone(), decl.clone()));
                                    break;
                                }
                            }
                        }
                        if matching_decl.is_some() {
                            break;
                        }
                    }
                }
                if let Some((class_name, decl)) = matching_decl {
                    self.validate_assoc_projection_args(&class_name, &decl, args, range);
                    let key = (owner_var, assoc.to_string(), self.projection_arg_key(args));
                    if let Some(&(existing, _)) = self.open_assoc_projections.get(&key) {
                        self.record_current_assoc_projection(existing, assoc, args);
                        return Ty::Var(existing);
                    }
                    let fresh = self.counter.fresh();
                    self.set_var_kind(fresh, Kind::Type);
                    self.open_assoc_projections
                        .insert(key, (fresh, args.to_vec()));
                    self.record_current_assoc_projection(fresh, assoc, args);
                    return Ty::Var(fresh);
                }
                self.messages.push(Message::error(
                    ErrorCode::GenericTypeError,
                    format!(
                        "Cannot project associated type `{}` from `{}` \
                         (no in-scope trait bound declares it)",
                        assoc, owner
                    ),
                    range.clone(),
                ));
                return Ty::Var(self.counter.fresh());
            }
        }

        // 3. Class-name owner outside definition: ground-only unique instance.
        if let Some(cdef) = self.generics.typeclass(owner).cloned() {
            let Some(decl) = cdef.assoc_type(assoc).cloned() else {
                self.messages.push(Message::error(
                    ErrorCode::GenericTypeError,
                    format!(
                        "Cannot find associated type `{}` on trait `{}`",
                        assoc, owner
                    ),
                    range.clone(),
                ));
                return Ty::Var(self.counter.fresh());
            };
            self.validate_assoc_projection_args(owner, &decl, args, range);
            let mut found: Option<Ty> = None;
            for inst in &self.generics.instances {
                if inst.class != owner {
                    continue;
                }
                if let Some(value) = inst.assoc_tys.get(assoc) {
                    let ty = self.instantiate_assoc_value(value, args);
                    if found.is_some() {
                        // Ambiguous across multiple instances — leave open.
                        found = None;
                        break;
                    }
                    found = Some(ty);
                }
            }
            if let Some(ty) = found {
                return ty;
            }
            // No unique ground instance — fresh var (caller may pin later).
            return Ty::Var(self.counter.fresh());
        }

        self.messages.push(Message::error(
            ErrorCode::GenericTypeError,
            format!("Cannot resolve type projection `{}::{}`", owner, assoc),
            range.clone(),
        ));
        Ty::Var(self.counter.fresh())
    }

    /// After discharging a ground (or unifying) instance, pin any open
    /// associated-type projections whose owner matches the instance args,
    /// and pin freshened assoc vars from trait method schemes.
    fn pin_assoc_types_for_instance(
        &mut self,
        class: &str,
        instance: &InstanceDef,
        scheme: Option<&Scheme>,
        range: &Range<usize>,
    ) {
        let Some(class_def) = self.generics.typeclass(class).cloned() else {
            return;
        };
        if class_def.assoc_types.is_empty() {
            return;
        }

        // Pin open `T::Elem` projections whose owner unifies with instance.args.
        let open_keys: Vec<((TyVarId, String, Vec<String>), (TyVarId, Vec<Ty>))> = self
            .open_assoc_projections
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        for ((owner_var, assoc_name, _arg_key), (assoc_var, assoc_args)) in open_keys {
            if class_def.assoc_type(&assoc_name).is_none() {
                continue;
            }
            // Owner must unify with the primary instance arg(s).
            let owner_ty = apply_ty_prune(&self.subst, &Ty::Var(owner_var));
            let matches_owner = instance
                .args
                .iter()
                .any(|arg| unify_with(&self.subst, &owner_ty, arg).is_ok());
            if !matches_owner {
                continue;
            }
            if let Some(value) = instance.assoc_tys.get(&assoc_name).cloned() {
                let concrete = self.instantiate_assoc_value(&value, &assoc_args);
                self.unify(
                    &Ty::Var(assoc_var),
                    &concrete,
                    range,
                    &format!("associated type `{}`", assoc_name),
                );
            }
        }

        let _ = scheme;
    }

    /// Pin freshened associated-type variables from a trait method
    /// scheme instantiation against a concrete instance.
    fn pin_assoc_vars_from_mapping(
        &mut self,
        class: &str,
        instance: &InstanceDef,
        scheme: &Scheme,
        mapping: &HashMap<TyVarId, TyVarId>,
        range: &Range<usize>,
    ) {
        if self.generics.typeclass(class).is_none() {
            return;
        }
        // Clone so unify can mutably borrow `self` in the loop.
        let pins: Vec<(String, TyVarId, Ty)> = scheme
            .assoc_projections
            .iter()
            .filter_map(|projection| {
                let &fresh = mapping.get(&projection.var)?;
                let value = instance.assoc_tys.get(&projection.name)?;
                let args = projection
                    .args
                    .iter()
                    .map(|arg| crate::typechecking::env::substitute_vars(arg, mapping))
                    .collect::<Vec<_>>();
                let concrete = self.instantiate_assoc_value(value, &args);
                Some((projection.name.clone(), fresh, concrete))
            })
            .collect();
        for (assoc_name, fresh, concrete) in pins {
            self.unify(
                &Ty::Var(fresh),
                &concrete,
                range,
                &format!("associated type `{}`", assoc_name),
            );
        }
    }

    /// Instantiate a scheme, returning the freshened type, constraints, and
    /// old→new bound-variable mapping (Phase 6 assoc pinning).
    fn instantiate_scheme_mapped(
        &mut self,
        scheme: &Scheme,
    ) -> (Ty, Vec<Constraint>, HashMap<TyVarId, TyVarId>) {
        use crate::typechecking::env::substitute_vars;
        let mut fresh_kinds = HashMap::new();
        let mapping: HashMap<TyVarId, TyVarId> = scheme
            .bounds
            .iter()
            .enumerate()
            .map(|(i, &v)| {
                let fresh = self.counter.fresh();
                fresh_kinds.insert(fresh, scheme.kind_at(i));
                (v, fresh)
            })
            .collect();
        self.var_kinds.extend(fresh_kinds);
        let ty = substitute_vars(&scheme.ty, &mapping);
        let constraints = scheme
            .constraints
            .iter()
            .map(|c| Constraint {
                class: c.class.clone(),
                args: c
                    .args
                    .iter()
                    .map(|a| substitute_vars(a, &mapping))
                    .collect(),
            })
            .collect();
        (ty, constraints, mapping)
    }

    /// Whether codegen should Ok-wrap bare returns for `fn_name`.
    pub fn fn_is_result_mode(&self, fn_name: &str) -> bool {
        self.result_mode_fns.contains(fn_name)
    }

    /// Whether `fn_name`'s Result Ok payload is itself a Result (nested).
    pub fn fn_result_ok_is_result(&self, fn_name: &str) -> bool {
        self.result_mode_ok_is_result.contains(fn_name)
    }

    fn note_result_mode_fn(&mut self, name: &str, ok: &Ty) {
        self.result_mode_fns.insert(name.to_string());
        let ok = apply_ty_prune(&self.subst, ok);
        if result_ok_err(&ok).is_some() {
            self.result_mode_ok_is_result.insert(name.to_string());
        }
    }

    /// Literal names of top-level `test("…") { … }` cases (source order).
    pub fn test_case_names(&self) -> &[String] {
        &self.test_case_names
    }

    /// Synthetic function name for the `n`-th harness test case.
    pub fn test_case_fn_name(index: usize) -> String {
        format!("__zs_test_{index}")
    }

    /// Whether `fn_name` returns (or was inferred to return) `Option<_>`.
    pub fn fn_is_option_mode(&self, fn_name: &str) -> bool {
        self.option_mode_fns.contains(fn_name)
    }

    /// Return the resolved result type of a registered function.
    pub fn fn_return_ty(&self, fn_name: &str) -> Option<Ty> {
        let scheme = self.env.lookup(fn_name)?;
        let mut ty = scheme.ty.clone();
        while let Ty::Fun(_, next) = ty {
            ty = *next;
        }
        Some(apply_ty_prune(&self.subst, &ty))
    }

    // ============================================================
    // ============================================================
    //  Native registration
    // ============================================================
    // ============================================================

    /// Register a native (built-in) function with the type system.
    ///
    /// `name` is the function's identifier as seen in user code;
    /// `params` are the parameter types in declaration order; `ret`
    /// is the return type. The signature is curried into a function
    /// type (`arg1 -> arg2 -> ... -> ret`).
    ///
    /// The binding is added to the top frame of the env so it's
    /// visible to every subsequent call. See [`Compiler::register`]
    /// for the public entry point.
    pub fn register_native(&mut self, name: &str, params: &[Ty], ret: &Ty) {
        let fn_ty = params.iter().rev().fold(ret.clone(), |acc, p| {
            Ty::Fun(Box::new(p.clone()), Box::new(acc))
        });

        self.env.insert_top(name.to_string(), Scheme::mono(fn_ty));
    }

    /// True when `expr` is a valid FFI type tag expression:
    /// `FFIType::X`, a bare primitive name, `[T]` / `(T, U)` (lowered to Ptr),
    /// or `FFIType::Struct` with aux id from a registered layout.
    fn is_ffi_type_expr(&self, expr: &Output) -> bool {
        self.ffi_type_tag_from_output(expr).is_some()
    }

    fn infer_ffi_dload(&mut self, args: &[Output], range: Range<usize>) -> Ty {
        if self.ffi_fn_in_scope("dload").is_none() {
            let _ = self.error_with_help(
                ErrorCode::UnknownValue,
                "Cannot find value `dload` in this scope".to_string(),
                range.clone(),
                Some("import it with `use ffi::{dload}`".to_string()),
            );
        }
        if let Some(path) = args.first() {
            let _ = self.infer(path);
        } else {
            let _ = self.error_with_help(
                ErrorCode::DeclareArity,
                "dload requires 1 argument (path)".to_string(),
                range,
                None,
            );
        }
        // dload → Result<int, Error>
        result_app_ty(int(), Ty::Con(common::BUILTIN_FFI_ERROR_ENUM.into()))
    }

    fn infer_ffi_declare(&mut self, args: &[Output], range: Range<usize>) -> Ty {
        if self.ffi_fn_in_scope("declare").is_none() {
            let _ = self.error_with_help(
                ErrorCode::UnknownValue,
                "Cannot find value `declare` in this scope".to_string(),
                range.clone(),
                Some("import it with `use ffi::{declare}`".to_string()),
            );
        }
        if args.len() == 4 || args.len() == 5 {
            self.infer(&args[0]);
            self.infer(&args[1]);
            match args[2].1.as_ref() {
                Expression::Tuple(_) => {
                    self.infer_ffi_type_expr(&args[2]);
                }
                _ => {
                    let mut m = Message::error(
                        ErrorCode::DeclareArity,
                        "declare(...) third argument must be an arguments tuple (T1, T2, ...)"
                            .to_string(),
                        args[2].0.into_range(),
                    );
                    m.push(Label::new(
                        "wrap the arg types in parentheses — (Int, Float) after `use ffi::types::{Int, Float, …}`"
                            .to_string(),
                        args[2].0.into_range(),
                    ));
                    self.messages.push(m);
                }
            }
            self.infer_ffi_type_expr(&args[3]);
            if args.len() == 5 {
                let flag_ty = self.infer(&args[4]);
                self.unify(
                    &flag_ty,
                    &boolean(),
                    &args[4].0.into_range(),
                    "declare variadic flag",
                );
            }
        } else {
            for arg in args {
                self.infer(arg);
            }
            let mut m = Message::error(
                ErrorCode::DeclareArity,
                "declare requires 4 or 5 arguments (lib, name, args_tuple, ret_type[, variadic])"
                    .to_string(),
                range.clone(),
            );
            m.push(Label::new(
                format!("got {} arguments", args.len()),
                range.clone(),
            ));
            self.messages.push(m);
        }
        // declare → Result<int, Error> (fn id or typed FFI error)
        result_app_ty(int(), Ty::Con(common::BUILTIN_FFI_ERROR_ENUM.into()))
    }

    fn qualified_class_field_key(class: &str, field: &str) -> String {
        format!("{class}::{field}")
    }

    fn ffi_param_invoke_key(fn_name: &str, param: &str) -> String {
        format!("{fn_name}::{param}")
    }

    fn declare_args_from_expr<'a>(expr: &'a Output<'a>) -> Option<&'a [Output<'a>]> {
        let init = unwrap_expr_wrappers(expr);
        let init = match init.1.as_ref() {
            Expression::Try(inner) => unwrap_expr_wrappers(inner),
            _ => init,
        };
        match init.1.as_ref() {
            Expression::Declare(dargs) => Some(dargs.as_slice()),
            Expression::Call { name: callee, args }
                if matches!(callee.1.as_ref(), Expression::Identifier("declare")) =>
            {
                args.as_deref()
            }
            _ => None,
        }
    }

    fn record_ffi_declare_metadata(
        &mut self,
        key: String,
        dargs: &[Output],
        store_field: bool,
    ) {
        if dargs.len() != 4 && dargs.len() != 5 {
            return;
        }
        let ret = self.ty_from_ffi_type_expr(&dargs[3]);
        let nfixed = match dargs[2].1.as_ref() {
            Expression::Tuple(items) => items.len(),
            _ => 0,
        };
        let variadic = if dargs.len() == 5 {
            matches!(dargs[4].1.as_ref(), Expression::Bool(true))
        } else {
            false
        };
        if store_field {
            self.ffi_fn_ret_by_field.insert(key.clone(), ret);
            self.ffi_fn_variadic_by_field.insert(key.clone(), variadic);
            self.ffi_fn_nfixed_by_field.insert(key, nfixed);
        } else {
            self.ffi_fn_ret_tys.insert(key.clone(), ret);
            self.ffi_fn_variadic.insert(key.clone(), variadic);
            self.ffi_fn_nfixed.insert(key, nfixed);
        }
    }

    fn class_name_for_field_receiver(&self, receiver: &Output) -> Option<String> {
        match receiver.1.as_ref() {
            Expression::Identifier(name) if *name == "self" => self.impl_owner.clone(),
            Expression::Identifier(name) => self
                .codegen_var_type(name)
                .cloned()
                .or_else(|| self.env.lookup(name).map(|s| s.ty.clone()))
                .and_then(|ty| self.class_owner_from_ty(&apply_ty_prune(&self.subst, &ty))),
            _ => None,
        }
    }

    fn ffi_invoke_fn_id_metadata(&self, expr: &Output) -> Option<(Ty, bool, usize)> {
        match expr.1.as_ref() {
            Expression::Identifier(name) => {
                if let Some(ty) = self.ffi_fn_ret_tys.get(*name) {
                    let variadic = self.ffi_fn_variadic.get(*name).copied().unwrap_or(false);
                    let nfixed = self.ffi_fn_nfixed.get(*name).copied().unwrap_or(0);
                    return Some((ty.clone(), variadic, nfixed));
                }
                if let Some(fn_name) = &self.current_function {
                    let key = Self::ffi_param_invoke_key(fn_name, name);
                    if let Some(&(ref ty, variadic, nfixed)) = self.ffi_fn_param_invoke_ret.get(&key)
                    {
                        return Some((ty.clone(), variadic, nfixed));
                    }
                }
                None
            }
            Expression::Access(receiver, field) => {
                let class = self.class_name_for_field_receiver(receiver)?;
                let key = Self::qualified_class_field_key(&class, field);
                let ret = self.ffi_fn_ret_by_field.get(&key)?.clone();
                let variadic = self
                    .ffi_fn_variadic_by_field
                    .get(&key)
                    .copied()
                    .unwrap_or(false);
                let nfixed = self.ffi_fn_nfixed_by_field.get(&key).copied().unwrap_or(0);
                Some((ret, variadic, nfixed))
            }
            _ => None,
        }
    }

    fn maybe_record_ffi_declare_for_field_assignment(&mut self, target: &Output, value: &Output) {
        let Expression::Access(receiver, field) = target.1.as_ref() else {
            return;
        };
        let Some(class) = self.class_name_for_field_receiver(receiver) else {
            return;
        };
        let Some(dargs) = Self::declare_args_from_expr(value) else {
            return;
        };
        let key = Self::qualified_class_field_key(&class, field);
        self.record_ffi_declare_metadata(key, dargs, true);
    }

    fn maybe_record_ffi_declare_for_let_init(&mut self, name: &str, init: &Output) {
        if let Some(dargs) = Self::declare_args_from_expr(init) {
            self.record_ffi_declare_metadata(name.to_string(), dargs, false);
            return;
        }
        if let Expression::Access(receiver, field) = init.1.as_ref() {
            if let Some(class) = self.class_name_for_field_receiver(receiver) {
                let key = Self::qualified_class_field_key(&class, field);
                if let Some(ret) = self.ffi_fn_ret_by_field.get(&key).cloned() {
                    let variadic = self
                        .ffi_fn_variadic_by_field
                        .get(&key)
                        .copied()
                        .unwrap_or(false);
                    let nfixed = self.ffi_fn_nfixed_by_field.get(&key).copied().unwrap_or(0);
                    self.ffi_fn_ret_tys.insert(name.to_string(), ret);
                    self.ffi_fn_variadic.insert(name.to_string(), variadic);
                    self.ffi_fn_nfixed.insert(name.to_string(), nfixed);
                }
            }
        }
    }

    fn record_ffi_param_invoke_flow(
        &mut self,
        fn_name: &str,
        param_names: &[String],
        arg_exprs: &[Output],
    ) {
        if arg_exprs.len() != param_names.len() {
            return;
        }
        for (param, arg) in param_names.iter().zip(arg_exprs.iter()) {
            let Some((ret, variadic, nfixed)) = self.ffi_invoke_fn_id_metadata(arg) else {
                continue;
            };
            let key = Self::ffi_param_invoke_key(fn_name, param);
            self.ffi_fn_param_invoke_ret
                .insert(key, (ret, variadic, nfixed));
        }
    }

    fn maybe_record_ffi_param_invoke_flow_for_call(
        &mut self,
        fn_name: &str,
        arg_exprs: &[Output],
    ) {
        let Some(param_names) = self.fn_param_names.get(fn_name).cloned() else {
            return;
        };
        self.record_ffi_param_invoke_flow(fn_name, &param_names, arg_exprs);
    }

    #[cfg(test)]
    pub(crate) fn test_ffi_param_invoke_ret(
        &self,
        key: &str,
    ) -> Option<&(Ty, bool, usize)> {
        self.ffi_fn_param_invoke_ret.get(key)
    }

    fn ffi_invoke_fn_id_metadata_prescan(
        &self,
        expr: &Output,
        local_class_scopes: &[HashMap<String, String>],
    ) -> Option<(Ty, bool, usize)> {
        match expr.1.as_ref() {
            Expression::Identifier(name) => {
                if let Some(ty) = self.ffi_fn_ret_tys.get(*name) {
                    let variadic = self.ffi_fn_variadic.get(*name).copied().unwrap_or(false);
                    let nfixed = self.ffi_fn_nfixed.get(*name).copied().unwrap_or(0);
                    return Some((ty.clone(), variadic, nfixed));
                }
                None
            }
            Expression::Access(receiver, field) => {
                let class = match receiver.1.as_ref() {
                    Expression::Identifier("self") => self.impl_owner.clone()?,
                    Expression::Identifier(name) => local_class_scopes
                        .iter()
                        .rev()
                        .find_map(|scope| scope.get(*name).cloned())?,
                    _ => return None,
                };
                let key = Self::qualified_class_field_key(&class, field);
                let ret = self.ffi_fn_ret_by_field.get(&key)?.clone();
                let variadic = self
                    .ffi_fn_variadic_by_field
                    .get(&key)
                    .copied()
                    .unwrap_or(false);
                let nfixed = self.ffi_fn_nfixed_by_field.get(&key).copied().unwrap_or(0);
                Some((ret, variadic, nfixed))
            }
            _ => None,
        }
    }

    fn record_ffi_param_invoke_flow_prescan(
        &mut self,
        fn_name: &str,
        param_names: &[String],
        arg_exprs: &[Output],
        local_class_scopes: &[HashMap<String, String>],
    ) {
        if arg_exprs.len() != param_names.len() {
            return;
        }
        for (param, arg) in param_names.iter().zip(arg_exprs.iter()) {
            let Some((ret, variadic, nfixed)) =
                self.ffi_invoke_fn_id_metadata_prescan(arg, local_class_scopes)
            else {
                continue;
            };
            let key = Self::ffi_param_invoke_key(fn_name, param);
            self.ffi_fn_param_invoke_ret
                .insert(key, (ret, variadic, nfixed));
        }
    }

    fn maybe_pre_record_field_declare(
        &mut self,
        target: &Output,
        value: &Output,
        local_class_scopes: &[HashMap<String, String>],
    ) {
        let Expression::Access(receiver, field) = target.1.as_ref() else {
            return;
        };
        let Some(dargs) = Self::declare_args_from_expr(value) else {
            return;
        };
        let class = match receiver.1.as_ref() {
            Expression::Identifier("self") => self.impl_owner.clone(),
            Expression::Identifier(name) => local_class_scopes
                .iter()
                .rev()
                .find_map(|scope| scope.get(*name).cloned()),
            _ => None,
        };
        let Some(class) = class else {
            return;
        };
        let key = Self::qualified_class_field_key(&class, field);
        self.record_ffi_declare_metadata(key, dargs, true);
    }

    fn infer_ffi_invoke(&mut self, args: &[Output], range: Range<usize>) -> Ty {
        if self.ffi_fn_in_scope("invoke").is_none() {
            let _ = self.error_with_help(
                ErrorCode::UnknownValue,
                "Cannot find value `invoke` in this scope".to_string(),
                range.clone(),
                Some("import it with `use ffi::{invoke}`".to_string()),
            );
        }
        let mut ret_ty = int();
        let mut variadic = false;
        let mut nfixed = 0usize;
        if args.len() == 3 {
            self.infer(&args[0]);
            self.infer(&args[1]);
            if let Some((ty, var, fixed)) = self.ffi_invoke_fn_id_metadata(&args[1]) {
                ret_ty = ty;
                variadic = var;
                nfixed = fixed;
            }
            match args[2].1.as_ref() {
                Expression::Tuple(items) => {
                    let mut tags = Vec::with_capacity(items.len());
                    for item in items {
                        let ty = self.infer(item);
                        tags.push(Self::ffi_tag_from_ty_static(&apply_ty_prune(
                            &self.subst,
                            &ty,
                        )));
                    }
                    if variadic {
                        if items.len() < nfixed {
                            let mut m = Message::error(
                                ErrorCode::InvokeArity,
                                format!(
                                    "variadic invoke expects at least {} argument(s), got {}",
                                    nfixed,
                                    items.len()
                                ),
                                args[2].0.into_range(),
                            );
                            m.push(Label::new(
                                "provide the fixed prefix plus any `...` arguments".to_string(),
                                args[2].0.into_range(),
                            ));
                            self.messages.push(m);
                        }
                        self.variadic_call_arg_tags
                            .insert((range.start, range.end), tags);
                    }
                }
                _ => {
                    let mut m = Message::error(
                        ErrorCode::InvokeArity,
                        "invoke(...) third argument must be an arguments tuple (v1, v2, ...)"
                            .to_string(),
                        args[2].0.into_range(),
                    );
                    m.push(Label::new(
                        "wrap the arg values in parentheses — (40, 2)".to_string(),
                        args[2].0.into_range(),
                    ));
                    self.messages.push(m);
                }
            }
        } else {
            for arg in args {
                self.infer(arg);
            }
            let mut m = Message::error(
                ErrorCode::InvokeArity,
                "invoke requires 3 arguments (lib, fn_id, args_tuple)".to_string(),
                range.clone(),
            );
            m.push(Label::new(
                format!("got {} arguments", args.len()),
                range.clone(),
            ));
            self.messages.push(m);
        }
        // invoke → Result<T, Error>
        result_app_ty(ret_ty, Ty::Con(common::BUILTIN_FFI_ERROR_ENUM.into()))
    }

    /// Resolve an FFI type expression to `(tag, aux)` for codegen.
    pub fn ffi_type_tag_from_output(&self, expr: &Output) -> Option<(u32, u32)> {
        use common::{tag, tag_from_type_name, tag_from_variant_name};
        match expr.1.as_ref() {
            Expression::Construct {
                enum_name,
                variant_name,
                ..
            } if common::is_builtin_ffi_enum(enum_name) => {
                // Qualified `ffi::types::Int` is always allowed. Legacy
                // `FFIType::Int` requires an explicit import binding.
                if *enum_name == common::BUILTIN_FFI_TYPE_ENUM
                    && !self.builtin_name_in_scope(common::BUILTIN_FFI_TYPE_ENUM)
                    && !self.ffi_tag_in_scope(variant_name)
                {
                    return None;
                }
                let tag = tag_from_variant_name(variant_name)?;
                Some((tag, 0))
            }
            Expression::Type(name) | Expression::Identifier(name) => {
                if let Some(id) = self.c_struct_id(name) {
                    return Some((tag::STRUCT, id));
                }
                // In-scope `use ffi::types::{…}` tags (`Int`, `Ptr`, …).
                if self.ffi_tag_in_scope(name) {
                    return tag_from_variant_name(name).map(|t| (t, 0));
                }
                // Bare lowercase primitives (`int`, `void`, …) stay
                // available without importing `ffi::types`.
                if name
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
                {
                    return tag_from_type_name(name).map(|t| (t, 0));
                }
                None
            }
            Expression::Array(items) if items.len() == 1 => Some((tag::PTR, 0)),
            Expression::Tuple(_) => Some((tag::PTR, 0)),
            _ => None,
        }
    }

    pub fn c_struct_id(&self, name: &str) -> Option<u32> {
        self.c_structs
            .iter()
            .position(|s| s.name == name)
            .map(|i| i as u32)
    }

    pub fn c_structs(&self) -> &[CStructDef] {
        &self.c_structs
    }

    pub fn callback_sigs(&self) -> &[CallbackSigDef] {
        &self.callback_sigs
    }

    /// Emit a diagnostic when `expr` is not a valid FFI type tag.
    fn require_ffi_type_expr(&mut self, expr: &Output) {
        if self.is_ffi_type_expr(expr) {
            return;
        }
        let mut m = Message::error(
            ErrorCode::InvalidFfiType,
            "Expected an FFI type tag".to_string(),
            expr.0.into_range(),
        );
        m.push(Label::new(
            "use `Int`/`Ptr` after `use ffi::types::{Int, Ptr, …}`, a bare type name (int, void, …), [T], (T, U), or a declared extern struct".to_string(),
            expr.0.into_range(),
        ));
        self.messages.push(m);
    }

    /// Infer an FFI type-tag expression (declare arg/ret positions).
    ///
    /// Consumes NodeIds in pre-walk order without treating bare names
    /// like `Point` / `int32` as value lookups. Nested Tuple / Array /
    /// Construct children are walked the same way (or via normal
    /// `infer` for `FFIType::X` constructors, which are real enum
    /// constructs).
    fn infer_ffi_type_expr(&mut self, expr: &Output) {
        self.require_ffi_type_expr(expr);
        match expr.1.as_ref() {
            Expression::Identifier(_) | Expression::Type(_) => {
                let id = self.ids.ids()[self.next_id_idx];
                self.next_id_idx += 1;
                let ty = self.ty_from_ffi_type_expr(expr);
                self.cache.insert(id, ty);
            }
            Expression::Tuple(items) => {
                let id = self.ids.ids()[self.next_id_idx];
                self.next_id_idx += 1;
                self.cache.insert(id, unit_ty());
                for item in items {
                    self.infer_ffi_type_expr(item);
                }
            }
            Expression::Array(items) => {
                let id = self.ids.ids()[self.next_id_idx];
                self.next_id_idx += 1;
                self.cache.insert(id, unit_ty());
                for item in items {
                    // Element annotations are `Type` / nested forms.
                    self.infer_ffi_type_expr(item);
                }
            }
            // `FFIType::Int`, etc. — real Construct nodes; use normal infer
            // so enum constructor typing + child IDs stay aligned.
            _ => {
                let _ = self.infer(expr);
            }
        }
    }

    /// Map an FFI type tag expression to the language `Ty` used for
    /// `invoke` result typing (void → unit, structs → structural record).
    fn ty_from_ffi_type_expr(&self, expr: &Output) -> Ty {
        use common::tag;
        match self.ffi_type_tag_from_output(expr) {
            Some((t, _)) if t == tag::VOID => unit_ty(),
            Some((t, _)) if t == tag::FLOAT => float(),
            Some((t, _)) if t == tag::STRING => string(),
            Some((t, _)) if t == tag::BOOL => boolean(),
            Some((t, id)) if t == tag::STRUCT => {
                if let Some(def) = self.c_structs.get(id as usize) {
                    let fields = def
                        .fields
                        .iter()
                        .map(|(name, enc)| {
                            let tag = if *enc <= tag::STRUCT {
                                *enc
                            } else {
                                *enc & 0xFFFF
                            };
                            let fty = match tag {
                                t if t == tag::FLOAT => float(),
                                t if t == tag::STRING => string(),
                                t if t == tag::BOOL => boolean(),
                                t if t == tag::VOID => unit_ty(),
                                // int / int32 / ptr / … — treat as int at the
                                // language level (narrow C widths are ABI-only).
                                _ => int(),
                            };
                            (name.clone(), fty)
                        })
                        .collect();
                    crate::typechecking::ty::record(fields)
                } else {
                    int()
                }
            }
            _ => int(),
        }
    }

    // ============================================================

    /// Register a class: store its name and the (visibility, name,
    /// type) of each field. The class itself becomes a `Ty::Con(key)`
    /// constructor (`module::Name`, or `Name` in the entry file) so
    /// later files can `use` it without colliding on the short name.
    ///
    /// Generic classes (`class Cell<T>`) store field types with
    /// `Con(param)` schema markers (schemaized from the in-scope type
    /// param vars) so each `new` site can freshen independently.
    fn register_class(&mut self, name: &str, fields: &[Output], range: &Range<usize>) {
        let key = self.qualify_module_name(name);
        // Bind the FQN before parsing fields so recursive types
        // (`next: Option<Node<T>>`) resolve to `module::Node`, not a dummy Con.
        self.classes.entry(key.clone()).or_insert_with(Vec::new);
        if !self.class_type_ids.contains_key(&key) {
            let id = self.next_class_type_id;
            self.next_class_type_id = self.next_class_type_id.saturating_add(1);
            if self.next_class_type_id == 0 {
                self.next_class_type_id = 1;
            }
            self.class_type_ids.insert(key.clone(), id);
        }
        self.generics
            .register_nominal_type(&key, &self.current_module);
        self.env
            .insert_top(key.clone(), Scheme::mono(Ty::Con(key.clone())));
        let mut field_info = Vec::new();
        for field in fields {
            if let Expression::Field {
                docs: _,
                visibility: vis,
                modifier,
                name: fname,
                ty: fty,
                init,
            } = field.1.as_ref()
            {
                let fname_str = match fname.1.as_ref() {
                    Expression::Identifier(n) => n.to_string(),
                    _ => {
                        self.messages.push(Message::error(
                            ErrorCode::GenericTypeError,
                            "Invalid field name".to_string(),
                            field.0.into_range(),
                        ));
                        continue;
                    }
                };
                let ty = self.parse_type_name(fty);
                if matches!(modifier, FieldModifier::Static) {
                    if init.is_none() {
                        self.messages.push(Message::error(
                            ErrorCode::GenericTypeError,
                            format!(
                                "Static field `{}` in class `{}` requires an initializer",
                                fname_str, name
                            ),
                            field.0.into_range(),
                        ));
                        continue;
                    }
                    let fqn = format!("{}::{}", key, fname_str);
                    self.register_static_slot(fqn, false, ty.clone(), field.0.into_range());
                    if let Some(init_expr) = init {
                        let init_ty = self.infer(init_expr);
                        self.coerce_or_unify(
                            &ty,
                            &init_ty,
                            Some(init_expr),
                            &field.0.into_range(),
                            "static field initializer",
                        );
                    }
                    continue;
                }
                if matches!(modifier, FieldModifier::Const) {
                    self.const_class_fields
                        .entry(key.clone())
                        .or_default()
                        .insert(fname_str.clone());
                }
                field_info.push((*vis, fname_str, ty));
            } else {
                self.messages.push(Message::error(
                    ErrorCode::GenericTypeError,
                    "Expected a field declaration".to_string(),
                    field.0.into_range(),
                ));
            }
        }
        // Schemaize param vars → `Con(name)` for generic class fields.
        if let Some(frame) = self.type_params_in_scope.last() {
            if !frame.is_empty() {
                let var_to_name: HashMap<TyVarId, String> =
                    frame.iter().map(|(n, id)| (*id, n.clone())).collect();
                for (_, _, ty) in &mut field_info {
                    *ty = schemaize_ty(ty, &var_to_name);
                }
            }
        }
        self.classes.insert(key, field_info);
        let _ = range;
    }

    /// Process an `impl Owner` / `impl Owner<T>` block:
    /// 1. Auto-register the owner class if it hasn't been declared
    ///    yet (so `impl` can appear before `class`).
    /// 2. Push a type-param scope and bind `self : Owner` or
    ///    `self : Owner<T, …>`.
    /// 3. For each method, run [`infer_function`] with `self`
    ///    prepended to the argument list, then store the method's
    ///    scheme (poly when the impl is generic) under the owner's name.
    fn infer_impl(
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
                    self.register_overload_candidate(
                        &fqn,
                        OverloadCandidate {
                            id: 0,
                            fixed_arity,
                            is_rest: has_rest,
                            scheme: scheme.clone(),
                            param_names,
                        },
                        &method.0.into_range(),
                    );
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

    fn check_drop_decl(
        &mut self,
        what: &str,
        owner_key: &str,
        owner_is_class: bool,
        is_static: bool,
        args: &Output,
        range: &Range<usize>,
    ) {
        let arity = match args.1.as_ref() {
            Expression::Fragment(items) => items
                .iter()
                .filter(|a| matches!(a.1.as_ref(), Expression::Argument { .. }))
                .count(),
            _ => 0,
        };
        let msg = if !what.is_empty() {
            Some("fn drop(self) is only allowed on inherent class impls, not trait instances")
        } else if !owner_is_class {
            Some("fn drop(self) is only allowed on nominal classes")
        } else if is_static {
            Some("fn drop must take self by value; static drop is not allowed")
        } else if arity != 0 {
            Some("fn drop(self) must have no extra parameters")
        } else if !self.classes_with_drop.insert(owner_key.to_string()) {
            Some("duplicate fn drop(self) for this class")
        } else {
            None
        };
        if let Some(msg) = msg {
            self.messages.push(Message::error(
                ErrorCode::InvalidDrop,
                msg.to_string(),
                range.clone(),
            ));
        }
    }

    /// Look up a class field, substituting type-param placeholders when
    /// the receiver is an applied generic class (`Cell<int>`).
    fn access_class_field(
        &mut self,
        class: &str,
        field: &str,
        args: &[Ty],
        range: Range<usize>,
    ) -> Ty {
        let Some(fields) = self.classes.get(class) else {
            return self.error(
                ErrorCode::UnknownField,
                format!("Cannot find field `{}` on class `{}`", field, class),
                range,
            );
        };
        let Some((_, _, fty)) = fields.iter().find(|(_, fname, _)| fname == field) else {
            let known: Vec<&str> = fields.iter().map(|(_, n, _)| n.as_str()).collect();
            return self.error_with_help(
                ErrorCode::UnknownField,
                format!("Cannot find field `{}` on class `{}`", field, class),
                range,
                Some(format!("the class has fields: {}", known.join(", "))),
            );
        };
        let fty = fty.clone();
        let params = self
            .generics
            .generic_type_ctors
            .get(class)
            .cloned()
            .unwrap_or_default();
        if params.is_empty() {
            return fty;
        }
        let mut map = HashMap::new();
        if args.is_empty() {
            for p in &params {
                map.insert(p.clone(), Ty::Var(self.counter.fresh()));
            }
        } else {
            for (p, a) in params.iter().zip(args.iter()) {
                map.insert(p.clone(), a.clone());
            }
        }
        subst_ty_params(&fty, &map)
    }

    // ============================================================
    //  Test harness cases
    // ============================================================

    /// Typecheck `test("desc") { body }` — name must be a string literal;
    /// body runs in Result<(), string> mode.
    fn infer_test_case(&mut self, name: &Output, body: &Output, range: &Range<usize>) -> Ty {
        let name_ty = self.infer(name);
        self.unify(&name_ty, &string(), &name.0.into_range(), "test case name");

        let desc = match unwrap_expr_wrappers(name).1.as_ref() {
            Expression::String(s) => (*s).to_string(),
            _ => {
                let _ = self.error_with_help(
                    ErrorCode::GenericTypeError,
                    "test case name must be a string literal".into(),
                    name.0.into_range(),
                    Some("write `test(\"description\") { … }`".into()),
                );
                format!("test_{}", self.test_case_names.len())
            }
        };
        let case_index = self.test_case_names.len();
        self.test_case_names.push(desc);
        let fn_name = Self::test_case_fn_name(case_index);

        let prev_result_mode = self.fn_result_mode.take();
        let prev_option_mode = self.fn_option_mode.take();
        let prev_ret = self.current_return_ty.take();
        self.fn_result_mode = Some((unit_ty(), string()));
        self.current_return_ty = Some(unit_ty());

        self.push_scope();
        let _ = self.infer(body);
        self.pop_scope();

        self.result_mode_fns.insert(fn_name);
        self.current_return_ty = prev_ret;
        self.fn_result_mode = prev_result_mode;
        self.fn_option_mode = prev_option_mode;

        let _ = range;
        unit_ty()
    }

    // ============================================================
    //  Functions (monomorphic recursion)
    // ============================================================

    fn infer_function(
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
                Some("add a block `{ … }` or use `#[ffi(...)]` for foreign declarations".into()),
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
        // Free-fn arg NodeId assign deferred (Hash / constraint-kind). Lambdas
        // call `assign_fn_arg_node_ids` at their infer site.
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
                // Unit / open vars may fall through (codegen emits a unit
                // epilogue with defers). Concrete non-unit returns must exit.
                let allow_fallthrough = matches!(&ret, Ty::Var(_))
                    || matches!(&ret, Ty::Con(n) if n == "unknown")
                    || matches!(&ret, Ty::Never)
                    || matches!(&ret, Ty::Con(n) if n == crate::typechecking::ty::UNIT)
                    || matches!(&ret, Ty::Tuple(items) if items.is_empty())
                    || (self.fn_result_mode.is_some()
                        && result_ok_err(&ret)
                            .map(|(ok, _)| {
                                let ok = apply_ty_prune(&self.subst, &ok);
                                matches!(&ok, Ty::Var(_))
                                    || matches!(&ok, Ty::Con(n) if n == crate::typechecking::ty::UNIT)
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

        if !is_generic {
            let resolved = apply_ty_prune(&self.subst, &fun_ty);
            if let Some(owner) = method_owner {
                self.env
                    .insert_top(format!("{owner}::{name}"), Scheme::mono(resolved));
            } else {
                self.env
                    .insert_top(name.to_string(), Scheme::mono(resolved));
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
                self.env.insert_top(fqn.clone(), scheme);
            }

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
        let candidates = self.overload_sets.entry(key.to_string()).or_default();
        new_candidate.id = candidates.len() as u32;
        let mut conflict = false;
        for existing in candidates.iter() {
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
            self.overload_decl_by_span.insert(
                (range.start, range.end),
                (
                    new_candidate.id,
                    new_candidate.fixed_arity,
                    new_candidate.is_rest,
                ),
            );
            candidates.push(new_candidate);
        }
    }

    /// True when two function schemes have unifiable parameter lists (same shape).
    fn schemes_params_overlap(a: &Scheme, b: &Scheme) -> bool {
        let pa = Self::fun_param_tys(&a.ty);
        let pb = Self::fun_param_tys(&b.ty);
        if pa.len() != pb.len() {
            return false;
        }
        let empty = crate::typechecking::subst::Subst::default();
        pa.iter()
            .zip(pb.iter())
            .all(|(x, y)| crate::typechecking::unify::unify_with(&empty, x, y).is_ok())
    }

    /// Peel `a -> b -> … -> r` into parameter types (left-to-right).
    fn fun_param_tys(ty: &Ty) -> Vec<Ty> {
        let mut params = Vec::new();
        let mut cur = ty;
        while let Ty::Fun(arg, ret) = cur {
            params.push((**arg).clone());
            cur = ret;
        }
        params
    }

    /// Human-readable overload signature for diagnostics: `(int, int)` or `(int…)+ (rest)`.
    fn overload_sig_label(c: &OverloadCandidate) -> String {
        let params = Self::fun_param_tys(&c.scheme.ty);
        let sig = params
            .iter()
            .map(|t| t.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        if c.is_rest {
            format!("({})+ (rest)", sig)
        } else {
            format!("({})", sig)
        }
    }

    /// Parse `fn(T x, ...args) -> R` function types.
    fn parse_fn_sig_type(&mut self, params: &Output, ret: &Output) -> Ty {
        // Sole bare `...args` in a fn type: opaque callable unified at spread calls.
        if let Expression::Fragment(children) = params.1.as_ref() {
            if children.len() == 1 {
                if let Expression::Argument {
                    ty: None,
                    is_rest: true,
                    ..
                } = children[0].1.as_ref()
                {
                    return Ty::Var(self.counter.fresh());
                }
            }
        }
        let param_tys = self.parse_arg_list(params);
        let ret_ty = self.parse_type_name(ret);
        let mut fun_ty = ret_ty;
        for (_, arg_ty) in param_tys.iter().rev() {
            fun_ty = Ty::Fun(Box::new(arg_ty.clone()), Box::new(fun_ty));
        }
        fun_ty
    }

    fn tuple_pack_ty_for_args(args: &Output, counter: &mut TyVarCounter) -> Option<Ty> {
        if let Expression::Fragment(children) = args.1.as_ref() {
            if children
                .last()
                .is_some_and(|c| {
                    matches!(
                        c.1.as_ref(),
                        Expression::Argument { ty: None, is_rest: true, .. }
                    )
                })
            {
                return Some(Ty::Var(counter.fresh()));
            }
        }
        None
    }

    fn try_infer_spread_call_target(
        &mut self,
        callee: &str,
        pack: &Output,
        range: &Range<usize>,
        id: Option<NodeId>,
    ) -> Option<Ty> {
        let scheme = self.env.lookup(callee)?.clone();
        let (fun_ty, fresh_constraints, fresh_mapping) = self.instantiate_scheme_mapped(&scheme);
        let pack_ty = self.infer(pack);
        let resolved_pack = apply_ty_prune(&self.subst, &pack_ty);
        let elems = match resolved_pack {
            Ty::Tuple(elems) => elems,
            Ty::Var(_) => {
                let ret = self
                    .current_return_ty
                    .clone()
                    .map(|t| apply_ty_prune(&self.subst, &t))
                    .unwrap_or_else(|| Ty::Var(self.counter.fresh()));
                if let Some(call_id) = id {
                    self.cache.insert(call_id, ret.clone());
                }
                return Some(ret);
            }
            _ => return None,
        };
        let mut fun = apply_ty_prune(&self.subst, &fun_ty);
        for elem in &elems {
            let Ty::Fun(arg, ret) = fun else {
                return None;
            };
            self.unify(&arg, elem, range, "spread call argument");
            fun = apply_ty_prune(&self.subst, &*ret);
        }
        self.spread_call_arity
            .insert((range.start, range.end), elems.len());
        if let Some(call_id) = id {
            self.cache.insert(call_id, fun.clone());
        }
        if !fresh_constraints.is_empty() {
            self.discharge_constraints(id, &fresh_constraints, range);
            self.pin_assoc_after_discharge(
                "",
                &fresh_constraints,
                Some(&scheme),
                &fresh_mapping,
                range,
            );
        }
        Some(fun)
    }

    pub fn spread_call_arity_at(&self, start: usize, end: usize) -> Option<usize> {
        self.spread_call_arity.get(&(start, end)).copied()
    }

    /// Assign NodeIds for a function/lambda parameter list so body infer
    /// stays lockstep with codegen.
    ///
    /// Codegen's `do_compile(args)` consumes the `Fragment` id and each
    /// `Argument` id, but does **not** walk type-annotation children.
    /// Identifier codegen still uses `codegen_var_types` (no span prefer).
    fn assign_fn_arg_node_ids(&mut self, args: &Output, arg_tys: &[(String, Ty)]) {
        match args.1.as_ref() {
            Expression::Fragment(children) => {
                if self.next_id_idx >= self.ids.ids().len() {
                    return;
                }
                let frag_id = self.ids.ids()[self.next_id_idx];
                self.next_id_idx += 1;
                self.cache.insert(frag_id, unit_ty());

                let mut ty_idx = 0usize;
                for child in children {
                    if self.next_id_idx >= self.ids.ids().len() {
                        break;
                    }
                    let id = self.ids.ids()[self.next_id_idx];
                    self.next_id_idx += 1;
                    let ty = if let Expression::Argument { .. } = child.1.as_ref() {
                        let t = arg_tys
                            .get(ty_idx)
                            .map(|(_, t)| t.clone())
                            .unwrap_or_else(unit_ty);
                        ty_idx += 1;
                        t
                    } else {
                        unit_ty()
                    };
                    self.cache.insert(id, ty.clone());
                    self.codegen_types_by_span
                        .entry((child.0.start, child.0.end))
                        .or_insert(ty);
                }
            }
            _ => {
                if self.next_id_idx < self.ids.ids().len() {
                    let id = self.ids.ids()[self.next_id_idx];
                    self.next_id_idx += 1;
                    let ty = arg_tys
                        .first()
                        .map(|(_, t)| t.clone())
                        .unwrap_or_else(unit_ty);
                    self.cache.insert(id, ty);
                }
            }
        }
    }

    /// Parse a function's argument list (a `Fragment` of
    /// `Argument(ty, name, is_rest)` nodes). Rest params become `[T]` or a
    /// heterogeneous tuple for bare `... name`.
    fn parse_arg_list(&mut self, args: &Output) -> Vec<(String, Ty)> {
        let mut out = Vec::new();
        if let Expression::Fragment(children) = args.1.as_ref() {
            let n = children.len();
            for (i, child) in children.iter().enumerate() {
                if let Expression::Argument {
                    ty,
                    name,
                    is_rest,
                    ..
                } = child.1.as_ref()
                {
                    if *is_rest {
                        if i + 1 != n {
                            let mut msg = Message::error(
                                ErrorCode::GenericTypeError,
                                format!("Rest parameter `{}` must be the last parameter", name),
                                child.0.into_range(),
                            );
                            msg.with_help(
                                "write fixed parameters first, then `T... name` or `... name`"
                                    .to_string(),
                            );
                            self.messages.push(msg);
                        }
                        if ty.is_none() {
                            let pack = self
                                .current_tuple_pack
                                .clone()
                                .unwrap_or_else(|| Ty::Var(self.counter.fresh()));
                            out.push((name.to_string(), pack));
                        } else {
                            let elem =
                                self.parse_return_type_name(ty.as_ref().expect("typed rest"));
                            out.push((name.to_string(), vec_app_ty(elem)));
                        }
                    } else {
                        out.push((
                            name.to_string(),
                            self.parse_return_type_name(ty.as_ref().expect("fixed param type")),
                        ));
                    }
                }
            }
        }
        out
    }

    /// Declaration-order parameter names for `fn_name`, if recorded.
    pub fn fn_param_names(&self, fn_name: &str) -> Option<&[String]> {
        self.fn_param_names.get(fn_name).map(|v| v.as_slice())
    }

    /// Whether `fn_name` has a trailing rest parameter (`T... name` or `... name`).
    pub fn fn_has_rest(&self, fn_name: &str) -> bool {
        self.fn_has_rest.get(fn_name).copied().unwrap_or(false)
    }

    /// Whether `fn_name`'s trailing rest is a heterogeneous tuple pack (`... name`).
    pub fn fn_tuple_rest(&self, fn_name: &str) -> bool {
        self.fn_tuple_rest.get(fn_name).copied().unwrap_or(false)
    }

    /// All registered overload candidates for `fn_name`, if any.
    pub fn overload_candidates(&self, fn_name: &str) -> Option<&[OverloadCandidate]> {
        self.overload_sets
            .get(fn_name)
            .or_else(|| {
                if self.current_module.is_empty() {
                    None
                } else {
                    self.overload_sets
                        .get(&format!("{}::{}", self.current_module, fn_name))
                }
            })
            .map(|v| v.as_slice())
    }

    /// True when `fn_name` has more than one overload candidate.
    pub fn is_overloaded(&self, fn_name: &str) -> bool {
        self.overload_candidates(fn_name)
            .map_or(false, |v| v.len() > 1)
    }

    /// Select an overload by arity only (no argument types).
    ///
    /// Returns [`None`] on no match **or** ambiguity — callers that need to
    /// distinguish those cases should use [`Self::select_overload_for_args`].
    pub fn select_overload(&self, fn_name: &str, argc: usize) -> Option<&OverloadCandidate> {
        match self.select_overload_for_args(fn_name, argc, &[]) {
            OverloadSelect::Selected(c) => Some(c),
            OverloadSelect::NoMatch | OverloadSelect::Ambiguous => None,
        }
    }

    /// Like [`Self::select_overload`], but disambiguate same-arity candidates
    /// with `arg_tys` (left-to-right parameter positions).
    ///
    /// Empty `arg_tys` only succeeds when a single candidate matches `argc`.
    pub fn select_overload_for_args(
        &self,
        fn_name: &str,
        argc: usize,
        arg_tys: &[Ty],
    ) -> OverloadSelect<'_> {
        let Some(candidates) = self.overload_candidates(fn_name) else {
            return OverloadSelect::NoMatch;
        };
        let arity_ok: Vec<&OverloadCandidate> = candidates
            .iter()
            .filter(|c| {
                if c.is_rest {
                    c.fixed_arity <= argc
                } else {
                    c.fixed_arity == argc
                }
            })
            .collect();
        if arity_ok.is_empty() {
            return OverloadSelect::NoMatch;
        }
        let fixed: Vec<&OverloadCandidate> =
            arity_ok.iter().copied().filter(|c| !c.is_rest).collect();
        let pool: Vec<&OverloadCandidate> = if !fixed.is_empty() {
            fixed
        } else {
            arity_ok
        };
        if pool.len() == 1 {
            return OverloadSelect::Selected(pool[0]);
        }
        if arg_tys.is_empty() {
            return OverloadSelect::Ambiguous;
        }
        let empty = crate::typechecking::subst::Subst::default();
        let mut matches: Vec<&OverloadCandidate> = Vec::new();
        for c in &pool {
            let params = Self::fun_param_tys(&c.scheme.ty);
            let n = params.len().min(arg_tys.len());
            let ok = (0..n).all(|i| {
                crate::typechecking::unify::unify_with(&empty, &params[i], &arg_tys[i]).is_ok()
            });
            if ok {
                matches.push(*c);
            }
        }
        match matches.len() {
            0 => OverloadSelect::NoMatch,
            1 => OverloadSelect::Selected(matches[0]),
            _ => {
                // Prefer a unique all-concrete candidate over generics.
                let concrete: Vec<_> = matches
                    .iter()
                    .copied()
                    .filter(|c| {
                        Self::fun_param_tys(&c.scheme.ty)
                            .iter()
                            .all(|t| matches!(t, Ty::Con(_)))
                    })
                    .collect();
                if concrete.len() == 1 {
                    OverloadSelect::Selected(concrete[0])
                } else {
                    OverloadSelect::Ambiguous
                }
            }
        }
    }

    fn ambiguous_overload_help(&self, fn_name: &str) -> String {
        let available: Vec<String> = self
            .overload_candidates(fn_name)
            .map(|cs| cs.iter().map(|c| Self::overload_sig_label(c)).collect())
            .unwrap_or_default();
        format!(
            "available overloads: {}; arguments do not uniquely select one",
            available.join(", ")
        )
    }

    /// The call-site selection result for the call spanning `(start, end)`.
    ///
    /// Returns `(fixed_arity, is_rest, candidate_id)` of the chosen candidate.
    pub fn selected_overload_at(&self, start: usize, end: usize) -> Option<(usize, bool, u32)> {
        self.selected_overloads_by_span.get(&(start, end)).copied()
    }

    /// Declaration-site overload identity for mangling the function table key.
    pub fn overload_decl_at(&self, start: usize, end: usize) -> Option<(u32, usize, bool)> {
        self.overload_decl_by_span.get(&(start, end)).copied()
    }

    /// Fill bitmask for a partial-application call site, if any.
    pub fn partial_fill_at(&self, start: usize, end: usize) -> Option<u32> {
        self.partial_fills_by_span.get(&(start, end)).copied()
    }

    /// Expand `...expr` spread nodes in a call argument list using inferred
    /// tuple/array types. Returns a flat argument list for arity checking.
    pub fn flatten_spread_call_args<'a>(&mut self, args: &[Output<'a>]) -> Vec<Output<'a>> {
        let mut out = Vec::new();
        for arg in args {
            if let Expression::Spread(inner) = arg.1.as_ref() {
                // Consume the `Spread` node's pre-walk ID (call sites flatten
                // instead of inferring `Expression::Spread` directly).
                if self.next_id_idx < self.ids.ids().len() {
                    self.next_id_idx += 1;
                }
                let ty = self.infer(inner);
                let resolved = apply_ty_prune(&self.subst, &ty);
                match resolved {
                    Ty::Tuple(elems) => {
                        self.spread_expanded_bases
                            .insert((inner.0.start, inner.0.end));
                        for i in 0..elems.len() {
                            out.push(self.synthetic_index(inner, i as i64));
                        }
                    }
                    Ty::Array { element: _, length } => {
                        if let ArrayLength::Static(n) = length {
                            self.spread_expanded_bases
                                .insert((inner.0.start, inner.0.end));
                            for i in 0..n {
                                out.push(self.synthetic_index(inner, i as i64));
                            }
                        } else {
                            let _ = self.error_with_help(
                                ErrorCode::GenericTypeError,
                                "cannot spread dynamic-length array".to_string(),
                                arg.0.into_range(),
                                Some(
                                    "use a tuple or a fixed-length array `[T; N]` at the spread site"
                                        .to_string(),
                                ),
                            );
                        }
                    }
                    _ => {
                        let _ = self.error_with_help(
                            ErrorCode::GenericTypeError,
                            format!("cannot spread value of type `{}`", resolved),
                            arg.0.into_range(),
                            Some("spread requires a tuple or array value".to_string()),
                        );
                    }
                }
            } else {
                out.push(arg.clone());
            }
        }
        out
    }

    fn synthetic_index<'a>(&mut self, base: &Output<'a>, idx: i64) -> Output<'a> {
        let span = base.0;
        (
            span,
            Box::new(Expression::Index(
                base.clone(),
                Some((span, Box::new(Expression::Integer(idx)))),
            )),
        )
    }

    fn infer_spread_index_elem(&mut self, target: &Output, index_expr: &Output) -> Ty {
        let target_ty = self
            .codegen_types_by_span
            .get(&(target.0.start, target.0.end))
            .cloned()
            .unwrap_or_else(|| self.infer_inner(target, None));
        let resolved = apply_ty_prune(&self.subst, &target_ty);
        let idx = match index_expr.1.as_ref() {
            Expression::Integer(i) => *i,
            _ => {
                return self.error(
                    ErrorCode::GenericTypeError,
                    "spread-expanded index must be a literal".to_string(),
                    index_expr.0.into_range(),
                );
            }
        };
        match resolved {
            Ty::Tuple(elems) => elems
                .get(idx as usize)
                .cloned()
                .unwrap_or_else(|| Ty::Var(self.counter.fresh())),
            Ty::Array {
                element,
                length: ArrayLength::Static(n),
            } => {
                if (idx as usize) < n {
                    element.as_ref().clone()
                } else {
                    Ty::Var(self.counter.fresh())
                }
            }
            _ => Ty::Var(self.counter.fresh()),
        }
    }

    /// Infer a call argument, skipping ID consumption for spread-expanded indices.
    ///
    /// Clears `current_expected` so an outer context (e.g. `return` typed as
    /// `[byte]`) does not coerce string/int literals inside the argument list
    /// before parameter types are applied via `coerce_or_unify`.
    fn infer_call_arg(&mut self, arg: &Output) -> Ty {
        if let Expression::Index(target, Some(index_expr)) = arg.1.as_ref() {
            if self
                .spread_expanded_bases
                .contains(&(target.0.start, target.0.end))
            {
                let ty = self.infer_spread_index_elem(target, index_expr);
                self.codegen_types_by_span
                    .entry((arg.0.start, arg.0.end))
                    .or_insert_with(|| ty.clone());
                return ty;
            }
        }
        let prev_expected = self.current_expected.take();
        let ty = self.infer(arg);
        self.current_expected = prev_expected;
        ty
    }

    /// Infer call arguments, reordering named args and packing rest.
    ///
    /// Named arguments may under-apply: omitted fixed parameters become
    /// holes and the call type is a residual `Fun` (partial application).
    /// Rest parameters are positional-only at the call site and pack into
    /// a single `[T]` (empty when no trailing args). Returns
    /// `(arg_tys, ordered_value_exprs)` in declaration order; for rest
    /// functions the last "expr" is a synthetic stand-in when packing
    /// (callers that need element exprs use codegen's split helper).
    fn infer_and_reorder_call_args<'a>(
        &mut self,
        fn_name: &str,
        args: &'a [Output<'a>],
        range: &Range<usize>,
    ) -> (Vec<Ty>, Vec<Output<'a>>) {
        let flat_args: Vec<Output<'a>> = self.flatten_spread_call_args(args);
        let args = flat_args.as_slice();
        let has_named = args
            .iter()
            .any(|a| matches!(a.1.as_ref(), Expression::NamedArg(..)));
        let has_rest = self.fn_has_rest.get(fn_name).copied().unwrap_or(false);

        if !has_named && !has_rest {
            let mut tys = Vec::with_capacity(args.len());
            let mut exprs = Vec::with_capacity(args.len());
            for arg in args {
                tys.push(self.infer_call_arg(arg));
                exprs.push(arg.clone());
            }
            self.maybe_record_ffi_param_invoke_flow_for_call(fn_name, &exprs);
            return (tys, exprs);
        }

        let Some(param_names) = self.fn_param_names.get(fn_name).cloned() else {
            if has_named {
                let mut msg = Message::error(
                    ErrorCode::GenericTypeError,
                    format!(
                        "Named arguments are not supported on this call to `{}`",
                        fn_name
                    ),
                    range.clone(),
                );
                msg.with_help(
                    "named arguments require a known function with declared parameter names"
                        .to_string(),
                );
                self.messages.push(msg);
            }
            let mut tys = Vec::with_capacity(args.len());
            let mut exprs = Vec::with_capacity(args.len());
            for arg in args {
                tys.push(self.infer_call_arg(arg));
                exprs.push(match arg.1.as_ref() {
                    Expression::NamedArg(_, v) => v.clone(),
                    _ => arg.clone(),
                });
            }
            return (tys, exprs);
        };

        let fixed_count = if has_rest {
            param_names.len().saturating_sub(1)
        } else {
            param_names.len()
        };
        let rest_name = if has_rest {
            param_names.get(fixed_count).cloned()
        } else {
            None
        };
        let fixed_names = &param_names[..fixed_count];

        let mut slots: Vec<Option<(Ty, Output)>> = vec![None; fixed_count];
        let mut rest_elems: Vec<(Ty, Output)> = Vec::new();
        let mut next_pos = 0usize;
        let mut seen_named = false;

        for arg in args {
            match arg.1.as_ref() {
                Expression::NamedArg(name, _) => {
                    if rest_name.as_deref() == Some(*name) {
                        let mut msg = Message::error(
                            ErrorCode::GenericTypeError,
                            format!(
                                "Cannot pass rest parameter `{}` by name in call to `{}`",
                                name, fn_name
                            ),
                            arg.0.into_range(),
                        );
                        msg.with_help(
                            "rest parameters are positional-only; pass trailing values after fixed args"
                                .to_string(),
                        );
                        self.messages.push(msg);
                        let _ = self.infer(arg);
                        continue;
                    }
                    seen_named = true;
                    let ty = self.infer(arg);
                    let value = match arg.1.as_ref() {
                        Expression::NamedArg(_, v) => v.clone(),
                        _ => unreachable!(),
                    };
                    match fixed_names.iter().position(|p| p == *name) {
                        Some(idx) => {
                            if slots[idx].is_some() {
                                self.messages.push(Message::error(
                                    ErrorCode::DuplicateField,
                                    format!(
                                        "Duplicate named argument `{}` in call to `{}`",
                                        name, fn_name
                                    ),
                                    arg.0.into_range(),
                                ));
                            } else {
                                slots[idx] = Some((ty, value));
                            }
                        }
                        None => {
                            let mut msg = Message::error(
                                ErrorCode::UnknownField,
                                format!(
                                    "Unknown named argument `{}` in call to `{}`",
                                    name, fn_name
                                ),
                                arg.0.into_range(),
                            );
                            if fixed_names.is_empty() {
                                msg.with_help(format!("`{}` has no named parameters", fn_name));
                            } else {
                                msg.with_help(format!(
                                    "expected one of: {}",
                                    fixed_names.join(", ")
                                ));
                            }
                            self.messages.push(msg);
                        }
                    }
                }
                _ => {
                    let ty = self.infer(arg);
                    // Skip fixed slots already filled by name so trailing
                    // positionals can pack into rest after `f(a: 1, 2, 3)`.
                    while next_pos < fixed_count && slots[next_pos].is_some() {
                        next_pos += 1;
                    }
                    if next_pos < fixed_count {
                        if seen_named {
                            let mut msg = Message::error(
                                ErrorCode::GenericTypeError,
                                format!(
                                    "Positional argument after named argument in call to `{}`",
                                    fn_name
                                ),
                                arg.0.into_range(),
                            );
                            msg.with_help(
                                "all positional arguments must come before named arguments"
                                    .to_string(),
                            );
                            self.messages.push(msg);
                        }
                        slots[next_pos] = Some((ty, arg.clone()));
                        next_pos += 1;
                    } else if has_rest {
                        // Trailing positionals after fixed (and after named
                        // fixed args) pack into the rest array.
                        rest_elems.push((ty, arg.clone()));
                        next_pos += 1;
                    } else if seen_named {
                        let mut msg = Message::error(
                            ErrorCode::GenericTypeError,
                            format!(
                                "Positional argument after named argument in call to `{}`",
                                fn_name
                            ),
                            arg.0.into_range(),
                        );
                        msg.with_help(
                            "all positional arguments must come before named arguments".to_string(),
                        );
                        self.messages.push(msg);
                    } else {
                        self.messages.push(Message::error(
                            ErrorCode::TooManyArguments,
                            format!("Function `{}` was called with too many arguments", fn_name),
                            arg.0.into_range(),
                        ));
                    }
                }
            }
        }

        // Positional-only under-application of fixed params: do not
        // synthesize an empty rest pack (preserves partial application).
        let pack_rest = has_rest
            && (has_named
                || next_pos >= fixed_count
                || args.len() >= fixed_count
                || fixed_count == 0);

        // `filled_mask` is a u64 on the VM stack (MakeFn / CallIndirect); cap
        // fixed arity so bit shifts never wrap. Abort early — continuing would
        // emit a truncated mask and mis-bind partials.
        if fixed_count > 64 {
            let msg = Message::error(
                ErrorCode::WrongArity,
                format!(
                    "Function `{}` has {} fixed parameters; at most 64 are supported for partial application",
                    fn_name, fixed_count
                ),
                range.clone(),
            );
            self.messages.push(msg);
            let mut tys = Vec::with_capacity(args.len());
            let mut exprs = Vec::with_capacity(args.len());
            for arg in args {
                tys.push(Ty::Var(self.counter.fresh()));
                exprs.push(match arg.1.as_ref() {
                    Expression::NamedArg(_, v) => v.clone(),
                    _ => arg.clone(),
                });
            }
            return (tys, exprs);
        }
        let mut tys = Vec::with_capacity(fixed_count + usize::from(pack_rest));
        let mut exprs = Vec::with_capacity(fixed_count + usize::from(pack_rest));
        let mut fill_mask: u32 = 0;
        let mut filled_slots: Vec<(usize, Ty)> = Vec::new();
        let mut saw_hole = false;
        for (i, slot) in slots.into_iter().enumerate() {
            match slot {
                Some((ty, expr)) => {
                    filled_slots.push((i, ty.clone()));
                    tys.push(ty);
                    exprs.push(expr);
                    fill_mask |= 1u32 << i;
                }
                None => {
                    saw_hole = true;
                    if pack_rest && !has_named {
                        let msg = Message::error(
                            ErrorCode::MissingField,
                            format!(
                                "Missing argument `{}` in call to `{}`",
                                fixed_names[i], fn_name
                            ),
                            range.clone(),
                        );
                        self.messages.push(msg);
                        tys.push(Ty::Var(self.counter.fresh()));
                        if let Some(a) = args.first() {
                            exprs.push(match a.1.as_ref() {
                                Expression::NamedArg(_, v) => v.clone(),
                                _ => a.clone(),
                            });
                        }
                    } else if has_named {
                        // Named under-apply: leave a hole (partial).
                    } else {
                        // Positional-only partial — stop before first hole.
                        break;
                    }
                }
            }
        }

        // Record fill mask whenever the call under-applied fixed params.
        let fixed_filled = fill_mask.count_ones() as usize;
        if fixed_filled < fixed_count && (has_named || !pack_rest) {
            self.partial_fills_by_span
                .insert((range.start, range.end), fill_mask);
            self.partial_filled_tys_by_span
                .insert((range.start, range.end), filled_slots);
        }
        let _ = saw_hole; // used for clarity in the None branch

        if pack_rest {
            let tuple_rest = self.fn_tuple_rest.get(fn_name).copied().unwrap_or(false);
            if tuple_rest {
                let elem_tys: Vec<Ty> = rest_elems
                    .iter()
                    .map(|(t, _)| apply_ty_prune(&self.subst, t))
                    .collect();
                let rest_ty = if elem_tys.is_empty() {
                    tuple_ty(vec![])
                } else {
                    tuple_ty(elem_tys)
                };
                tys.push(rest_ty);
            } else {
                let mut elem_ty: Option<Ty> = None;
                for (t, _) in &rest_elems {
                    let t_pruned = apply_ty_prune(&self.subst, t);
                    match &elem_ty {
                        None => elem_ty = Some(t_pruned),
                        Some(prev) => {
                            let prev_pruned = apply_ty_prune(&self.subst, prev);
                            if unify_with(&self.subst, &prev_pruned, &t_pruned).is_err() {
                                let _ = self.error_with_help(
                                ErrorCode::TypeMismatch,
                                format!(
                                    "rest argument type mismatch: expected `{}`, found `{}`",
                                    prev_pruned, t_pruned
                                ),
                                range.clone(),
                                Some(
                                    "all trailing rest arguments must share the same element type"
                                        .to_string(),
                                ),
                            );
                            }
                        }
                    }
                }
                let element = elem_ty.unwrap_or_else(|| Ty::Var(self.counter.fresh()));
                let rest_ty = vec_app_ty(element);
                tys.push(rest_ty);
            }
            // Stand-in for apply_function's expression hooks (array packing
            // is a codegen concern). Prefer the first rest elem if any.
            if let Some((_, e)) = rest_elems.first() {
                exprs.push(e.clone());
            } else if let Some(a) = args.first() {
                exprs.push(match a.1.as_ref() {
                    Expression::NamedArg(_, v) => v.clone(),
                    _ => a.clone(),
                });
            }
        }

        if exprs.len() != tys.len() {
            exprs.clear();
        }
        if has_named || has_rest {
            self.maybe_record_ffi_param_invoke_flow_for_call(fn_name, &exprs);
        }
        (tys, exprs)
    }

    /// Like [`infer_and_reorder_call_args`] but uses `candidate`'s
    /// `param_names` and `is_rest` rather than the global maps.
    ///
    /// Used by the overload-dispatch path so each overload's ABI is applied
    /// independently of what `fn_param_names` / `fn_has_rest` happen to store.
    fn infer_and_reorder_call_args_with_candidate<'a>(
        &mut self,
        fn_name: &str,
        candidate: &OverloadCandidate,
        args: &'a [Output<'a>],
        range: &Range<usize>,
    ) -> (Vec<Ty>, Vec<Output<'a>>) {
        // Save any existing entries, overwrite with candidate's data, call the
        // shared implementation, then restore so we don't clobber the maps.
        let prev_params = self
            .fn_param_names
            .insert(fn_name.to_string(), candidate.param_names.clone());
        let prev_rest = self
            .fn_has_rest
            .insert(fn_name.to_string(), candidate.is_rest);
        let result = self.infer_and_reorder_call_args(fn_name, args, range);
        match prev_params {
            Some(v) => {
                self.fn_param_names.insert(fn_name.to_string(), v);
            }
            None => {
                self.fn_param_names.remove(fn_name);
            }
        }
        match prev_rest {
            Some(v) => {
                self.fn_has_rest.insert(fn_name.to_string(), v);
            }
            None => {
                self.fn_has_rest.remove(fn_name);
            }
        }
        result
    }

    // ============================================================
    //  Enums and pattern matching
    // ============================================================

    /// Pre-pass: collect top-level `fn` parameter names (syntactic) for FFI
    /// call-site flow before main inference.
    fn pre_collect_free_function_param_names(&mut self, ast: &Output) {
        let children = match ast.1.as_ref() {
            Expression::Program(c) | Expression::Fragment(c) | Expression::Block(c) => c.as_slice(),
            _ => return,
        };
        for child in children {
            if let Expression::Function { name, args, .. } = child.1.as_ref() {
                if self.fn_param_names.contains_key(*name) {
                    continue;
                }
                let names = Self::syntactic_param_names(args);
                if !names.is_empty() {
                    self.fn_param_names.insert(name.to_string(), names);
                }
            }
        }
    }

    /// Stub inherent `impl Type { fn … }` signatures so trait-instance
    /// bodies that appear earlier in the file can call those methods (COI-115).
    fn pre_register_inherent_methods(&mut self, ast: &Output) {
        let children = match ast.1.as_ref() {
            Expression::Program(c) | Expression::Fragment(c) | Expression::Block(c) => c.as_slice(),
            _ => return,
        };
        for child in children {
            let stmt = Self::pre_pass_unwrap_stmt(child);
            if let Expression::Implementation {
                what,
                owner,
                type_params,
                methods,
            } = stmt.1.as_ref()
            {
                if !what.is_empty() {
                    continue;
                }
                self.stub_inherent_impl_methods(
                    owner,
                    type_params,
                    methods,
                    &stmt.0.into_range(),
                );
            }
        }
    }

    fn stub_inherent_impl_methods(
        &mut self,
        owner: &str,
        type_params: &[parser::ast::TypeParam],
        methods: &[Output],
        _range: &Range<usize>,
    ) {
        let msg_len = self.messages.len();
        let saved_idx = self.next_id_idx;
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
        let mut impl_constraints: Vec<Constraint> = Vec::new();
        if pushed {
            for (tp, var) in type_params.iter().zip(param_vars.iter()) {
                for bound in &tp.bounds {
                    impl_constraints.push(Constraint {
                        class: bound.to_string(),
                        args: vec![Ty::Var(*var)],
                    });
                }
            }
        }

        for method in methods {
            let (vis, body) = match method.1.as_ref() {
                Expression::Method(vis, body) => (*vis, body),
                _ => continue,
            };
            let Expression::Function {
                name,
                is_static,
                args,
                returns,
                where_constraints,
                ..
            } = body.1.as_ref()
            else {
                continue;
            };
            self.current_tuple_pack = Self::tuple_pack_ty_for_args(args, &mut self.counter);
            let arg_tys = self.parse_arg_list(args);
            self.current_tuple_pack = None;
            let param_names: Vec<String> = arg_tys.iter().map(|(n, _)| n.clone()).collect();
            let fqn = format!("{}::{}", owner_key, name);
            // FQN only — a bare `join` / `iter` key would shadow
            // `use path::{join}` and other free-function imports.
            self.fn_param_names.insert(fqn.clone(), param_names);
            let has_rest = matches!(args.1.as_ref(), Expression::Fragment(children)
                if children.last().is_some_and(|c| {
                    matches!(c.1.as_ref(), Expression::Argument { is_rest: true, .. })
                }));
            self.fn_has_rest.insert(fqn.clone(), has_rest);

            let mut fun_ty = match returns {
                Some(r) => self.parse_return_type_name(r),
                None => Ty::Var(self.counter.fresh()),
            };
            for (_, arg_ty) in arg_tys.iter().rev() {
                fun_ty = Ty::Fun(Box::new(arg_ty.clone()), Box::new(fun_ty));
            }
            if !*is_static {
                fun_ty = Ty::Fun(Box::new(owner_ty.clone()), Box::new(fun_ty));
            }
            let mut constraints = impl_constraints.clone();
            for wc in where_constraints {
                let args: Vec<Ty> = wc.args.iter().map(|a| self.parse_type_name(a)).collect();
                constraints.push(Constraint {
                    class: wc.class.to_string(),
                    args,
                });
            }
            let scheme = if param_vars.is_empty() {
                Scheme::mono(fun_ty)
            } else {
                Scheme::poly(param_vars.clone(), constraints.clone(), fun_ty)
            };
            self.env.insert_top(fqn.clone(), scheme.clone());
            self.methods
                .entry(owner_key.clone())
                .or_default()
                .insert(name.to_string(), (vis, scheme));
            if *is_static {
                self.static_methods
                    .entry(owner_key.clone())
                    .or_default()
                    .insert(name.to_string());
            }
            if !constraints.is_empty() {
                let dict_n = constraints.len();
                self.fn_dict_arity.insert(fqn.clone(), dict_n);
                self.generic_fns.insert(fqn.clone());
                self.generics.generic_fns.insert(fqn);
            }
        }

        self.pop_type_params_for_type_parsing(pushed);
        self.next_id_idx = saved_idx;
        self.messages.truncate(msg_len);
    }

    fn syntactic_param_names(args: &Output) -> Vec<String> {
        let mut names = Vec::new();
        if let Expression::Fragment(children) = args.1.as_ref() {
            for child in children {
                if let Expression::Argument { name, .. } = child.1.as_ref() {
                    names.push(name.to_string());
                }
            }
        }
        names
    }

    /// Pre-pass: apply top-level `use` imports so FFI type tags resolve during
    /// the later `declare` metadata scan.
    fn pre_process_top_level_uses(&mut self, ast: &Output) {
        let children = match ast.1.as_ref() {
            Expression::Program(c) | Expression::Fragment(c) | Expression::Block(c) => c.as_slice(),
            _ => return,
        };
        for child in children {
            self.pre_process_top_level_use_node(child);
        }
    }

    /// Walk statement wrappers and brace-`use` `Fragment`s so
    /// `use ffi::types::{Int, Float}` applies each binding.
    fn pre_process_top_level_use_node(&mut self, node: &Output) {
        match node.1.as_ref() {
            Expression::Statement(inner)
            | Expression::ExprStatement(inner)
            | Expression::Expr(inner)
            | Expression::Group(inner) => self.pre_process_top_level_use_node(inner),
            Expression::Fragment(children) | Expression::Block(children) => {
                for child in children {
                    self.pre_process_top_level_use_node(child);
                }
            }
            Expression::Use { path, name, alias } => {
                if name != "*" {
                    let _ = self.apply_virtual_use(path, name, alias.as_deref());
                }
            }
            _ => {}
        }
    }

    /// Pre-pass: record `declare` metadata and param call-site flow before main
    /// inference so helpers defined before their callers still refine `invoke`.
    fn pre_pass_ffi_invoke_param_flow(&mut self, ast: &Output) {
        let mut local_class_scopes = Vec::new();
        self.pre_pass_ffi_invoke_param_flow_walk(ast, &mut local_class_scopes);
    }

    fn pre_pass_ffi_invoke_param_flow_walk(
        &mut self,
        node: &Output,
        local_class_scopes: &mut Vec<HashMap<String, String>>,
    ) {
        match node.1.as_ref() {
            Expression::Statement(inner)
            | Expression::ExprStatement(inner)
            | Expression::Expr(inner)
            | Expression::Group(inner) => {
                self.pre_pass_ffi_invoke_param_flow_walk(inner, local_class_scopes);
            }
            Expression::Program(children)
            | Expression::Fragment(children)
            | Expression::Block(children) => {
                self.pre_pass_ffi_invoke_fragment(children, local_class_scopes);
            }
            Expression::Implementation { owner, methods, .. } => {
                let prev_owner = self.impl_owner.clone();
                self.impl_owner = Some(owner.to_string());
                for method in methods {
                    self.pre_pass_ffi_invoke_param_flow_walk(method, local_class_scopes);
                }
                self.impl_owner = prev_owner;
            }
            Expression::Function { args, body, .. } => {
                let mut scope = HashMap::new();
                if let Expression::Fragment(children) = args.1.as_ref() {
                    for child in children {
                        if let Expression::Argument {
                            ty: Some(ty),
                            name,
                            is_rest: false,
                            ..
                        } = child.1.as_ref()
                        {
                            if let Expression::Identifier(class) = ty.1.as_ref() {
                                scope.insert(name.to_string(), class.to_string());
                            }
                        }
                    }
                }
                local_class_scopes.push(scope);
                if let Some(body) = body {
                    self.pre_pass_ffi_invoke_param_flow_walk(body, local_class_scopes);
                }
                local_class_scopes.pop();
            }
            Expression::Call { name, args } => {
                if let Expression::Identifier(fn_name) = name.1.as_ref() {
                    let arg_exprs: Vec<Output> = args
                        .as_deref()
                        .unwrap_or(&[])
                        .iter()
                        .map(|arg| match arg.1.as_ref() {
                            Expression::NamedArg(_, value) => value.clone(),
                            _ => arg.clone(),
                        })
                        .collect();
                    if let Some(param_names) = self.fn_param_names.get(*fn_name).cloned() {
                        self.record_ffi_param_invoke_flow_prescan(
                            fn_name,
                            &param_names,
                            &arg_exprs,
                            local_class_scopes,
                        );
                    }
                }
                self.pre_pass_ffi_invoke_param_flow_walk(name, local_class_scopes);
                if let Some(args) = args {
                    for arg in args {
                        self.pre_pass_ffi_invoke_param_flow_walk(arg, local_class_scopes);
                    }
                }
            }
            Expression::Assignment(target, value) => {
                self.maybe_pre_record_field_declare(target, value, local_class_scopes);
                self.pre_pass_ffi_invoke_param_flow_walk(target, local_class_scopes);
                self.pre_pass_ffi_invoke_param_flow_walk(value, local_class_scopes);
            }
            Expression::Class { fields, .. } => {
                for field in fields {
                    self.pre_pass_ffi_invoke_param_flow_walk(field, local_class_scopes);
                }
            }
            Expression::Lambda { args, body, .. } => {
                self.pre_pass_ffi_invoke_param_flow_walk(args, local_class_scopes);
                self.pre_pass_ffi_invoke_param_flow_walk(body, local_class_scopes);
            }
            Expression::TestCase { name, body } => {
                self.pre_pass_ffi_invoke_param_flow_walk(name, local_class_scopes);
                self.pre_pass_ffi_invoke_param_flow_walk(body, local_class_scopes);
            }
            Expression::If(branches) => {
                for branch in branches {
                    self.pre_pass_ffi_invoke_param_flow_walk(branch, local_class_scopes);
                }
            }
            Expression::Branch(cond, body) => {
                if let Some(cond) = cond {
                    self.pre_pass_ffi_invoke_param_flow_walk(cond, local_class_scopes);
                }
                self.pre_pass_ffi_invoke_param_flow_walk(body, local_class_scopes);
            }
            Expression::Loop {
                iterable,
                body,
                identifier,
            } => {
                self.pre_pass_ffi_invoke_param_flow_walk(iterable, local_class_scopes);
                if let Some(identifier) = identifier {
                    self.pre_pass_ffi_invoke_param_flow_walk(identifier, local_class_scopes);
                }
                self.pre_pass_ffi_invoke_param_flow_walk(body, local_class_scopes);
            }
            Expression::For {
                init,
                cond,
                step,
                body,
            } => {
                if let Some(init) = init {
                    self.pre_pass_ffi_invoke_param_flow_walk(init, local_class_scopes);
                }
                self.pre_pass_ffi_invoke_param_flow_walk(cond, local_class_scopes);
                self.pre_pass_ffi_invoke_param_flow_walk(body, local_class_scopes);
                if let Some(step) = step {
                    self.pre_pass_ffi_invoke_param_flow_walk(step, local_class_scopes);
                }
            }
            Expression::Match { scrutinee, arms } => {
                self.pre_pass_ffi_invoke_param_flow_walk(scrutinee, local_class_scopes);
                for arm in arms {
                    self.pre_pass_ffi_invoke_param_flow_walk(&arm.body, local_class_scopes);
                }
            }
            Expression::Method(_, body) => {
                self.pre_pass_ffi_invoke_param_flow_walk(body, local_class_scopes);
            }
            Expression::Access(receiver, _)
            | Expression::OptionalAccess(receiver, _) => {
                self.pre_pass_ffi_invoke_param_flow_walk(receiver, local_class_scopes);
            }
            Expression::Instantiate(class, args) => {
                self.pre_pass_ffi_invoke_param_flow_walk(class, local_class_scopes);
                if let Some(args) = args {
                    for arg in args {
                        self.pre_pass_ffi_invoke_param_flow_walk(arg, local_class_scopes);
                    }
                }
            }
            Expression::Return(expr)
            | Expression::ImplicitReturn(expr)
            | Expression::Raise(expr)
            | Expression::Try(expr)
            | Expression::Readonly(expr)
            | Expression::Spread(expr) => {
                self.pre_pass_ffi_invoke_param_flow_walk(expr, local_class_scopes);
            }
            Expression::Tuple(items)
            | Expression::List(items)
            | Expression::Declare(items)
            | Expression::Invoke(items) => {
                for item in items {
                    self.pre_pass_ffi_invoke_param_flow_walk(item, local_class_scopes);
                }
            }
            Expression::Array(items) => {
                for item in items {
                    self.pre_pass_ffi_invoke_param_flow_walk(item, local_class_scopes);
                }
            }
            Expression::Index(target, index) => {
                self.pre_pass_ffi_invoke_param_flow_walk(target, local_class_scopes);
                if let Some(index) = index {
                    self.pre_pass_ffi_invoke_param_flow_walk(index, local_class_scopes);
                }
            }
            Expression::StaticDecl { ty, init, .. } => {
                if let Some(ty) = ty {
                    self.pre_pass_ffi_invoke_param_flow_walk(ty, local_class_scopes);
                }
                self.pre_pass_ffi_invoke_param_flow_walk(init, local_class_scopes);
            }
            Expression::Dload(path) => {
                self.pre_pass_ffi_invoke_param_flow_walk(path, local_class_scopes);
            }
            Expression::Done(handle) => {
                self.pre_pass_ffi_invoke_param_flow_walk(handle, local_class_scopes);
            }
            _ => {}
        }
    }

    fn pre_pass_ffi_invoke_fragment(
        &mut self,
        children: &[Output],
        local_class_scopes: &mut Vec<HashMap<String, String>>,
    ) {
        let mut i = 0;
        while i < children.len() {
            let child = &children[i];
            let stmt = Self::pre_pass_unwrap_stmt(child);
            match stmt.1.as_ref() {
                Expression::Class { .. }
                | Expression::Function { .. }
                | Expression::Implementation { .. }
                | Expression::EnumDecl { .. } => {
                    self.pre_pass_ffi_invoke_param_flow_walk(stmt, local_class_scopes);
                    i += 1;
                }
                Expression::Variable(var_name, _) => {
                    self.pre_pass_ffi_invoke_param_flow_walk(stmt, local_class_scopes);
                    if i + 1 < children.len() {
                        let next = Self::pre_pass_unwrap_stmt(&children[i + 1]);
                        if !is_declaration_like(next) {
                            let unwrapped = unwrap_expr_wrappers(next);
                            let unwrapped = match unwrapped.1.as_ref() {
                                Expression::Try(inner) => unwrap_expr_wrappers(inner),
                                _ => unwrapped,
                            };
                            if let Some(dargs) = Self::declare_args_from_expr(unwrapped) {
                                self.record_ffi_declare_metadata(var_name.to_string(), dargs, false);
                            }
                            if let Expression::Instantiate(class, _) = unwrapped.1.as_ref() {
                                if let Expression::Identifier(class_name) = class.1.as_ref() {
                                    if let Some(scope) = local_class_scopes.last_mut() {
                                        scope.insert(var_name.to_string(), class_name.to_string());
                                    }
                                }
                            }
                            self.pre_pass_ffi_invoke_param_flow_walk(next, local_class_scopes);
                            i += 2;
                            continue;
                        }
                    }
                    i += 1;
                }
                _ => {
                    self.pre_pass_ffi_invoke_param_flow_walk(stmt, local_class_scopes);
                    i += 1;
                }
            }
        }
    }

    fn pre_pass_unwrap_stmt<'a>(node: &'a Output<'a>) -> &'a Output<'a> {
        let mut current = node;
        for _ in 0..8 {
            current = match current.1.as_ref() {
                Expression::Statement(inner)
                | Expression::ExprStatement(inner)
                | Expression::Expr(inner)
                | Expression::Group(inner) => inner,
                _ => break,
            };
        }
        current
    }

    fn stub_free_function_signature(
        &mut self,
        name: &str,
        type_params: &[parser::ast::TypeParam],
        args: &Output,
        returns: Option<&Output>,
        where_constraints: &[parser::ast::WhereConstraint],
        range: &Range<usize>,
    ) {
        let key = if self.current_module.is_empty() {
            name.to_string()
        } else {
            format!("{}::{}", self.current_module, name)
        };
        if self.env.lookup(&key).is_some()
            || self.env.lookup(name).is_some()
            || self.forward_free_fn_schemes.contains_key(&key)
            || self.forward_free_fn_schemes.contains_key(name)
        {
            return;
        }

        let msg_len = self.messages.len();
        let is_generic = !type_params.is_empty();
        let mut param_vars: Vec<TyVarId> = Vec::new();
        let mut param_kinds: Vec<Kind> = Vec::new();
        let mut param_frame: HashMap<String, TyVarId> = HashMap::new();
        for tp in type_params {
            let var = self.counter.fresh();
            let kind = self.resolve_type_param_kind(tp);
            self.set_var_kind(var, kind.clone());
            param_frame.insert(tp.name.to_string(), var);
            param_vars.push(var);
            param_kinds.push(kind);
        }
        if is_generic {
            self.type_params_in_scope.push(param_frame);
        }

        let mut param_constraints: Vec<Constraint> = Vec::new();
        for (tp, var) in type_params.iter().zip(param_vars.iter()) {
            for bound in &tp.bounds {
                param_constraints.push(Constraint {
                    class: bound.to_string(),
                    args: vec![Ty::Var(*var)],
                });
            }
        }
        for wc in where_constraints {
            let args: Vec<Ty> = wc.args.iter().map(|a| self.parse_type_name(a)).collect();
            param_constraints.push(Constraint {
                class: wc.class.to_string(),
                args,
            });
        }

        let arg_tys: Vec<Ty> = if let Expression::Fragment(children) = args.1.as_ref() {
            children
                .iter()
                .filter_map(|c| {
                    if let Expression::Argument { ty: Some(ty), .. } = c.1.as_ref() {
                        Some(self.parse_type_name(ty))
                    } else {
                        None
                    }
                })
                .collect()
        } else {
            Vec::new()
        };
        let param_names = Self::syntactic_param_names(args);
        self.fn_param_names.insert(key.clone(), param_names.clone());
        if key != name {
            self.fn_param_names.insert(name.to_string(), param_names);
        }

        let ret_ty = match returns {
            Some(r) => self.parse_return_type_name(r),
            None => Ty::Var(self.counter.fresh()),
        };
        let mut fun_ty = ret_ty;
        for arg_ty in arg_tys.iter().rev() {
            fun_ty = Ty::Fun(Box::new(arg_ty.clone()), Box::new(fun_ty));
        }
        fun_ty = Self::seal_nullary_fun_ty(fun_ty, arg_tys.len(), false);

        if is_generic {
            self.type_params_in_scope.pop();
            let scheme = Scheme::poly_with_kinds(
                param_vars,
                param_kinds,
                param_constraints,
                fun_ty,
            );
            self.forward_free_fn_schemes
                .insert(name.to_string(), scheme.clone());
            if key != name {
                self.forward_free_fn_schemes.insert(key, scheme);
            }
        } else {
            self.forward_free_fn_schemes
                .insert(key, Scheme::mono(fun_ty));
        }
        self.messages.truncate(msg_len);
        let _ = range;
    }


    /// Forward-declare module-level `fn` signatures after `push_scope` so
    /// `impl` methods can call helpers defined later in the file.
    fn pre_register_free_functions(&mut self, ast: &Output) {
        let children = match ast.1.as_ref() {
            Expression::Program(c) | Expression::Fragment(c) | Expression::Block(c) => c.as_slice(),
            _ => return,
        };
        for child in children {
            if let Expression::Function {
                name,
                type_params,
                args,
                returns,
                where_constraints,
                ..
            } = child.1.as_ref()
            {
                self.stub_free_function_signature(
                    name,
                    type_params,
                    args,
                    returns.as_ref(),
                    where_constraints,
                    &child.0.into_range(),
                );
            }
        }
    }

    /// Pre-pass: register enum shapes before main inference (forward refs).
    fn pre_register_enums(&mut self, ast: &Output) -> Result<(), Vec<Message>> {
        let mut errors = Vec::new();
        self.pre_register_enums_walk(ast, &mut errors);
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    fn pre_register_enums_walk(&mut self, node: &Output, errors: &mut Vec<Message>) {
        use parser::ast::EnumVariantPayload;
        match node.1.as_ref() {
            Expression::TypeAlias { ty, .. } => {
                self.pre_register_enums_walk(ty, errors);
            }
            Expression::EnumDecl {
                docs: _,
                name,
                type_params,
                variants,
                ..
            } => {
                let name_str = self.qualify_module_name(name);
                let previous_generic_ctor = self.register_generic_type_ctor(name, type_params);
                let pushed = self.push_type_params_for_type_parsing(type_params);
                let mut variant_names = Vec::new();
                let mut arities = Vec::new();
                let mut payloads: Vec<EnumVariantPayloadTy> = Vec::new();

                for v in variants {
                    if let Expression::EnumVariant {
                        docs: _,
                        name: vname,
                        payload,
                    } = v.1.as_ref()
                    {
                        variant_names.push(vname.to_string());
                        let payload_ty = match payload {
                            EnumVariantPayload::Unit => {
                                arities.push(0);
                                EnumVariantPayloadTy::Unit
                            }
                            EnumVariantPayload::Tuple(parts) => {
                                let mut tys = Vec::with_capacity(parts.len());
                                for p in parts {
                                    tys.push(self.parse_type_name(p));
                                }
                                arities.push(tys.len());
                                EnumVariantPayloadTy::Tuple(tys)
                            }
                            EnumVariantPayload::Record(fields) => {
                                let mut pairs = Vec::with_capacity(fields.len());
                                for f in fields {
                                    let fty = self.parse_type_name(&f.value);
                                    pairs.push((f.name.to_string(), fty));
                                }
                                arities.push(pairs.len());
                                EnumVariantPayloadTy::Record(pairs)
                            }
                        };
                        payloads.push(payload_ty);
                    }
                }

                // Check 1: duplicate enum name (including built-ins still in scope).
                if common::is_builtin_enum(&name_str) {
                    if self.builtin_name_in_scope(&name_str)
                        || common::is_builtin_ffi_enum(&name_str)
                    {
                        let mut msg = Message::error(
                            ErrorCode::DuplicateEnum,
                            format!("Cannot redeclare built-in enum `{}`", name_str),
                            node.0.into_range(),
                        );
                        if common::is_poly_builtin_enum(&name_str) {
                            msg.with_help(format!(
                                "`{}` is in the prelude; free the short name with `use prelude::{} as OtherName;` before redefining, or pick a different name",
                                name_str, name_str
                            ));
                        } else {
                            msg.with_help(format!(
                                "`{}` is a compiler FFI type; use `ffi::types::{{…}}` instead of redeclaring it",
                                name_str
                            ));
                        }
                        errors.push(msg);
                        self.restore_generic_type_ctor(&name_str, previous_generic_ctor);
                        self.pop_type_params_for_type_parsing(pushed);
                        return;
                    }
                    // Prelude enum short name was rebound — drop the
                    // compiler registration so the user enum can take over.
                    self.enums.remove(&name_str);
                    self.enum_tags.remove(&name_str);
                    self.enum_payloads.remove(&name_str);
                    self.enum_arities.remove(&name_str);
                    self.generics.generic_type_ctors.remove(&name_str);
                    self.generics.nominal_type_modules.remove(&name_str);
                }
                if self.enums.contains_key(&name_str) {
                    let mut msg = Message::error(
                        ErrorCode::DuplicateEnum,
                        format!("Duplicate enum `{}`", name_str),
                        node.0.into_range(),
                    );
                    msg.with_help(format!(
                        "an enum named `{}` was already declared; remove or rename this declaration",
                        name_str
                    ));
                    errors.push(msg);
                    self.restore_generic_type_ctor(&name_str, previous_generic_ctor);
                    self.pop_type_params_for_type_parsing(pushed);
                    return;
                }

                // Check 2: variant name collides with a previously
                // registered enum's variant name (cross-enum).
                for vn in &variant_names {
                    let taken = self.enum_tags.values().any(|tags| tags.contains_key(vn));
                    if taken {
                        let mut msg = Message::error(
                            ErrorCode::DuplicateConstructor,
                            format!(
                                "Duplicate constructor `{}` (also declared by another enum)",
                                vn
                            ),
                            node.0.into_range(),
                        );
                        msg.with_help(
                            "constructor names must be unique across all enums".to_string(),
                        );
                        errors.push(msg);
                        self.restore_generic_type_ctor(&name_str, previous_generic_ctor);
                        self.pop_type_params_for_type_parsing(pushed);
                        return;
                    }
                }

                // Check 3: variant name shadows a built-in
                // (currently no such checks — natives are registered
                // with full names like `print` and don't share the
                // `::` namespace. Reserved for future use.)

                // Reserve. We use `BTreeMap` for tags (lookups are
                // by variant name, not order). The `Vec` for
                // variant order is the canonical declaration order.
                let mut tag_map = BTreeMap::new();
                for (i, vn) in variant_names.iter().enumerate() {
                    tag_map.insert(vn.clone(), i as u32);
                }

                // Generic enums store payloads with `Con(param)` schema
                // markers (same convention as builtin Option/Result) so
                // construct/match can freshen independently per site.
                let payloads = if pushed {
                    let frame = self
                        .type_params_in_scope
                        .last()
                        .expect("type-param frame just pushed");
                    let var_to_name: HashMap<TyVarId, String> =
                        frame.iter().map(|(n, id)| (*id, n.clone())).collect();
                    payloads
                        .iter()
                        .map(|p| schemaize_payload(p, &var_to_name))
                        .collect()
                } else {
                    payloads
                };

                self.enums.insert(name_str.clone(), variant_names);
                self.enum_tags.insert(name_str.clone(), tag_map);
                self.enum_payloads.insert(name_str.clone(), payloads);
                self.enum_arities.insert(name_str.clone(), arities);
                self.generics
                    .register_nominal_type(&name_str, &self.current_module);
                self.pop_type_params_for_type_parsing(pushed);
            }

            // Recurse into the same children that `id::pre_walk` would
            // visit. We mirror the structure of `pre_walk_children`
            // but only need to find nested EnumDecls — most
            // branches can just walk their expression children.
            Expression::Noop(_)
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
            | Expression::Use { .. }
            | Expression::Module(_, _)
            | Expression::Variable(_, _)
            | Expression::Constant(_, _)
            | Expression::Argument { .. }
            | Expression::Field { .. }
            | Expression::QualifiedAccess { .. }
            | Expression::ExternBlock { .. }
            | Expression::ExternStruct(_) => {}

            Expression::Expr(e)
            | Expression::Group(e)
            | Expression::Statement(e)
            | Expression::ExprStatement(e)
            | Expression::Return(e)
            | Expression::ImplicitReturn(e)
            |             Expression::Raise(e)
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
            | Expression::LetDestructure { rhs: e, .. }
            | Expression::NamedArg(_, e) => {
                self.pre_register_enums_walk(e, errors);
            }
            Expression::Defer { body, .. } => {
                self.pre_register_enums_walk(body, errors);
            }

            Expression::TypeApp { args, .. } => {
                for a in args {
                    self.pre_register_enums_walk(a, errors);
                }
            }

            Expression::TypeFun(arg, ret) => {
                self.pre_register_enums_walk(arg, errors);
                self.pre_register_enums_walk(ret, errors);
            }

            Expression::CompoundAssign(name, _, value) => {
                self.pre_register_enums_walk(name, errors);
                self.pre_register_enums_walk(value, errors);
            }

            Expression::Assignment(name, value) => {
                self.pre_register_enums_walk(name, errors);
                self.pre_register_enums_walk(value, errors);
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
            | Expression::Coalesce(l, r) => {
                self.pre_register_enums_walk(l, errors);
                self.pre_register_enums_walk(r, errors);
            }
            Expression::Cast(expr, ty) => {
                self.pre_register_enums_walk(expr, errors);
                self.pre_register_enums_walk(ty, errors);
            }
            Expression::Range { start, end, .. } => {
                self.pre_register_enums_walk(start, errors);
                self.pre_register_enums_walk(end, errors);
            }

            Expression::Resume(target, arg) => {
                self.pre_register_enums_walk(target, errors);
                if let Some(a) = arg {
                    self.pre_register_enums_walk(a, errors);
                }
            }

            Expression::Block(cs)
            | Expression::Program(cs)
            | Expression::Fragment(cs)
            | Expression::List(cs)
            | Expression::Declare(cs)
            | Expression::Invoke(cs) => {
                for c in cs {
                    self.pre_register_enums_walk(c, errors);
                }
            }
            Expression::Dload(path) => self.pre_register_enums_walk(path, errors),
            Expression::Done(handle) => self.pre_register_enums_walk(handle, errors),
            Expression::Tuple(items) => {
                for c in items {
                    self.pre_register_enums_walk(c, errors);
                }
            }
            Expression::Array(items) => {
                for c in items {
                    self.pre_register_enums_walk(c, errors);
                }
            }
            Expression::Index(target, index) => {
                self.pre_register_enums_walk(target, errors);
                if let Some(index) = index {
                    self.pre_register_enums_walk(index, errors);
                }
            }
            Expression::Readonly(inner) => self.pre_register_enums_walk(inner, errors),
            Expression::StaticDecl { ty, init, .. } => {
                if let Some(ty) = ty {
                    self.pre_register_enums_walk(ty, errors);
                }
                self.pre_register_enums_walk(init, errors);
            }
            Expression::Dict(fields) => {
                for f in fields {
                    self.pre_register_enums_walk(&f.value, errors);
                }
            }
            Expression::If(branches) => {
                for b in branches {
                    self.pre_register_enums_walk(b, errors);
                }
            }
            Expression::Implementation { methods, .. } => {
                for m in methods {
                    self.pre_register_enums_walk(m, errors);
                }
            }
            Expression::Class { fields, .. } => {
                for f in fields {
                    self.pre_register_enums_walk(f, errors);
                }
            }

            Expression::Function { args, body, .. } => {
                self.pre_register_enums_walk(args, errors);
                if let Some(body) = body {
                    self.pre_register_enums_walk(body, errors);
                }
            }
            Expression::Lambda { args, body, .. } => {
                self.pre_register_enums_walk(args, errors);
                self.pre_register_enums_walk(body, errors);
            }

            Expression::TestCase { name, body } => {
                self.pre_register_enums_walk(name, errors);
                self.pre_register_enums_walk(body, errors);
            }

            Expression::Branch(cond, body) => {
                if let Some(c) = cond {
                    self.pre_register_enums_walk(c, errors);
                }
                self.pre_register_enums_walk(body, errors);
            }

            Expression::Call { name, args } => {
                self.pre_register_enums_walk(name, errors);
                if let Some(a) = args {
                    for arg in a {
                        self.pre_register_enums_walk(arg, errors);
                    }
                }
            }

            Expression::Loop {
                iterable,
                body,
                identifier,
            } => {
                self.pre_register_enums_walk(iterable, errors);
                if let Some(i) = identifier {
                    self.pre_register_enums_walk(i, errors);
                }
                self.pre_register_enums_walk(body, errors);
            }

            Expression::For {
                init,
                cond,
                step,
                body,
            } => {
                if let Some(init) = init {
                    self.pre_register_enums_walk(init, errors);
                }
                self.pre_register_enums_walk(cond, errors);
                self.pre_register_enums_walk(body, errors);
                if let Some(step) = step {
                    self.pre_register_enums_walk(step, errors);
                }
            }

            Expression::Match { scrutinee, arms } => {
                self.pre_register_enums_walk(scrutinee, errors);
                for arm in arms {
                    // Patterns are not expressions — no recursion
                    // into the pattern body. (Constructor patterns
                    // contain only nested patterns.)
                    self.pre_register_enums_walk(&arm.body, errors);
                }
            }

            // The `EnumDecl` arm above handles every EnumDecl in
            // the tree; no second arm is needed here. `EnumVariant`
            // and `Construct` are still reachable (e.g. inside a
            // function body) and just recurse.
            Expression::EnumVariant { .. } => {}
            Expression::Construct { .. } => {}

            Expression::Method(_, body) => {
                self.pre_register_enums_walk(body, errors);
            }
            Expression::Access(receiver, _) | Expression::OptionalAccess(receiver, _) => {
                self.pre_register_enums_walk(receiver, errors);
            }
            Expression::Instantiate(class, args) => {
                self.pre_register_enums_walk(class, errors);
                if let Some(a) = args {
                    for arg in a {
                        self.pre_register_enums_walk(arg, errors);
                    }
                }
            }

            // New generic-system nodes — recurse into children.
            Expression::Forall { ty, .. } => self.pre_register_enums_walk(ty, errors),
            Expression::TypeClass { methods, .. } => {
                for m in methods {
                    self.pre_register_enums_walk(m, errors);
                }
            }
            Expression::TypeClassImpl { args, methods, .. } => {
                for a in args {
                    self.pre_register_enums_walk(a, errors);
                }
                for m in methods {
                    self.pre_register_enums_walk(m, errors);
                }
            }
            Expression::AssocTypeDecl { .. } => {}
            Expression::TypeProjection { args, .. } => {
                for arg in args {
                    self.pre_register_enums_walk(arg, errors);
                }
            }
            Expression::AssocTypeDef { ty, .. } => self.pre_register_enums_walk(ty, errors),
            Expression::Spread(inner) => self.pre_register_enums_walk(inner, errors),
            Expression::TypeFnSig { params, ret } => {
                self.pre_register_enums_walk(params, errors);
                self.pre_register_enums_walk(ret, errors);
            }
            Expression::AttrDecl {
                docs: _,
                args,
                returns,
                body,
                ..
            } => {
                self.pre_register_enums_walk(args, errors);
                if let Some(returns) = returns {
                    self.pre_register_enums_walk(returns, errors);
                }
                self.pre_register_enums_walk(body, errors);
            }
        }
    }

    // ---- Enum declarations ----

    fn infer_enum_decl(&mut self, name: &str, variants: &[Output], _range: &Range<usize>) {
        use parser::ast::EnumVariantPayload;
        let name_str = self.qualify_module_name(name);
        // Look up the pre-reserved shape. If missing, the
        // pre-pass rejected this enum (duplicate / collision);
        // the caller has already pushed a diagnostic. Just walk
        // the children to keep IDs aligned.
        let pre_shape = match self.enums.get(&name_str).cloned() {
            Some(v) => v,
            None => {
                for v in variants {
                    let _ = self.infer(v);
                }
                return;
            }
        };
        let pre_payloads = match self.enum_payloads.get(&name_str).cloned() {
            Some(p) => p,
            None => {
                for v in variants {
                    let _ = self.infer(v);
                }
                return;
            }
        };

        // Walk each variant. We delegate to `self.infer(v)` for the
        // whole variant — its `EnumVariant` arm in `infer_inner`
        // recurses into the payload children. That gives us
        // exactly `1 + N` IDs per variant where N is the number
        // of payload entries the pre-walk visited (1 per Tuple
        // element, 1 per Record field's value, 0 for Unit). The
        // pre-pass has already built the typed payload, so the
        // infer recursion is purely for ID-alignment.
        let mut built_variants: Vec<(String, EnumVariantPayloadTy)> = Vec::new();
        for (i, v) in variants.iter().enumerate() {
            // Consume IDs for the variant itself + its payload
            // before any early `continue`. The pre-walk visited
            // this node and its payload regardless of whether we
            // accept it.
            let _ = self.infer(v);

            if let Expression::EnumVariant {
                docs: _,
                name: vname,
                payload,
            } = v.1.as_ref()
            {
                let vname_str = vname.to_string();
                let pre_pay = match pre_payloads.get(i) {
                    Some(p) => p.clone(),
                    None => {
                        continue;
                    }
                };

                // Sanity: name + payload arity should match the
                // pre-pass shape. If not, the pre-pass has already
                // complained — skip registering this variant but
                // keep IDs aligned (already done above).
                if pre_shape.get(i) != Some(&vname_str) {
                    continue;
                }
                let expected_count = match &pre_pay {
                    EnumVariantPayloadTy::Unit => 0,
                    EnumVariantPayloadTy::Tuple(tys) => tys.len(),
                    EnumVariantPayloadTy::Record(fields) => fields.len(),
                };
                let actual_count = match payload {
                    EnumVariantPayload::Unit => 0,
                    EnumVariantPayload::Tuple(parts) => parts.len(),
                    EnumVariantPayload::Record(fields) => fields.len(),
                };
                if expected_count != actual_count {
                    continue;
                }
                built_variants.push((vname_str, pre_pay));
            }
        }

        // Build the Ty::Sum.
        let sum_ty = Ty::Sum {
            name: name_str.clone(),
            variants: built_variants.clone(),
        };

        // Register the enum itself as a type.
        self.env
            .insert_top(name_str.clone(), Scheme::mono(Ty::Con(name_str.clone())));

        // Register each variant as a callable in the env. Use the
        // qualified name `EnumName::VariantName` as the binding
        // key — `Construct` looks up by qualified name in this
        // map.
        for (i, (vname, payload_ty)) in built_variants.iter().enumerate() {
            // Field count = 0 for Unit, N for Tuple/Record.
            // Same arity, regardless of shape — the shape
            // discrimination happens at call-site / pattern
            // inference, not at the constructor's HM type.
            let arity = payload_ty.field_count();
            let ctor_ty = Ty::Constructor {
                owner: Box::new(sum_ty.clone()),
                tag: i as u32,
                arity,
            };
            let scheme = if arity == 0 {
                Scheme::mono(ctor_ty)
            } else {
                // Curried: arg1 -> arg2 -> ... -> Constructor.
                // Field order matches declaration order for both
                // Tuple and Record — codegen reorders record
                // call sites to declaration order before pushing
                // the MAKE_ENUM.
                let arg_tys: Vec<Ty> = payload_ty.field_types().into_iter().cloned().collect();
                let mut fun_ty = ctor_ty;
                for arg_ty in arg_tys.iter().rev() {
                    fun_ty = Ty::Fun(Box::new(arg_ty.clone()), Box::new(fun_ty));
                }
                Scheme::mono(fun_ty)
            };
            let qualified = format!("{}::{}", name_str, vname);
            self.env.insert_top(qualified, scheme);
        }
    }

    /// Constructor application with shape/arity checking.
    ///
    /// Also resolves `Class::static_method(...)` when `enum_name` is a
    /// class (parsed as Construct because `Class::name(...)` shares the
    /// enum-constructor surface syntax).
    fn infer_construct(
        &mut self,
        enum_name: &str,
        variant_name: &str,
        fields: &parser::ast::EnumConstructPayload<'_>,
        range: Range<usize>,
        call_id: Option<NodeId>,
    ) -> Ty {
        use parser::ast::EnumConstructPayload;
        // Surface path `ffi::types::Int` maps to the internal `FFIType` registry.
        let registry_name = if common::is_builtin_ffi_enum(enum_name) {
            // Legacy `FFIType::X` requires an import; `ffi::types::X` is always OK.
            if enum_name == common::BUILTIN_FFI_TYPE_ENUM
                && !self.builtin_name_in_scope(common::BUILTIN_FFI_TYPE_ENUM)
                && !self.ffi_tag_in_scope(variant_name)
            {
                return self.error_with_help(
                    ErrorCode::UnknownEnum,
                    format!("Cannot find enum `{}` in this scope", enum_name),
                    range,
                    Some(
                        "import tags with `use ffi::types::{Int, Ptr, …}` (or write `ffi::types::Int`)"
                            .to_string(),
                    ),
                );
            }
            common::BUILTIN_FFI_TYPE_ENUM.to_string()
        } else if let Some(key) = self.resolve_enum_key(enum_name) {
            key
        } else {
            enum_name.to_string()
        };
        let enum_str = registry_name;
        let variant_str = variant_name.to_string();

        // Look up the enum. Error if not registered.
        let tags = match self.enum_tags.get(&enum_str) {
            Some(t) => t.clone(),
            None => {
                let static_fqn = self.class_member_fqn(enum_name, variant_name);
                // Bare / `()` Unit form: prefer static field, then 0-arg
                // static method (`Counter::fresh()`).
                if matches!(fields, EnumConstructPayload::Unit) {
                    if let Some(ty) = self.static_slot_types.get(&static_fqn).cloned() {
                        return apply_ty_prune(&self.subst, &ty);
                    }
                }
                if let Some(ty) = self.try_infer_static_method_call(
                    enum_name,
                    variant_name,
                    fields,
                    range.clone(),
                    call_id,
                ) {
                    return ty;
                }
                if self.has_method(enum_name, variant_name) {
                    return self.error_with_help(
                        ErrorCode::GenericTypeError,
                        format!(
                            "`{}` is an instance method; call it on a value (`obj.{}(...)`)",
                            static_fqn, variant_name
                        ),
                        range,
                        Some(format!(
                            "or declare `static fn {}` to call it as `{}`",
                            variant_name, static_fqn
                        )),
                    );
                }
                return self.error(
                    ErrorCode::UnknownEnum,
                    format!("Cannot find enum `{}` in this scope", enum_name),
                    range,
                );
            }
        };

        // Look up the variant tag.
        let tag = match tags.get(&variant_str) {
            Some(t) => *t,
            None => {
                return self.error(
                    ErrorCode::UnknownVariant,
                    format!(
                        "Cannot find variant `{}` on enum `{}`",
                        variant_str, enum_str
                    ),
                    range,
                );
            }
        };

        let arity = self
            .enum_arities
            .get(&enum_str)
            .and_then(|a| a.get(tag as usize).copied())
            .unwrap_or(0);

        // Polymorphic enums (builtin Option/Result or user
        // `enum Box<T>`): mint fresh payload vars so each construct
        // site gets an independent applied type.
        let (expected_payload, poly_sum_owner) = if self.is_poly_enum(&enum_str) {
            let (payload, owner) = self.fresh_poly_construct_payload(&enum_str, &variant_str);
            (payload, Some(owner))
        } else {
            let payload = self
                .enum_payloads
                .get(&enum_str)
                .and_then(|p| p.get(tag as usize).cloned())
                .unwrap_or(EnumVariantPayloadTy::Unit);
            (payload, None)
        };

        // Shape vs arity: record shapes defer to field-by-field checks.
        let (shape_matches, same_shape_with_wrong_arity) = match (&expected_payload, fields) {
            (EnumVariantPayloadTy::Unit, EnumConstructPayload::Unit) => (true, false),
            (EnumVariantPayloadTy::Tuple(_), EnumConstructPayload::Tuple(args)) => {
                let want = expected_payload.field_count();
                (args.len() == want, args.len() != want)
            }
            (EnumVariantPayloadTy::Record(_), EnumConstructPayload::Record(_)) => {
                // Defer the arity check to the field-by-field
                // pass below, which produces more specific
                // diagnostics ("Missing field `x`" instead of
                // "expects 2 arguments, got 1").
                (true, false)
            }
            _ => (false, false),
        };

        if !shape_matches {
            if same_shape_with_wrong_arity {
                return self.error(
                    ErrorCode::ConstructorArity,
                    format!(
                        "Constructor `{}::{}` expects {} arguments, got {}",
                        enum_str,
                        variant_str,
                        expected_payload.field_count(),
                        match fields {
                            EnumConstructPayload::Unit => 0,
                            EnumConstructPayload::Tuple(args) => args.len(),
                            EnumConstructPayload::Record(parts) => parts.len(),
                        },
                    ),
                    range,
                );
            }
            let call_shape = match fields {
                EnumConstructPayload::Unit => "unit",
                EnumConstructPayload::Tuple(_) => "tuple",
                EnumConstructPayload::Record(_) => "record",
            };
            let help = match (&expected_payload, fields) {
                (
                    EnumVariantPayloadTy::Tuple(tys),
                    EnumConstructPayload::Record(_),
                ) if tys.len() == 1 => {
                    let wrapped = crate::typechecking::pretty::format_ty_for_diag(
                        &self.subst,
                        &tys[0],
                    );
                    format!(
                        "`{enum_str}::{variant_str}` is a tuple variant wrapping `{wrapped}`; \
                         construct with `{enum_str}::{variant_str}(value)`, or declare a record \
                         variant `{variant_str} {{ …fields… }}` if you want named fields at the call site"
                    )
                }
                (EnumVariantPayloadTy::Record(_), _) => {
                    format!("use record syntax for `{enum_str}::{variant_str}`")
                }
                _ => format!("use tuple / unit syntax for `{enum_str}::{variant_str}`"),
            };
            return self.error_with_help(
                ErrorCode::PayloadShapeMismatch,
                format!(
                    "Constructor `{}::{}` payload shape mismatch (declared as {}, called as {})",
                    enum_str,
                    variant_str,
                    payload_kind_name(&expected_payload),
                    call_shape,
                ),
                range,
                Some(help),
            );
        }

        // Field-by-field type check.
        match fields {
            EnumConstructPayload::Unit => {}
            EnumConstructPayload::Tuple(args) => {
                let expected_tys = expected_payload.field_types();
                for (arg, expected_ty) in args.iter().zip(expected_tys.iter()) {
                    let arg_ty = self.infer(arg);
                    self.unify(
                        expected_ty,
                        &arg_ty,
                        &arg.0.into_range(),
                        &format!("constructor `{}::{}` argument", enum_str, variant_str),
                    );
                }
            }
            EnumConstructPayload::Record(parts) => {
                // Build a name → value map for the call site, then
                // walk the DECLARATION order. Each declared field
                // must be supplied exactly once; the codegen
                // reorders the bytecode accordingly.
                let mut call_site: std::collections::HashMap<&str, &Output> =
                    std::collections::HashMap::with_capacity(parts.len());
                for p in parts {
                    if call_site.insert(p.name, &p.value).is_some() {
                        return self.error_with_help(
                            ErrorCode::DuplicateField,
                            format!(
                                "Duplicate field `{}` in record constructor `{}::{}`",
                                p.name, enum_str, variant_str,
                            ),
                            range,
                            Some("each field must be supplied exactly once".to_string()),
                        );
                    }
                }
                let EnumVariantPayloadTy::Record(decl_fields) = &expected_payload else {
                    // unreachable — shape_matches already proved it
                    unreachable!();
                };
                for (decl_name, decl_ty) in decl_fields.iter() {
                    let arg = match call_site.get(decl_name.as_str()) {
                        Some(a) => *a,
                        None => {
                            return self.error_with_help(
                                ErrorCode::MissingField,
                                format!(
                                    "Missing field `{}` in record constructor `{}::{}`",
                                    decl_name, enum_str, variant_str,
                                ),
                                range,
                                Some(format!("add `{}: <expr>` to the call site", decl_name,)),
                            );
                        }
                    };
                    let arg_ty = self.infer(arg);
                    self.unify(
                        decl_ty,
                        &arg_ty,
                        &arg.0.into_range(),
                        &format!(
                            "constructor `{}::{}.{}` argument",
                            enum_str, variant_str, decl_name,
                        ),
                    );
                }
                // Check for any unknown field names (extra
                // fields supplied at the call site).
                for p in parts {
                    if !decl_fields.iter().any(|(dn, _)| dn == p.name) {
                        return self.error_with_help(
                            ErrorCode::UnknownField,
                            format!(
                                "Unknown field `{}` in record constructor `{}::{}`",
                                p.name, enum_str, variant_str,
                            ),
                            range,
                            Some(format!(
                                "the declared fields are: {}",
                                decl_fields
                                    .iter()
                                    .map(|(n, _)| format!("`{}`", n))
                                    .collect::<Vec<_>>()
                                    .join(", "),
                            )),
                        );
                    }
                }
            }
        }

        // Build the result. The owner is the full `Ty::Sum` so
        // later unifications (in match patterns) can compare tag
        // and arity directly.
        let sum_ty = if let Some(owner) = poly_sum_owner {
            // Re-read payload types after unify so Ok/Some carry
            // the concrete argument type.
            apply_ty_prune(&self.subst, &owner)
        } else {
            Ty::Sum {
                name: enum_str.clone(),
                variants: self
                    .enum_payloads
                    .get(&enum_str)
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .zip(self.enums.get(&enum_str).cloned().unwrap_or_default())
                    .map(|(p, n)| (n, p))
                    .collect(),
            }
        };

        Ty::Constructor {
            owner: Box::new(sum_ty),
            tag,
            arity,
        }
    }

    /// True when `name` is a polymorphic enum (builtin Option/Result
    /// or a user enum registered in `generic_type_ctors`).
    fn is_poly_enum(&self, name: &str) -> bool {
        common::is_poly_builtin_enum(name) || self.generics.generic_type_ctors.contains_key(name)
    }

    /// Fresh payload + owning type for a polymorphic construct site.
    ///
    /// Builtin Option/Result keep structural `Ty::Sum` owners (bridged
    /// to `Ty::App` annotations via unify). User generic enums return
    /// `Ty::App(Con(name), args)` so they unify directly with
    /// annotations like `Box<int>`.
    fn fresh_poly_construct_payload(
        &mut self,
        enum_name: &str,
        variant_name: &str,
    ) -> (EnumVariantPayloadTy, Ty) {
        if common::is_builtin_option_enum(enum_name) {
            let t = Ty::Var(self.counter.fresh());
            let owner = option_ty(t.clone());
            let payload = if variant_name == "Some" {
                EnumVariantPayloadTy::Tuple(vec![t])
            } else {
                EnumVariantPayloadTy::Unit
            };
            return (payload, owner);
        }
        if common::is_builtin_result_enum(enum_name) {
            let t = Ty::Var(self.counter.fresh());
            let e = Ty::Var(self.counter.fresh());
            let owner = result_ty(t.clone(), e.clone());
            let payload = if variant_name == "Ok" {
                EnumVariantPayloadTy::Tuple(vec![t])
            } else {
                EnumVariantPayloadTy::Tuple(vec![e])
            };
            return (payload, owner);
        }

        // User generic enum: freshen schema payloads (`Con(param)`).
        let params = self
            .generics
            .generic_type_ctors
            .get(enum_name)
            .cloned()
            .unwrap_or_default();
        let mut map = HashMap::new();
        let mut args = Vec::with_capacity(params.len());
        for p in &params {
            let v = Ty::Var(self.counter.fresh());
            args.push(v.clone());
            map.insert(p.clone(), v);
        }
        let tag = self
            .enum_tags
            .get(enum_name)
            .and_then(|t| t.get(variant_name).copied())
            .unwrap_or(0);
        let schema = self
            .enum_payloads
            .get(enum_name)
            .and_then(|p| p.get(tag as usize).cloned())
            .unwrap_or(EnumVariantPayloadTy::Unit);
        let payload = subst_payload_params(&schema, &map);
        let owner = Ty::App(Box::new(Ty::Con(enum_name.to_string())), args);
        (payload, owner)
    }

    /// Payload type for a pattern arm: prefer the scrutinee Sum's
    /// concrete payloads for poly enums, else App-applied args, else
    /// the registry schema.
    fn poly_or_registry_payload(
        &mut self,
        enum_name: &str,
        tag: u32,
        expected_ty: &Ty,
        pattern_range: &Range<usize>,
    ) -> Option<EnumVariantPayloadTy> {
        let resolved = apply_ty_prune(&self.subst, expected_ty);
        let sum = match &resolved {
            Ty::Sum { name, variants } if name == enum_name => Some(variants.clone()),
            Ty::Constructor { owner, .. } => match owner.as_ref() {
                Ty::Sum { name, variants } if name == enum_name => Some(variants.clone()),
                _ => None,
            },
            _ => None,
        };
        if let Some(variants) = sum {
            if let Some((_, payload)) = variants.get(tag as usize) {
                return Some(payload.clone());
            }
        }
        if let Some(payload) = self.poly_payload_from_app(enum_name, tag, &resolved) {
            return Some(payload);
        }
        if self.is_poly_enum(enum_name) {
            // Scrutinee not yet pinned — freshen an applied type and
            // unify so bindings share type vars with the scrutinee.
            let owner = self.fresh_poly_app_ty(enum_name);
            self.unify(
                expected_ty,
                &owner,
                pattern_range,
                "poly enum pattern scrutinee",
            );
            let resolved = apply_ty_prune(&self.subst, &owner);
            if let Some(payload) = self.poly_payload_from_app(enum_name, tag, &resolved) {
                return Some(payload);
            }
        }
        self.enum_payloads
            .get(enum_name)
            .and_then(|p| p.get(tag as usize).cloned())
    }

    /// Build `Enum<α, …>` with a fresh type variable per type param.
    fn fresh_poly_app_ty(&mut self, enum_name: &str) -> Ty {
        if common::is_builtin_option_enum(enum_name) {
            return option_app_ty(Ty::Var(self.counter.fresh()));
        }
        if common::is_builtin_result_enum(enum_name) {
            return result_app_ty(Ty::Var(self.counter.fresh()), Ty::Var(self.counter.fresh()));
        }
        let params = self
            .generics
            .generic_type_ctors
            .get(enum_name)
            .cloned()
            .unwrap_or_default();
        let args: Vec<Ty> = params
            .iter()
            .map(|_| Ty::Var(self.counter.fresh()))
            .collect();
        Ty::App(Box::new(Ty::Con(enum_name.to_string())), args)
    }

    /// Extract a variant payload from an applied poly enum type
    /// (`Option<int>`, `Box<int>`, …).
    fn poly_payload_from_app(
        &self,
        enum_name: &str,
        tag: u32,
        ty: &Ty,
    ) -> Option<EnumVariantPayloadTy> {
        match ty {
            Ty::App(con, args)
                if matches!(con.as_ref(), Ty::Con(name) if name == enum_name)
                    && common::is_builtin_option_enum(enum_name) =>
            {
                match tag {
                    0 => Some(EnumVariantPayloadTy::Unit),
                    1 => args
                        .first()
                        .cloned()
                        .map(|inner| EnumVariantPayloadTy::Tuple(vec![inner])),
                    _ => None,
                }
            }
            Ty::App(con, args)
                if matches!(con.as_ref(), Ty::Con(name) if name == enum_name)
                    && common::is_builtin_result_enum(enum_name) =>
            {
                match tag {
                    0 => args
                        .first()
                        .cloned()
                        .map(|ok| EnumVariantPayloadTy::Tuple(vec![ok])),
                    1 => args
                        .get(1)
                        .cloned()
                        .map(|err| EnumVariantPayloadTy::Tuple(vec![err])),
                    _ => None,
                }
            }
            Ty::App(con, args)
                if matches!(con.as_ref(), Ty::Con(name) if name == enum_name)
                    && self.generics.generic_type_ctors.contains_key(enum_name) =>
            {
                let param_names = self.generics.generic_type_ctors.get(enum_name)?;
                if param_names.len() != args.len() {
                    return None;
                }
                let mut map = HashMap::new();
                for (p, a) in param_names.iter().zip(args.iter()) {
                    map.insert(p.clone(), a.clone());
                }
                let schema = self.enum_payloads.get(enum_name)?.get(tag as usize)?;
                Some(subst_payload_params(schema, &map))
            }
            Ty::Constructor { owner, .. } => self.poly_payload_from_app(enum_name, tag, owner),
            _ => None,
        }
    }

    // ---- Match ----

    fn infer_match(&mut self, scrutinee: &Output, arms: &[MatchArm], range: Range<usize>) -> Ty {
        let scrutinee_ty = self.infer(scrutinee);
        let resolved_scrutinee = apply_ty_prune(&self.subst, &scrutinee_ty);

        // Set up current_match_lhs for `Expression::Default`
        // (which Decision C preserves but is unreachable in real
        // source — wildcard patterns never reach it).
        let prev = self.current_match_lhs.replace(scrutinee_ty.clone());

        let mut result_ty = Ty::Var(self.counter.fresh());
        let mut first = true;
        let mut coverage: Vec<ArmCoverage> = Vec::with_capacity(arms.len());

        if arms.is_empty() {
            self.current_match_lhs = prev;
            return self.error(
                ErrorCode::GenericTypeError,
                "match has no arms".to_string(),
                range,
            );
        }

        for arm in arms {
            // Step 1: each arm gets a fresh env frame so the
            // pattern's bindings don't leak.
            self.push_scope();

            // Step 2: type the pattern, binding variables. The
            // pattern AST doesn't carry its own range today, so we
            // pass the arm's body range as a reasonable proxy for
            // error anchoring — it's close enough that ariadne
            // points near the offending pattern instead of at byte
            // 0 of the source.
            let pattern_range = arm.pattern.0.into_range();
            let pat_ty = self.infer_pattern(&arm.pattern.1, &resolved_scrutinee, &pattern_range);

            // Narrow an Identifier scrutinee to the matched variant so
            // `p.0` / `p.field` inside the arm use tagged field lookup
            // (tuple indices are shared across variants otherwise).
            let mut refined_scrut: Option<(String, Option<Ty>)> = None;
            if let Expression::Identifier(scrut_name) = scrutinee.1.as_ref()
                && let Pattern::Constructor {
                    enum_name,
                    variant_name,
                    ..
                } = &arm.pattern.1
            {
                if let Some(tag) = self
                    .enum_tags
                    .get(*enum_name)
                    .and_then(|t| t.get(*variant_name).copied())
                {
                    let arity = self
                        .enum_arities
                        .get(*enum_name)
                        .and_then(|a| a.get(tag as usize).copied())
                        .unwrap_or(0);
                    let owner = match &resolved_scrutinee {
                        Ty::Sum { .. } => resolved_scrutinee.clone(),
                        Ty::Constructor { owner, .. } => owner.as_ref().clone(),
                        Ty::Con(name) => {
                            let variant_names =
                                self.enums.get(name.as_str()).cloned().unwrap_or_default();
                            let payloads = self
                                .enum_payloads
                                .get(name.as_str())
                                .cloned()
                                .unwrap_or_default();
                            let variants: Vec<(String, EnumVariantPayloadTy)> =
                                variant_names.into_iter().zip(payloads).collect();
                            Ty::Sum {
                                name: name.clone(),
                                variants,
                            }
                        }
                        other => other.clone(),
                    };
                    let ctor = Ty::Constructor {
                        owner: Box::new(owner),
                        tag,
                        arity,
                    };
                    let prev_cg = self.codegen_var_types.get(*scrut_name).cloned();
                    self.env
                        .insert_top((*scrut_name).to_string(), Scheme::mono(ctor.clone()));
                    self.codegen_var_types
                        .insert((*scrut_name).to_string(), ctor);
                    refined_scrut = Some(((*scrut_name).to_string(), prev_cg));
                }
            }

            // Step 3: unify pattern type with scrutinee.
            self.unify(
                &resolved_scrutinee,
                &pat_ty,
                &arm.body.0.into_range(),
                "match pattern against scrutinee",
            );

            // Step 4: capture coverage info.
            let arm_cov = self.arm_coverage(&arm.pattern.1, &pattern_range);
            coverage.push(arm_cov);

            // Step 5: infer body, unify with result.
            let body_ty = self.infer(&arm.body);
            if let Some((name, prev_cg)) = refined_scrut {
                match prev_cg {
                    Some(ty) => {
                        self.codegen_var_types.insert(name, ty);
                    }
                    None => {
                        self.codegen_var_types.remove(&name);
                    }
                }
            }
            if first {
                result_ty = body_ty;
                first = false;
            } else {
                result_ty = self.join_ty(
                    &result_ty,
                    &body_ty,
                    &arm.body.0.into_range(),
                    "match arm body",
                );
            }

            // Step 6: pop the per-arm env frame.
            self.pop_scope();
        }

        self.current_match_lhs = prev;

        // Record for the post-pass exhaustiveness check. The
        // scrutinee type stored here is the resolved (pruned)
        // version at the time of the match; the post-pass will
        // re-apply the current substitution to handle any
        // variables bound by intervening code.
        self.pending_exhaustive.push(PendingExhaustive {
            scrutinee_ty: resolved_scrutinee,
            arms: coverage,
            match_range: range,
        });

        result_ty
    }

    /// Type-check a pattern against an expected type, binding
    /// variables into the current env frame. Returns the pattern's
    /// type, which is the **expected** type (the sum type, not
    /// the constructor type) — patterns desugar the scrutinee, so
    /// the pattern's type IS the scrutinee's type. The tag
    /// matching (which determines whether the arm is reachable) is
    /// captured separately in [`ArmCoverage`].
    ///
    /// `pattern_range` is the source range of the pattern itself —
    /// or, when not available, a reasonable proxy (the arm's body
    /// range). It is used to anchor pattern-related diagnostics
    /// (`unknown constructor`, `wrong arity`) so ariadne points at
    /// the offending pattern instead of byte 0 of the source.
    fn infer_pattern(
        &mut self,
        pattern: &Pattern,
        expected_ty: &Ty,
        pattern_range: &Range<usize>,
    ) -> Ty {
        use parser::ast::PatternPayload;
        match pattern {
            Pattern::Wildcard => {
                // Wildcard matches anything, binds nothing. The
                // body's bindings (if any) come from nested
                // patterns; wildcard itself has no payload.
                expected_ty.clone()
            }
            Pattern::Binding { name } => {
                // `name => body` binds `name` to the scrutinee in
                // the arm's env. This makes the arm cover every
                // case (Rust semantics). Also record in the
                // codegen side-table so `e.kind` on a match-bound
                // enum (e.g. `Result::Err(e)`) emits LoadField,
                // not GetField.
                let pruned = apply_ty_prune(&self.subst, expected_ty);
                self.env
                    .insert_top(name.to_string(), Scheme::mono(pruned.clone()));
                self.record_codegen_var_type(name.to_string(), pruned.clone());
                pruned
            }
            Pattern::Constructor {
                enum_name,
                variant_name,
                payload,
            } => {
                // 1. Look up the variant's tag in the registry.
                let enum_str = self
                    .resolve_enum_key(enum_name)
                    .unwrap_or_else(|| enum_name.to_string());
                let variant_str = variant_name.to_string();
                let tag_opt = self
                    .enum_tags
                    .get(&enum_str)
                    .and_then(|t| t.get(&variant_str).copied());
                let tag = match tag_opt {
                    Some(t) => t,
                    None => {
                        // Unknown constructor in a pattern is an
                        // error. Record the error and return the
                        // expected type so the arm body is still
                        // processed.
                        self.messages.push(Message::error(
                            ErrorCode::UnknownConstructorPattern,
                            format!(
                                "Pattern references unknown constructor `{}::{}`",
                                enum_str, variant_str
                            ),
                            pattern_range.clone(),
                        ));
                        return expected_ty.clone();
                    }
                };
                let _arity = self
                    .enum_arities
                    .get(&enum_str)
                    .and_then(|a| a.get(tag as usize).copied())
                    .unwrap_or(0);
                let expected_payload = self
                    .poly_or_registry_payload(&enum_str, tag, expected_ty, pattern_range)
                    .unwrap_or(EnumVariantPayloadTy::Unit);

                let (shape_matches, same_shape_with_wrong_arity) =
                    match (&expected_payload, payload) {
                        (EnumVariantPayloadTy::Unit, PatternPayload::Unit) => (true, false),
                        (EnumVariantPayloadTy::Tuple(_), PatternPayload::Tuple(parts)) => {
                            let want = expected_payload.field_count();
                            (parts.len() == want, parts.len() != want)
                        }
                        (EnumVariantPayloadTy::Record(_), PatternPayload::Record(_)) => {
                            // Defer the arity check to the
                            // field-by-field pass below, which
                            // produces more specific diagnostics.
                            (true, false)
                        }
                        _ => (false, false),
                    };
                if !shape_matches {
                    if same_shape_with_wrong_arity {
                        return self.error_with_help(
                            ErrorCode::GenericTypeError,
                            format!(
                                "Constructor pattern `{}::{}` expects {} sub-patterns, got {}",
                                enum_str,
                                variant_str,
                                expected_payload.field_count(),
                                match payload {
                                    PatternPayload::Unit => 0,
                                    PatternPayload::Tuple(parts) => parts.len(),
                                    PatternPayload::Record(fields) => fields.len(),
                                },
                            ),
                            pattern_range.clone(),
                            Some("check the variant's declared payload arity".to_string()),
                        );
                    }
                    return self.error_with_help(
                        ErrorCode::PayloadShapeMismatch, format!(
                            "Constructor pattern `{}::{}` payload shape mismatch (declared as {}, pattern uses {})",
                            enum_str,
                            variant_str,
                            payload_kind_name(&expected_payload),
                            match payload {
                                PatternPayload::Unit => "unit",
                                PatternPayload::Tuple(_) => "tuple",
                                PatternPayload::Record(_) => "record",
                            },
                        ),
                        pattern_range.clone(),
                        Some("check the variant's declared payload shape".to_string()),
                    );
                }

                // 3. Recurse into each sub-pattern with the
                // corresponding payload type. The payload type
                // comes from the pre-pass's `enum_payloads`
                // (already resolved, e.g. `int` for
                // `Option::Some(int)`).
                match payload {
                    PatternPayload::Unit => {}
                    PatternPayload::Tuple(parts) => {
                        let expected_tys = expected_payload.field_types();
                        for (sub_pat, expected_ty) in parts.iter().zip(expected_tys.iter()) {
                            let _ = self.infer_pattern(&sub_pat.1, expected_ty, pattern_range);
                        }
                    }
                    PatternPayload::Record(fields) => {
                        // Build a name → pattern map for the
                        // pattern site, then walk DECLARATION
                        // order. Each declared field must be
                        // present exactly once; the codegen binds
                        // in slot order (= declaration order).
                        let mut pattern_site: std::collections::HashMap<&str, &Pattern> =
                            std::collections::HashMap::with_capacity(fields.len());
                        for pf in fields {
                            if pattern_site.insert(pf.name, &pf.pattern.1).is_some() {
                                return self.error_with_help(
                                    ErrorCode::DuplicateField,
                                    format!(
                                        "Duplicate field `{}` in record pattern `{}::{}`",
                                        pf.name, enum_str, variant_str,
                                    ),
                                    pattern_range.clone(),
                                    Some("each field must appear exactly once".to_string()),
                                );
                            }
                        }
                        let EnumVariantPayloadTy::Record(decl_fields) = &expected_payload else {
                            unreachable!()
                        };
                        for (decl_name, decl_ty) in decl_fields.iter() {
                            let sub_pat = match pattern_site.get(decl_name.as_str()) {
                                Some(p) => *p,
                                None => {
                                    return self.error_with_help(
                                        ErrorCode::MissingField,
                                        format!(
                                            "Missing field `{}` in record pattern `{}::{}`",
                                            decl_name, enum_str, variant_str,
                                        ),
                                        pattern_range.clone(),
                                        Some(format!(
                                            "add `{0}: _` (or `{0}: binding`) to the pattern",
                                            decl_name,
                                        )),
                                    );
                                }
                            };
                            let _ = self.infer_pattern(sub_pat, decl_ty, pattern_range);
                        }
                        // Check for unknown field names.
                        for pf in fields {
                            if !decl_fields.iter().any(|(dn, _)| dn == pf.name) {
                                return self.error_with_help(
                                    ErrorCode::UnknownField,
                                    format!(
                                        "Unknown field `{}` in record pattern `{}::{}`",
                                        pf.name, enum_str, variant_str,
                                    ),
                                    pattern_range.clone(),
                                    Some(format!(
                                        "the declared fields are: {}",
                                        decl_fields
                                            .iter()
                                            .map(|(n, _)| format!("`{}`", n))
                                            .collect::<Vec<_>>()
                                            .join(", "),
                                    )),
                                );
                            }
                        }
                    }
                }

                // 4. The pattern's type is the *expected* type —
                // patterns desugar the scrutinee, so the pattern
                // returns whatever the scrutinee had. (If the
                // scrutinee was a Ty::Constructor for a specific
                // tag, the pattern is still of that same type;
                // exhaustiveness checking will report the
                // arm as unreachable.)
                expected_ty.clone()
            }
        }
    }

    /// Bind names from an irrefutable `let` pattern against `expected_ty`
    /// (the RHS type). Supports nested tuples/records and `_`.
    fn infer_let_pattern(
        &mut self,
        pattern: &parser::ast::LetPattern,
        expected_ty: &Ty,
        pattern_range: &Range<usize>,
    ) -> Ty {
        // Reject `let (x, x) = …` / nested duplicate binders up front.
        {
            let mut seen = std::collections::HashSet::new();
            if let Some(dup) = Self::first_duplicate_let_binder(pattern, &mut seen) {
                return self.error_with_help(
                    ErrorCode::VariableRedeclaration,
                    format!("Duplicate binder `{dup}` in let pattern"),
                    pattern_range.clone(),
                    Some("each name may appear at most once in a let pattern".to_string()),
                );
            }
        }
        self.infer_let_pattern_inner(pattern, expected_ty, pattern_range)
    }

    fn first_duplicate_let_binder<'a>(
        pattern: &'a parser::ast::LetPattern<'a>,
        seen: &mut std::collections::HashSet<&'a str>,
    ) -> Option<&'a str> {
        use parser::ast::LetPattern;
        match pattern {
            LetPattern::Wildcard => None,
            LetPattern::Binding { name } => {
                if !seen.insert(*name) {
                    Some(*name)
                } else {
                    None
                }
            }
            LetPattern::Tuple(parts) => {
                for p in parts {
                    if let Some(d) = Self::first_duplicate_let_binder(p, seen) {
                        return Some(d);
                    }
                }
                None
            }
            LetPattern::Record(fields) => {
                for pf in fields {
                    if let Some(d) = Self::first_duplicate_let_binder(&pf.pattern, seen) {
                        return Some(d);
                    }
                }
                None
            }
        }
    }

    fn infer_let_pattern_inner(
        &mut self,
        pattern: &parser::ast::LetPattern,
        expected_ty: &Ty,
        pattern_range: &Range<usize>,
    ) -> Ty {
        use parser::ast::LetPattern;
        let expected = apply_ty_prune(&self.subst, expected_ty);
        match pattern {
            LetPattern::Wildcard => expected,
            LetPattern::Binding { name } => {
                self.env
                    .insert_top(name.to_string(), Scheme::mono(expected.clone()));
                self.record_codegen_var_type(name.to_string(), expected.clone());
                expected
            }
            LetPattern::Tuple(parts) => {
                let elem_tys = match &expected {
                    Ty::Tuple(tys) => {
                        if tys.len() != parts.len() {
                            return self.error_with_help(
                                ErrorCode::GenericTypeError,
                                format!(
                                    "tuple pattern has {} elements, but value has type `{}`",
                                    parts.len(),
                                    expected,
                                ),
                                pattern_range.clone(),
                                Some("adjust the pattern or the RHS tuple arity".to_string()),
                            );
                        }
                        tys.clone()
                    }
                    Ty::Var(_) => {
                        let fresh: Vec<Ty> = parts
                            .iter()
                            .map(|_| Ty::Var(self.counter.fresh()))
                            .collect();
                        let tup = Ty::Tuple(fresh.clone());
                        self.unify(&expected, &tup, pattern_range, "let tuple destructure");
                        fresh
                    }
                    other => {
                        return self.error_with_help(
                            ErrorCode::GenericTypeError,
                            format!("cannot destructure type `{}` with a tuple pattern", other,),
                            pattern_range.clone(),
                            Some("RHS must be a tuple".to_string()),
                        );
                    }
                };
                for (sub, ty) in parts.iter().zip(elem_tys.iter()) {
                    let _ = self.infer_let_pattern_inner(sub, ty, pattern_range);
                }
                expected_ty.clone()
            }
            LetPattern::Record(fields) => {
                let decl_fields = match &expected {
                    Ty::Record { fields: decl } => decl.clone(),
                    Ty::Var(_) => {
                        // Synthesize a record type from the pattern field names.
                        let mut synth = Vec::with_capacity(fields.len());
                        let mut seen = std::collections::HashSet::new();
                        for pf in fields {
                            if !seen.insert(pf.name) {
                                return self.error_with_help(
                                    ErrorCode::DuplicateField,
                                    format!("Duplicate field `{}` in record pattern", pf.name),
                                    pattern_range.clone(),
                                    Some("each field must appear exactly once".to_string()),
                                );
                            }
                            synth.push((pf.name.to_string(), Ty::Var(self.counter.fresh())));
                        }
                        synth.sort_by(|a, b| a.0.cmp(&b.0));
                        let rec = Ty::Record {
                            fields: synth.clone(),
                        };
                        self.unify(&expected, &rec, pattern_range, "let record destructure");
                        synth
                    }
                    other => {
                        return self.error_with_help(
                            ErrorCode::GenericTypeError,
                            format!("cannot destructure type `{}` with a record pattern", other,),
                            pattern_range.clone(),
                            Some("RHS must be a record (dict)".to_string()),
                        );
                    }
                };
                let mut pattern_site: std::collections::HashMap<&str, &LetPattern> =
                    std::collections::HashMap::with_capacity(fields.len());
                for pf in fields {
                    if pattern_site.insert(pf.name, &pf.pattern).is_some() {
                        return self.error_with_help(
                            ErrorCode::DuplicateField,
                            format!("Duplicate field `{}` in record pattern", pf.name),
                            pattern_range.clone(),
                            Some("each field must appear exactly once".to_string()),
                        );
                    }
                }
                for pf in fields {
                    let Some((_, fty)) = decl_fields.iter().find(|(n, _)| n == pf.name) else {
                        return self.error_with_help(
                            ErrorCode::UnknownField,
                            format!("Cannot find field `{}` on record `{}`", pf.name, expected,),
                            pattern_range.clone(),
                            Some(format!(
                                "the record has fields: {}",
                                decl_fields
                                    .iter()
                                    .map(|(n, _)| format!("`{}`", n))
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            )),
                        );
                    };
                    let _ = self.infer_let_pattern_inner(&pf.pattern, fty, pattern_range);
                }
                expected_ty.clone()
            }
        }
    }

    /// Inspect the first non-trivial sub-pattern of a payload and
    /// report which inner tag (if any) it tests. Two arms of the
    /// same outer tag are reachable as long as their inner coverage
    /// differs — e.g. `Result::Ok(Option::Some(v))` and
    /// `Result::Ok(Option::None)` are two distinct reachable arms.
    /// The codegen's inner `JUMP_IF_MATCH` test chain guarantees
    /// this at runtime; the typechecker just needs to stay out of
    /// the way.
    fn pattern_coverage(
        pattern: &Pattern,
        enum_tags: &BTreeMap<String, BTreeMap<String, u32>>,
    ) -> CoverageTree {
        match pattern {
            Pattern::Wildcard | Pattern::Binding { .. } => CoverageTree::Any,
            Pattern::Constructor {
                enum_name,
                variant_name,
                payload,
                ..
            } => {
                let tag = enum_tags
                    .get(enum_name.to_string().as_str())
                    .and_then(|t| t.get(variant_name.to_string().as_str()).copied());
                let inner = Self::payload_coverage(payload, enum_tags);
                tag.map(|t| CoverageTree::Tag(t, vec![inner]))
                    .unwrap_or(CoverageTree::Any)
            }
        }
    }

    fn payload_coverage(
        payload: &parser::ast::PatternPayload<'_>,
        enum_tags: &BTreeMap<String, BTreeMap<String, u32>>,
    ) -> CoverageTree {
        use parser::ast::PatternPayload;
        match payload {
            PatternPayload::Unit => CoverageTree::Any,
            PatternPayload::Tuple(parts) => CoverageTree::Tuple(
                parts
                    .iter()
                    .map(|p| Self::pattern_coverage(&p.1, enum_tags))
                    .collect(),
            ),
            PatternPayload::Record(fields) => CoverageTree::Record(
                fields
                    .iter()
                    .map(|f| {
                        (
                            f.name.to_string(),
                            Self::pattern_coverage(&f.pattern.1, enum_tags),
                        )
                    })
                    .collect(),
            ),
        }
    }

    /// Payload coverage for constructor arms (full inner tree).
    fn inner_coverage(
        payload: &parser::ast::PatternPayload<'_>,
        enum_tags: &BTreeMap<String, BTreeMap<String, u32>>,
    ) -> CoverageTree {
        Self::payload_coverage(payload, enum_tags)
    }

    /// Capture per-arm coverage info for the deferred
    /// exhaustiveness check.
    fn arm_coverage(&self, pattern: &Pattern, range: &Range<usize>) -> ArmCoverage {
        match pattern {
            Pattern::Wildcard => ArmCoverage {
                tag: None,
                inner: CoverageTree::Any,
                is_catchall: true,
                range: range.clone(),
            },
            Pattern::Binding { .. } => ArmCoverage {
                tag: None,
                inner: CoverageTree::Any,
                is_catchall: true,
                range: range.clone(),
            },
            Pattern::Constructor {
                enum_name,
                variant_name,
                payload,
                ..
            } => {
                // Imported short names resolve via env/FQN — same as
                // `infer_pattern` / `infer_construct`.
                let enum_key = self
                    .resolve_enum_key(enum_name)
                    .unwrap_or_else(|| enum_name.to_string());
                let tag = self
                    .enum_tags
                    .get(enum_key.as_str())
                    .and_then(|t| t.get(variant_name.to_string().as_str()).copied());
                let inner = Self::inner_coverage(payload, &self.enum_tags);
                ArmCoverage {
                    tag,
                    inner,
                    is_catchall: false,
                    range: range.clone(),
                }
            }
        }
    }

    /// Post-pass: run every deferred exhaustiveness check. By this
    /// point the substitution is closed, so the scrutinee type is
    /// fully resolved (any free type variables that were bound
    /// since the match site are visible here).
    fn run_pending_exhaustiveness(&mut self) {
        // Drain into a local so we can release the borrow on
        // `self` before mutating `self.messages`.
        let pending: Vec<PendingExhaustive> = std::mem::take(&mut self.pending_exhaustive);
        for p in &pending {
            self.check_exhaustiveness(p);
        }
    }

    /// Verify a single match site. Records diagnostics but does
    /// not abort — error recovery continues.
    fn check_exhaustiveness(&mut self, pending: &PendingExhaustive) {
        // Re-resolve the scrutinee under the current substitution
        // so any variables bound between the match site and the
        // post-pass are visible.
        let resolved = apply_ty_prune(&self.subst, &pending.scrutinee_ty);

        // Track which (outer tag, inner coverage) pairs have been
        // seen and whether a catch-all (wildcard / binding) is
        // present. Two arms with the same outer tag but DIFFERENT
        // inner coverage (e.g. `Result::Ok(Option::Some(v))` vs
        // `Result::Ok(Option::None)`) are both reachable — the
        // codegen's inner `JUMP_IF_MATCH` chain dispatches between
        // them at runtime. Only when both the outer tag AND the
        // inner coverage match an earlier arm is the arm truly
        // unreachable.
        let mut seen: BTreeMap<u32, BTreeSet<CoverageTree>> = BTreeMap::new();
        let mut has_catchall = false;
        for arm in &pending.arms {
            if arm.is_catchall {
                has_catchall = true;
            } else if let Some(t) = arm.tag {
                let inner_seen = seen.entry(t).or_default();
                if !inner_seen.insert(arm.inner.clone()) {
                    // Duplicate (tag, inner coverage) — this arm
                    // is unreachable.
                    self.messages.push(Message::error(
                        ErrorCode::UnreachableArm,
                        "Unreachable arm: this pattern is matched by an earlier arm".to_string(),
                        arm.range.clone(),
                    ));
                }
            }
        }

        if has_catchall {
            // A wildcard / binding arm covers every remaining
            // case. No further error needed.
            return;
        }

        // Unwrap a Constructor to its parent sum/app. For Ty::Var /
        // Ty::Con, no exhaustiveness check.
        let variants = match &resolved {
            Ty::Sum { variants, .. } => Some(variants.clone()),
            Ty::Constructor { owner, .. } => match owner.as_ref() {
                Ty::Sum { variants, .. } => Some(variants.clone()),
                other => self.poly_variants_from_app(other),
            },
            Ty::Con(name) if self.enums.contains_key(name.as_str()) => {
                let variant_names = self.enums.get(name.as_str()).cloned().unwrap_or_default();
                let payloads = self
                    .enum_payloads
                    .get(name.as_str())
                    .cloned()
                    .unwrap_or_default();
                Some(variant_names.into_iter().zip(payloads).collect())
            }
            other => self.poly_variants_from_app(other),
        };

        if let Some(variants) = variants {
            // An outer tag is "covered" for the purpose of the
            // non-exhaustive check if any arm with that tag
            // exists. The inner coverage only matters for the
            // duplicate-arm check above.
            let covered: BTreeSet<u32> = seen.into_keys().collect();
            let missing: Vec<String> = variants
                .iter()
                .enumerate()
                .filter(|(tag, _)| !covered.contains(&(*tag as u32)))
                .map(|(_, (n, _))| n.clone())
                .collect();
            if !missing.is_empty() {
                let names = missing
                    .iter()
                    .map(|s| format!("`{}`", s))
                    .collect::<Vec<_>>()
                    .join(", ");
                let mut msg = Message::error(
                    ErrorCode::NonExhaustiveMatch,
                    format!("Non-exhaustive match: variants not covered: {}", names),
                    pending.match_range.clone(),
                );
                msg.with_help(
                    "add a wildcard arm `_ => ...` to cover the remaining cases".to_string(),
                );
                self.messages.push(msg);
            }
        }
    }

    /// Variant list for exhaustiveness from an applied poly enum type.
    fn poly_variants_from_app(&self, ty: &Ty) -> Option<Vec<(String, EnumVariantPayloadTy)>> {
        match ty {
            Ty::App(con, args) if matches!(con.as_ref(), Ty::Con(name) if common::is_builtin_option_enum(name)) =>
            {
                let inner = args.first()?.clone();
                Some(vec![
                    ("None".into(), EnumVariantPayloadTy::Unit),
                    ("Some".into(), EnumVariantPayloadTy::Tuple(vec![inner])),
                ])
            }
            Ty::App(con, args) if matches!(con.as_ref(), Ty::Con(name) if common::is_builtin_result_enum(name)) =>
            {
                let ok = args.first()?.clone();
                let err = args.get(1)?.clone();
                Some(vec![
                    ("Ok".into(), EnumVariantPayloadTy::Tuple(vec![ok])),
                    ("Err".into(), EnumVariantPayloadTy::Tuple(vec![err])),
                ])
            }
            Ty::App(con, args) if matches!(con.as_ref(), Ty::Con(name) if self.generics.generic_type_ctors.contains_key(name)) =>
            {
                let Ty::Con(enum_name) = con.as_ref() else {
                    return None;
                };
                let param_names = self.generics.generic_type_ctors.get(enum_name)?;
                if param_names.len() != args.len() {
                    return None;
                }
                let mut map = HashMap::new();
                for (p, a) in param_names.iter().zip(args.iter()) {
                    map.insert(p.clone(), a.clone());
                }
                let names = self.enums.get(enum_name)?;
                let payloads = self.enum_payloads.get(enum_name)?;
                if names.len() != payloads.len() {
                    return None;
                }
                Some(
                    names
                        .iter()
                        .zip(payloads.iter())
                        .map(|(n, p)| (n.clone(), subst_payload_params(p, &map)))
                        .collect(),
                )
            }
            Ty::Constructor { owner, .. } => self.poly_variants_from_app(owner),
            _ => None,
        }
    }

    fn string_fn_for_call(&self, ident: &str) -> Option<StringBuiltin> {
        self.string_fn_in_scope(ident).or_else(|| {
            ident
                .strip_prefix("string::")
                .and_then(StringBuiltin::from_name)
        })
    }

    fn infer_string_format_call(&mut self, args: &[Output], range: Range<usize>) -> Ty {
        let Some((fmt, rest)) = args.split_first() else {
            return self.error(
                ErrorCode::WrongArity,
                "`string::format` expects at least 1 argument".to_string(),
                range,
            );
        };
        let params = Some(rest.to_vec());
        if !matches!(fmt.1.as_ref(), Expression::String(_)) {
            let _ = self.infer(fmt);
            for arg in rest {
                let _ = self.infer(arg);
            }
            return self.error_with_help(
                ErrorCode::GenericTypeError,
                "`string::format` requires a string literal as its first argument".to_string(),
                fmt.0.into_range(),
                Some(
                    "literal format strings allow the compiler to check `%` specifiers".to_string(),
                ),
            );
        }
        self.infer_print(fmt, &params, range, "string::format");
        string()
    }

    /// Type-check a `string::format` call: the format string must be a
    /// literal, and each `%X` specifier's corresponding argument must have a
    /// matching type.
    fn infer_print(
        &mut self,
        fmt: &Output,
        params: &Option<Vec<Output>>,
        range: Range<usize>,
        ctx: &str,
    ) {
        let fmt_ty = self.infer(fmt);
        self.unify(&fmt_ty, &string(), &fmt.0.into_range(), "print format");

        // Pull the format string out of the literal so we can
        // parse its specifiers. If the format isn't a string
        // literal, skip validation (the user has a type error
        // elsewhere; we shouldn't cascade).
        let fmt_str = match fmt.1.as_ref() {
            Expression::String(s) => Some(s.to_string()),
            _ => None,
        };

        let mut spec_index = 0usize;
        if let (Some(s), Some(p)) = (fmt_str.as_deref(), params) {
            for (i, ch) in s.char_indices() {
                if ch == '%' {
                    // Look ahead for the specifier.
                    let rest = &s[i + 1..];
                    let mut chars = rest.chars();
                    if let Some(spec) = chars.next() {
                        // Handle `%%` (literal %). It consumes
                        // no argument.
                        if spec == '%' {
                            continue;
                        }
                        // We have `%X`. Validate the Nth arg.
                        if let Some(arg) = p.get(spec_index) {
                            let arg_ty = self.infer(arg);
                            let arg_range = arg.0.into_range();
                            let arg_ty_pruned = apply_ty_prune(&self.subst, &arg_ty);
                            self.check_format_arg(
                                spec,
                                &arg_ty_pruned,
                                &arg_range,
                                ctx,
                                spec_index,
                            );
                            spec_index += 1;
                        } else {
                            // Specifier with no arg — also an
                            // error.
                            let mut msg = Message::error(
                                ErrorCode::GenericTypeError,
                                format!(
                                    "Format string has more specifiers than arguments \
                                     (`%{}` is argument #{})",
                                    spec,
                                    spec_index + 1
                                ),
                                range.clone(),
                            );
                            msg.with_help(format!(
                                "add an argument for `%%{}` in the call site",
                                spec
                            ));
                            self.messages.push(msg);
                            return;
                        }
                    } else {
                        // Trailing `%` with no specifier. Skip.
                        break;
                    }
                }
            }
        } else if let Some(p) = params {
            // No specifiers (or non-literal format) — type-check
            // each param and discard (the VM still consumes the
            // args at the bytecode level, even if the format
            // string contains no specifiers).
            for arg in p {
                let _ = self.infer(arg);
            }
        }
    }

    /// Validate one format argument against its `%X` specifier.
    fn check_format_arg(
        &mut self,
        spec: char,
        arg_ty: &Ty,
        arg_range: &Range<usize>,
        ctx: &str,
        spec_index: usize,
    ) {
        if spec == 'v' {
            match arg_ty {
                Ty::Var(v) => {
                    if self.user_dict_index(*v, "Show").is_none() {
                        self.bind_matching_abstract_constraints(Some(*v), "Show");
                    }
                    if self.user_dict_index(*v, "Show").is_some() {
                        self.record_bound_display(arg_range, *v);
                    } else {
                        let mut msg = Message::error(
                            ErrorCode::FormatSpecifierMismatch,
                            format!(
                                "Format specifier `%v` requires a `Show` instance, found {}",
                                arg_ty
                            ),
                            arg_range.clone(),
                        );
                        msg.with_help(format!(
                            "add a `T: Show` bound, or use a concrete type; \
                             while checking `{}` format argument #{}",
                            ctx,
                            spec_index + 1
                        ));
                        self.messages.push(msg);
                    }
                }
                other => {
                    if !self.is_showable_for_format(other) {
                        let mut msg = Message::error(
                            ErrorCode::FormatSpecifierMismatch,
                            format!(
                                "Format specifier `%v` requires a `Show` instance, found {}",
                                other
                            ),
                            arg_range.clone(),
                        );
                        msg.with_help(format!(
                            "implement `Show` for this type, or use a concrete specifier; \
                             while checking `{}` format argument #{}",
                            ctx,
                            spec_index + 1
                        ));
                        self.messages.push(msg);
                    }
                }
            }
            return;
        }

        // Concrete specifiers on an open type:
        // - quantified type parameters (`fn f<T>(T x)`) must use `%v`
        // - free inference vars (e.g. coroutine send) unify with the
        //   specifier's expected type (same as using the value in a
        //   typed context)
        if let Ty::Var(v) = arg_ty {
            let is_type_param = self
                .type_params_in_scope
                .iter()
                .any(|frame| frame.values().any(|id| id == v));
            if is_type_param {
                let mut msg = Message::error(
                    ErrorCode::FormatSpecifierMismatch,
                    format!(
                        "Format specifier `%{}` cannot be used with an open type `{}`",
                        spec, arg_ty
                    ),
                    arg_range.clone(),
                );
                msg.with_help(format!(
                    "use `%v` (requires `Show`) instead of `%{}`; \
                     while checking `{}` format argument #{}",
                    spec,
                    ctx,
                    spec_index + 1
                ));
                self.messages.push(msg);
                return;
            }
            let expected_ty = match spec {
                'i' | 'd' | 'b' | 'x' | 'u' | 'p' => int(),
                'f' => float(),
                's' => string(),
                'z' => boolean(),
                _ => {
                    let mut msg = Message::error(
                        ErrorCode::FormatSpecifierMismatch,
                        format!("Unknown format specifier `%{}`", spec),
                        arg_range.clone(),
                    );
                    msg.with_help(format!(
                        "while checking `{}` format argument #{}",
                        ctx,
                        spec_index + 1
                    ));
                    self.messages.push(msg);
                    return;
                }
            };
            // `byte` is printable with integer format specs (same runtime word).
            if matches!(spec, 'i' | 'd' | 'b' | 'x' | 'u' | 'p') && Self::is_byte_ty(arg_ty) {
                return;
            }
            self.unify(
                arg_ty,
                &expected_ty,
                arg_range,
                &format!("{} format argument #{}", ctx, spec_index + 1),
            );
            return;
        }

        let expected = format_specifier_type(spec);
        if !type_matches_specifier(arg_ty, spec) {
            let mut msg = Message::error(
                ErrorCode::FormatSpecifierMismatch,
                format!(
                    "Format specifier `%{}` requires {}, found {}",
                    spec, expected, arg_ty
                ),
                arg_range.clone(),
            );
            msg.with_help(format!(
                "while checking `{}` format argument #{}",
                ctx,
                spec_index + 1
            ));
            self.messages.push(msg);
        }
    }

    fn is_showable_for_format(&self, ty: &Ty) -> bool {
        let resolved = apply_ty_prune(&self.subst, ty);
        match resolved {
            Ty::Var(_) => false,
            Ty::Tuple(items) => items.iter().all(|item| self.is_showable_for_format(item)),
            Ty::Record { fields } => fields
                .iter()
                .all(|(_, field_ty)| self.is_showable_for_format(field_ty)),
            other => {
                let lookup = show_lookup_ty(&other);
                self.generics.has_instance("Show", &lookup)
            }
        }
    }

    // ============================================================
    // ============================================================
    //  Codegen helpers
    // ============================================================
    // ============================================================

    /// Map surface enum paths (`ffi::types`, short `E`) to the registry key
    /// (`module::E` after COI-110).
    fn registry_enum_key(&self, enum_name: &str) -> String {
        if common::is_builtin_ffi_enum(enum_name) {
            common::BUILTIN_FFI_TYPE_ENUM.to_string()
        } else {
            self.resolve_enum_key_for_codegen(enum_name)
                .unwrap_or_else(|| enum_name.to_string())
        }
    }

    pub fn tag_for(&self, enum_name: &str, variant_name: &str) -> Option<u32> {
        let key = self.registry_enum_key(enum_name);
        self.enum_tags
            .get(&key)
            .and_then(|t| t.get(variant_name).copied())
    }

    /// Payload arity for `(enum_name, variant_name)`.
    pub fn arity_for(&self, enum_name: &str, variant_name: &str) -> Option<usize> {
        let key = self.registry_enum_key(enum_name);
        self.tag_for(enum_name, variant_name).and_then(|t| {
            self.enum_arities
                .get(&key)
                .and_then(|a| a.get(t as usize).copied())
        })
    }

    /// Variants in source-declaration order: `(name, tag, payload_types)`.
    pub fn enum_variants(&self, enum_name: &str) -> Option<Vec<(String, u32, Vec<Ty>)>> {
        let key = self.registry_enum_key(enum_name);
        let names = self.enums.get(&key)?.clone();
        let tags = self.enum_tags.get(&key)?.clone();
        let payloads = self.enum_payloads.get(&key)?.clone();
        let mut out = Vec::with_capacity(names.len());
        for (i, name) in names.iter().enumerate() {
            let tag = tags.get(name).copied().unwrap_or(i as u32);
            let payload_tys: Vec<Ty> = match payloads.get(i) {
                Some(EnumVariantPayloadTy::Unit) => Vec::new(),
                Some(EnumVariantPayloadTy::Tuple(tys)) => tys.clone(),
                Some(EnumVariantPayloadTy::Record(fields)) => {
                    fields.iter().map(|(_, ty)| ty.clone()).collect()
                }
                None => Vec::new(),
            };
            out.push((name.clone(), tag, payload_tys));
        }
        Some(out)
    }

    /// Look up the declared payload for `(enum_name, variant_name)`
    /// as a list of `(field_name, field_type)` pairs in
    /// DECLARATION order. The codegen uses this to reorder record
    /// call-site fields to declaration order (the VM's
    /// `MAKE_ENUM` pushes payload args in pop order — the first
    /// popped is `payload[0]`).
    ///
    /// For Unit variants, returns an empty Vec. For Tuple
    /// variants, the field names are synthetic (`"0"`, `"1"`, …)
    /// — see `EnumVariantPayloadTy::field_pairs`. For Record
    /// variants, the field names are the declared names.
    pub fn payload_tys_for(&self, enum_name: &str, variant_name: &str) -> Vec<(String, Ty)> {
        let key = self.registry_enum_key(enum_name);
        let tag = match self.tag_for(enum_name, variant_name) {
            Some(t) => t,
            None => return Vec::new(),
        };
        match self
            .enum_payloads
            .get(&key)
            .and_then(|p| p.get(tag as usize))
        {
            Some(payload) => payload.field_pairs(),
            None => Vec::new(),
        }
    }

    /// Field index in a record-shaped variant (codegen).
    ///
    /// When `specific_tag` is set (match-narrowed receiver), only that
    /// variant is searched — required for shared tuple indices `"0"`, `"1"`, …
    pub fn field_index_for(&self, enum_name: &str, field: &str) -> Option<(String, u16)> {
        self.field_index_for_tagged(enum_name, field, None)
    }

    /// Like [`Self::field_index_for`], optionally restricted to one variant tag.
    pub fn field_index_for_tagged(
        &self,
        enum_name: &str,
        field: &str,
        specific_tag: Option<u32>,
    ) -> Option<(String, u16)> {
        let key = self.registry_enum_key(enum_name);
        let payloads = self.enum_payloads.get(&key)?;
        let names = self.enums.get(&key)?;
        if let Some(tag) = specific_tag {
            let i = tag as usize;
            let payload = payloads.get(i)?;
            let variant_name = names.get(i)?.clone();
            match payload {
                EnumVariantPayloadTy::Record(fields) => {
                    for (j, (fname, _)) in fields.iter().enumerate() {
                        if fname == field {
                            return Some((variant_name, j as u16));
                        }
                    }
                }
                EnumVariantPayloadTy::Tuple(parts) => {
                    if let Ok(idx) = field.parse::<usize>() {
                        if idx < parts.len() {
                            return Some((variant_name, idx as u16));
                        }
                    }
                }
                _ => {}
            }
            return None;
        }
        // Prefer declared record field names.
        for (i, payload) in payloads.iter().enumerate() {
            if let EnumVariantPayloadTy::Record(fields) = payload {
                for (j, (fname, _)) in fields.iter().enumerate() {
                    if fname == field {
                        let variant_name = names
                            .get(i)
                            .cloned()
                            .unwrap_or_else(|| format!("variant_{}", i));
                        return Some((variant_name, j as u16));
                    }
                }
            }
        }
        // Synthetic tuple indices `"0"`, `"1"`, … (used by derive expansion
        // and any AST-level Access that targets a tuple payload slot).
        if let Ok(idx) = field.parse::<usize>() {
            let mut match_count = 0;
            let mut found: Option<(String, u16)> = None;
            for (i, payload) in payloads.iter().enumerate() {
                if let EnumVariantPayloadTy::Tuple(parts) = payload {
                    if idx < parts.len() {
                        match_count += 1;
                        let variant_name = names
                            .get(i)
                            .cloned()
                            .unwrap_or_else(|| format!("variant_{}", i));
                        found = Some((variant_name, idx as u16));
                    }
                }
            }
            if match_count == 1 {
                return found;
            }
        }
        None
    }

    /// Record / tuple-payload field type by enum and field name (chained
    /// `Expression::Access` codegen, derive expansion).
    pub fn field_type_for(&self, enum_name: &str, field: &str) -> Option<Ty> {
        let payloads = self.enum_payloads.get(enum_name)?;
        for payload in payloads {
            if let EnumVariantPayloadTy::Record(fields) = payload {
                for (fname, fty) in fields {
                    if fname == field {
                        return Some(fty.clone());
                    }
                }
            }
        }
        if let Ok(idx) = field.parse::<usize>() {
            let mut match_count = 0;
            let mut found: Option<Ty> = None;
            for payload in payloads {
                if let EnumVariantPayloadTy::Tuple(parts) = payload {
                    if let Some(fty) = parts.get(idx) {
                        match_count += 1;
                        found = Some(fty.clone());
                    }
                }
            }
            if match_count == 1 {
                return found;
            }
        }
        None
    }

    /// Enter or refine Result mode for the enclosing function.
    /// Returns the Ok payload type `T`.
    fn ensure_result_mode(&mut self, err_ty: &Ty, range: &Range<usize>) -> Ty {
        if self.fn_option_mode.is_some() {
            return self.error(
                ErrorCode::ConflictingErrorType,
                "cannot mix Option `?` and Result `raise`/`?` in the same function".into(),
                range.clone(),
            );
        }
        if let Some((ok, err)) = self.fn_result_mode.clone() {
            self.unify(&err, err_ty, range, "error type");
            return apply_ty_prune(&self.subst, &ok);
        }
        if let Some(ret) = self.current_return_ty.clone() {
            let resolved = apply_ty_prune(&self.subst, &ret);
            if let Some((ok, err)) = result_ok_err(&resolved) {
                self.fn_result_mode = Some((ok.clone(), err.clone()));
                self.unify(&err, err_ty, range, "error type");
                self.current_return_ty = Some(ok.clone());
                return ok;
            }
            if is_option_ty(&resolved) {
                return self.error(
                    ErrorCode::ConflictingErrorType,
                    "function returns Option; cannot use Result `raise`/`?`".into(),
                    range.clone(),
                );
            }
            // Pin a free / non-Result return to Result<ok, err>.
            let ok = Ty::Var(self.counter.fresh());
            let result = result_ty(ok.clone(), err_ty.clone());
            self.unify(&ret, &result, range, "result return type");
            self.fn_result_mode = Some((ok.clone(), err_ty.clone()));
            self.current_return_ty = Some(ok.clone());
            ok
        } else {
            self.error(
                ErrorCode::InvalidTry,
                "`raise` / `?` outside of a function".into(),
                range.clone(),
            )
        }
    }

    /// Enter or refine Option mode for the enclosing function.
    fn ensure_option_mode(&mut self, inner_ty: &Ty, range: &Range<usize>) {
        if self.fn_result_mode.is_some() {
            self.error(
                ErrorCode::ConflictingErrorType,
                "cannot mix Result `raise`/`?` and Option `?` in the same function".into(),
                range.clone(),
            );
            return;
        }
        if let Some(inner) = self.fn_option_mode.clone() {
            self.unify(&inner, inner_ty, range, "option payload");
            return;
        }
        if let Some(ret) = self.current_return_ty.clone() {
            let resolved = apply_ty_prune(&self.subst, &ret);
            if is_option_ty(&resolved) {
                if let Some(existing) = option_inner(&resolved) {
                    self.unify(&existing, inner_ty, range, "option payload");
                }
                self.fn_option_mode = Some(inner_ty.clone());
                return;
            }
            if is_result_ty(&resolved) {
                self.error(
                    ErrorCode::ConflictingErrorType,
                    "function returns Result; cannot use Option `?`".into(),
                    range.clone(),
                );
                return;
            }
            let opt = option_ty(inner_ty.clone());
            self.unify(&ret, &opt, range, "option return type");
            self.fn_option_mode = Some(inner_ty.clone());
        } else {
            self.error(
                ErrorCode::InvalidTry,
                "`?` outside of a function".into(),
                range.clone(),
            );
        }
    }

    /// Resolve `ty.field` for optional chaining (inner of Option).
    fn field_type_from_ty(&mut self, ty: &Ty, field: &str, range: &Range<usize>) -> Ty {
        let resolved = apply_ty_prune(&self.subst, ty);
        match &resolved {
            Ty::Sum { name, variants } => {
                self.access_field_in_sum(name, variants, None, field, range.clone())
            }
            Ty::Constructor { tag, owner, .. } => match owner.as_ref() {
                Ty::Sum { name, variants } => {
                    self.access_field_in_sum(name, variants, Some(*tag), field, range.clone())
                }
                _ => self.error(
                    ErrorCode::UnknownField,
                    format!("Cannot access field `{}`", field),
                    range.clone(),
                ),
            },
            Ty::Record { fields } => {
                if let Some((_, fty)) = fields.iter().find(|(n, _)| n == field) {
                    fty.clone()
                } else {
                    self.error(
                        ErrorCode::UnknownField,
                        format!("Cannot find field `{}` on record", field),
                        range.clone(),
                    )
                }
            }
            Ty::Con(name) => {
                if let Some(fty) = self.class_field_ty(name, field) {
                    fty.clone()
                } else if let Some(fty) = self.field_type_for(name, field) {
                    fty
                } else {
                    self.error(
                        ErrorCode::UnknownField,
                        format!("Cannot find field `{}` on `{}`", field, name),
                        range.clone(),
                    )
                }
            }
            // Unpinned Option payload (e.g. `Option::None` alone): unify
            // with a structural record that has this field so `none?.v`
            // typechecks and pins `T` for coalesce / later use.
            Ty::Var(_) => {
                let field_ty = Ty::Var(self.counter.fresh());
                let record = Ty::Record {
                    fields: vec![(field.to_string(), field_ty.clone())],
                };
                self.unify(
                    &resolved,
                    &record,
                    range,
                    "optional access field on unpinned Option payload",
                );
                field_ty
            }
            _ => self.error(
                ErrorCode::UnknownField,
                format!("Cannot access field `{}` on non-record type", field),
                range.clone(),
            ),
        }
    }

    /// Variable type from codegen side-table.
    pub fn codegen_var_type(&self, name: &str) -> Option<&Ty> {
        self.codegen_var_types.get(name)
    }

    /// Number of global static slots allocated during typechecking.
    pub fn static_slot_count(&self) -> u32 {
        self.next_static_slot
    }

    /// Static slot index for a fully-qualified name (`Class::field`, `mod::x`).
    pub fn static_slot_index(&self, fqn: &str) -> Option<u32> {
        self.static_slots.get(fqn).map(|(id, _)| *id)
    }

    /// Whether a static slot is declared `static const` / class static const.
    pub fn is_static_const_fqn(&self, fqn: &str) -> bool {
        self.static_slots.get(fqn).map(|(_, c)| *c).unwrap_or(false)
    }

    /// Static slot for a name in the current module namespace.
    pub fn static_slot_for_module_name(&self, name: &str) -> Option<u32> {
        self.static_slot_index(&self.qualify_module_name(name))
    }

    /// Whether `class.field` is declared `const`.
    pub fn is_const_class_field(&self, class: &str, field: &str) -> bool {
        self.const_class_fields
            .get(class)
            .is_some_and(|fields| fields.contains(field))
    }

    fn resolve_enum_key(&self, name: &str) -> Option<String> {
        if self.enum_tags.contains_key(name) {
            return Some(name.to_string());
        }
        let qualified = self.qualify_module_name(name);
        if self.enum_tags.contains_key(&qualified) {
            return Some(qualified);
        }
        if let Some(scheme) = self.env.lookup(name) {
            let ty = apply_ty_prune(&self.subst, &scheme.ty);
            if let Ty::Con(n) = &ty
                && self.enum_tags.contains_key(n)
            {
                return Some(n.clone());
            }
        }
        None
    }

    /// Codegen-only: resolve a Construct enum head when `current_module` may
    /// no longer match the defining module. Prefer normal resolution; if that
    /// misses, accept a unique `…::Name` registry key.
    fn resolve_enum_key_for_codegen(&self, name: &str) -> Option<String> {
        if let Some(key) = self.resolve_enum_key(name) {
            return Some(key);
        }
        if name.contains("::") {
            return None;
        }
        let suffix = format!("::{name}");
        let mut hits: Vec<String> = self
            .enum_tags
            .keys()
            .filter(|k| k.ends_with(&suffix))
            .cloned()
            .collect();
        if hits.len() == 1 {
            hits.pop()
        } else {
            None
        }
    }

    fn qualify_module_name(&self, name: &str) -> String {
        if self.current_module.is_empty() {
            name.to_string()
        } else {
            format!("{}::{}", self.current_module, name)
        }
    }

    /// Compile-time type id for `InitTyped` (`0` if the name is not a class).
    pub fn class_type_id(&self, name: &str) -> u32 {
        let key = self.resolve_class_key(name).unwrap_or_else(|| name.to_string());
        self.class_type_ids.get(&key).copied().unwrap_or(0)
    }

    pub fn class_has_drop(&self, name: &str) -> bool {
        let key = self.resolve_class_key(name).unwrap_or_else(|| name.to_string());
        self.classes_with_drop.contains(&key)
    }

    pub fn classes_with_drop(&self) -> impl Iterator<Item = &String> {
        self.classes_with_drop.iter()
    }

    /// Resolve a source identifier or `use` alias to the class table key.
    pub fn resolve_class_key(&self, name: &str) -> Option<String> {
        if self.classes.contains_key(name) {
            return Some(name.to_string());
        }
        let qualified = self.qualify_module_name(name);
        if qualified != name && self.classes.contains_key(&qualified) {
            return Some(qualified);
        }
        let scheme = self.env.lookup(name)?;
        let n = Self::class_name_of_ty(&scheme.ty)?;
        if self.classes.contains_key(n) {
            Some(n.to_string())
        } else {
            None
        }
    }

    fn class_member_fqn(&self, owner: &str, member: &str) -> String {
        let owner = self
            .resolve_class_key(owner)
            .unwrap_or_else(|| owner.to_string());
        format!("{}::{}", owner, member)
    }

    /// Allocate a synthetic static slot (e.g. `extern` library / fn-id handles).
    ///
    /// Reuses an existing index when `fqn` was already allocated. Unlike
    /// [`Self::register_static_slot`], duplicates are not a type error — FFI
    /// lowering may see the same library name across modules.
    pub fn alloc_synthetic_static_slot(&mut self, fqn: String, ty: Ty) -> u32 {
        if let Some(&(id, _)) = self.static_slots.get(&fqn) {
            return id;
        }
        let id = self.next_static_slot;
        self.next_static_slot += 1;
        self.static_slots.insert(fqn.clone(), (id, true));
        self.static_slot_types.insert(fqn, ty);
        id
    }

    fn register_static_slot(
        &mut self,
        fqn: String,
        is_const: bool,
        ty: Ty,
        range: Range<usize>,
    ) -> u32 {
        if self.static_slots.contains_key(&fqn) {
            let id = self.static_slots.get(&fqn).map(|(id, _)| *id).unwrap_or(0);
            let _ = self.error_with_help(
                ErrorCode::GenericTypeError,
                format!("Duplicate static slot `{}`", fqn),
                range,
                Some(
                    "each `static let` / `static const` / class `static` field must have a unique name"
                        .to_string(),
                ),
            );
            return id;
        }
        let id = self.next_static_slot;
        self.next_static_slot += 1;
        self.static_slots.insert(fqn.clone(), (id, is_const));
        self.static_slot_types.insert(fqn, ty);
        id
    }

    fn warn_shallow_const_binding(&mut self, name: &str, ty: &Ty, range: Range<usize>) {
        let pruned = apply_ty_prune(&self.subst, ty);
        if crate::typechecking::ty::is_shallow_const_mutable(&pruned) {
            let mut msg = Message::warn(
                ErrorCode::GenericTypeError,
                format!(
                    "binding `{}` is constant, but the underlying value of type `{}` is still mutable",
                    name, pruned
                ),
                range,
            );
            msg.with_help(
                "mutations through fields, indices, or `vec.push` will still succeed".to_string(),
            );
            self.messages.push(msg);
        }
    }

    fn check_readonly_external_mutation(&mut self, receiver_ty: &Ty, range: Range<usize>) {
        let pruned = apply_ty_prune(&self.subst, receiver_ty);
        if matches!(pruned, Ty::Readonly(_)) {
            let _ = self.error_with_help(
                ErrorCode::InvalidAssignment,
                "Cannot mutate a `readonly` value from outside its defining methods".to_string(),
                range,
                Some("rebind the variable with a new `readonly` value, or mutate via `self` inside an inherent method".to_string()),
            );
        }
    }

    fn infer_array_append_assign(
        &mut self,
        arr: &Output,
        value: &Output,
        range: Range<usize>,
    ) -> Ty {
        let _ = self.infer(arr);
        let _ = self.infer(value);
        self.error_with_help(
            ErrorCode::InvalidAssignment,
            "append assignment `arr[] = value` is no longer supported".to_string(),
            range,
            Some("use `vec.push(value)` on a `Vec<T>` instead".to_string()),
        )
    }

    /// For-in lowering info recorded during typecheck (by Loop node id).
    pub fn for_in_info_at(&self, id: NodeId) -> Option<&ForInInfo> {
        self.for_in_infos.get(&id)
    }

    /// For-in lowering info by source span (fallback when ids misalign).
    pub fn for_in_info_for_span(&self, start: usize, end: usize) -> Option<&ForInInfo> {
        self.for_in_infos_by_span.get(&(start, end))
    }

    /// Resolve `for x in` iterable type to `Item` and record [`ForInInfo`].
    ///
    /// Builtin synthesis covers arrays, homogeneous tuples/records, and
    /// coroutines. Otherwise looks up `IntoIterator` / `Iterator` instances.
    fn resolve_for_in_iterable(
        &mut self,
        te: &Ty,
        loop_id: Option<NodeId>,
        iterable_range: &Range<usize>,
        loop_range: &Range<usize>,
    ) -> Option<Ty> {
        // ---- Builtin synthesis ----
        if let Some((item, kind)) = self.builtin_for_in_kind(te, iterable_range) {
            self.record_for_in_info(
                loop_id,
                loop_range,
                ForInInfo {
                    kind,
                    item_ty: item.clone(),
                },
            );
            return Some(item);
        }

        // ---- User IntoIterator / Iterator ----
        match self.find_unique_instance("IntoIterator", &[te.clone()], iterable_range) {
            Ok(Some(into_inst)) => {
                let item = into_inst
                    .assoc_tys
                    .get("Item")
                    .map(|v| v.ty.clone())
                    .unwrap_or_else(|| Ty::Var(self.counter.fresh()));
                let into_iter_ty = into_inst
                    .assoc_tys
                    .get("IntoIter")
                    .map(|v| v.ty.clone())
                    .unwrap_or_else(|| Ty::Var(self.counter.fresh()));
                let into_fqn = into_inst.method_fqns.get("into_iter").cloned();
                match self.find_unique_instance("Iterator", &[into_iter_ty.clone()], iterable_range)
                {
                    Ok(Some(iter_inst)) => {
                        if let Some(iter_item) = iter_inst.assoc_tys.get("Item") {
                            self.unify(
                                &item,
                                &iter_item.ty,
                                iterable_range,
                                "IntoIterator/Iterator Item",
                            );
                        }
                        let next_fqn = iter_inst.method_fqns.get("next").cloned();
                        match (into_fqn, next_fqn) {
                            (Some(into_iter_fqn), Some(next_fqn)) => {
                                let item_ty = apply_ty_prune(&self.subst, &item);
                                self.record_for_in_info(
                                    loop_id,
                                    loop_range,
                                    ForInInfo {
                                        kind: ForInKind::Custom {
                                            into_iter_fqn,
                                            next_fqn,
                                        },
                                        item_ty: item_ty.clone(),
                                    },
                                );
                                Some(item_ty)
                            }
                            _ => {
                                let _ = self.error_with_help(
                                    ErrorCode::GenericTypeError,
                                    "IntoIterator/Iterator instance is missing method implementations"
                                        .to_string(),
                                    iterable_range.clone(),
                                    Some(
                                        "implement `into_iter` and `next` for the iterable type"
                                            .to_string(),
                                    ),
                                );
                                None
                            }
                        }
                    }
                    Ok(None) => {
                        let _ = self.error_with_help(
                            ErrorCode::GenericTypeError,
                            format!(
                                "type `{}` is IntoIterator but its IntoIter is not Iterator",
                                te
                            ),
                            iterable_range.clone(),
                            Some(format!(
                                "add `impl Iterator<{}>` with matching `type Item`",
                                into_iter_ty
                            )),
                        );
                        None
                    }
                    Err(()) => None,
                }
            }
            Ok(None) => {
                let _ = self.error_with_help(
                    ErrorCode::GenericTypeError,
                    format!("type `{}` is not iterable", te),
                    iterable_range.clone(),
                    Some(
                        "implement `IntoIterator` / `Iterator`, or use an array, homogeneous tuple/dict, range, or coroutine"
                            .to_string(),
                    ),
                );
                None
            }
            Err(()) => None,
        }
    }

    fn record_for_in_info(
        &mut self,
        loop_id: Option<NodeId>,
        loop_range: &Range<usize>,
        info: ForInInfo,
    ) {
        if let Some(id) = loop_id {
            self.for_in_infos.insert(id, info.clone());
        }
        self.for_in_infos_by_span
            .insert((loop_range.start, loop_range.end), info);
    }

    /// Builtin iterable shapes → `(Item, ForInKind)`. Returns `None` when
    /// the type is not a recognised builtin iterable (caller falls through
    /// to trait instance lookup). Emits diagnostics for hetero tuple/dict.
    fn builtin_for_in_kind(&mut self, te: &Ty, range: &Range<usize>) -> Option<(Ty, ForInKind)> {
        match te {
            Ty::Array { element, .. } => Some((element.as_ref().clone(), ForInKind::Array)),
            other if vec_element_ty(other).is_some() => {
                Some((vec_element_ty(other)?.clone(), ForInKind::Array))
            }
            Ty::Tuple(elems) => {
                if elems.is_empty() {
                    let _ = self.error_with_help(
                        ErrorCode::GenericTypeError,
                        "empty tuple is not iterable".to_string(),
                        range.clone(),
                        Some("tuple for-in requires at least one element".to_string()),
                    );
                    return None;
                }
                match self.homogeneous_types(elems, range, "tuple") {
                    Some(item) => Some((item, ForInKind::Tuple { arity: elems.len() })),
                    None => None,
                }
            }
            Ty::Record { fields } => {
                let value_tys: Vec<Ty> = fields.iter().map(|(_, ty)| ty.clone()).collect();
                if value_tys.is_empty() {
                    // Vacuously homogeneous; Item = (string, α).
                    let v = Ty::Var(self.counter.fresh());
                    return Some((tuple_ty(vec![string(), v]), ForInKind::Dict));
                }
                match self.homogeneous_types(&value_tys, range, "dict") {
                    Some(v) => Some((tuple_ty(vec![string(), v]), ForInKind::Dict)),
                    None => None,
                }
            }
            Ty::App(head, args) => {
                if matches!(head.as_ref(), Ty::Con(n) if n == "coroutine") && args.len() == 2 {
                    return Some((args[0].clone(), ForInKind::Coroutine));
                }
                if matches!(head.as_ref(), Ty::Con(n) if n == RANGE) && args.len() == 1 {
                    return self.range_for_in_kind(&args[0], false, range);
                }
                if matches!(head.as_ref(), Ty::Con(n) if n == RANGE_INCLUSIVE) && args.len() == 1 {
                    return self.range_for_in_kind(&args[0], true, range);
                }
                None
            }
            _ => None,
        }
    }

    /// `for` over `Range<T>` / `RangeInclusive<T>` — iteration needs a
    /// stepped numeric element (`int` / `byte` / `float`). Construction
    /// only requires `Ord`; non-steppable Ord types get a diagnostic.
    fn range_for_in_kind(
        &mut self,
        elem: &Ty,
        inclusive: bool,
        range: &Range<usize>,
    ) -> Option<(Ty, ForInKind)> {
        let type_name = if inclusive {
            RANGE_INCLUSIVE
        } else {
            RANGE
        };
        let float = self.require_range_numeric_step(elem, type_name, range);
        let elem = apply_ty_prune(&self.subst, elem);
        Some((elem, ForInKind::Range { inclusive, float }))
    }

    /// Shared numeric-step gate for `for` and `.to_vec()`.
    ///
    /// Returns `true` when the element is `float` (ADDF path), `false` for
    /// `int`/`byte` or after diagnosing a non-steppable type (int-style
    /// recovery so codegen still emits).
    fn require_range_numeric_step(
        &mut self,
        elem: &Ty,
        type_name: &str,
        range: &Range<usize>,
    ) -> bool {
        let elem = apply_ty_prune(&self.subst, elem);
        match &elem {
            Ty::Con(n) if n == "float" => true,
            Ty::Con(n) if n == "int" || n == "byte" => false,
            other => {
                let _ = self.error_with_help(
                    ErrorCode::GenericTypeError,
                    format!("cannot iterate over `{type_name}<{other}>`"),
                    range.clone(),
                    Some(
                        "`for` and `.to_vec()` require element type `int`, `byte`, or `float` \
                         (construction only needs `Ord`; there is no successor protocol)"
                            .to_string(),
                    ),
                );
                false
            }
        }
    }

    /// Reject `.to_vec()` on a non-numeric `Range` / `RangeInclusive`.
    fn constrain_range_to_vec(&mut self, owner: &str, recv_ty: &Ty, range: &Range<usize>) {
        if owner != RANGE && owner != RANGE_INCLUSIVE {
            return;
        }
        let Some((elem, _)) = range_app(recv_ty) else {
            return;
        };
        let _ = self.require_range_numeric_step(elem, owner, range);
    }

    /// All types unify to one element type, or diagnose heterogeneity.
    fn homogeneous_types(&mut self, tys: &[Ty], range: &Range<usize>, kind: &str) -> Option<Ty> {
        let first = apply_ty_prune(&self.subst, &tys[0]);
        for other in tys.iter().skip(1) {
            let other = apply_ty_prune(&self.subst, other);
            if unify_with(&self.subst, &first, &other).is_err() {
                let _ = self.error_with_help(
                    ErrorCode::GenericTypeError,
                    format!(
                        "heterogeneous {}: element types `{}` and `{}` do not match",
                        kind, first, other
                    ),
                    range.clone(),
                    Some(format!("{} elements must all share one type", kind)),
                );
                return None;
            }
        }
        // Bind any open vars across the set.
        let mut local = self.subst.clone();
        for other in tys.iter().skip(1) {
            if let Ok(s) = unify_with(&local, &first, other) {
                local = s;
            }
        }
        self.subst = compose(&local, &self.subst);
        Some(apply_ty_prune(&self.subst, &first))
    }

    /// True if `name` is a registered class (source ident, alias, or FQN).
    pub fn is_class(&self, name: &str) -> bool {
        self.resolve_class_key(name).is_some()
    }

    /// Class name from `Con(C)` or `App(Con(C), _)` (Phase 7).
    pub fn class_name_of_ty(ty: &Ty) -> Option<&str> {
        match strip_readonly(ty) {
            Ty::Con(n) => Some(n.as_str()),
            Ty::App(head, _) => match head.as_ref() {
                Ty::Con(n) => Some(n.as_str()),
                _ => None,
            },
            _ => None,
        }
    }

    fn class_owner_from_ty(&self, ty: &Ty) -> Option<String> {
        Self::class_name_of_ty(ty)
            .filter(|n| self.classes.contains_key(*n))
            .map(|n| n.to_string())
    }

    /// True if `ty` is a registered class instance (`Con` or `App`).
    pub fn ty_is_class(&self, ty: &Ty) -> bool {
        Self::class_name_of_ty(ty).is_some_and(|n| self.is_class(n))
    }

    /// Declared type of a class field (codegen / Access).
    pub fn class_field_ty(&self, class: &str, field: &str) -> Option<&Ty> {
        self.classes
            .get(class)?
            .iter()
            .find(|(_, fname, _)| fname == field)
            .map(|(_, _, ty)| ty)
    }

    /// Class fields in declaration order: `(name, Ty)`.
    pub fn class_fields(&self, class: &str) -> Option<Vec<(String, Ty)>> {
        self.classes.get(class).map(|fields| {
            fields
                .iter()
                .map(|(_, name, ty)| (name.clone(), ty.clone()))
                .collect()
        })
    }

    /// Method FQN lookup helper — returns whether the method exists.
    pub fn has_method(&self, owner: &str, method: &str) -> bool {
        let owner = self
            .resolve_class_key(owner)
            .unwrap_or_else(|| owner.to_string());
        self.methods
            .get(&owner)
            .is_some_and(|m| m.contains_key(method))
    }

    /// True when `owner::method` was declared as `static fn`.
    pub fn is_static_method(&self, owner: &str, method: &str) -> bool {
        let owner = self
            .resolve_class_key(owner)
            .unwrap_or_else(|| owner.to_string());
        self.static_methods
            .get(&owner)
            .is_some_and(|m| m.contains(method))
    }

    /// Type `Class::static_method(args)` (parsed as Construct).
    fn try_infer_static_method_call(
        &mut self,
        owner: &str,
        method: &str,
        fields: &parser::ast::EnumConstructPayload<'_>,
        range: Range<usize>,
        call_id: Option<NodeId>,
    ) -> Option<Ty> {
        use parser::ast::EnumConstructPayload;
        if !self.is_static_method(owner, method) {
            return None;
        }
        let owner = self
            .resolve_class_key(owner)
            .unwrap_or_else(|| owner.to_string());
        let fqn = format!("{}::{}", owner, method);
        let scheme = self
            .methods
            .get(&owner)
            .and_then(|m| m.get(method))
            .map(|(_, s)| s.clone())?;
        let fun_ty = self.instantiate_ty(&scheme);

        // Named-arg / rest reorder when the call uses a tuple payload.
        let arg_tys = match fields {
            EnumConstructPayload::Unit => Vec::new(),
            EnumConstructPayload::Tuple(args) => {
                if self.fn_has_rest(&fqn) || self.fn_param_names.contains_key(&fqn) {
                    let (tys, _) = self.infer_and_reorder_call_args(&fqn, args, &range);
                    tys
                } else {
                    args.iter().map(|a| self.infer(a)).collect()
                }
            }
            EnumConstructPayload::Record(parts) => {
                // Static methods don't take record payloads as ctors —
                // still infer children for ID alignment, then error.
                for p in parts {
                    let _ = self.infer(&p.value);
                }
                return Some(self.error_with_help(
                    ErrorCode::GenericTypeError,
                    format!(
                        "static method `{}` is called with parentheses, not a record literal",
                        fqn
                    ),
                    range,
                    Some(format!(
                        "write `{}(...)` with positional or named arguments",
                        fqn
                    )),
                ));
            }
        };

        Some(self.apply_function(Some(&fqn), &fun_ty, &arg_tys, None, call_id, range))
    }

    /// True if `name` was declared as `async fn`.
    pub fn is_async_function(&self, name: &str) -> bool {
        self.async_functions.contains(name)
    }

    /// Whether `name` is a generic function (has type params).
    pub fn is_generic_fn(&self, name: &str) -> bool {
        // Exact keys only: defining modules register bare + FQN; importers get
        // the local alias via [`Self::reexport_module_item`]. A `::{name}`
        // suffix scan would mis-tag when another module exports the same local.
        self.generics.generic_fns.contains(name)
    }

    /// Bind `local` to the defining module's scheme for `fqn` when known;
    /// otherwise insert a fresh monomorphic placeholder (historical disk-module
    /// ABI). Marks generic aliases so codegen emits dictionaries.
    fn reexport_module_item(&mut self, fqn: &str, local: &str) {
        if let Some(scheme) = self.env.lookup(fqn).cloned() {
            self.env.insert_top(local.to_string(), scheme);
        } else if self.env.lookup(local).is_none() {
            self.env
                .insert_top(local.to_string(), Scheme::mono(Ty::Var(self.counter.fresh())));
        }
        // Only the exact defining FQN — never a `::{local}` suffix heuristic
        // (another module's generic with the same short name would mis-tag).
        if self.generics.generic_fns.contains(fqn) {
            self.generics.generic_fns.insert(local.to_string());
            self.generic_fns.insert(local.to_string());
            if let Some(arity) = self.fn_dict_arity.get(fqn).copied() {
                self.fn_dict_arity.insert(local.to_string(), arity);
            }
        } else {
            self.generics.generic_fns.remove(local);
            self.generic_fns.remove(local);
            self.fn_dict_arity.remove(local);
        }
    }

    /// Whether `fqn` (`Owner::method`) is an inherent method, and its visibility.
    pub fn inherent_method_visibility(&self, fqn: &str) -> Option<Visibility> {
        let (owner, method) = fqn.rsplit_once("::")?;
        self.methods.get(owner)?.get(method).map(|(vis, _)| *vis)
    }

    /// Number of *user-defined* trait dict slots expected by a generic
    /// function.  Returns 0 for non-generic functions or functions whose
    /// constraints are all built-in classes (Num / Ord / Eq / Show).
    pub fn dict_arity_for(&self, fn_name: &str) -> usize {
        // Exact key only (see [`Self::is_generic_fn`]); re-export copies arity
        // under the local alias when the defining FQN is generic.
        self.fn_dict_arity.get(fn_name).copied().unwrap_or(0)
    }

    /// True for compiler-built-in typeclasses (Num / Ord / Eq / Show).
    ///
    /// These still use the dictionary ABI in shared generic bodies. Ground
    /// Num/Ord/Eq calls may monomorphize to direct opcodes; `Show` does not
    /// (see `monomorphize::candidate_for_call`).
    pub fn is_builtin_class(class: &str) -> bool {
        matches!(
            class,
            "Add"
                | "Sub"
                | "Mul"
                | "Div"
                | "Num"
                | "Lt"
                | "Le"
                | "Gt"
                | "Ge"
                | "Ord"
                | "Eq"
                | "Show"
                | "Length"
        )
    }

    /// Return the FQN for an instance method, if registered.
    /// `class` is e.g. `"Num"`, `args` are concrete types, `method` is `"add"`.
    pub fn instance_method_fqn(&self, class: &str, args: &[Ty], method: &str) -> Option<&str> {
        self.generics
            .find_instance(class, args)
            .and_then(|inst| inst.method_fqns.get(method).map(|s| s.as_str()))
    }

    /// Read-only access to the generics registry (for codegen).
    pub fn generics(&self) -> &crate::typechecking::generics::Generics {
        &self.generics
    }

    /// Infer without updating the NodeId cache (codegen helper).
    pub fn infer_for_codegen(&mut self, expr: &Output) -> Ty {
        let saved_idx = self.next_id_idx;
        let ty = self.infer_inner(expr, None);
        self.next_id_idx = saved_idx;
        // Don't insert into cache — the ID we restored might be
        // wrong for this AST node, and overwriting a correct entry
        // would be worse than skipping this insertion.
        ty
    }

    /// Field access on enum record payloads (`specific_tag` narrows the variant).
    fn access_field_in_sum(
        &mut self,
        enum_name: &str,
        variants: &[(String, EnumVariantPayloadTy)],
        specific_tag: Option<u32>,
        field: &str,
        range: Range<usize>,
    ) -> Ty {
        if let Some(tag) = specific_tag {
            // Statically known variant. Look up the payload and
            // either return the field's type or emit a tailored
            // diagnostic.
            let variant_idx = tag as usize;
            if variant_idx >= variants.len() {
                return self.error_with_help(
                    ErrorCode::GenericTypeError,
                    format!("Cannot access field `{}` on non-record type", field),
                    range,
                    Some("only values of record-shaped enum types expose fields".to_string()),
                );
            }
            let (variant_name, payload) = &variants[variant_idx];
            match payload {
                EnumVariantPayloadTy::Record(fields) => {
                    for (fname, fty) in fields {
                        if fname == field {
                            return fty.clone();
                        }
                    }
                    // Record-shaped variant, but doesn't declare
                    // the field.
                    let hint = build_record_field_hint(enum_name, variants);
                    self.error_with_help(
                        ErrorCode::GenericTypeError,
                        format!("Type `{}` has no field `{}`", enum_name, field),
                        range,
                        hint,
                    )
                }
                EnumVariantPayloadTy::Tuple(parts) => {
                    if let Ok(idx) = field.parse::<usize>() {
                        if let Some(fty) = parts.get(idx) {
                            return fty.clone();
                        }
                    }
                    self.error_with_help(
                        ErrorCode::GenericTypeError,
                        format!("Cannot access field `{}` on tuple variant", field),
                        range,
                        Some(format!(
                            "variant `{}::{}` is a {}-tuple; use a match binding or index 0..{}",
                            enum_name,
                            variant_name,
                            parts.len(),
                            parts.len().saturating_sub(1),
                        )),
                    )
                }
                _ => self.error_with_help(
                    ErrorCode::GenericTypeError,
                    format!("Cannot access field `{}` on non-record variant", field),
                    range,
                    Some(format!(
                        "variant `{}::{}` is {}; only record-shaped variants expose named fields",
                        enum_name,
                        variant_name,
                        payload_kind_name(payload),
                    )),
                ),
            }
        } else {
            // Untagged receiver: find every record-/tuple-shaped variant
            // that declares the field (named record fields, or synthetic
            // `"0"`/`"1"`/… tuple indices).
            let mut candidates: Vec<&Ty> = Vec::new();
            for (_variant_name, payload) in variants {
                if let EnumVariantPayloadTy::Record(fields) = payload {
                    for (fname, fty) in fields {
                        if fname == field {
                            candidates.push(fty);
                        }
                    }
                } else if let EnumVariantPayloadTy::Tuple(parts) = payload {
                    if let Ok(idx) = field.parse::<usize>() {
                        if let Some(fty) = parts.get(idx) {
                            candidates.push(fty);
                        }
                    }
                }
            }
            match candidates.len() {
                0 => {
                    let hint = build_record_field_hint(enum_name, variants);
                    self.error_with_help(
                        ErrorCode::GenericTypeError,
                        format!("Type `{}` has no field `{}`", enum_name, field),
                        range,
                        hint,
                    )
                }
                1 => candidates[0].clone(),
                _ => {
                    self.error_with_help(
                        ErrorCode::GenericTypeError,
                        format!(
                            "Field `{}` exists in multiple variants of `{}`; \
                             narrow with match first",
                            field, enum_name
                        ),
                        range,
                        Some(
                            "field access requires a unique field type; use a `match` to \
                             determine the active variant before reading the field"
                                .to_string(),
                        ),
                    );
                    candidates[0].clone()
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "infer.tests.rs"]
mod tests;
