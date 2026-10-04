//! Phase-2 HIR lowering: function bodies inside [`crate::hir::lower`]'s
//! core subset are emitted from HIR instead of the AST walk.
//!
//! [`Compiler::compile_function_decl_into`] keeps the prologue, slots for
//! parameters, the fall-through return and function records; only the body
//! walk is replaced. Every check runs before the first emit, so a refused
//! body falls back to the AST codegen with nothing to roll back.

use super::*;
use crate::hir::lower;
use crate::hir::{BinOp, Callee, HirBody, HirId, HirKind, Lit, LocalId, UnOp};

/// Labels of one enclosing HIR loop.
#[derive(Clone, Copy)]
struct HirLoop {
    top: IlLabel,
    exit: IlLabel,
}

/// Per-body lowering state.
struct HirEmit {
    /// Frame slot of each [`LocalId`], once bound.
    slots: Vec<Option<u32>>,
    /// Resolved table key of each direct call, by call node.
    calls: HashMap<u32, String>,
    /// Calls in return position that lower to `TailCall`.
    tail_calls: HashSet<u32>,
    loops: Vec<HirLoop>,
}

impl Compiler {
    /// Build the module's HIR for lowering when `--hir` is on.
    pub(super) fn build_hir_for_lowering(&mut self, module: &str, ast: &Output<'_>) {
        self.hir_fns.clear();
        self.hir_module = None;
        if !self.hir_lowering {
            return;
        }
        let hir = crate::hir::build_module(&self.checker, &self.typed_sidecar, module, ast);
        for (i, body) in hir.bodies.iter().enumerate() {
            if body.kind == crate::hir::BodyKind::Function {
                self.hir_fns.insert(body.span, i);
            }
        }
        self.hir_module = Some(hir);
    }

    /// Lower the body of the function declared at `span` from HIR. `false`
    /// leaves the body to the AST walk (lowering off, or the body is outside
    /// the subset).
    pub(super) fn try_lower_hir_function(&mut self, span: &SimpleSpan, body: &Output<'_>) -> bool {
        if !self.hir_lowering
            || self.compiling_mono_clone
            || self.compiling_result_mode
            || self.compiling_two_word_enum.is_some()
        {
            return false;
        }
        let Some(&index) = self.hir_fns.get(&(span.start, span.end)) else {
            return false;
        };
        let Some(module) = self.hir_module.take() else {
            return false;
        };
        let hir = &module.bodies[index];
        // The body's pre-order position; the emit cursor may still sit on
        // parameter nodes before it, never past it.
        let table = self.checker.id_table();
        let body_pos = table
            .walk_id(body, table.ids().get(self.emit_idx).copied())
            .map(|id| id.0 as usize)
            .filter(|&pos| pos >= self.emit_idx && pos < table.len());
        let plan = if body_pos.is_some() { None } else { Some("emit-cursor") }
            .or_else(|| lower::refusal(hir)).map_or_else(|| self.plan_hir_body(hir), Err);
        let lowered = match plan {
            Ok(mut emit) => {
                if let Some(root) = hir.root {
                    self.hir_effect(hir, &mut emit, root);
                }
                self.skip_emit_ids_in(body_pos.unwrap_or(self.emit_idx), body);
                crate::il::opt::note_hir_lowered();
                true
            }
            Err(reason) => {
                crate::il::opt::note_hir_fallback(reason);
                false
            }
        };
        self.hir_module = Some(module);
        lowered
    }

    /// Move the emit-order id cursor past `body`'s subtree (which starts at
    /// pre-order position `pos`), where the AST walk would have left it.
    fn skip_emit_ids_in(&mut self, pos: usize, body: &Output<'_>) {
        fn count(node: &Output<'_>) -> usize {
            let mut n = 1;
            crate::typechecking::id::walk_children(node, &mut |c| n += count(c));
            n
        }
        self.emit_idx = (pos + count(body)).min(self.checker.id_table().len());
    }

    /// Bind parameter slots and resolve every call; any refusal here is the
    /// fallback reason.
    fn plan_hir_body(&self, hir: &HirBody) -> Result<HirEmit, &'static str> {
        let mut emit = HirEmit {
            slots: vec![None; hir.locals.len()],
            calls: HashMap::new(),
            tail_calls: HashSet::new(),
            loops: Vec::new(),
        };
        for &param in &hir.params {
            let slot = self
                .lookup_slot(&hir.local(param).name)
                .ok_or("parameter-slot")?;
            emit.slots[param.0 as usize] = Some(slot);
        }
        for (i, expr) in hir.exprs.iter().enumerate() {
            if let HirKind::Call {
                callee: Callee::Named { name, .. },
                args,
            } = &expr.kind
            {
                let key = self.resolve_hir_callee(hir, HirId(i as u32), name, args.len())?;
                emit.calls.insert(i as u32, key);
            }
        }
        for expr in &hir.exprs {
            if let HirKind::Return(Some(value)) = expr.kind
                && let Some(key) = emit.calls.get(&value.0)
                && self.hir_tail_call_ok(key)
            {
                emit.tail_calls.insert(value.0);
            }
        }
        Ok(emit)
    }

    /// The table key a direct call to `name` reaches, or why it is not a
    /// plain `CALL` to a scalar user function. The special forms are checked
    /// in `compile_call_expr`'s order, so a name those claim is never lowered
    /// as a user call.
    fn resolve_hir_callee(
        &self,
        hir: &HirBody,
        call: HirId,
        name: &str,
        argc: usize,
    ) -> Result<String, &'static str> {
        let node = hir.expr(call);
        let (start, end) = node.span;
        if name == "len" || self.checker.bare_construct_at(start, end).is_some() {
            return Err("callee-builtin");
        }
        if !name.contains("::")
            && (self.string_builtin_for_call(name).is_some()
                || self.checker.prelude_fn_in_scope(name).is_some()
                || self.checker.ffi_fn_in_scope(name).is_some()
                || self.checker.io_fn_in_scope(name).is_some()
                || self.checker.thread_fn_in_scope(name).is_some()
                || self.checker.gc_fn_in_scope(name).is_some()
                || self.checker.host_fn_in_scope(name).is_some())
        {
            return Err("callee-builtin");
        }
        if name.contains("::") && self.string_builtin_for_call(name).is_some() {
            return Err("callee-builtin");
        }
        if self.existential_method_hint(node.node, start, end).is_some()
            || self.bound_method_hint(node.node, start, end).is_some()
            || self.sidecar_dicts(node.node, start, end).is_some_and(|d| !d.is_empty())
        {
            return Err("callee-trait");
        }
        if self.sidecar_overload(node.node, start, end).is_some()
            || self.checker.partial_fill_at(start, end).is_some()
        {
            return Err("callee-overload");
        }
        let mut key = self.resolve_free_fn(name);
        let known = |k: &str| self.functions.contains_key(k) || self.fn_entry_labels.contains_key(k);
        if !known(&key) && !self.namespace.is_empty() && !key.contains("::") {
            key = format!("{}::{}", self.namespace, key);
        }
        if !known(&key) {
            return Err("callee-unknown");
        }
        if self.lookup_extern_runtime(&key).is_some() || self.native.contains_key(&key) {
            return Err("callee-native");
        }
        let lookup = strip_overload_key(&key);
        if self.checker.is_overloaded(lookup) || self.checker.is_overloaded(name) {
            return Err("callee-overload");
        }
        if self.checker.is_generic_fn(lookup) {
            return Err("callee-generic");
        }
        if self.coroutine_fns.contains(&key) || self.coroutine_fns.contains(lookup) {
            return Err("callee-coroutine");
        }
        if self.two_word_return_kind(lookup).is_some() || self.two_word_return_kind(&key).is_some() {
            return Err("callee-pair");
        }
        match self
            .fn_arities
            .get(&key)
            .or_else(|| self.fn_arities.get(lookup))
        {
            Some(&(fixed, false)) if fixed as usize == argc => {}
            _ => return Err("callee-arity"),
        }
        let params = self.checker.fn_param_tys(lookup).ok_or("callee-signature")?;
        if params.len() != argc || !params.iter().all(lower::is_scalar) {
            return Err("callee-signature");
        }
        Ok(key)
    }

    /// `return f(..)` may jump instead of call, under the rules
    /// [`Self::resolve_tail_callee_key`] applies to the AST.
    fn hir_tail_call_ok(&self, key: &str) -> bool {
        let Some(cur) = self.current_function_table_key.as_deref() else {
            return false;
        };
        self.fn_defers.is_empty()
            && (self.same_fn_key(key, cur) || self.both_in_rec_cycle(cur, key))
            && self.tail_call_abi_matches(key)
    }

    fn hir_slot(emit: &HirEmit, local: LocalId) -> u32 {
        emit.slots[local.0 as usize].expect("HIR local read before its let")
    }

    /// Push exactly one word for `id`.
    fn hir_value(&mut self, hir: &HirBody, emit: &mut HirEmit, id: HirId) {
        match &hir.expr(id).kind {
            HirKind::Lit(Lit::Int(n)) => {
                let n = *n;
                if (0..=i32::MAX as i64).contains(&n) {
                    self.bytecode.push_const(n as i32);
                } else {
                    let idx = self.intern_constant(Value::from(n).raw() as u64);
                    self.bytecode.push_const_pool(idx);
                }
            }
            HirKind::Lit(Lit::Float(f)) => {
                let idx = self.intern_constant(Value::from(*f).raw() as u64);
                self.bytecode.push_const_pool(idx);
            }
            HirKind::Lit(Lit::Bool(b)) => self.bytecode.push(Byte::new_with_value(
                Instruction::CONST,
                Value::from(*b).raw() as _,
            )),
            HirKind::Local(local) => {
                let slot = Self::hir_slot(emit, *local);
                self.bytecode.push_load(slot);
            }
            HirKind::Bin { op, lhs, rhs } => {
                let float = hir.expr(*lhs).ty.as_ref().is_some_and(lower::is_float);
                self.hir_value(hir, emit, *lhs);
                self.hir_value(hir, emit, *rhs);
                self.bytecode.push(Byte::new(Self::hir_bin_instruction(*op, float)));
            }
            HirKind::Logic { and, lhs, rhs } => {
                // The AST codegen evaluates both sides into `AND` / `OR`.
                self.hir_value(hir, emit, *lhs);
                self.hir_value(hir, emit, *rhs);
                self.bytecode.push(Byte::new(if *and {
                    Instruction::AND
                } else {
                    Instruction::OR
                }));
            }
            HirKind::Un { op, operand } => {
                let float = hir.expr(*operand).ty.as_ref().is_some_and(lower::is_float);
                self.hir_value(hir, emit, *operand);
                self.bytecode.push(Byte::new(match op {
                    UnOp::Neg if float => Instruction::NEGF,
                    UnOp::Neg => Instruction::NEG,
                    UnOp::BitNot => Instruction::NOT,
                    UnOp::Not => Instruction::LogNot,
                }));
            }
            HirKind::Call { args, .. } => {
                for &arg in args {
                    self.hir_value(hir, emit, arg);
                }
                let key = emit.calls[&id.0].clone();
                let kind = if emit.tail_calls.contains(&id.0) {
                    crate::il::EntryKind::TailCall
                } else {
                    crate::il::EntryKind::Call
                };
                let ok = self.emit_named_entry_on_module_ret(&key, args.len() as u32, kind, 1);
                debug_assert!(ok, "planned HIR call target `{key}` has an entry");
            }
            HirKind::If {
                cond,
                then,
                els: Some(els),
            } => {
                let else_l = self.bytecode.fresh_label();
                let end = self.bytecode.fresh_label();
                self.hir_value(hir, emit, *cond);
                self.hir_jump(IlJumpKind::JumpIfFalse, else_l);
                self.hir_value(hir, emit, *then);
                self.hir_jump(IlJumpKind::Unconditional, end);
                self.bytecode.bind_label(else_l);
                self.hir_value(hir, emit, *els);
                self.bytecode.bind_label(end);
            }
            HirKind::Block {
                stmts,
                tail: Some(tail),
            } => {
                for &s in stmts {
                    self.hir_stmt(hir, emit, s);
                }
                self.hir_value(hir, emit, *tail);
            }
            HirKind::Break | HirKind::Continue | HirKind::Return(_) => {
                self.hir_effect(hir, emit, id);
            }
            other => unreachable!("HIR lowering admitted {other:?}"),
        }
    }

    /// `id` as a block statement, with the statement's source location on
    /// every op it emits (line breakpoints, backtraces).
    fn hir_stmt(&mut self, hir: &HirBody, emit: &mut HirEmit, id: HirId) {
        let il_start = self.bytecode.il_mut().raw_len();
        self.hir_effect(hir, emit, id);
        let (start, end) = hir.expr(id).span;
        self.fill_statement_locs(il_start, SimpleSpan::from(start..end));
    }

    /// Run `id` for its effect; the operand stack is left as found.
    fn hir_effect(&mut self, hir: &HirBody, emit: &mut HirEmit, id: HirId) {
        match &hir.expr(id).kind {
            HirKind::Block { stmts, tail } => {
                for &s in stmts {
                    self.hir_stmt(hir, emit, s);
                }
                if let Some(t) = tail {
                    self.hir_stmt(hir, emit, *t);
                }
            }
            HirKind::Let {
                local,
                init: Some(init),
            } => {
                // Value first: its operands live above every bound slot.
                self.hir_value(hir, emit, *init);
                let slot = self.hir_bind_local(hir, *local);
                emit.slots[local.0 as usize] = Some(slot);
                self.bytecode.push_store_pop(slot);
            }
            HirKind::Assign { place, value } => {
                let HirKind::Local(local) = hir.expr(*place).kind else {
                    unreachable!("HIR lowering admitted a non-local assignment place");
                };
                self.hir_value(hir, emit, *value);
                let slot = Self::hir_slot(emit, local);
                self.bytecode.push_store_pop(slot);
            }
            HirKind::If { cond, then, els } => {
                let end = self.bytecode.fresh_label();
                self.hir_value(hir, emit, *cond);
                match els {
                    Some(els) => {
                        let else_l = self.bytecode.fresh_label();
                        self.hir_jump(IlJumpKind::JumpIfFalse, else_l);
                        self.hir_effect(hir, emit, *then);
                        self.hir_jump(IlJumpKind::Unconditional, end);
                        self.bytecode.bind_label(else_l);
                        self.hir_effect(hir, emit, *els);
                    }
                    None => {
                        self.hir_jump(IlJumpKind::JumpIfFalse, end);
                        self.hir_effect(hir, emit, *then);
                    }
                }
                self.bytecode.bind_label(end);
            }
            HirKind::Loop { body } => {
                let top = self.bytecode.fresh_label();
                let exit = self.bytecode.fresh_label();
                self.bytecode.bind_label(top);
                emit.loops.push(HirLoop { top, exit });
                // `while c { b }` keeps the AST loop shape: test, body, back edge.
                if let Some((cond, then)) = lower::while_shape(hir, *body) {
                    self.hir_value(hir, emit, cond);
                    self.hir_jump(IlJumpKind::JumpIfFalse, exit);
                    self.hir_effect(hir, emit, then);
                } else {
                    self.hir_effect(hir, emit, *body);
                }
                emit.loops.pop();
                self.hir_jump(IlJumpKind::Unconditional, top);
                self.bytecode.bind_label(exit);
            }
            HirKind::Break => {
                let target = emit.loops.last().expect("break inside a loop").exit;
                self.hir_jump(IlJumpKind::Unconditional, target);
            }
            HirKind::Continue => {
                let target = emit.loops.last().expect("continue inside a loop").top;
                self.hir_jump(IlJumpKind::Unconditional, target);
            }
            HirKind::Return(value) => match lower::returned_value(hir, *value) {
                Some(v) if emit.tail_calls.contains(&v.0) => {
                    // `TailCall` is the terminator; the callee returns for us.
                    self.hir_value(hir, emit, v);
                }
                Some(v) => {
                    self.hir_value(hir, emit, v);
                    self.emit_run_defers();
                    self.bytecode.push_return();
                }
                None => {
                    self.emit_run_defers();
                    self.bytecode.push_const(0);
                    self.bytecode.push_return();
                }
            },
            HirKind::Lit(_)
            | HirKind::Local(_)
            | HirKind::Bin { .. }
            | HirKind::Logic { .. }
            | HirKind::Un { .. }
            | HirKind::Call { .. } => {
                self.hir_value(hir, emit, id);
                self.bytecode.push_pop();
            }
            other => unreachable!("HIR lowering admitted {other:?}"),
        }
    }

    fn hir_jump(&mut self, kind: IlJumpKind, target: IlLabel) {
        self.bytecode.push_op(IlOp::Jump {
            kind,
            target,
            loc: DebugLoc::unknown(),
            hint: Default::default(),
        });
    }

    /// A fresh slot for `local`. HIR locals are distinct even when their
    /// names repeat, so a shadowing `let` gets its own `__shadow_` slot
    /// (the debugger shows it under the source name).
    fn hir_bind_local(&mut self, hir: &HirBody, local: LocalId) -> u32 {
        let name = &hir.local(local).name;
        let key = if self.context.variables.key(name).is_some() {
            format!("__shadow_{name}_{}", local.0)
        } else {
            name.clone()
        };
        let slot = self.context.variables.intern(key.clone()) as u32;
        self.record_debug_local(&key, slot);
        slot
    }

    fn hir_bin_instruction(op: BinOp, float: bool) -> Instruction {
        use Instruction as I;
        match op {
            BinOp::IntAdd => I::ADD,
            BinOp::IntSub => I::SUB,
            BinOp::IntMul => I::MUL,
            BinOp::IntDiv => I::DIV,
            BinOp::IntRem => I::MOD,
            BinOp::IntPow => I::Pow,
            BinOp::FloatAdd => I::ADDF,
            BinOp::FloatSub => I::SUBF,
            BinOp::FloatMul => I::MULF,
            BinOp::FloatDiv => I::DIVF,
            BinOp::FloatRem => I::MODF,
            BinOp::FloatPow => I::PowF,
            BinOp::Shl => I::SHL,
            BinOp::Shr => I::SHR,
            BinOp::BitAnd => I::BITAND,
            BinOp::BitOr => I::BITOR,
            BinOp::BitXor => I::XOR,
            BinOp::Eq => I::EQ,
            BinOp::Ne => I::NEQ,
            // `Instruction::LE` is `<` (the AST's `Expression::Le`).
            BinOp::Lt if float => I::LEF,
            BinOp::Lt => I::LE,
            BinOp::Le if float => I::LEQF,
            BinOp::Le => I::LEQ,
            BinOp::Gt if float => I::GTF,
            BinOp::Gt => I::GT,
            BinOp::Ge if float => I::GEQF,
            BinOp::Ge => I::GEQ,
            BinOp::StrConcat | BinOp::Overloaded(_) => {
                unreachable!("HIR lowering admitted a non-primitive operator")
            }
        }
    }
}
