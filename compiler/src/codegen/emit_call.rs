//! Call lowering extracted from `do_compile` (stack-margin style).

use super::*;

impl Compiler {
    pub(super) fn compile_call_expr<'compiler>(
        &mut self,
        name: &Output<'compiler>,
        args: &Option<Vec<Output<'compiler>>>,
        ast: &(SimpleSpan, Box<Expression<'compiler>>),
        self_id: Option<crate::typechecking::id::NodeId>,
        span: &SimpleSpan,
    ) -> CodeBuf {
        let mut bytecode = CodeBuf::new();
        // Fold `len(literal)` before trait dispatch so string/tuple
        // lengths become CONST instead of Length thunk + ArrayLen.
        if let Expression::Identifier(raw) = name.1.as_ref()
            && *raw == "len"
            && let Some(ConstValue::Int(n)) =
                crate::const_fold::eval_expr(ast, self.const_env())
        {
            if let Some(items) = args.as_ref() {
                for arg in items {
                    self.discard_compile(arg);
                }
            }
            self.emit_const_value(&ConstValue::Int(n), &mut bytecode);
            return bytecode;
        }

        if let Expression::Identifier(_) = name.1.as_ref()
            && let Some((en, vn)) = self.checker.bare_construct_at(span.start, span.end)
        {
            let en = en.clone();
            let vn = vn.clone();
            let fields = match args {
                None => parser::ast::EnumConstructPayload::Unit,
                Some(a) if a.is_empty() => parser::ast::EnumConstructPayload::Unit,
                Some(a) => parser::ast::EnumConstructPayload::Tuple(a.clone()),
            };
            return self.compile_construct_expr(&en, &vn, &fields, ast);
        }

        if let Expression::Identifier(fname) = name.1.as_ref() {
            if let Some(kind) = self.string_builtin_for_call(fname) {
                let arg_slice = args.as_deref().unwrap_or(&[]);
                match kind {
                    crate::typechecking::StringBuiltin::Format => {
                        if let Some((format, rest)) = arg_slice.split_first() {
                            let params = rest.to_vec();
                            self.emit_format_expression(format, Some(&params));
                        }
                    }
                    crate::typechecking::StringBuiltin::FromBytes
                    | crate::typechecking::StringBuiltin::ToBytes => {
                        if let Some(native_name) = kind.native_name() {
                            self.emit_host_native_invoke(native_name, arg_slice);
                            self.emit_host_option_boundary(ast);
                        }
                    }
                }
                return bytecode;
            }
        } else if let Expression::QualifiedAccess { owner, member } = name.1.as_ref() {
            let fqn = format!("{}::{}", owner, member);
            if let Some(kind) = self.string_builtin_for_call(&fqn) {
                let arg_slice = args.as_deref().unwrap_or(&[]);
                match kind {
                    crate::typechecking::StringBuiltin::Format => {
                        if let Some((format, rest)) = arg_slice.split_first() {
                            let params = rest.to_vec();
                            self.emit_format_expression(format, Some(&params));
                        }
                    }
                    crate::typechecking::StringBuiltin::FromBytes
                    | crate::typechecking::StringBuiltin::ToBytes => {
                        if let Some(native_name) = kind.native_name() {
                            self.emit_host_native_invoke(native_name, arg_slice);
                            self.emit_host_option_boundary(ast);
                        }
                    }
                }
                return bytecode;
            }
        }
        // `assert` from `prelude::test` (auto-imported).
        if let Expression::Identifier(fname) = name.1.as_ref()
            && let Some(kind) = self.checker.prelude_fn_in_scope(fname)
        {
            let arg_slice = args.as_deref().unwrap_or(&[]);
            match kind {
                crate::typechecking::PreludeFn::Assert => {
                    self.emit_assert(arg_slice);
                }
                crate::typechecking::PreludeFn::BlockOn => {
                    self.emit_block_on(arg_slice);
                }
                crate::typechecking::PreludeFn::Matrix => {
                    // Zero-cost wrap: runtime is the nested data.
                    if let Some(arg) = arg_slice.first() {
                        bytecode.append(&mut self.do_compile(arg));
                    }
                }
                crate::typechecking::PreludeFn::Dot
                | crate::typechecking::PreludeFn::MatMul
                | crate::typechecking::PreludeFn::Cross => {
                    self.emit_linear_algebra(
                        &mut bytecode,
                        self_id,
                        span.start,
                        span.end,
                        arg_slice,
                    );
                }
                crate::typechecking::PreludeFn::Ord
                | crate::typechecking::PreludeFn::Char => {
                    self.emit_prelude_host_call(arg_slice, kind.as_str());
                }
                crate::typechecking::PreludeFn::Sin
                | crate::typechecking::PreludeFn::Cos
                | crate::typechecking::PreludeFn::Tan
                | crate::typechecking::PreludeFn::Sqrt
                | crate::typechecking::PreludeFn::Floor
                | crate::typechecking::PreludeFn::Ceil
                | crate::typechecking::PreludeFn::Exp
                | crate::typechecking::PreludeFn::Ln
                | crate::typechecking::PreludeFn::Pow => {
                    self.emit_prelude_host_call(
                        arg_slice,
                        kind.math_native_name().expect("scalar math native"),
                    );
                }
            }
            return bytecode;
        }
        // `dload` / `declare` / `invoke` after `use ffi::{…}`.
        if let Expression::Identifier(fname) = name.1.as_ref()
            && let Some(kind) = self.checker.ffi_fn_in_scope(fname)
        {
            let arg_slice = args.as_deref().unwrap_or(&[]);
            match kind {
                crate::typechecking::FfiBuiltin::Dload => {
                    if let Some(path) = arg_slice.first() {
                        let mut bc = self.do_compile(path);
                        self.bytecode.append(&mut bc);
                        self.bytecode.push(Byte::new(Instruction::FfiLoad));
                    }
                }
                crate::typechecking::FfiBuiltin::Declare => {
                    self.emit_ffi_declare(*span, arg_slice);
                }
                crate::typechecking::FfiBuiltin::Invoke => {
                    self.emit_ffi_invoke(*span, arg_slice);
                }
            }
            return bytecode;
        }
        // `open` / `read` / … after `use io::{…}` (or `use io::read as …`).
        if let Expression::Identifier(fname) = name.1.as_ref()
            && let Some(kind) = self.checker.io_fn_in_scope(fname)
        {
            self.emit_io_host_invoke(kind, args.as_deref().unwrap_or(&[]));
            self.emit_host_option_boundary(ast);
            return bytecode;
        }
        if let Expression::Identifier(fname) = name.1.as_ref()
            && let Some(kind) = self.checker.thread_fn_in_scope(fname)
        {
            self.emit_thread_host_invoke(kind, args.as_deref().unwrap_or(&[]));
            self.emit_host_option_boundary(ast);
            return bytecode;
        }
        if let Expression::Identifier(fname) = name.1.as_ref()
            && let Some(kind) = self.checker.gc_fn_in_scope(fname)
        {
            self.emit_host_native_invoke(kind.native_name(), args.as_deref().unwrap_or(&[]));
            self.emit_host_option_boundary(ast);
            return bytecode;
        }
        if let Expression::Identifier(fname) = name.1.as_ref()
            && let Some(registry) = self.checker.host_fn_in_scope(fname)
        {
            self.emit_host_native_invoke(registry, args.as_deref().unwrap_or(&[]));
            self.emit_host_option_boundary(ast);
            return bytecode;
        }

        if let Some(hint) = self.existential_method_hint(self_id, span.start, span.end)
        {
            if self.emit_existential_method_call(&mut bytecode, name, args.as_ref(), &hint)
            {
                return bytecode;
            }
        }

        if let Some(hint) = self.bound_method_hint(self_id, span.start, span.end)
        {
            let dict_name = format!("__dict{}", hint.dict_index);
            if let Some(dict_slot) = self.lookup_slot(&dict_name) {
                if hint.has_receiver
                    && let Expression::Access(recv, _) = name.1.as_ref()
                {
                    bytecode.append(&mut self.do_compile(recv));
                }
                if let Some(items) = args {
                    for arg in items {
                        self.append_with_existential_pack(&mut bytecode, arg);
                    }
                }
                // Hidden trailing dictionary argument for sibling/default
                // dispatch inside the selected implementation.
                bytecode.push_load(dict_slot);
                bytecode.push_load(dict_slot);
                bytecode.push_const(hint.method_slot as i32);
                bytecode.push_index();
                bytecode.push(
                    Byte::new(Instruction::CallIndirect)
                        .with_operand_u32(hint.arity as u32 + 1),
                );
                return bytecode;
            }
            if self.compiling_mono_clone
                && self.try_emit_ground_bound_method(
                    &mut bytecode,
                    name,
                    args.as_ref(),
                    &hint,
                )
            {
                return bytecode;
            }
            if !self.compiling_mono_clone {
                let mut message = Message::error(
                    ErrorCode::UnknownFunction,
                    "Missing trait dictionary".to_string(),
                    span.into_range(),
                );
                message.push(DiagLabel::new(
                    format!("dictionary slot `{}` is not available", dict_name),
                    span.into_range(),
                ));
                self.messages.push(message);
                return bytecode;
            }
        }

        // Method call: `recv.method(args)`.
        if let Expression::Access(recv, method) = name.1.borrow() {
            // Ground trait method (`recv.into()`, …): typechecker
            // discharged a concrete instance into `call_dicts_at`.
            // Emit receiver + args + dictionary, then direct CALL to
            // the instance method (dict ABI, trailing dict arg).
            let ground_trait = self
                .sidecar_dicts(self_id, span.start, span.end)
                .and_then(|dicts| dicts.first())
                .and_then(|instance| {
                    let fqn = instance.method_fqns.get(*method)?.clone();
                    if self.functions.contains_key(&fqn)
                        || self.fn_entry_labels.contains_key(&fqn)
                    {
                        Some((instance.class.clone(), instance.args.clone(), fqn))
                    } else {
                        None
                    }
                });
            if let Some((class, inst_args, fqn)) = ground_trait {
                bytecode.append(&mut self.do_compile(recv));
                // Box the receiver when the instance method prologue
                // expects an unbox (same contract as Eq/Ord direct calls).
                // Prefer `receiver_type` for identifiers/access; fall
                // back to `codegen_expr_ty` so inline receivers like
                // `new Celsius(0).into()` still get boxed.
                // Peel Constructor/Sum → Con so `ty_to_value_tag` matches
                // instance-head unbox tags (Con(enum) → Instance). Raw
                // Constructor types returned None and skipped boxing.
                if let Some(recv_ty) = self
                    .receiver_type(recv)
                    .or_else(|| self.codegen_expr_ty(recv))
                {
                    let box_ty = Self::show_lookup_ty_for_instance(&recv_ty);
                    Self::emit_box_if_needed(&mut bytecode, &box_ty);
                }
                let mut nargs = 1u32; // receiver
                if let Some(items) = args {
                    for arg in items {
                        self.append_with_existential_pack(&mut bytecode, arg);
                        nargs += 1;
                    }
                }
                if self.emit_instance_dict(&mut bytecode, &class, &inst_args) {
                    nargs += 1; // trailing dictionary
                }
                if !self.emit_direct_fn_call(&mut bytecode, &fqn, nargs) {
                    self.missing_call_target(&fqn, span.into_range());
                }
                return bytecode;
            }

            // Same fallback as ground-trait calls: inline receivers
            // like `(new Point(1, 2)).sum()` are not identifiers, so
            // `receiver_type` alone used to leave `owner` empty.
            let recv_ty = self
                .receiver_type(recv)
                .or_else(|| self.codegen_expr_ty(recv));
            let owner = recv_ty
                .as_ref()
                .and_then(|ty| {
                    Checker::class_name_of_ty(ty)
                        .filter(|n| self.checker.is_class(n))
                        .map(|n| n.to_string())
                })
                .unwrap_or_default();
            let fqn = self
                .context
                .methods
                .get(&owner)
                .and_then(|m| m.get(*method))
                .cloned();
            if let Some(fqn_base) = fqn {
                let nargs = args.as_ref().map(|items| items.len()).unwrap_or(0);
                let fqn = if let Some((fa, is_rest, id)) =
                    self.sidecar_overload(self_id, span.start, span.end)
                {
                    let keyed = overload_fn_key(&fqn_base, fa, is_rest, id);
                    if self.functions.contains_key(&keyed) {
                        keyed
                    } else {
                        fqn_base.clone()
                    }
                } else if self.checker.is_overloaded(&fqn_base) {
                    // Forward call inside an impl that later gained
                    // more overloads — TC may not have recorded a
                    // selection (set had size 1 at infer time).
                    // Prefer arg types so same-arity overloads match
                    // the checker path (`select_overload_for_args`).
                    let arg_tys: Vec<Ty> = args
                        .as_ref()
                        .map(|items| {
                            items
                                .iter()
                                .filter_map(|a| {
                                    let value = match a.1.as_ref() {
                                        Expression::NamedArg(_, v) => v,
                                        _ => a,
                                    };
                                    self.codegen_expr_ty(value)
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    // Only pass types when every arg resolved; a
                    // partial vec would shift positions under unify.
                    let tys = if arg_tys.len() == nargs {
                        arg_tys.as_slice()
                    } else {
                        &[]
                    };
                    match self.checker.select_overload_for_args(&fqn_base, nargs, tys) {
                        crate::typechecking::infer::OverloadSelect::Selected(c) => {
                            let keyed = overload_fn_key(
                                &fqn_base,
                                c.fixed_arity,
                                c.is_rest,
                                c.id,
                            );
                            if self.functions.contains_key(&keyed) {
                                keyed
                            } else {
                                fqn_base
                            }
                        }
                        _ => fqn_base,
                    }
                } else {
                    fqn_base
                };
                let fqn = if self.range_to_vec_elem_is_float(recv_ty.as_ref())
                    && (fqn == "Range::to_vec" || fqn == "RangeInclusive::to_vec")
                {
                    fqn.replace("::to_vec", "::__float_to_vec")
                } else {
                    fqn
                };
                if self.functions.contains_key(&fqn) || self.fn_entry_labels.contains_key(&fqn) {
                    let call_name = fqn.clone();
                    // Inline `Vec::push` as ArrayPush — avoids CALL/frame for fill loops.
                    // Stage when the value may STORE/Seek (format, match, `new
                    // Class`, …): locals and the operand stack share memory, so
                    // leaving the vec under a clobbering emit drops the push.
                    // Format/match/host write into `self.bytecode` (not the
                    // local buffer), so those paths stage on `self.bytecode`
                    // before returning.
                    if fqn == format!("{}::push", common::BUILTIN_VEC_TYPE)
                        && args.as_ref().map(|a| a.len()) == Some(1)
                    {
                        let arg = &args.as_ref().unwrap()[0];
                        let value = match arg.1.as_ref() {
                            Expression::NamedArg(_, v) => v,
                            _ => arg,
                        };
                        if self.arg_emits_on_self_bytecode(value) {
                            let mut recv_bc = self.do_compile(recv);
                            self.bytecode.append(&mut recv_bc);
                            let recv_tmp = self.alloc_temp_slot();
                            self.bytecode.push_store_pop(recv_tmp);
                            let _ = self.do_compile(value);
                            let val_tmp = self.alloc_temp_slot();
                            self.bytecode.push_store_pop(val_tmp);
                            self.bytecode.push_load(recv_tmp);
                            self.bytecode.push_load(val_tmp);
                            self.bytecode.push(Byte::new(Instruction::ArrayPush));
                            self.bytecode.push_pop();
                            self.bytecode.push_const(0);
                            return bytecode;
                        }
                        bytecode.append(&mut self.do_compile(recv));
                        if self.expr_may_clobber_operand_stack(value) {
                            let recv_tmp = self.alloc_temp_slot();
                            bytecode.push_store_pop(recv_tmp);
                            bytecode.append(&mut self.do_compile(value));
                            let val_tmp = self.alloc_temp_slot();
                            bytecode.push_store_pop(val_tmp);
                            bytecode.push_load(recv_tmp);
                            bytecode.push_load(val_tmp);
                        } else {
                            bytecode.append(&mut self.do_compile(value));
                        }
                        bytecode.push(Byte::new(Instruction::ArrayPush));
                        bytecode.push_pop();
                        bytecode.push_const(0);
                        return bytecode;
                    }
                    // Same ABI as free generics: box top-level type
                    // params, append trait dictionaries, unbox returns.
                    let lookup_name = strip_overload_key(&fqn).to_string();
                    let is_generic = self.checker.is_generic_fn(&lookup_name);
                    let box_generic_args = is_generic
                        && self.generic_has_toplevel_type_param_args(&lookup_name);
                    // Stage the receiver into a temp *before* user args.
                    // Leaving it on the operand stack while arg staging
                    // `STORE`s into temps clobbers it (locals and the
                    // operand stack share memory) — nested calls like
                    // `self.inner.put(x, true)` then mutate the wrong object.
                    bytecode.append(&mut self.do_compile(recv));
                    let recv_tmp = self.alloc_temp_slot();
                    bytecode.push_store_pop(recv_tmp);
                    let arg_slice = args.as_deref().unwrap_or(&[]);
                    self.consume_spread_emit_ids(arg_slice);
                    let (fixed, rest, pack_rest) =
                        self.split_call_args_for_rest(&lookup_name, arg_slice);
                    let mut arg_temps: Vec<u32> = Vec::new();
                    for arg in &fixed {
                        self.append_with_existential_pack(&mut bytecode, arg);
                        if box_generic_args {
                            if let Some(arg_ty) = self.codegen_expr_ty(arg) {
                                Self::emit_box_if_needed(&mut bytecode, &arg_ty);
                            }
                        }
                        let tmp = self.alloc_temp_slot();
                        bytecode.push_store_pop(tmp);
                        arg_temps.push(tmp);
                    }
                    let nargs = if pack_rest {
                        for arg in &rest {
                            self.append_with_existential_pack(&mut bytecode, arg);
                            if box_generic_args {
                                if let Some(arg_ty) = self.codegen_expr_ty(arg) {
                                    Self::emit_box_if_needed(&mut bytecode, &arg_ty);
                                }
                            }
                        }
                        if self.checker.fn_tuple_rest(&lookup_name) {
                            bytecode.push_make_tuple(rest.len() as u32);
                        } else {
                            bytecode.push_make_array(rest.len() as u32);
                        }
                        let tmp = self.alloc_temp_slot();
                        bytecode.push_store_pop(tmp);
                        arg_temps.push(tmp);
                        (fixed.len() + 1) as u32
                    } else {
                        fixed.len() as u32
                    };
                    bytecode.push_load(recv_tmp);
                    for tmp in &arg_temps {
                        bytecode.push_load(*tmp);
                    }
                    let dict_count = if is_generic {
                        let mut call_arg_tys: Vec<Ty> = Vec::new();
                        if let Some(ty) = recv_ty.clone() {
                            call_arg_tys.push(ty);
                        }
                        for arg in &fixed {
                            match self.codegen_expr_ty(arg) {
                                Some(ty) => call_arg_tys.push(ty),
                                None => {
                                    self.messages.push(Message::error(
                                        ErrorCode::CodegenError,
                                        "missing sidecar type for generic method argument"
                                            .to_string(),
                                        arg.0.into_range(),
                                    ));
                                }
                            }
                        }
                        if pack_rest {
                            call_arg_tys.push(self.synthesize_rest_array_ty(&rest));
                        }
                        let mut forwarded = 0;
                        if let Some(indices) = self.forwarded_dicts_hint(self_id, span.start, span.end)
                        {
                            for dict_index in indices {
                                if let Some(slot) =
                                    self.lookup_slot(&format!("__dict{}", dict_index))
                                {
                                    bytecode.push_load(slot);
                                    forwarded += 1;
                                }
                            }
                        }
                        let call_ret_ty = self.codegen_expr_ty(ast);
                        forwarded
                            + self.emit_call_site_dicts(
                                &mut bytecode,
                                &lookup_name,
                                &call_arg_tys,
                                call_ret_ty.as_ref(),
                            )
                    } else {
                        0
                    };
                    let call_arity = 1 + nargs + dict_count as u32;
                    if !self.emit_direct_fn_call(&mut bytecode, &call_name, call_arity) {
                        self.missing_call_target(&call_name, span.into_range());
                    }
                    if (self.expr_is_niche_option(ast) || self.force_niche_option)
                        && (lookup_name == format!("{}::pop", common::BUILTIN_VEC_TYPE)
                            || lookup_name == format!("{}::remove", common::BUILTIN_VEC_TYPE))
                    {
                        Self::emit_boxed_option_to_niche(&mut bytecode);
                    }
                    if is_generic && self.generic_return_is_boxed(&lookup_name) {
                        if let Some(call_ty) = self.codegen_expr_ty(ast) {
                            Self::emit_unbox_if_needed(&mut bytecode, &call_ty);
                        }
                    }
                } else {
                    let mut message = Message::error(
                        ErrorCode::UnknownFunction,
                        "Unknown method".to_string(),
                        span.into_range(),
                    );
                    message.push(DiagLabel::new(
                        format!("Unable to call unknown method '{}'", fqn),
                        span.into_range(),
                    ));
                    self.messages.push(message);
                }
            } else {
                let mut message = Message::error(
                    ErrorCode::UnknownFunction,
                    "Unknown method".to_string(),
                    span.into_range(),
                );
                message.push(DiagLabel::new(
                    format!("Unable to call method '{}' on '{}'", method, owner),
                    span.into_range(),
                ));
                self.messages.push(message);
            }
        } else {
            if matches!(name.1.as_ref(), Expression::Lambda { .. }) {
                let arg_slice = args.as_deref().unwrap_or(&[]);
                self.consume_spread_emit_ids(arg_slice);
                let flat_args = self.flatten_call_args_for_emit(arg_slice);
                for arg in &flat_args {
                    bytecode.append(&mut self.do_compile(arg));
                }
                bytecode.append(&mut self.do_compile(name));
                bytecode.push(
                    Byte::new(Instruction::CallIndirect)
                        .with_operand_u32(flat_args.len() as u32),
                );
                return bytecode;
            }
            if let Expression::Identifier(raw) = name.1.as_ref() {
                if *raw == "len" {
                    let provided = args.as_ref().map(|items| items.len()).unwrap_or(0);
                    if let Some(items) = args
                        && items.len() == 1
                    {
                        // Prefer compile-time length when known from
                        // literals / static types.
                        if let Some(ConstValue::Int(n)) =
                            crate::const_fold::eval_expr(ast, self.const_env())
                        {
                            self.discard_compile(&items[0]);
                            self.emit_const_value(
                                &ConstValue::Int(n),
                                &mut bytecode,
                            );
                            return bytecode;
                        }
                        if let Some(n) = self.static_len_of(&items[0]) {
                            bytecode.append(&mut self.do_compile(&items[0]));
                            bytecode.push_pop();
                            self.emit_const_value(
                                &ConstValue::Int(n as i64),
                                &mut bytecode,
                            );
                            return bytecode;
                        }
                        // Structural aggregates → ArrayLen. Custom types
                        // with `Length` use the instance method below.
                        let arg_ty = self.codegen_expr_ty(&items[0]).map(|ty| {
                            crate::typechecking::subst::apply_ty_prune(
                                self.checker.subst(),
                                &ty,
                            )
                        });
                        let structural = arg_ty
                            .as_ref()
                            .is_some_and(Checker::is_structural_len_ty_for_codegen);
                        if structural {
                            bytecode.append(&mut self.do_compile(&items[0]));
                            bytecode.push(Byte::new(Instruction::ArrayLen));
                            return bytecode;
                        }
                        if let Some(ty) = arg_ty.as_ref()
                            && let Some(fqn) = self.len_instance_method_fqn(ty)
                            && (self.functions.contains_key(&fqn)
                                || self.fn_entry_labels.contains_key(&fqn))
                        {
                            bytecode.append(&mut self.do_compile(&items[0]));
                            if !self.emit_direct_fn_call(&mut bytecode, &fqn, 1) {
                                self.missing_call_target(&fqn, span.into_range());
                            }
                            return bytecode;
                        }
                        if structural {
                            bytecode.append(&mut self.do_compile(&items[0]));
                            bytecode.push(Byte::new(Instruction::ArrayLen));
                            return bytecode;
                        }
                        bytecode.append(&mut self.do_compile(&items[0]));
                        bytecode.push(Byte::new(Instruction::ArrayLen));
                        return bytecode;
                    } else {
                        let mut message = Message::error(
                            ErrorCode::TooManyArguments,
                            "Invalid len call".to_string(),
                            span.into_range(),
                        );
                        message.push(DiagLabel::new(
                            format!("len expects 1 argument, got {}", provided),
                            span.into_range(),
                        ));
                        self.messages.push(message);
                        return bytecode;
                    }
                }
            }

            let identifier = self.resolve_variable_checked(name);
            let n = self.resolve_free_fn(&identifier);
            // Non-entry modules register `ns::name`, but sibling
            // calls use the bare name. Typecheck inserts bare
            // names so TC can pass while codegen misses — retry
            // the current module FQN before reporting unknown.
            let n = if self.functions.contains_key(&n)
                || self.fn_entry_labels.contains_key(&n)
                || self.lookup_extern_runtime(&n).is_some()
                || self.native.contains_key(&n)
            {
                n
            } else if !self.namespace.is_empty() && !n.contains("::") {
                let qualified = format!("{}::{}", self.namespace, n);
                if self.functions.contains_key(&qualified)
                    || self.fn_entry_labels.contains_key(&qualified)
                    || self
                        .sidecar_overload(self_id, span.start, span.end)
                        .is_some()
                {
                    qualified
                } else {
                    n
                }
            } else {
                n
            };

            // Arity-overload table key (when the typechecker selected one).
            let n = if let Some((fa, is_rest, id)) =
                self.sidecar_overload(self_id, span.start, span.end)
            {
                let keyed = overload_fn_key(&n, fa, is_rest, id);
                if self.functions.contains_key(&keyed) {
                    keyed
                } else {
                    // Try bare-name key when FQN wasn't used at registration.
                    let simple = n.rsplit("::").next().unwrap_or(&n);
                    let keyed_simple = overload_fn_key(simple, fa, is_rest, id);
                    if self.functions.contains_key(&keyed_simple) {
                        keyed_simple
                    } else {
                        n
                    }
                }
            } else {
                n
            };

            if let Some((lib_slot, fn_id_slot)) = self.lookup_extern_runtime(&n) {
                // Same discipline as HostInvoke: emit lib/fn_id first,
                // then compile args onto `self.bytecode`. Nested IO
                // HostInvoke writes directly to `self.bytecode` and
                // returns an empty slice — staging args into a side
                // Vec first left those bytes *before* the LOADs, so
                // MakeTuple packed the wrong stack values.
                let arity = if let Some(items) = args {
                    items.len()
                } else {
                    0
                };
                let variadic = self.checker.is_extern_variadic(&n);
                let depth_on_entry = self.expr_depth;
                self.bytecode
                    .push(Byte::new(Instruction::LoadStatic).with_operand_u32(lib_slot));
                self.bytecode
                    .push(Byte::new(Instruction::LoadStatic).with_operand_u32(fn_id_slot));
                self.expr_depth = depth_on_entry + 2;
                if let Some(items) = args {
                    for arg in items {
                        let mut arg_bc = self.do_compile(arg);
                        self.bytecode.append(&mut arg_bc);
                        self.expr_depth += 1;
                    }
                }
                self.bytecode.push_make_tuple(arity as u32);
                let mut operand = arity as u32 & 0xFFFF;
                if variadic {
                    let call_span = (span.start, span.end);
                    let arg_refs: Vec<_> = args
                        .as_ref()
                        .map(|items| items.iter().collect())
                        .unwrap_or_default();
                    if let Some(tags) = self.resolve_call_ffi_tags(
                        Some(&n),
                        call_span,
                        &arg_refs,
                    ) {
                        for &(tag, aux) in &tags {
                            emit_ffi_type_const(&mut self.bytecode, tag, aux);
                        }
                        self.bytecode.push_make_tuple(tags.len() as u32);
                        operand |= 1 << 16;
                    }
                }
                self.bytecode
                    .push(Byte::new(Instruction::FfiInvoke).with_operand_u32(operand));
                self.expr_depth = depth_on_entry;
                self.emit_result_unwrap_or_panic();
            } else if let Some(&native_id) = self.native.get(&n) {
                // Same stack order as `emit_io_host_invoke`: id first,
                // then args (nested HostInvoke may write to `self.bytecode`).
                let arity = if let Some(items) = args {
                    items.len()
                } else {
                    0
                };
                let depth_on_entry = self.expr_depth;
                self.bytecode
                    .push(Byte::new(Instruction::CONST).with_value_u32(native_id as u32));
                self.expr_depth = depth_on_entry + 1;
                if let Some(items) = args {
                    for arg in items {
                        let mut arg_bc = self.do_compile(arg);
                        self.bytecode.append(&mut arg_bc);
                        self.expr_depth += 1;
                    }
                }
                self.bytecode.push_host_invoke(arity as u32);
                self.expr_depth = depth_on_entry;
            } else if self.functions.contains_key(&n) || self.fn_entry_labels.contains_key(&n) {
                let offset = self.functions.get(&n).copied();
                let mono_offset = self.mono_call_offset(&n, args.as_ref());
                let target_offset = mono_offset.or(offset);
                let lookup_name = strip_overload_key(&n).to_string();
                let pair_kind = self.two_word_return_kind(&lookup_name);
                let is_generic_src = self.checker.is_generic_fn(&lookup_name);
                let is_generic = is_generic_src && mono_offset.is_none();
                // Only box bare `T` args for the shared dict ABI. Nested
                // params like `[T]` (e.g. `collections::sort`) keep the
                // native representation even when not monomorphized.
                let box_generic_args =
                    is_generic && self.generic_has_toplevel_type_param_args(&lookup_name);
                let arg_slice = args.as_deref().unwrap_or(&[]);
                self.consume_spread_emit_ids(arg_slice);
                let flat_arg_slice = self.flatten_call_args_for_emit(arg_slice);

                if self.try_emit_par_specialized_call(&n, Some(arg_slice), &mut bytecode) {
                    return bytecode;
                }

                if pair_kind.is_none()
                    && !is_generic_src
                    && !self.coroutine_fns.contains(&n)
                    && !self.coroutine_fns.contains(&lookup_name)
                    && self.try_emit_inline_direct_call(&n, Some(arg_slice), &mut bytecode)
                {
                    return bytecode;
                }

                // One-level self-unroll: peel recursive callee body once;
                // nested self-calls remain CALL/Entry.
                if pair_kind.is_none()
                    && !is_generic_src
                    && !self.coroutine_fns.contains(&n)
                    && !self.coroutine_fns.contains(&lookup_name)
                    && self.try_emit_self_unroll_call(&n, Some(arg_slice), &mut bytecode)
                {
                    return bytecode;
                }

                // Base-case peel that reads leaf args in place instead of
                // spilling them; falls through to the spilling peel below.
                if let Some(off) = target_offset
                    && !is_generic_src
                    && !is_instance_method_fqn(&self.checker, &lookup_name)
                    && !self.coroutine_fns.contains(&n)
                    && !self.coroutine_fns.contains(&lookup_name)
                    && self.try_emit_remat_peel_call(
                        &n,
                        Some(arg_slice),
                        &mut bytecode,
                        off as u32,
                    )
                {
                    return bytecode;
                }

                // Caller-side base-case peel: cmp-jmp before CALL when
                // the callee opens with fused/unfused compare + imm/slot return.
                // Instance methods with known entries use CALL (not CallIndirect).
                if let Some(off) = target_offset
                    && pair_kind.is_none()
                    && !is_generic_src
                    && !self.coroutine_fns.contains(&n)
                    && !self.coroutine_fns.contains(&lookup_name)
                    && self.try_emit_predicate_peel_call(
                        &n,
                        Some(arg_slice),
                        &mut bytecode,
                        off as u32,
                        /*is_indirect=*/ false,
                    )
                {
                    return bytecode;
                }

                // Partial application → MakeFn (not CALL).
                let (fa, is_rest, _id) = self
                    .sidecar_overload(self_id, span.start, span.end)
                    .or_else(|| {
                        let names = self.checker.fn_param_names(&lookup_name)?;
                        let rest = self.checker.fn_has_rest(&lookup_name);
                        let fixed = if rest {
                            names.len().saturating_sub(1)
                        } else {
                            names.len()
                        };
                        Some((fixed, rest, 0))
                    })
                    .or_else(|| {
                        self.fn_arities
                            .get(&lookup_name)
                            .or_else(|| self.fn_arities.get(&n))
                            .map(|(a, r)| (*a as usize, *r, 0))
                    })
                    .unwrap_or((0, false, 0));
                let fill_mask =
                    self.checker
                        .partial_fill_at(span.start, span.end)
                        .or_else(|| {
                            // Spread args count as their expanded arity, not one slot.
                            let argc = flat_arg_slice.len();
                            if !is_rest && fa > 0 && argc < fa {
                                Some((1u32 << argc).wrapping_sub(1))
                            } else {
                                None
                            }
                        });
                if let Some(off) = target_offset
                    && let Some(mask) = fill_mask.filter(|_| pair_kind.is_none())
                {
                    // Emit filled values in declaration order (already
                    // the order of `flat_arg_slice` after named reorder at TC).
                    for arg in &flat_arg_slice {
                        let value = match arg.1.as_ref() {
                            Expression::NamedArg(_, v) => v,
                            _ => arg,
                        };
                        bytecode.append(&mut self.do_compile(value));
                    }
                    let n_filled = mask.count_ones();
                    bytecode.push_const(mask as i32);
                    bytecode.push(Byte::new(Instruction::CodePtr).with_operand_u32(off as u32));
                    bytecode.push(Byte::new(Instruction::MakeFn).with_operand_u32(
                        make_fn_operand(0, n_filled, fa as u32, is_rest),
                    ));
                    return bytecode;
                }

                let value_arity = self.emit_call_args_with_rest(
                    &lookup_name,
                    arg_slice,
                    &mut bytecode,
                    box_generic_args,
                );

                // ── Dictionary-passing calling convention ──────────────────
                // For non-monomorphized generic calls, append one dict tuple
                // per constraint after the value args. Each dict is a
                // MakeTuple of method code offsets (CodePtr per method in
                // declaration order). Builtin and user instances share this
                // ABI; ground calls may still monomorphize away from the
                // shared body. Dictionaries are for generic bodies only.
                let dict_count = if is_generic {
                    let (fixed, rest, pack_rest) =
                        self.split_call_args_for_rest(&lookup_name, arg_slice);
                    let mut call_arg_tys: Vec<crate::typechecking::Ty> = Vec::new();
                    for arg in &fixed {
                        match self.codegen_expr_ty(arg) {
                            Some(ty) => call_arg_tys.push(ty),
                            None => {
                                self.messages.push(Message::error(
                                    ErrorCode::CodegenError,
                                    "missing sidecar type for generic call argument".to_string(),
                                    arg.0.into_range(),
                                ));
                            }
                        }
                    }
                    // Rest-only generics (`T... xs`) have empty `fixed`;
                    // bind `T` from the packed `[T]` / `[T; N]` arg.
                    if pack_rest {
                        call_arg_tys.push(self.synthesize_rest_array_ty(&rest));
                    }
                    let mut forwarded = 0;
                    if let Some(indices) =
                        self.forwarded_dicts_hint(self_id, span.start, span.end)
                    {
                        for dict_index in indices {
                            if let Some(slot) =
                                self.lookup_slot(&format!("__dict{}", dict_index))
                            {
                                bytecode.push_load(slot);
                                forwarded += 1;
                            }
                        }
                    }
                    let call_ret_ty = self.codegen_expr_ty(ast);
                    forwarded
                        + self.emit_call_site_dicts(
                            &mut bytecode,
                            &lookup_name,
                            &call_arg_tys,
                            call_ret_ty.as_ref(),
                        )
                } else {
                    0
                };

                let arity = value_arity + dict_count as u32;
                let entry_kind = if self.coroutine_fns.contains(&lookup_name)
                    || self.coroutine_fns.contains(&n)
                {
                    crate::il::EntryKind::MakeCoro
                } else {
                    crate::il::EntryKind::Call
                };
                let two_word = matches!(entry_kind, crate::il::EntryKind::Call)
                    .then(|| pair_kind.clone())
                    .flatten();
                let ret_words = if two_word.is_some() { 2 } else { 1 };
                if let Some(off) = mono_offset {
                    bytecode.push(Self::packed_entry_byte_ret(
                        entry_kind,
                        arity,
                        off as u32,
                        ret_words,
                    ));
                } else if !self.emit_named_entry_ret(&mut bytecode, &n, arity, entry_kind, ret_words)
                {
                    self.missing_call_target(&n, span.into_range());
                }
                let vec_option_call = [
                    format!("{}::pop", common::BUILTIN_VEC_TYPE),
                    format!("{}::remove", common::BUILTIN_VEC_TYPE),
                ]
                .iter()
                .any(|name| lookup_name == *name || n == *name);
                if (self.expr_is_niche_option(ast) || self.force_niche_option)
                    && vec_option_call
                {
                    Self::emit_boxed_option_to_niche(&mut bytecode);
                }
                if let Some(enum_name) = two_word {
                    if self.unbox_enum_context == 0 {
                        self.emit_box_pair_after_call(&mut bytecode, &enum_name);
                    }
                }
                // Generic→concrete unbox: only when the return type
                // parameter was boxed as a top-level argument
                // (`id<T>(T) -> T`). Nested params (`F<A> -> A`) are
                // not boxed at construction, so unboxing would zero
                // a valid immediate (Phase 5 HKT / Container::first).
                    if is_generic && self.generic_return_is_boxed(&lookup_name) {
                    if let Some(call_ty) = self.codegen_expr_ty(ast) {
                        Self::emit_unbox_if_needed(&mut bytecode, &call_ty);
                    }
                    } else if is_generic
                        && (self.expr_is_niche_option(ast) || self.force_niche_option)
                    {
                        Self::emit_boxed_option_to_niche(&mut bytecode);
                    }
            } else if self.fn_entry_labels.contains_key(&n) {
                // Reserved by phased emit (COI-109) but body not yet bound.
                let lookup_name = strip_overload_key(&n).to_string();
                let arg_slice = args.as_deref().unwrap_or(&[]);
                self.consume_spread_emit_ids(arg_slice);
                let value_arity = self.emit_call_args_with_rest(
                    &lookup_name,
                    arg_slice,
                    &mut bytecode,
                    false,
                );
                if !self.emit_direct_fn_call(&mut bytecode, &n, value_arity) {
                    let mut message = Message::error(
                        ErrorCode::UnknownFunction,
                        "Unknown function".to_string(),
                        span.into_range(),
                    );
                    message.push(DiagLabel::new(
                        format!("Unable to call unknown function '{}'", n),
                        span.into_range(),
                    ));
                    self.messages.push(message);
                }
            } else if let Some(slot) = self.lookup_slot(&identifier) {
                // Local holding a function value: escaped PolyFn
                // (`let f = show` / `return show`), rank-n parameter, or
                // a PolyFn returned from another call. Emit args, optional
                // application dictionaries, then CallIndirect.
                let arg_slice = args.as_deref().unwrap_or(&[]);
                self.consume_spread_emit_ids(arg_slice);
                let flat_args = self.flatten_call_args_for_emit(arg_slice);
                let value_arity = flat_args.len() as u32;
                let mut arg_tys = Vec::new();
                let polyfn_source = self.polyfn_sources.get(&identifier).cloned();
                // Box for PolyFn locals — including those assigned from a
                // call that returns a captured PolyFn (no polyfn_sources
                // entry). Mono ObjFn / partials / lambdas stay unboxed.
                let needs_arg_box = self.local_call_needs_arg_boxing(&identifier);
                for arg in &flat_args {
                    self.append_with_existential_pack(&mut bytecode, arg);
                    if needs_arg_box {
                        if let Some(arg_ty) = self.codegen_expr_ty(arg) {
                            Self::emit_box_if_needed(&mut bytecode, &arg_ty);
                            arg_tys.push(arg_ty);
                        }
                    }
                }
                let mut dict_count = 0u32;
                if let Some(source) = polyfn_source.as_ref() {
                    if let Some(indices) =
                        self.forwarded_dicts_hint(self_id, span.start, span.end)
                    {
                        for dict_index in indices {
                            if let Some(dict_slot) =
                                self.lookup_slot(&format!("__dict{}", dict_index))
                            {
                                bytecode.push_load(dict_slot);
                                dict_count += 1;
                            }
                        }
                    }
                    let call_ret_ty = self.codegen_expr_ty(ast);
                    dict_count += self.emit_call_site_dicts(
                        &mut bytecode,
                        source,
                        &arg_tys,
                        call_ret_ty.as_ref(),
                    ) as u32;
                }
                // Pack value arity + application dict arity so the VM can
                // merge captured evidence with apply-site dictionaries.
                bytecode.push_load(slot);
                bytecode.push(
                    Byte::new(Instruction::CallIndirect)
                        .with_operand_u32(value_arity | (dict_count << 16)),
                );
                // Generic→concrete unbox for polyfn call site.
                if self.local_polyfn_call_needs_unbox(
                    &identifier,
                    Some((span.start, span.end)),
                ) {
                    let call_ty = self.codegen_expr_ty(ast);
                    let unbox_ty = match call_ty {
                        Some(t) if Self::ty_to_value_tag(&t).is_some() => Some(t),
                        _ => {
                            let arg_tys: Vec<Ty> = flat_args
                                .iter()
                                .filter_map(|a| self.codegen_expr_ty(a))
                                .collect();
                            // Binder forall (not call-site instantiated Fun).
                            let binder = {
                                use crate::typechecking::subst::apply_ty_prune;
                                let mut found = None;
                                for frame in self.mono_codegen_var_types.iter().rev() {
                                    if let Some(ty) = frame.get(&identifier) {
                                        found = Some(apply_ty_prune(
                                            self.checker.subst(),
                                            ty,
                                        ));
                                        break;
                                    }
                                }
                                found.or_else(|| {
                                    self.checker.codegen_var_type(&identifier).map(|ty| {
                                        apply_ty_prune(self.checker.subst(), ty)
                                    })
                                })
                            };
                            binder.and_then(|vt| {
                                Self::instantiate_polyfn_app_result(&vt, &arg_tys)
                            })
                        }
                    };
                    if let Some(ty) = unbox_ty {
                        Self::emit_unbox_if_needed(&mut bytecode, &ty);
                    }
                }
            } else {
                let mut message = Message::error(
                    ErrorCode::UnknownFunction,
                    "Unknown function".to_string(),
                    span.into_range(),
                );
                message.push(DiagLabel::new(
                    format!("Unable to call unknown function '{}'", n),
                    span.into_range(),
                ));
                self.messages.push(message);
            }
        } // end non-method Call
        bytecode
    }

    /// Direct `CALL`, or `Entry` when the callee body is still ahead.
    ///
    /// `dest` must be a fragment that will be appended onto [`Self::bytecode`]
    /// (not `self.bytecode` itself). Forward refs flush `dest` first so the
    /// reserved module label is not remapped as fragment-local.
    pub(super) fn emit_direct_fn_call(&mut self, dest: &mut CodeBuf, name: &str, arity: u32) -> bool {
        self.emit_named_entry(dest, name, arity, crate::il::EntryKind::Call)
    }

    pub(super) fn emit_named_entry(
        &mut self,
        dest: &mut CodeBuf,
        name: &str,
        arity: u32,
        kind: crate::il::EntryKind,
    ) -> bool {
        match kind {
            crate::il::EntryKind::Call => {
                let two_word = self.two_word_return_kind(name);
                let ret_words = if two_word.is_some() { 2 } else { 1 };
                let ok = self.emit_named_entry_ret(dest, name, arity, kind, ret_words);
                if ok
                    && self.unbox_enum_context == 0
                    && let Some(enum_name) = two_word
                {
                    self.emit_box_pair_after_call(dest, &enum_name);
                }
                ok
            }
            crate::il::EntryKind::CodePtr | crate::il::EntryKind::MakePolyFn => {
                if !self.deny_two_word_address_of(name, 0..0) {
                    return false;
                }
                self.emit_named_entry_ret(dest, name, arity, kind, 1)
            }
            _ => self.emit_named_entry_ret(dest, name, arity, kind, 1),
        }
    }

    /// `true` when `name`'s two-word classification is safe to ignore here
    /// (i.e. it stays one word — the common case). `false` when `name`
    /// returns a known two-word layout: this call site takes its address
    /// (`CodePtr` / `MakePolyFn` / FFI callback / partial application),
    /// which needs the one-word ABI (task cut: `CallIndirect`, PolyFn,
    /// FFI, coroutines keep boxed `ObjEnum`). Records a diagnostic instead
    /// of silently mis-encoding the target as a unary entry.
    pub(super) fn deny_two_word_address_of(
        &mut self,
        name: &str,
        range: std::ops::Range<usize>,
    ) -> bool {
        let Some(enum_name) = self.two_word_return_kind(name) else {
            return true;
        };
        let mut message = Message::error(
            ErrorCode::CodegenError,
            format!(
                "`{name}` returns a known two-word `{enum_name}` layout and cannot be used as a function value"
            ),
            range.clone(),
        );
        message.push(DiagLabel::new(
            "direct calls are fine; taking its address (assigning it, passing it as a callback, or partially applying it) is not supported for this return layout".to_string(),
            range,
        ));
        self.messages.push(message);
        false
    }

    /// Same as [`Self::emit_named_entry`] with an explicit `CALL` return
    /// width (`1` or `2` words). Non-`Call` kinds ignore `ret_words`.
    pub(super) fn emit_named_entry_ret(
        &mut self,
        dest: &mut CodeBuf,
        name: &str,
        arity: u32,
        kind: crate::il::EntryKind,
        ret_words: u32,
    ) -> bool {
        if let Some(&offset) = self.functions.get(name) {
            dest.push(Self::packed_entry_byte_ret(
                kind,
                arity,
                offset as u32,
                ret_words,
            ));
            true
        } else if let Some(label) = self.fn_entry_labels.get(name).copied() {
            self.bytecode.append(dest);
            self.bytecode
                .il_mut()
                .emit_entry_ret_at(kind, arity, label, DebugLoc::unknown(), ret_words);
            true
        } else {
            false
        }
    }

    /// Same as [`Self::emit_named_entry`] onto the module buffer.
    pub(super) fn emit_named_entry_on_module(
        &mut self,
        name: &str,
        arity: u32,
        kind: crate::il::EntryKind,
    ) -> bool {
        match kind {
            crate::il::EntryKind::Call => {
                let two_word = self.two_word_return_kind(name);
                let ret_words = if two_word.is_some() { 2 } else { 1 };
                let ok = self.emit_named_entry_on_module_ret(name, arity, kind, ret_words);
                if ok
                    && self.unbox_enum_context == 0
                    && let Some(enum_name) = two_word
                {
                    let mut bytecode = std::mem::take(&mut self.bytecode);
                    self.emit_box_pair_after_call(&mut bytecode, &enum_name);
                    self.bytecode = bytecode;
                }
                ok
            }
            crate::il::EntryKind::CodePtr | crate::il::EntryKind::MakePolyFn => {
                if !self.deny_two_word_address_of(name, 0..0) {
                    return false;
                }
                self.emit_named_entry_on_module_ret(name, arity, kind, 1)
            }
            _ => self.emit_named_entry_on_module_ret(name, arity, kind, 1),
        }
    }

    /// Same as [`Self::emit_named_entry_on_module`] with an explicit `CALL`
    /// return width (`1` or `2` words). Non-`Call` kinds ignore `ret_words`.
    pub(super) fn emit_named_entry_on_module_ret(
        &mut self,
        name: &str,
        arity: u32,
        kind: crate::il::EntryKind,
        ret_words: u32,
    ) -> bool {
        if let Some(&offset) = self.functions.get(name) {
            self.bytecode
                .push(Self::packed_entry_byte_ret(kind, arity, offset as u32, ret_words));
            true
        } else if let Some(label) = self.fn_entry_labels.get(name).copied() {
            self.bytecode
                .il_mut()
                .emit_entry_ret_at(kind, arity, label, DebugLoc::unknown(), ret_words);
            true
        } else {
            false
        }
    }

    pub(super) fn packed_entry_byte_ret(
        kind: crate::il::EntryKind,
        arity: u32,
        offset: u32,
        ret_words: u32,
    ) -> Byte {
        let inst = match kind {
            crate::il::EntryKind::Call => Instruction::CALL,
            crate::il::EntryKind::TailCall => Instruction::TailCall,
            crate::il::EntryKind::MakeCoro => Instruction::MakeCoro,
            crate::il::EntryKind::CodePtr => Instruction::CodePtr,
            crate::il::EntryKind::MakePolyFn => Instruction::MakePolyFn,
        };
        match kind {
            crate::il::EntryKind::CodePtr | crate::il::EntryKind::MakePolyFn => {
                Byte::new(inst).with_operand_u32(offset)
            }
            crate::il::EntryKind::Call => {
                Byte::new(inst).with_call_packed_ret(arity, offset, ret_words)
            }
            _ => Byte::new(inst).with_call_packed(arity, offset),
        }
    }

    pub(super) fn missing_call_target(&mut self, name: &str, range: std::ops::Range<usize>) {
        let mut message = Message::error(
            ErrorCode::CodegenError,
            format!("missing function entry `{name}`"),
            range.clone(),
        );
        message.push(DiagLabel::new(
            format!("no bound or reserved entry for `{name}`"),
            range,
        ));
        self.messages.push(message);
    }

    pub(super) fn emit_call_indirect(bytecode: &mut impl EmitBuf, target_offset: u32, arity: u32) {
        bytecode.push(Byte::new(Instruction::CodePtr).with_operand_u32(target_offset));
        bytecode.push(Byte::new(Instruction::CallIndirect).with_operand_u32(arity));
    }

}
