use super::*;
use crate::typechecking::{CStructDef, ForInInfo, ForInKind, PairNicheAbi};
use reporting::{ErrorCode, Message};

#[path = "emit_call.rs"]
mod emit_call;
#[path = "emit_match.rs"]
mod emit_match;

#[cfg(any(test, feature = "dissect"))]
type FinalizeIlOut = Option<crate::dissect::IlSnapshot>;
#[cfg(not(any(test, feature = "dissect")))]
type FinalizeIlOut = ();

/// Lowered form of a [`ParCombine`](crate::typechecking::ParCombine): the single
/// instruction that folds the joined arm results once they are all on the stack.
enum ParCombinePlan {
    /// `ADD` / `SUB` / `MUL` over exactly two arms.
    Bin(Instruction),
    /// Rebuild a call with the arm results as arguments.
    Call { entry: u32, arity: u32 },
    /// `(arm0, …)` tuple pack.
    Tuple { arity: u32 },
    /// `MakeEnum` with the variant's tag and payload arity.
    Enum { tag: u16, arity: u16 },
}

impl Compiler {
    /// Expose inferred state to language tooling after a module is checked.
    pub fn checker(&self) -> &crate::typechecking::Checker {
        &self.checker
    }

    pub fn checker_mut(&mut self) -> &mut crate::typechecking::Checker {
        &mut self.checker
    }

    pub fn aliases(&self) -> &HashMap<String, String> {
        &self.aliases
    }

    pub fn module_items(&self) -> &HashMap<String, Vec<String>> {
        &self.module_items
    }

    /// Run HM inference for a module without emitting bytecode.
    pub fn typecheck_module<'compiler>(
        &mut self,
        module: &str,
        ast: &(SimpleSpan, Box<Expression<'compiler>>),
    ) {
        self.checker.set_current_module(module);
        let _ = self.checker.check_program(ast);
        self.typed_sidecar = self.checker.typed_sidecar();
        self.messages.extend(self.checker.take_messages());
    }

    /// Record `#[derive]` constructor aliases from attribute expansion.
    pub(crate) fn apply_expand_result(
        &mut self,
        module: &str,
        expand: crate::attrs::ExpandResult,
    ) {
        self.messages.extend(expand.messages);
        for (k, v) in expand.decorated_class_ctors {
            let key = if module.is_empty() {
                k
            } else {
                format!("{module}::{k}")
            };
            let ctor_fn = if module.is_empty() {
                v
            } else {
                format!("{module}::{v}")
            };
            self.decorated_class_ctors.insert(key, ctor_fn);
        }
    }

    /// Expand `#[derive]` then typecheck. Does not parse or emit.
    pub fn expand_and_check<'a>(
        &mut self,
        module: &str,
        ast: &mut (SimpleSpan, Box<Expression<'a>>),
    ) {
        let expand = crate::attrs::expand_program(ast);
        self.apply_expand_result(module, expand);
        self.typecheck_module(module, ast);
    }

    /// Parse, expand attributes, typecheck. Shared by pipeline compile and
    /// `typecheck_project` / LSP.
    pub fn parse_expand_check<'a>(
        &mut self,
        module: &str,
        src: &'a str,
    ) -> Result<(SimpleSpan, Box<Expression<'a>>), reporting::Message> {
        let mut ast = parser::Pratt::default().parse(src)?;
        self.expand_and_check(module, &mut ast);
        Ok(ast)
    }

    pub fn constants(&self) -> &[u64] {
        &self.constants
    }

    pub fn strings(&self) -> &[String] {
        &self.strings
    }

    /// Operand-stack capacity recommended by recursion-depth analysis.
    pub fn operand_stack_slots(&self) -> u32 {
        self.operand_stack_slots
    }

    pub fn set_source_file(&mut self, path: impl Into<std::path::PathBuf>) {
        self.current_source_file = Some(path.into());
    }

    pub fn source_files_list(&self) -> Vec<String> {
        self.source_file_list.clone()
    }

    pub fn debug_locs(&self) -> &[DebugLoc] {
        &self.debug_locs
    }

    /// Function entry symbols for panic backtraces (sorted by `entry_pc`).
    pub fn fn_debug_symbols(&self) -> Vec<FnDebugSym> {
        let mut syms: Vec<FnDebugSym> = self
            .functions
            .iter()
            .map(|(name, &pc)| FnDebugSym {
                name: name.clone(),
                entry_pc: pc as u32,
            })
            .collect();
        syms.sort_by_key(|s| s.entry_pc);
        syms
    }

    fn pad_debug_locs(&mut self) {
        while self.debug_locs.len() < self.bytecode.len() {
            self.debug_locs.push(DebugLoc::unknown());
        }
        if self.debug_locs.len() > self.bytecode.len() {
            self.debug_locs.truncate(self.bytecode.len());
        }
    }

    /// Run registered `defer` thunks in LIFO order.
    ///
    /// For each thunk: LOAD `use (…)` captures from the enclosing frame, then
    /// `CALL` the thunk entry with that arity (push return IP + new frame whose
    /// slots 0..N-1 are the captures). The thunk ends in `RETURN`, which
    /// resumes at the next op. A following `POP` discards the thunk's sentinel
    /// return value so a pending function return value stays on top.
    fn emit_run_defers(&mut self) {
        let defers = self.fn_defers.clone();
        for (label, captures) in defers.iter().rev() {
            for cap in captures {
                if let Some(slot) = self.lookup_slot(cap) {
                    self.bytecode.push_load(slot);
                } else {
                    // Typecheck should have rejected unknown captures; emit a
                    // zero so the CALL arity still matches.
                    debug_assert!(
                        false,
                        "defer capture `{cap}` missing from enclosing frame at codegen"
                    );
                    self.bytecode.push(Byte::new_with_value(
                        Instruction::CONST,
                        Value::default().raw() as _,
                    ));
                }
            }
            self.bytecode
                .emit_entry(EntryKind::Call, captures.len() as u32, *label);
            self.bytecode.push_pop();
        }
    }

    fn loc_from_span(&mut self, span: SimpleSpan) -> DebugLoc {
        let file = self.intern_source_file();
        if file == DEBUG_FILE_UNKNOWN {
            return DebugLoc::unknown();
        }
        let start = span.start as u32;
        let end = span.end.max(span.start + 1) as u32;
        DebugLoc {
            file,
            start_byte: start,
            end_byte: end,
        }
    }

    fn intern_source_file(&mut self) -> u32 {
        let Some(ref path) = self.current_source_file else {
            return DEBUG_FILE_UNKNOWN;
        };
        let key = path.to_string_lossy().into_owned();
        if let Some(&id) = self.source_file_indices.get(&key) {
            return id;
        }
        let id = self.source_file_list.len() as u32;
        self.source_file_list.push(key.clone());
        self.source_file_indices.insert(key, id);
        id
    }

    fn emit_byte(&mut self, span: SimpleSpan, b: Byte) {
        self.pad_debug_locs();
        let loc = self.loc_from_span(span);
        self.bytecode.push(b);
        self.debug_locs.push(loc);
    }

    fn emit_bytes(&mut self, span: SimpleSpan, bytes: &mut CodeBuf) {
        if bytes.is_empty() {
            return;
        }
        let loc = self.loc_from_span(span);
        for op in bytes.il_mut().ops_mut() {
            op.set_loc(loc);
        }
        let n = bytes.len();
        self.bytecode.append(bytes);
        self.pad_debug_locs();
        let start = self.debug_locs.len().saturating_sub(n);
        for slot in &mut self.debug_locs[start..] {
            *slot = loc;
        }
    }

    /// Number of global static slots for the VM table.
    pub fn static_slot_count(&self) -> u32 {
        self.checker.static_slot_count()
    }

    /// Prologue `JMP` target: static initializers and/or `extern` setup
    /// run at `setup_entry_offset`; otherwise jump straight to `main`.
    pub fn prologue_jmp_target(&self) -> u32 {
        if self.static_slot_count() > 0
            || self.has_extern_block()
            || self.checker.classes_with_drop().next().is_some()
        {
            self.setup_entry_offset
        } else {
            self.functions
                .get("main")
                .copied()
                .unwrap_or(self.program_start_offset as usize) as u32
        }
    }

    /// Bytecode offset of `main`, if bound.
    pub fn main_offset(&self) -> Option<u32> {
        self.functions.get("main").copied().map(|o| o as u32)
    }

    /// Harness test cases emitted this compile: `(description, fn offset)`.
    pub fn test_cases(&self) -> &[(String, u32)] {
        &self.test_cases
    }

    /// Include harness `test("…")` / `#[test]` declarations in the compile unit.
    pub fn set_include_tests(&mut self, include: bool) {
        self.include_tests = include;
    }

    pub fn include_tests(&self) -> bool {
        self.include_tests
    }

    /// Apply an [`crate::OptLevel`] preset to IL opts and tiny-inline budgets.
    pub fn set_opt_level(&mut self, level: crate::OptLevel) {
        self.opt_options = level.options();
        self.inline_cost.max_inline_cost = level.inline_max_cost();
        self.inline_cost.inline_across_modules = level.inline_across_modules();
        if !level.inline_across_modules() {
            self.inline_cost.max_cross_module_inline_cost = 0;
        }
        self.bytecode.set_opt_options(self.opt_options.clone());
    }

    /// Enable or disable IL opt-stat collection (COI-131).
    pub fn set_collect_opt_stats(&mut self, on: bool) {
        self.opt_options.collect_stats = on;
        self.bytecode.set_opt_options(self.opt_options.clone());
    }

    pub fn intern_constant(&mut self, value: u64) -> u32 {
        let idx = self.constants.len() as u32;
        self.constants.push(value);
        idx
    }

    pub fn intern_string(&mut self, value: impl AsRef<str>) -> u32 {
        let value = value.as_ref();
        if let Some(&idx) = self.string_indices.get(value) {
            return idx;
        }
        let idx = self.strings.len() as u32;
        self.strings.push(value.to_string());
        self.string_indices.insert(value.to_string(), idx);
        idx
    }

    fn push_string_literal(&mut self, bytecode: &mut impl EmitBuf, value: impl AsRef<str>) {
        let idx = self.intern_string(value);
        bytecode.push_string(idx);
    }

    fn const_env(&self) -> &HashMap<String, ConstValue> {
        self.const_env_stack
            .last()
            .expect("const_env_stack initialized in compile_unfused")
    }

    fn const_env_mut(&mut self) -> &mut HashMap<String, ConstValue> {
        self.const_env_stack
            .last_mut()
            .expect("const_env_stack must be non-empty during codegen")
    }

    fn push_const_env(&mut self) {
        let parent = self.const_env().clone();
        self.const_env_stack.push(parent);
    }

    fn pop_const_env(&mut self) {
        self.const_env_stack.pop();
    }

    fn emit_const_value(&mut self, v: &ConstValue, bytecode: &mut CodeBuf) {
        match v {
            ConstValue::Int(n) => {
                if (0..=i32::MAX as i64).contains(n) {
                    bytecode.push_const(*n as i32);
                } else {
                    let bits = Value::from(*n).raw() as u64;
                    let idx = self.intern_constant(bits);
                    bytecode.push_const_pool(idx);
                }
            }
            ConstValue::Float(n) => {
                let bits = Value::from(*n).raw() as u64;
                let idx = self.intern_constant(bits);
                bytecode.push_const_pool(idx);
            }
            ConstValue::Bool(b) => {
                bytecode.push(Byte::new_with_value(
                    Instruction::CONST,
                    Value::from(*b).raw() as _,
                ));
            }
            ConstValue::Str(s) => {
                self.push_string_literal(bytecode, s);
            }
        }
    }

    /// If `ast` folds to a scalar, emit it and return true.
    ///
    /// When `allow_mul_shl` is false, skip `x * 2^n` → `SHL` so trait/`Mul`
    /// dictionary calls (`bound_operator_call`) still dispatch through
    /// `emit_bound_operator_call` for non-primitive `T * 2^n`.
    fn try_emit_folded_expr(
        &mut self,
        ast: &(SimpleSpan, Box<Expression<'_>>),
        bytecode: &mut CodeBuf,
        allow_mul_shl: bool,
    ) -> bool {
        if let Some(v) = crate::const_fold::eval_expr(ast, self.const_env()) {
            self.emit_const_value(&v, bytecode);
            true
        } else if let Some(inner) = crate::const_fold::strength_reduced_inner(ast) {
            let mut inner_bc = self.do_compile(inner);
            bytecode.append(&mut inner_bc);
            true
        } else if let Some(bit) =
            crate::const_fold::strength_reduce_bitops(ast, self.const_env())
        {
            match bit {
                crate::const_fold::StrengthBitop::Identity(inner) => {
                    let mut inner_bc = self.do_compile(inner);
                    bytecode.append(&mut inner_bc);
                    true
                }
                crate::const_fold::StrengthBitop::Const(k) => {
                    self.emit_const_value(&crate::const_fold::ConstValue::Int(k), bytecode);
                    true
                }
            }
        } else if allow_mul_shl
            && let Some((inner, shift)) = crate::const_fold::strength_mul_to_shl(ast, self.const_env())
        {
            // Defense-in-depth: only emit int SHL when the non-const operand
            // is a known integer-like immediate (`int` or `byte` — extend
            // this match if more int-like primitives are added). VM `SHL`
            // uses `as_int`; `float * k` is rejected at typecheck. Unknown
            // types fall through to MUL / dictionary dispatch.
            use crate::typechecking::subst::apply_ty_prune;
            use crate::typechecking::ty::{BYTE, INT};
            let inner_is_int_like = self.codegen_expr_ty(inner).is_some_and(|ty| {
                matches!(
                    apply_ty_prune(self.checker.subst(), &ty),
                    Ty::Con(ref n) if n == INT || n == BYTE
                )
            });
            if !inner_is_int_like {
                return false;
            }
            let mut inner_bc = self.do_compile(inner);
            bytecode.append(&mut inner_bc);
            bytecode.push_const(shift as i32);
            bytecode.push(Byte::new(Instruction::SHL));
            true
        } else if allow_mul_shl
            && let Some((inner, shift)) =
                crate::const_fold::strength_div_to_shr(ast, self.const_env())
        {
            // Signed `/ 2^n` is toward-zero; VM `SHR` is arithmetic `i32 >>`.
            // Equivalent only for non-negative dividends (including `byte`).
            use crate::typechecking::subst::apply_ty_prune;
            use crate::typechecking::ty::{BYTE, INT};
            let inner_ty = self.codegen_expr_ty(inner).map(|ty| {
                apply_ty_prune(self.checker.subst(), &ty)
            });
            let is_byte = matches!(inner_ty, Some(Ty::Con(ref n)) if n == BYTE);
            let is_int = matches!(inner_ty, Some(Ty::Con(ref n)) if n == INT);
            let safe = is_byte
                || (is_int
                    && crate::const_fold::strength_div_dividend_nonneg(inner, self.const_env()));
            if !safe {
                return false;
            }
            let mut inner_bc = self.do_compile(inner);
            bytecode.append(&mut inner_bc);
            bytecode.push_const(shift as i32);
            bytecode.push(Byte::new(Instruction::SHR));
            true
        } else if let Some((base, kind)) = crate::const_fold::strength_pow_int(ast, self.const_env()) {
            use crate::typechecking::subst::apply_ty_prune;
            use crate::typechecking::ty::{BYTE, INT};
            let base_is_int_like = self.codegen_expr_ty(base).is_some_and(|ty| {
                matches!(
                    apply_ty_prune(self.checker.subst(), &ty),
                    Ty::Con(ref n) if n == INT || n == BYTE
                )
            });
            if !base_is_int_like {
                return false;
            }
            match kind {
                crate::const_fold::StrengthPow::ConstOne => {
                    // Still walk `base` for NodeId alignment, then push 1.
                    self.discard_compile(base);
                    bytecode.push_const(1);
                    true
                }
                crate::const_fold::StrengthPow::Square => {
                    // Dup-safe bases only: Identifier or pure (no Call / IO).
                    if !(matches!(
                        unwrap_expr_output(base).1.as_ref(),
                        Expression::Identifier(_)
                    ) || Self::call_arg_is_pure(base))
                    {
                        return false;
                    }
                    let mut base_bc = self.do_compile(base);
                    bytecode.append(&mut base_bc);
                    bytecode.push(Byte::new(Instruction::DUPLICATE));
                    bytecode.push(Byte::new(Instruction::MUL));
                    true
                }
            }
        } else {
            false
        }
    }

    fn discard_compile(&mut self, ast: &(SimpleSpan, Box<Expression<'_>>)) {
        // Walk for NodeId alignment / side tables, but drop any bytes that
        // direct-to-`self.bytecode` emitters (Print/Format/control flow) wrote.
        let bc_len = self.bytecode.len();
        let dbg_len = self.debug_locs.len();
        let _ = self.do_compile(ast);
        self.bytecode.truncate(bc_len);
        self.debug_locs.truncate(dbg_len);
    }

    fn discard_if_branch(&mut self, branch: &Output<'_>) {
        if let Expression::Branch(cond, body) = branch.1.as_ref() {
            if let Some(c) = cond {
                self.discard_compile(c);
            }
            self.discard_compile(body);
        }
    }

    /// Rewrite `if (!c) { A } else { B }` as `if (c) { B } else { A }` so the
    /// condition can fuse into `BinSlot*Jmpf` / `CmpJmpf` without `LogNotJmpf`.
    fn try_invert_not_if_else<'a>(branches: &'a [Output<'a>]) -> Option<Vec<Output<'a>>> {
        if branches.len() != 2 {
            return None;
        }
        let Expression::Branch(Some(cond), then_body) = branches[0].1.as_ref() else {
            return None;
        };
        let Expression::Branch(else_cond, else_body) = branches[1].1.as_ref() else {
            return None;
        };
        if else_cond.is_some() {
            return None;
        }
        let cond = unwrap_expr_output(cond);
        let Expression::LogicalNot(inner) = cond.1.as_ref() else {
            return None;
        };
        let inner = unwrap_expr_output(inner).clone();
        Some(vec![
            (
                branches[0].0,
                Box::new(Expression::Branch(Some(inner), else_body.clone())),
            ),
            (
                branches[1].0,
                Box::new(Expression::Branch(None, then_body.clone())),
            ),
        ])
    }

    /// Constant-folded `if` / `else if` / `else`. Returns true when handled.
    fn try_compile_const_if(&mut self, branches: &[Output<'_>]) -> bool {
        let mut i = 0usize;
        while i < branches.len() {
            let Expression::Branch(cond, body) = branches[i].1.as_ref() else {
                return false;
            };
            match cond {
                Some(c) => match crate::const_fold::eval_expr(c, self.const_env()) {
                    Some(ConstValue::Bool(true)) => {
                        for j in 0..i {
                            self.discard_if_branch(&branches[j]);
                        }
                        self.discard_compile(c);
                        let mut body_bc = self.do_compile(body);
                        self.bytecode.append(&mut body_bc);
                        for j in (i + 1)..branches.len() {
                            self.discard_if_branch(&branches[j]);
                        }
                        return true;
                    }
                    Some(ConstValue::Bool(false)) => {
                        self.discard_compile(c);
                        self.discard_compile(body);
                        i += 1;
                    }
                    _ => return false,
                },
                None => {
                    for j in 0..i {
                        self.discard_if_branch(&branches[j]);
                    }
                    let mut body_bc = self.do_compile(body);
                    self.bytecode.append(&mut body_bc);
                    for j in (i + 1)..branches.len() {
                        self.discard_if_branch(&branches[j]);
                    }
                    return true;
                }
            }
        }
        for b in branches {
            self.discard_if_branch(b);
        }
        true
    }

    /// Whether `body` is a tail self-call eligible for TCO.
    fn expr_is_tail_self_call(&self, expr: &Output<'_>) -> bool {
        if self.fn_defers.is_empty() {
            // defer check only
        } else {
            return false;
        }
        let Some(cur) = self.current_function_table_key.as_ref() else {
            return false;
        };
        let call_expr = match expr.1.as_ref() {
            Expression::Call { .. } => expr,
            Expression::Expr(inner) | Expression::Group(inner) => {
                if matches!(inner.1.as_ref(), Expression::Call { .. }) {
                    inner
                } else {
                    return false;
                }
            }
            _ => return false,
        };
        let Expression::Call { name, .. } = call_expr.1.as_ref() else {
            return false;
        };
        let Expression::Identifier(fname) = name.1.as_ref() else {
            return false;
        };
        let mut call_key = self.resolve_free_fn(fname);
        if let Some((fa, is_rest, id)) = self.sidecar_overload(
            None,
            call_expr.0.start,
            call_expr.0.end,
        )
        {
            let keyed = overload_fn_key(&call_key, fa, is_rest, id);
            if self.functions.contains_key(&keyed) {
                call_key = keyed;
            }
        } else if !self.functions.contains_key(&call_key) {
            if let Some(q) = self.current_function_qualified.as_ref() {
                if call_key == *q || call_key == strip_overload_key(cur) {
                    call_key = cur.clone();
                }
            }
        }
        if &call_key != cur {
            return false;
        }
        let qualified = self.current_function_qualified.as_deref().unwrap_or("");
        !self.coroutine_fns.contains(qualified) && !self.coroutine_fns.contains(&call_key)
    }

    fn return_is_tail_match(&self, expr: &Output<'_>) -> bool {
        let match_expr = match expr.1.as_ref() {
            Expression::Match { .. } => Some(expr),
            Expression::Expr(inner) | Expression::Group(inner) => {
                if matches!(inner.1.as_ref(), Expression::Match { .. }) {
                    Some(inner)
                } else {
                    None
                }
            }
            _ => None,
        };
        let Some(match_expr) = match_expr else {
            return false;
        };
        let Expression::Match { arms, .. } = match_expr.1.as_ref() else {
            return false;
        };
        !arms.is_empty() && arms.iter().all(|arm| self.expr_is_tail_self_call(&arm.body))
    }

    /// Whether an explicit `return Result::Ok/Err(…)` should skip the
    /// result-mode Ok-wrap (COI-113). Nested `Result<Result<…>, …>` still
    /// wraps `return Result::Ok(payload)`.
    fn skip_result_ok_wrap_for_return(&self, expr: &Output<'_>) -> bool {
        let node = unwrap_expr_output(expr);
        let Expression::Construct {
            enum_name,
            variant_name,
            ..
        } = node.1.as_ref()
        else {
            if let Expression::Fragment(items) = node.1.as_ref()
                && items.len() == 1
            {
                return self.skip_result_ok_wrap_for_return(&items[0]);
            }
            return false;
        };
        let is_result =
            *enum_name == common::BUILTIN_RESULT_ENUM || enum_name.ends_with("::Result");
        if !is_result {
            return false;
        }
        match *variant_name {
            "Err" => true,
            "Ok" => !self.compiling_result_ok_is_result,
            _ => false,
        }
    }

    /// `return self(...)` tail-call when eligible.
    fn try_emit_tail_call_expr(&mut self, expr: &Output<'_>, bytecode: &mut CodeBuf) -> bool {
        if !self.fn_defers.is_empty() {
            return false;
        }
        let Some(cur) = self.current_function_table_key.clone() else {
            return false;
        };
        let call_expr = match expr.1.as_ref() {
            Expression::Call { .. } => expr,
            Expression::Expr(inner) | Expression::Group(inner) => {
                if matches!(inner.1.as_ref(), Expression::Call { .. }) {
                    inner
                } else {
                    return false;
                }
            }
            _ => return false,
        };
        let Expression::Call { name, args } = call_expr.1.as_ref() else {
            return false;
        };
        let Expression::Identifier(fname) = name.1.as_ref() else {
            return false;
        };
        let mut call_key = self.resolve_free_fn(fname);
        if let Some((fa, is_rest, id)) = self.sidecar_overload(
            None,
            call_expr.0.start,
            call_expr.0.end,
        )
        {
            let keyed = overload_fn_key(&call_key, fa, is_rest, id);
            if self.functions.contains_key(&keyed) {
                call_key = keyed;
            } else {
                let simple = call_key
                    .rsplit("::")
                    .next()
                    .unwrap_or(&call_key)
                    .to_string();
                let keyed_simple = overload_fn_key(&simple, fa, is_rest, id);
                if self.functions.contains_key(&keyed_simple) {
                    call_key = keyed_simple;
                }
            }
        } else if !self.functions.contains_key(&call_key) {
            if let Some(q) = self.current_function_qualified.as_ref() {
                if call_key == *q || call_key == strip_overload_key(&cur) {
                    call_key = cur.clone();
                }
            }
        }
        if call_key != cur {
            return false;
        }
        let qualified = self.current_function_qualified.as_deref().unwrap_or("");
        if self.coroutine_fns.contains(qualified) || self.coroutine_fns.contains(&call_key) {
            return false;
        }
        let arg_slice = args.as_deref().unwrap_or(&[]);
        let lookup = strip_overload_key(&cur).to_string();
        let arity = self.emit_call_args_with_rest(&lookup, arg_slice, bytecode, false);
        let Some(&target) = self.functions.get(&cur) else {
            return false;
        };
        // Packed abs PC; CodeBuf::push rewrites to IlOp::Entry via entry_at_offset.
        bytecode
            .push(Byte::new(Instruction::TailCall).with_call_packed(arity as u32, target as u32));
        true
    }

    /// Max emitting ops for a compare+branch tiny-inline diamond.
    const TINY_INLINE_DIAMOND_MAX_OPS: usize = 24;
    /// Max emitting ops for a one-level self-unroll peel.
    const SELF_UNROLL_MAX_OPS: usize = 48;

    fn is_tiny_inline_il(ops: &[IlOp]) -> bool {
        if ops.is_empty() || ops.len() > 64 {
            return false;
        }
        if Self::is_tiny_inline_diamond_il(ops) {
            return true;
        }
        if ops.iter().any(|op| op.is_control()) {
            return false;
        }
        // Sole fused return: expand to producer at the call site (no RETURN).
        if ops.len() == 1 {
            match &ops[0] {
                IlOp::LoadReturnSlot { .. }
                | IlOp::ConstReturnImm { .. }
                | IlOp::BinReturn { .. } => return true,
                _ => {
                    if let Some(b) = ops[0].as_plain_byte()
                        && matches!(
                            *b.bytecode(),
                            Instruction::LoadReturnSlot
                                | Instruction::ConstReturnImm
                                | Instruction::BinReturn
                        )
                    {
                        return true;
                    }
                }
            }
        }
        // Pure micro-body: ≤3 compute ops + terminal Return / fused *Return.
        if Self::is_pure_micro_inline_il(ops) {
            return true;
        }
        // Inliner copies opcodes until the first `RETURN` and leaves that
        // value on the stack. Early-return / branched bodies therefore
        // truncate (else-arm dropped). Only allow a single terminal RETURN
        // and no control-flow jumps.
        let return_idxs: Vec<usize> = ops
            .iter()
            .enumerate()
            .filter(|(_, op)| op.is_plain_return())
            .map(|(i, _)| i)
            .collect();
        if return_idxs.len() != 1 || return_idxs[0] != ops.len() - 1 {
            return false;
        }
        !ops.iter().any(|op| Self::inline_forbidden_op(op))
    }

    /// One compare+branch diamond: `if cond { return A; } return B;` (no calls).
    ///
    /// Emitting shape (labels omitted by [`CodeBuf::code_slice_ops`]):
    /// `cond…; JumpIfFalse; then…; Return; else…; Return`.
    fn is_tiny_inline_diamond_il(ops: &[IlOp]) -> bool {
        if ops.is_empty() || ops.len() > Self::TINY_INLINE_DIAMOND_MAX_OPS {
            return false;
        }
        if ops.iter().any(|op| matches!(op, IlOp::Entry { .. })) {
            return false;
        }
        let jump_idxs: Vec<usize> = ops
            .iter()
            .enumerate()
            .filter(|(_, op)| matches!(op, IlOp::Jump { .. }))
            .map(|(i, _)| i)
            .collect();
        if jump_idxs.len() != 1 {
            return false;
        }
        let j = jump_idxs[0];
        let IlOp::Jump {
            kind: IlJumpKind::JumpIfFalse,
            ..
        } = &ops[j]
        else {
            return false;
        };
        if j == 0 || j + 1 >= ops.len() {
            return false;
        }
        // Cond / arms must not contain nested control or forbidden ops.
        if ops[..j].iter().any(|op| {
            op.is_control() || Self::inline_forbidden_op(op) || Self::inline_is_return(op)
        }) {
            return false;
        }
        let Some(then_end) = Self::diamond_arm_end(ops, j + 1) else {
            return false;
        };
        if then_end + 1 >= ops.len() {
            return false;
        }
        let else_start = then_end + 1;
        let Some(else_end) = Self::diamond_arm_end(ops, else_start) else {
            return false;
        };
        if else_end != ops.len() - 1 {
            return false;
        }
        let then_arm = &ops[j + 1..=then_end];
        let else_arm = &ops[else_start..=else_end];
        Self::diamond_arm_ok(then_arm) && Self::diamond_arm_ok(else_arm)
    }

    fn inline_is_return(op: &IlOp) -> bool {
        op.is_plain_return()
            || matches!(
                op,
                IlOp::LoadReturnSlot { .. } | IlOp::ConstReturnImm { .. } | IlOp::BinReturn { .. }
            )
            || matches!(
                op.as_plain_byte(),
                Some(b) if matches!(
                    *b.bytecode(),
                    Instruction::RETURN
                        | Instruction::LoadReturnSlot
                        | Instruction::ConstReturnImm
                        | Instruction::BinReturn
                )
            )
    }

    /// Index of the last op of an arm starting at `start` (inclusive).
    fn diamond_arm_end(ops: &[IlOp], start: usize) -> Option<usize> {
        if start >= ops.len() {
            return None;
        }
        // Sole fused *Return arm.
        if Self::inline_is_fused_return(&ops[start]) {
            return Some(start);
        }
        for i in start..ops.len() {
            if ops[i].is_control() {
                return None;
            }
            if ops[i].is_plain_return() {
                return Some(i);
            }
        }
        None
    }

    fn inline_is_fused_return(op: &IlOp) -> bool {
        matches!(
            op,
            IlOp::LoadReturnSlot { .. } | IlOp::ConstReturnImm { .. } | IlOp::BinReturn { .. }
        ) || matches!(
            op.as_plain_byte(),
            Some(b) if matches!(
                *b.bytecode(),
                Instruction::LoadReturnSlot
                    | Instruction::ConstReturnImm
                    | Instruction::BinReturn
            )
        )
    }

    fn diamond_arm_ok(arm: &[IlOp]) -> bool {
        if arm.is_empty() {
            return false;
        }
        if Self::inline_is_fused_return(&arm[0]) {
            return arm.len() == 1;
        }
        if arm
            .iter()
            .any(|op| op.is_control() || Self::inline_forbidden_op(op))
        {
            return false;
        }
        arm.last().is_some_and(|op| op.is_plain_return())
            && arm[..arm.len() - 1]
                .iter()
                .all(|op| !Self::inline_is_return(op))
    }

    /// Body eligible for one-level self-unroll at a call site to `self_entry`.
    fn is_self_unroll_il(ops: &[IlOp], self_entry: Option<IlLabel>) -> bool {
        if ops.is_empty() || ops.len() > Self::SELF_UNROLL_MAX_OPS {
            return false;
        }
        let Some(self_entry) = self_entry else {
            return false;
        };
        let mut saw_self_call = false;
        for op in ops {
            match op {
                IlOp::Entry {
                    kind: EntryKind::TailCall,
                    ..
                } => {
                    // Tail-call bodies leave dead fallthrough and rely on
                    // post-emit opts for arg order — unsafe to peel pre-opt.
                    return false;
                }
                IlOp::Entry {
                    kind: EntryKind::Call,
                    target,
                    ..
                } => {
                    if *target == self_entry {
                        saw_self_call = true;
                    }
                }
                IlOp::Entry { .. } | IlOp::PrologueJmp { .. } => return false,
                IlOp::HostInvoke { .. } => return false,
                IlOp::Jump {
                    kind: IlJumpKind::JumpIfMatch { .. },
                    ..
                } => return false,
                _ => {
                    if let Some(b) = op.as_plain_byte() {
                        match *b.bytecode() {
                            Instruction::TailCall => return false,
                            Instruction::CALL => {}
                            Instruction::MakeCoro
                            | Instruction::YieldCoro
                            | Instruction::YieldFromCoro
                            | Instruction::HostInvoke
                            | Instruction::FfiInvoke
                            | Instruction::JumpIfMatch => return false,
                            _ => {}
                        }
                    }
                }
            }
        }
        saw_self_call
            && !ops.iter().any(|op| {
                matches!(
                    op,
                    IlOp::Print { .. }
                        | IlOp::GetField { .. }
                        | IlOp::SetField { .. }
                        | IlOp::MakeTuple { .. }
                        | IlOp::MakeArray { .. }
                        | IlOp::MakeEnum { .. }
                )
            })
    }

    /// ≤3 pure producers + terminal Return / fused *Return (Load/Const/Bin/…).
    fn is_pure_micro_inline_il(ops: &[IlOp]) -> bool {
        if ops.is_empty() || ops.len() > 4 {
            return false;
        }
        let last = ops.last().unwrap();
        let terminal_ok = last.is_plain_return()
            || matches!(
                last,
                IlOp::LoadReturnSlot { .. } | IlOp::ConstReturnImm { .. } | IlOp::BinReturn { .. }
            )
            || matches!(
                last.as_plain_byte(),
                Some(b) if matches!(
                    *b.bytecode(),
                    Instruction::LoadReturnSlot
                        | Instruction::ConstReturnImm
                        | Instruction::BinReturn
                        | Instruction::RETURN
                )
            );
        if !terminal_ok {
            return false;
        }
        let compute = &ops[..ops.len() - 1];
        if compute.len() > 3 {
            return false;
        }
        compute.iter().all(|op| {
            matches!(
                op,
                IlOp::Load { .. }
                    | IlOp::Const { .. }
                    | IlOp::ConstPool { .. }
                    | IlOp::String { .. }
                    | IlOp::Dup { .. }
                    | IlOp::Bin { .. }
                    | IlOp::BinSlotImm { .. }
                    | IlOp::BinSlotSlot { .. }
            ) || matches!(
                op.as_plain_byte(),
                Some(b) if matches!(
                    *b.bytecode(),
                    Instruction::LOAD
                        | Instruction::CONST
                        | Instruction::STRING
                        | Instruction::DUPLICATE
                        | Instruction::ADD
                        | Instruction::SUB
                        | Instruction::MUL
                        | Instruction::DIV
                        | Instruction::MOD
                        | Instruction::BinSlotImm
                        | Instruction::BinSlotSlot
                        | Instruction::EQ
                        | Instruction::NEQ
                        | Instruction::LE
                        | Instruction::LEQ
                        | Instruction::GT
                        | Instruction::GEQ
                )
            )
        })
    }

    fn inline_forbidden_op(op: &IlOp) -> bool {
        matches!(
            op,
            IlOp::HostInvoke { .. }
                | IlOp::Print { .. }
                | IlOp::GetField { .. }
                | IlOp::SetField { .. }
                | IlOp::LoadField { .. }
                | IlOp::MakeTuple { .. }
                | IlOp::MakeArray { .. }
                | IlOp::MakeEnum { .. }
        ) || match op.as_plain_byte() {
            None => true,
            Some(b) => matches!(
                *b.bytecode(),
                Instruction::CALL
                    | Instruction::TailCall
                    | Instruction::MakeCoro
                    | Instruction::CallIndirect
                    | Instruction::YieldCoro
                    | Instruction::YieldFromCoro
                    | Instruction::LoadField
                    | Instruction::MakeEnum
                    | Instruction::MakeArray
                    | Instruction::MakeTuple
                    | Instruction::JumpIfMatch
                    | Instruction::Unpack
                    | Instruction::UnpackAt
                    | Instruction::Seek
                    // Frame-slot operands the copy paths do not remap.
                    | Instruction::INC
                    | Instruction::DEC
                    | Instruction::HostInvoke
                    | Instruction::FfiInvoke
                    | Instruction::PRINT
                    | Instruction::GetField
                    | Instruction::SetField
                    | Instruction::JMP
                    | Instruction::JMPF
                    | Instruction::JMPT
                    | Instruction::BinReturn
                    | Instruction::CmpJmpf
                    | Instruction::CmpJmpt
                    | Instruction::BinSlotImmJmpf
                    | Instruction::BinSlotImmJmpt
                    | Instruction::BinSlotSlotJmpf
                    | Instruction::BinSlotSlotJmpt
                    | Instruction::BinSlotSlotConstJmpf
                    | Instruction::BinSlotSlotConstJmpt
                    | Instruction::LogNotJmpf
                    | Instruction::LogNotJmpt
                    | Instruction::LoadReturnSlot
                    | Instruction::ConstReturnImm
            ),
        }
    }

    /// Expand a fused `*Return` byte into the producer left on the caller's stack.
    fn expand_fused_return_for_inline(byte: &Byte, temps: &[u32]) -> Option<Byte> {
        match *byte.bytecode() {
            Instruction::ConstReturnImm => {
                Some(Byte::new(Instruction::CONST).with_const_inline(byte.operand_u32() as i32))
            }
            Instruction::LoadReturnSlot => {
                let slot = byte.operand_u32() as usize;
                let &tmp = temps.get(slot)?;
                Some(Byte::new(Instruction::LOAD).with_load_store_slot(tmp))
            }
            _ => None,
        }
    }

    /// Expand sole `BinReturn` at a call site: reload caller temps then the plain op.
    fn expand_bin_return_for_inline(byte: &Byte, temps: &[u32], out: &mut CodeBuf) -> bool {
        if *byte.bytecode() != Instruction::BinReturn {
            return false;
        }
        let op: Instruction = byte.bin_return_op().into();
        for &tmp in temps {
            out.push_load(tmp);
        }
        out.push(Byte::new(op));
        true
    }

    /// Remap callee-frame slots in fused `BinSlot*` to caller temps.
    ///
    /// Returns `None` if any slot is out of arity or the remapped index exceeds
    /// the `u8` packing used by these opcodes.
    fn remap_bin_slot_for_inline(byte: &Byte, temps: &[u32]) -> Option<Byte> {
        match *byte.bytecode() {
            Instruction::BinSlotImm => {
                let (op, slot, imm) = byte.bin_slot_imm_parts();
                let &tmp = temps.get(slot)?;
                if tmp > u8::MAX as u32 {
                    return None;
                }
                Some(
                    Byte::new(Instruction::BinSlotImm).with_bin_slot_imm(op, tmp as u8, imm as i16),
                )
            }
            Instruction::BinSlotSlot => {
                let (op, a, b) = byte.bin_slot_slot_parts();
                let &ta = temps.get(a)?;
                let &tb = temps.get(b)?;
                if ta > u8::MAX as u32 || tb > u8::MAX as u32 {
                    return None;
                }
                Some(Byte::new(Instruction::BinSlotSlot).with_bin_slot_slot(op, ta as u8, tb as u8))
            }
            _ => None,
        }
    }

    /// Inline a tiny direct call, or emit nothing at all.
    ///
    /// A refusal must leave `bytecode` byte-for-byte as it was: arg prep and
    /// partially copied body ops would otherwise run *and* be followed by the
    /// real `CALL`, and their `STORE`s would write caller slots.
    ///
    /// The attempt keeps writing into the caller's buffer rather than a scratch
    /// one, because the diamond path flushes that buffer into `self.bytecode` to
    /// hold both in program order — handing it an empty scratch would sink the
    /// caller's prefix (e.g. method-receiver staging) *after* the inlined body.
    fn try_emit_inline_direct_call(
        &mut self,
        fqn: &str,
        args: Option<&[Output<'_>]>,
        bytecode: &mut CodeBuf,
    ) -> bool {
        let prefix = Self::emit_attempt_prefix(bytecode);
        if self.try_inline_direct_call_into(fqn, args, bytecode) {
            return true;
        }
        Self::restore_emit_attempt(bytecode, prefix);
        false
    }

    /// Snapshot a speculative emit target so [`Self::restore_emit_attempt`] can
    /// undo a refused attempt. `None` means it was empty (the common case).
    fn emit_attempt_prefix(bytecode: &CodeBuf) -> Option<CodeBuf> {
        if bytecode.is_empty() {
            None
        } else {
            Some(bytecode.clone())
        }
    }

    fn restore_emit_attempt(bytecode: &mut CodeBuf, prefix: Option<CodeBuf>) {
        match prefix {
            Some(p) => *bytecode = p,
            None => bytecode.clear(),
        }
    }

    /// Resolve a function body byte span, including provisional self-bodies.
    ///
    /// While a function is still streaming into `self.bytecode`, the span is
    /// recorded as `(start, start)`. Callers then use `bytecode.len()` as the
    /// live end so self-recursive peels can see the opening predicate.
    /// The third flag is `true` when the body is still incomplete (no "rest"
    /// required for peel matching).
    fn resolve_fn_span(&self, fqn: &str) -> Option<(usize, usize, bool)> {
        let &(start, end) = self.fn_bytecode_spans.get(fqn)?;
        if end > start {
            return Some((start, end, false));
        }
        let cur = self.bytecode.len();
        if cur > start {
            Some((start, cur, true))
        } else {
            None
        }
    }

    fn record_fn_span(&mut self, key: String, start: usize, end: usize) {
        self.fn_defining_module
            .insert(key.clone(), self.namespace.clone());
        self.fn_inline_spans.insert(key.clone(), (start, end));
        self.fn_bytecode_spans.insert(key, (start, end));
    }

    fn resolve_inline_span(&self, fqn: &str) -> Option<(usize, usize, bool)> {
        let &(start, end) = self.fn_inline_spans.get(fqn)?;
        if end > start {
            Some((start, end, false))
        } else {
            self.resolve_fn_span(fqn)
        }
    }

    fn callee_is_cross_module(&self, fqn: &str) -> bool {
        let def = self
            .fn_defining_module
            .get(fqn)
            .or_else(|| self.fn_defining_module.get(strip_overload_key(fqn)));
        match def {
            Some(module) => module != &self.namespace,
            None => false,
        }
    }

    /// Free module functions are importable; inherent methods need `pub`.
    /// Uses [`Checker::can_access_member`] with no impl owner (foreign site).
    fn callee_is_visible_for_inline(&self, lookup: &str) -> bool {
        match self.checker.inherent_method_visibility(lookup) {
            None => true,
            Some(vis) => {
                let owner = lookup.rsplit_once("::").map(|(o, _)| o).unwrap_or("");
                crate::typechecking::Checker::can_access_member(vis, owner, None)
            }
        }
    }

    fn try_inline_direct_call_into(
        &mut self,
        fqn: &str,
        args: Option<&[Output<'_>]>,
        bytecode: &mut CodeBuf,
    ) -> bool {
        // Incomplete self-bodies are not safe to tiny-inline (missing else/rest).
        let Some((start, end, provisional)) = self.resolve_inline_span(fqn) else {
            return false;
        };
        if provisional {
            return false;
        }
        let lookup = strip_overload_key(fqn).to_string();
        if crate::profile::current_function_is_cold(&lookup)
            || crate::profile::current_function_is_cold(fqn)
        {
            return false;
        }
        if self.checker.fn_has_rest(&lookup) {
            return false;
        }
        let ops = self.bytecode.code_slice_ops(start, end);
        let recursive = self.current_function_qualified.as_deref() == Some(fqn)
            || self.current_function_table_key.as_deref() == Some(fqn)
            || self.current_function_qualified.as_deref() == Some(lookup.as_str())
            || self.current_function_table_key.as_deref() == Some(lookup.as_str());
        let cost = super::inline_cost::estimate_inline_cost(&ops);
        let site = super::inline_cost::CallInfo {
            recursive,
            cross_module: self.callee_is_cross_module(fqn),
            visible: self.callee_is_visible_for_inline(&lookup),
            hot: crate::profile::current_function_is_hot(&lookup)
                || crate::profile::current_function_is_hot(fqn),
            ..Default::default()
        };
        let mut cost_opts = self.inline_cost.clone();
        if site.hot {
            // PGO: hot callees get the force-inline (relaxed) budget, not the
            // tighter `hot_call_threshold` used by the cost-policy flag.
            cost_opts.max_inline_cost = cost_opts.max_inline_cost.max(cost_opts.cold_call_threshold);
            cost_opts.hot_call_threshold = cost_opts.max_inline_cost;
        }
        if !super::inline_cost::should_inline_function(cost, &site, &cost_opts) {
            return false;
        }
        if !Self::is_tiny_inline_il(&ops) {
            return false;
        }
        let arg_slice = args.unwrap_or(&[]);
        let mut temps = Vec::new();
        let flat = self.flatten_call_args_for_emit(arg_slice);
        for arg in &flat {
            let value = match arg.1.as_ref() {
                Expression::NamedArg(_, v) => v,
                _ => arg,
            };
            bytecode.append(&mut self.do_compile(value));
            let tmp = self.alloc_temp_slot();
            bytecode.push_store_pop(tmp);
            temps.push(tmp);
        }
        // Compare+branch diamond: emit CFG into `self.bytecode`, stash the
        // result in a temp, and leave a LOAD in `bytecode` so parents that
        // accumulate into a local Vec keep program order.
        //
        // On emit failure, roll back and clear arg prep so peel/call can
        // re-emit cleanly — a partial diamond leaves `JMP end_label` unbound
        // (resolves to PC 0) and poisons later fallbacks.
        if Self::is_tiny_inline_diamond_il(&ops) {
            let raw = self.bytecode.code_slice_raw_ops(start, end);
            let rollback = self.bytecode.len();
            self.bytecode.append(bytecode);
            if !self.emit_cfg_inline_body(&raw, &temps, /*allow_calls=*/ false) {
                self.bytecode.truncate(rollback);
                bytecode.clear();
                return false;
            }
            let result = self.alloc_temp_slot();
            self.bytecode.push_store_pop(result);
            bytecode.push_load(result);
            crate::il::opt::note_function_inlined();
            return true;
        }
        let slice = self.bytecode.code_slice_bytes(start, end);
        if slice.len() == 1
            && let Some(expanded) = Self::expand_fused_return_for_inline(&slice[0], &temps)
        {
            bytecode.push(expanded);
            crate::il::opt::note_function_inlined();
            return true;
        }
        if slice.len() == 1 && Self::expand_bin_return_for_inline(&slice[0], &temps, bytecode) {
            crate::il::opt::note_function_inlined();
            return true;
        }
        for byte in &slice {
            if matches!(byte.bytecode(), Instruction::RETURN) {
                break;
            }
            if matches!(byte.bytecode(), Instruction::LOAD) {
                let Some(slot) = byte.load_store_single_slot() else {
                    return false;
                };
                let Some(&tmp) = temps.get(slot as usize) else {
                    return false;
                };
                bytecode.push_load(tmp);
            } else if matches!(
                byte.bytecode(),
                Instruction::STORE | Instruction::StorePop
            ) {
                // Must be remapped like LOAD: a verbatim copy writes the
                // *callee's* slot number into the caller's frame. Only
                // parameter slots have a caller temp; locals refuse the inline.
                let Some(slot) = byte.load_store_single_slot() else {
                    return false;
                };
                let Some(&tmp) = temps.get(slot as usize) else {
                    return false;
                };
                bytecode.push_store_pop(tmp);
            } else if matches!(
                byte.bytecode(),
                Instruction::BinSlotImm | Instruction::BinSlotSlot
            ) {
                let Some(remapped) = Self::remap_bin_slot_for_inline(byte, &temps) else {
                    return false;
                };
                bytecode.push(remapped);
            } else {
                bytecode.push(*byte);
            }
        }
        crate::il::opt::note_function_inlined();
        true
    }

    /// One-level self-unroll: peel callee body once at a self-`CALL` site.
    /// Nested self-calls remain `CALL`/`Entry`. Emits into `self.bytecode`.
    fn try_emit_self_unroll_call(
        &mut self,
        fqn: &str,
        args: Option<&[Output<'_>]>,
        bytecode: &mut CodeBuf,
    ) -> bool {
        let prefix = Self::emit_attempt_prefix(bytecode);
        if self.try_self_unroll_call_into(fqn, args, bytecode) {
            return true;
        }
        // The inner bail clears `bytecode` after flushing it into `self.bytecode`,
        // which would drop the caller's prefix along with the attempt.
        Self::restore_emit_attempt(bytecode, prefix);
        false
    }

    fn try_self_unroll_call_into(
        &mut self,
        fqn: &str,
        args: Option<&[Output<'_>]>,
        bytecode: &mut CodeBuf,
    ) -> bool {
        // Need a finished body: mid-compile self-unroll would copy a partial CFG.
        let Some((start, end, provisional)) = self.resolve_fn_span(fqn) else {
            return false;
        };
        if provisional {
            return false;
        }
        let lookup = strip_overload_key(fqn).to_string();
        if self.checker.fn_has_rest(&lookup) {
            return false;
        }
        if self.coroutine_fns.contains(fqn) || self.coroutine_fns.contains(&lookup) {
            return false;
        }
        // Skip callees that use defer (body would miss deferred side effects).
        // `fn_defers` is only populated while compiling the callee; once its
        // body is finished the stack is empty — refuse bodies that contain
        // MakeCoro / Yield (already gated) and nested `fn` defs (not in span).
        let ops = self.bytecode.code_slice_ops(start, end);
        let self_entry = self.fn_entry_labels.get(fqn).copied();
        if !Self::is_self_unroll_il(&ops, self_entry) {
            return false;
        }
        // Refuse locals beyond arity (temps only cover args).
        let arity = self.flatten_call_args_for_emit(args.unwrap_or(&[])).len();
        if Self::body_uses_slot_past(&ops, arity) {
            return false;
        }
        let arg_slice = args.unwrap_or(&[]);
        let mut temps = Vec::new();
        let flat = self.flatten_call_args_for_emit(arg_slice);
        for arg in &flat {
            let value = match arg.1.as_ref() {
                Expression::NamedArg(_, v) => v,
                _ => arg,
            };
            bytecode.append(&mut self.do_compile(value));
            let tmp = self.alloc_temp_slot();
            bytecode.push_store_pop(tmp);
            temps.push(tmp);
        }
        let raw = self.bytecode.code_slice_raw_ops(start, end);
        let rollback = self.bytecode.len();
        self.bytecode.append(bytecode);
        if !self.emit_cfg_inline_body(&raw, &temps, /*allow_calls=*/ true) {
            self.bytecode.truncate(rollback);
            bytecode.clear();
            return false;
        }
        let result = self.alloc_temp_slot();
        self.bytecode.push_store_pop(result);
        bytecode.push_load(result);
        true
    }

    /// Caller-side predicate peel (2B): when callee opens with compare+JMPF and an
    /// immediate/slot base return, evaluate that check before `CALL` so base cases
    /// skip the frame. Nested/false path still `CALL`s.
    fn try_emit_predicate_peel_call(
        &mut self,
        fqn: &str,
        args: Option<&[Output<'_>]>,
        bytecode: &mut CodeBuf,
        target_offset: u32,
        is_indirect: bool,
    ) -> bool {
        let prefix = Self::emit_attempt_prefix(bytecode);
        let rollback = self.bytecode.len();
        if self.try_predicate_peel_call_into(fqn, args, bytecode, target_offset, is_indirect) {
            return true;
        }
        // `remap_peel_ops_ok` pre-checks the slot remaps, so the emit-time bails
        // are defensive — but without this they would leave arg prep plus a
        // half-built diamond whose labels never bind.
        self.bytecode.truncate(rollback);
        Self::restore_emit_attempt(bytecode, prefix);
        false
    }

    /// Shared refusals for both peel flavours: rest params, coroutines and
    /// un-monomorphized generics change the callee ABI the peel replicates.
    fn peel_callee_shape_ok(&self, fqn: &str, lookup: &str) -> bool {
        !self.checker.fn_has_rest(lookup)
            && !self.coroutine_fns.contains(fqn)
            && !self.coroutine_fns.contains(lookup)
            && !self.checker.is_generic_fn(lookup)
    }

    fn try_predicate_peel_call_into(
        &mut self,
        fqn: &str,
        args: Option<&[Output<'_>]>,
        bytecode: &mut CodeBuf,
        target_offset: u32,
        is_indirect: bool,
    ) -> bool {
        let Some((start, end, provisional)) = self.resolve_fn_span(fqn) else {
            return false;
        };
        // A self-recursive site reads its own in-progress body, which works, but
        // the peel loses to the frame it avoids: on `tak` it grows the body from
        // 13 to 28 words and re-emits the guard unfused at every non-base call.
        // See `docs/internals/limitations.md` for the measurement.
        if provisional {
            return false;
        }
        let lookup = strip_overload_key(fqn).to_string();
        if !self.peel_callee_shape_ok(fqn, &lookup) {
            return false;
        }
        let ops = self.bytecode.code_slice_raw_ops(start, end);
        let Some(peel) = Self::match_predicate_peel_shape(&ops, !provisional) else {
            return false;
        };
        drop(ops);
        let arg_slice = args.unwrap_or(&[]);
        let flat = self.flatten_call_args_for_emit(arg_slice);
        if flat.len() < peel.arity_hint {
            return false;
        }
        // Pre-check slot remapping against arity (temps will be 1:1 with flat).
        let fake_temps: Vec<u32> = (0..flat.len() as u32).collect();
        if !Self::remap_peel_ops_ok(&peel, &fake_temps) {
            return false;
        }
        // Evaluate args into temps (reuse pure-first when mixed).
        let mut temps = Vec::with_capacity(flat.len());
        if Self::should_reorder_pure_call_args(&flat) {
            let mut slots = vec![0u32; flat.len()];
            for (i, arg) in flat.iter().enumerate() {
                if !Self::call_arg_is_pure(arg) {
                    continue;
                }
                let value = match arg.1.as_ref() {
                    Expression::NamedArg(_, v) => v,
                    _ => arg,
                };
                bytecode.append(&mut self.do_compile(value));
                let tmp = self.alloc_temp_slot();
                bytecode.push_store_pop(tmp);
                slots[i] = tmp;
            }
            for (i, arg) in flat.iter().enumerate() {
                if Self::call_arg_is_pure(arg) {
                    continue;
                }
                let value = match arg.1.as_ref() {
                    Expression::NamedArg(_, v) => v,
                    _ => arg,
                };
                if self.arg_emits_on_self_bytecode(value) {
                    self.stage_call_arg_to_temp(value, false, &mut slots[i]);
                } else {
                    bytecode.append(&mut self.do_compile(value));
                    let tmp = self.alloc_temp_slot();
                    bytecode.push_store_pop(tmp);
                    slots[i] = tmp;
                }
            }
            temps = slots;
        } else {
            for arg in &flat {
                let value = match arg.1.as_ref() {
                    Expression::NamedArg(_, v) => v,
                    _ => arg,
                };
                if self.arg_emits_on_self_bytecode(value) {
                    let mut slot = 0u32;
                    self.stage_call_arg_to_temp(value, false, &mut slot);
                    temps.push(slot);
                } else {
                    bytecode.append(&mut self.do_compile(value));
                    let tmp = self.alloc_temp_slot();
                    bytecode.push_store_pop(tmp);
                    temps.push(tmp);
                }
            }
        }

        // Emit into self.bytecode then leave a LOAD of the result in `bytecode`
        // so parents that accumulate into a local Vec keep program order.
        self.bytecode.append(bytecode);
        let do_call = self.bytecode.fresh_label();
        let join = self.bytecode.fresh_label();

        // Remapped condition; JMPF → do_call (false path continues into CALL).
        for op in &peel.cond {
            if !self.emit_peel_remapped_op(op, &temps) {
                return false;
            }
        }
        self.bytecode.push_op(IlOp::Jump {
            kind: IlJumpKind::JumpIfFalse,
            target: do_call,
            loc: DebugLoc::unknown(),
            hint: Default::default(),
        });
        // Base-case then-arm value.
        if !self.emit_peel_remapped_op(&peel.then_value, &temps) {
            return false;
        }
        self.bytecode.push_op(IlOp::Jump {
            kind: IlJumpKind::Unconditional,
            target: join,
            loc: DebugLoc::unknown(),
            hint: Default::default(),
        });
        self.bytecode.bind_label(do_call);
        for &tmp in &temps {
            self.bytecode.push_load(tmp);
        }
        let arity = temps.len() as u32;
        if is_indirect {
            Self::emit_call_indirect(&mut self.bytecode, target_offset, arity);
        } else {
            self.bytecode
                .push(Byte::new(Instruction::CALL).with_call_packed(arity, target_offset));
        }
        self.bytecode.bind_label(join);
        let result = self.alloc_temp_slot();
        self.bytecode.push_store_pop(result);
        bytecode.push_load(result);
        true
    }

    /// Max guard ops the re-materializing peel will rewrite at a call site.
    const PEEL_REMAT_MAX_COND_OPS: usize = 8;

    /// Caller-side predicate peel that reads leaf arguments in place instead of
    /// spilling them into frame slots.
    ///
    /// [`Self::try_predicate_peel_call_into`] stores every argument to a temp so
    /// the peeled guard and the `CALL` can both read it. An argument that compiles
    /// to a single pure byte needs no such slot: the byte is simply emitted in both
    /// places, which drops one `STORE` and one spill `LOAD` per argument and leaves
    /// the guard reading the caller's own locals. Anything longer keeps its temp,
    /// because the guard copy and the call copy would each pay for it.
    fn try_emit_remat_peel_call(
        &mut self,
        fqn: &str,
        args: Option<&[Output<'_>]>,
        bytecode: &mut CodeBuf,
        target_offset: u32,
    ) -> bool {
        let prefix = Self::emit_attempt_prefix(bytecode);
        let rollback = self.bytecode.len();
        if self.try_remat_peel_call_into(fqn, args, bytecode, target_offset) {
            return true;
        }
        // Every refusal is decided before the first emit; this only guards
        // against a future check slipping in after one.
        self.bytecode.truncate(rollback);
        Self::restore_emit_attempt(bytecode, prefix);
        false
    }

    fn try_remat_peel_call_into(
        &mut self,
        fqn: &str,
        args: Option<&[Output<'_>]>,
        bytecode: &mut CodeBuf,
        target_offset: u32,
    ) -> bool {
        let Some((start, end)) = self.fn_bytecode_spans.get(fqn).copied() else {
            return false;
        };
        let lookup = strip_overload_key(fqn).to_string();
        if !self.peel_callee_shape_ok(fqn, &lookup) {
            return false;
        }
        // Partial application lowers to `MakeFn`, not `CALL`, so only a
        // saturated fixed-arity call site may be peeled.
        let Some(&(fixed_arity, has_rest)) = self.fn_arities.get(fqn) else {
            return false;
        };
        if has_rest {
            return false;
        }
        let ops = self.bytecode.code_slice_raw_ops(start, end);
        let Some(peel) = Self::match_predicate_peel_shape(&ops, true) else {
            return false;
        };
        drop(ops);
        if peel.cond.len() > Self::PEEL_REMAT_MAX_COND_OPS {
            return false;
        }
        let flat = self.flatten_call_args_for_emit(args.unwrap_or(&[]));
        if flat.len() != fixed_arity as usize || flat.len() < peel.arity_hint {
            return false;
        }
        let Some(plan) = Self::peel_remat_plan(&peel, flat.len()) else {
            return false;
        };
        // The guard reads some arguments ahead of the others and the false path
        // evaluates them again, so no argument may carry a side effect.
        if !flat.iter().all(Self::peel_arg_is_pure) {
            return false;
        }
        // Guard-referenced arguments must be re-materializable up front: once an
        // argument has been compiled a refusal can no longer be rolled back.
        if !plan
            .guard_args
            .iter()
            .all(|&i| Self::peel_arg_is_remat_shape(&flat[i]))
        {
            return false;
        }

        // The caller prefix belongs ahead of everything below, so flush it before
        // compiling arguments — a spilled argument must not slip in front of it.
        self.bytecode.append(bytecode);
        let mut argv: Vec<Vec<Byte>> = Vec::with_capacity(flat.len());
        for arg in &flat {
            let value = match arg.1.as_ref() {
                Expression::NamedArg(_, v) => v,
                _ => arg,
            };
            let before = self.bytecode.len();
            let mut bytes = self.do_compile(value);
            let split = self.bytecode.len() != before;
            let plain = Self::codebuf_plain_bytes(&bytes);
            if split || plain.as_ref().is_none_or(|p| !Self::peel_remat_bytes_ok(p)) {
                self.bytecode.append(&mut bytes);
                let tmp = self.alloc_temp_slot();
                self.bytecode.push_store_pop(tmp);
                argv.push(vec![
                    Byte::new(Instruction::LOAD).with_load_store_slot(tmp),
                ]);
            } else {
                argv.push(plain.unwrap());
            }
        }

        let do_call = self.bytecode.fresh_label();
        let join = self.bytecode.fresh_label();
        for op in &plan.cond {
            self.emit_peel_remat_op(op, &argv);
        }
        self.bytecode.push_op(IlOp::Jump {
            kind: IlJumpKind::JumpIfFalse,
            target: do_call,
            loc: DebugLoc::unknown(),
            hint: Default::default(),
        });
        self.emit_peel_remat_op(&plan.then_value, &argv);
        self.bytecode.push_op(IlOp::Jump {
            kind: IlJumpKind::Unconditional,
            target: join,
            loc: DebugLoc::unknown(),
            hint: Default::default(),
        });
        self.bytecode.bind_label(do_call);
        for bytes in &argv {
            for byte in bytes {
                self.bytecode.push(*byte);
            }
        }
        self.bytecode.push(
            Byte::new(Instruction::CALL).with_call_packed(argv.len() as u32, target_offset),
        );
        self.bytecode.bind_label(join);
        let result = self.alloc_temp_slot();
        self.bytecode.push_store_pop(result);
        bytecode.push_load(result);
        true
    }

    /// Rewrite a matched guard against caller arguments, or `None` when any op is
    /// outside the re-materializable set or reads past `argc`.
    fn peel_remat_plan(peel: &PredicatePeel, argc: usize) -> Option<PeelRematPlan> {
        let mut guard_args = Vec::new();
        let mut cond = Vec::with_capacity(peel.cond.len());
        for op in &peel.cond {
            cond.push(Self::peel_remat_op(op, argc, &mut guard_args)?);
        }
        let then_value = Self::peel_remat_op(&peel.then_value, argc, &mut guard_args)?;
        Some(PeelRematPlan {
            cond,
            then_value,
            guard_args,
        })
    }

    fn peel_remat_op(op: &IlOp, argc: usize, args: &mut Vec<usize>) -> Option<PeelRematOp> {
        fn note(idx: usize, argc: usize, args: &mut Vec<usize>) -> Option<usize> {
            if idx >= argc {
                return None;
            }
            if !args.contains(&idx) {
                args.push(idx);
            }
            Some(idx)
        }
        match op {
            IlOp::Load { slot, .. } => Some(PeelRematOp::Arg(note(*slot as usize, argc, args)?)),
            IlOp::BinSlotImm {
                op: bin, slot, imm, ..
            } => Some(PeelRematOp::ArgImm {
                op: Instruction::from(*bin),
                idx: note(*slot as usize, argc, args)?,
                imm: *imm as i32,
            }),
            IlOp::BinSlotSlot { op: bin, a, b, .. } => Some(PeelRematOp::ArgArg {
                op: Instruction::from(*bin),
                a: note(*a as usize, argc, args)?,
                b: note(*b as usize, argc, args)?,
            }),
            IlOp::Const { .. } | IlOp::ConstPool { .. } | IlOp::String { .. } | IlOp::Dup { .. } => {
                Some(PeelRematOp::Copy(op.clone()))
            }
            IlOp::Bin { op: bin, .. } if Self::peel_remat_bin_ok(*bin) => {
                Some(PeelRematOp::Copy(op.clone()))
            }
            other => {
                let byte = other.as_plain_byte()?;
                match *byte.bytecode() {
                    Instruction::LOAD => Some(PeelRematOp::Arg(note(
                        byte.load_store_single_slot()? as usize,
                        argc,
                        args,
                    )?)),
                    Instruction::BinSlotImm => {
                        let (bin, slot, imm) = byte.bin_slot_imm_parts();
                        Some(PeelRematOp::ArgImm {
                            op: Instruction::from(bin),
                            idx: note(slot, argc, args)?,
                            imm: imm as i32,
                        })
                    }
                    Instruction::BinSlotSlot => {
                        let (bin, a, b) = byte.bin_slot_slot_parts();
                        Some(PeelRematOp::ArgArg {
                            op: Instruction::from(bin),
                            a: note(a, argc, args)?,
                            b: note(b, argc, args)?,
                        })
                    }
                    Instruction::CONST | Instruction::DUPLICATE => {
                        Some(PeelRematOp::Copy(other.clone()))
                    }
                    bin if Self::peel_remat_bin_ok(bin) => Some(PeelRematOp::Copy(other.clone())),
                    _ => None,
                }
            }
        }
    }

    /// Integer binary ops the peel may duplicate: total, trap-free on the operand
    /// shapes a guard produces, and unfusable back to a plain opcode.
    fn peel_remat_bin_ok(op: Instruction) -> bool {
        matches!(
            op,
            Instruction::ADD
                | Instruction::SUB
                | Instruction::MUL
                | Instruction::BITAND
                | Instruction::BITOR
                | Instruction::XOR
                | Instruction::EQ
                | Instruction::NEQ
                | Instruction::LE
                | Instruction::LEQ
                | Instruction::GT
                | Instruction::GEQ
                | Instruction::AND
                | Instruction::OR
        )
    }

    fn emit_peel_remat_op(&mut self, op: &PeelRematOp, argv: &[Vec<Byte>]) {
        let push_arg = |buf: &mut CodeBuf, idx: usize| {
            for byte in &argv[idx] {
                buf.push(*byte);
            }
        };
        match op {
            PeelRematOp::Arg(idx) => push_arg(&mut self.bytecode, *idx),
            PeelRematOp::ArgImm { op: bin, idx, imm } => {
                push_arg(&mut self.bytecode, *idx);
                self.bytecode.push_const(*imm);
                self.bytecode.push(Byte::new(*bin));
            }
            PeelRematOp::ArgArg { op: bin, a, b } => {
                push_arg(&mut self.bytecode, *a);
                push_arg(&mut self.bytecode, *b);
                self.bytecode.push(Byte::new(*bin));
            }
            PeelRematOp::Copy(il) => match il.as_plain_byte() {
                Some(byte) => self.bytecode.push(byte),
                None => self.bytecode.push_op(il.clone()),
            },
        }
    }

    /// Compiled argument that re-materializes for free: exactly one byte pushing
    /// one value out of the caller's frame, independent of the operand stack. That
    /// last part rules out `DUPLICATE`, whose two copies would read two tops.
    fn codebuf_plain_bytes(buf: &CodeBuf) -> Option<Vec<Byte>> {
        let mut out = Vec::new();
        for op in buf.ops() {
            if !op.emits_code() {
                continue;
            }
            out.push(op.as_plain_byte()?);
        }
        Some(out)
    }

    fn peel_remat_bytes_ok(bytes: &[Byte]) -> bool {
        let [byte] = bytes else {
            return false;
        };
        match *byte.bytecode() {
            // A packed multi-slot LOAD pushes more than one value.
            Instruction::LOAD => byte.load_store_single_slot().is_some(),
            Instruction::CONST | Instruction::BinSlotImm | Instruction::BinSlotSlot => true,
            _ => false,
        }
    }

    /// Side-effect-free argument expression: safe to evaluate out of order with
    /// its siblings and, when the compiled bytes allow, more than once.
    fn peel_arg_is_pure(expr: &Output<'_>) -> bool {
        match expr.1.as_ref() {
            Expression::NamedArg(_, v)
            | Expression::Group(v)
            | Expression::Expr(v)
            | Expression::Negate(v)
            | Expression::Not(v)
            | Expression::LogicalNot(v)
            | Expression::Positive(v)
            | Expression::Cast(v, _) => Self::peel_arg_is_pure(v),
            Expression::Identifier(_)
            | Expression::Integer(_)
            | Expression::Float(_)
            | Expression::Bool(_) => true,
            Expression::Add(a, b)
            | Expression::Sub(a, b)
            | Expression::Mul(a, b)
            | Expression::Div(a, b)
            | Expression::Mod(a, b)
            | Expression::Shl(a, b)
            | Expression::Shr(a, b)
            | Expression::Xor(a, b)
            | Expression::And(a, b)
            | Expression::BitAnd(a, b)
            | Expression::Or(a, b)
            | Expression::BitOr(a, b)
            | Expression::Eq(a, b)
            | Expression::Neq(a, b)
            | Expression::Leq(a, b)
            | Expression::Geq(a, b)
            | Expression::Le(a, b)
            | Expression::Gt(a, b) => Self::peel_arg_is_pure(a) && Self::peel_arg_is_pure(b),
            _ => false,
        }
    }

    /// Argument that can plausibly compile to one byte: a name or an int/bool
    /// literal. Checked before anything is emitted, since a refusal after an
    /// argument has been compiled can no longer be rolled back.
    fn peel_arg_is_remat_shape(expr: &Output<'_>) -> bool {
        match expr.1.as_ref() {
            Expression::NamedArg(_, v) | Expression::Group(v) | Expression::Expr(v) => {
                Self::peel_arg_is_remat_shape(v)
            }
            Expression::Identifier(_) | Expression::Integer(_) | Expression::Bool(_) => true,
            _ => false,
        }
    }

    /// Opening shape: `cond…; JumpIfFalse; (Const|Load) [; Return]; Label? …`
    /// with an imm/slot base return. `arity_hint` is 1 + max slot referenced.
    ///
    /// When `require_rest` is false (provisional self-body), accept an opening
    /// early-return even if the recursive remainder has not been emitted yet.
    fn match_predicate_peel_shape(ops: &[IlOp], require_rest: bool) -> Option<PredicatePeel> {
        // Skip leading labels.
        let mut i = 0usize;
        while i < ops.len() && matches!(ops[i], IlOp::Label(_)) {
            i += 1;
        }
        if i >= ops.len() {
            return None;
        }
        let jump_idx = (i..ops.len()).find(|&j| {
            matches!(
                ops[j],
                IlOp::Jump {
                    kind: IlJumpKind::JumpIfFalse,
                    ..
                }
            )
        })?;
        if jump_idx == i {
            return None;
        }
        let cond = &ops[i..jump_idx];
        // Cond must be pure producers only (no control / calls / effects).
        if cond.iter().any(|op| {
            op.is_control()
                || matches!(
                    op,
                    IlOp::HostInvoke { .. }
                        | IlOp::Print { .. }
                        | IlOp::Entry { .. }
                        | IlOp::SetField { .. }
                        | IlOp::GetField { .. }
                )
                || matches!(
                    op.as_plain_byte(),
                    Some(b) if matches!(
                        *b.bytecode(),
                        Instruction::CALL
                            | Instruction::TailCall
                            | Instruction::HostInvoke
                            | Instruction::PRINT
                            | Instruction::FfiInvoke
                    )
                )
        }) {
            return None;
        }
        // Pure cond ops: Load/Const/Bin/BinSlot*/Dup/ConstPool.
        if !cond.iter().all(|op| {
            matches!(
                op,
                IlOp::Load { .. }
                    | IlOp::Const { .. }
                    | IlOp::ConstPool { .. }
                    | IlOp::Dup { .. }
                    | IlOp::Bin { .. }
                    | IlOp::BinSlotImm { .. }
                    | IlOp::BinSlotSlot { .. }
            ) || matches!(
                op.as_plain_byte(),
                Some(b) if matches!(
                    *b.bytecode(),
                    Instruction::LOAD
                        | Instruction::CONST
                        | Instruction::DUPLICATE
                        | Instruction::ADD
                        | Instruction::SUB
                        | Instruction::MUL
                        | Instruction::DIV
                        | Instruction::MOD
                        | Instruction::EQ
                        | Instruction::NEQ
                        | Instruction::LE
                        | Instruction::LEQ
                        | Instruction::GT
                        | Instruction::GEQ
                        | Instruction::AND
                        | Instruction::OR
                        | Instruction::BITAND
                        | Instruction::BITOR
                        | Instruction::SHL
                        | Instruction::SHR
                        | Instruction::XOR
                        | Instruction::BinSlotImm
                        | Instruction::BinSlotSlot
                )
            )
        }) {
            return None;
        }
        let then_start = jump_idx + 1;
        if then_start >= ops.len() {
            return None;
        }
        // Then: fused *Return, or Const/Load + Return, or sole Const/Load before label.
        let (then_value, then_end) = if Self::inline_is_fused_return(&ops[then_start]) {
            match &ops[then_start] {
                IlOp::ConstReturnImm { imm, loc } => (
                    IlOp::Const {
                        imm: *imm as i32,
                        loc: *loc,
                    },
                    then_start,
                ),
                IlOp::LoadReturnSlot { slot, loc } => (
                    IlOp::Load {
                        slot: *slot,
                        loc: *loc,
                    },
                    then_start,
                ),
                _ => return None, // BinReturn not an imm/slot base
            }
        } else {
            let v = match &ops[then_start] {
                IlOp::Const { .. } | IlOp::Load { .. } => ops[then_start].clone(),
                other => {
                    if let Some(b) = other.as_plain_byte() {
                        match *b.bytecode() {
                            Instruction::CONST => IlOp::Const {
                                imm: b.operand_u32() as i32,
                                loc: DebugLoc::unknown(),
                            },
                            Instruction::LOAD => {
                                let slot = b.load_store_single_slot()?;
                                IlOp::Load {
                                    slot,
                                    loc: DebugLoc::unknown(),
                                }
                            }
                            _ => return None,
                        }
                    } else {
                        return None;
                    }
                }
            };
            // The value must be returned, not fall through: a peel replaces the
            // callee's `return`, so a bare value would be the wrong result.
            if then_start + 1 >= ops.len() || !ops[then_start + 1].is_plain_return() {
                return None;
            }
            (v, then_start + 1)
        };
        // After then-arm there should be more body (otherwise tiny-inline diamond
        // would have taken it). Require at least one emitting op past the peel
        // unless this is a provisional self-body (rest not emitted yet).
        let after = then_end + 1;
        if require_rest {
            let has_rest = ops[after..].iter().any(|op| !matches!(op, IlOp::Label(_)));
            if !has_rest {
                return None;
            }
        }
        let mut arity_hint = 0usize;
        let bump_slot = |slot: u32, hint: &mut usize| {
            *hint = (*hint).max(slot as usize + 1);
        };
        for op in cond {
            match op {
                IlOp::Load { slot, .. } => bump_slot(*slot, &mut arity_hint),
                IlOp::BinSlotImm { slot, .. } => bump_slot(*slot as u32, &mut arity_hint),
                IlOp::BinSlotSlot { a, b, .. } => {
                    bump_slot(*a as u32, &mut arity_hint);
                    bump_slot(*b as u32, &mut arity_hint);
                }
                _ => {}
            }
        }
        if let IlOp::Load { slot, .. } = &then_value {
            bump_slot(*slot, &mut arity_hint);
        }
        Some(PredicatePeel {
            cond: cond.to_vec(),
            then_value,
            arity_hint,
        })
    }

    fn remap_peel_ops_ok(peel: &PredicatePeel, temps: &[u32]) -> bool {
        let ok_slot = |s: u32| (s as usize) < temps.len();
        for op in &peel.cond {
            match op {
                IlOp::Load { slot, .. } if !ok_slot(*slot) => return false,
                IlOp::BinSlotImm { slot, .. } if !ok_slot(*slot as u32) => return false,
                IlOp::BinSlotSlot { a, b, .. } if !ok_slot(*a as u32) || !ok_slot(*b as u32) => {
                    return false;
                }
                _ => {}
            }
        }
        match &peel.then_value {
            IlOp::Load { slot, .. } => ok_slot(*slot),
            IlOp::Const { .. } => true,
            _ => false,
        }
    }

    fn emit_peel_remapped_op(&mut self, op: &IlOp, temps: &[u32]) -> bool {
        match op {
            IlOp::Load { slot, loc } => {
                let Some(&tmp) = temps.get(*slot as usize) else {
                    return false;
                };
                self.bytecode.push_op(IlOp::Load {
                    slot: tmp,
                    loc: *loc,
                });
                true
            }
            IlOp::Const { imm, loc } => {
                self.bytecode.push_op(IlOp::Const {
                    imm: *imm,
                    loc: *loc,
                });
                true
            }
            IlOp::ConstPool { idx, loc } => {
                self.bytecode.push_op(IlOp::ConstPool {
                    idx: *idx,
                    loc: *loc,
                });
                true
            }
            IlOp::String { idx, loc } => {
                self.bytecode.push_op(IlOp::String {
                    idx: *idx,
                    loc: *loc,
                });
                true
            }
            IlOp::Dup { loc } => {
                self.bytecode.push_op(IlOp::Dup { loc: *loc });
                true
            }
            IlOp::Bin { op: bin, loc } => {
                self.bytecode.push_op(IlOp::Bin {
                    op: *bin,
                    loc: *loc,
                });
                true
            }
            IlOp::BinSlotImm {
                op: bin,
                slot,
                imm,
                loc,
            } => {
                let Some(&tmp) = temps.get(*slot as usize) else {
                    return false;
                };
                if tmp > u8::MAX as u32 {
                    return false;
                }
                self.bytecode.push_op(IlOp::BinSlotImm {
                    op: *bin,
                    slot: tmp as u8,
                    imm: *imm,
                    loc: *loc,
                });
                true
            }
            IlOp::BinSlotSlot { op: bin, a, b, loc } => {
                let Some(&ta) = temps.get(*a as usize) else {
                    return false;
                };
                let Some(&tb) = temps.get(*b as usize) else {
                    return false;
                };
                if ta > u8::MAX as u32 || tb > u8::MAX as u32 {
                    return false;
                }
                self.bytecode.push_op(IlOp::BinSlotSlot {
                    op: *bin,
                    a: ta as u8,
                    b: tb as u8,
                    loc: *loc,
                });
                true
            }
            other => {
                if let Some(b) = other.as_plain_byte() {
                    self.bytecode.push(b);
                    true
                } else {
                    false
                }
            }
        }
    }

    fn body_uses_slot_past(ops: &[IlOp], arity: usize) -> bool {
        for op in ops {
            match op {
                IlOp::Load { slot, .. } | IlOp::StorePop { slot, .. } => {
                    if *slot as usize >= arity {
                        return true;
                    }
                }
                IlOp::LoadReturnSlot { slot, .. } => {
                    if *slot as usize >= arity {
                        return true;
                    }
                }
                IlOp::BinSlotImm { slot, .. } => {
                    if *slot as usize >= arity {
                        return true;
                    }
                }
                IlOp::BinSlotSlot { a, b, .. } => {
                    if *a as usize >= arity || *b as usize >= arity {
                        return true;
                    }
                }
                _ => {
                    if let Some(b) = op.as_plain_byte() {
                        match *b.bytecode() {
                            Instruction::LOAD | Instruction::STORE => {
                                if b.load_store_single_slot()
                                    .is_some_and(|s| s as usize >= arity)
                                {
                                    return true;
                                }
                            }
                            Instruction::LoadReturnSlot => {
                                if b.operand_u32() as usize >= arity {
                                    return true;
                                }
                            }
                            Instruction::BinSlotImm => {
                                if b.bin_slot_imm_parts().1 >= arity {
                                    return true;
                                }
                            }
                            Instruction::BinSlotSlot => {
                                let (_, a, bslot) = b.bin_slot_slot_parts();
                                if a >= arity || bslot >= arity {
                                    return true;
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
        }
        false
    }

    /// Copy a CFG-bearing callee body into `self.bytecode`, remapping slots to
    /// `temps`. Strips `RETURN` / fused `*Return` so the value stays on stack.
    /// When `allow_calls` is set, `Entry`/`CALL` are preserved (self-unroll).
    fn emit_cfg_inline_body(&mut self, ops: &[IlOp], temps: &[u32], allow_calls: bool) -> bool {
        use std::collections::HashMap;
        let mut label_map: HashMap<u32, IlLabel> = HashMap::new();
        let mut ensure_label = |id: u32, bc: &mut CodeBuf| -> IlLabel {
            *label_map.entry(id).or_insert_with(|| bc.fresh_label())
        };
        // Pre-allocate labels referenced by jumps.
        for op in ops {
            if let IlOp::Jump { target, .. } = op {
                let _ = ensure_label(target.0, &mut self.bytecode);
            }
            if let IlOp::Label(l) = op {
                let _ = ensure_label(l.0, &mut self.bytecode);
            }
        }
        let end_label = self.bytecode.fresh_label();
        let mut saw_value = false;
        for op in ops {
            match op {
                IlOp::Label(l) => {
                    let mapped = ensure_label(l.0, &mut self.bytecode);
                    self.bytecode.bind_label(mapped);
                }
                IlOp::JoinLabel(l) => {
                    let mapped = ensure_label(l.0, &mut self.bytecode);
                    self.bytecode.bind_join_label(mapped);
                }
                IlOp::Jump { kind, target, loc, hint } => {
                    let mapped = ensure_label(target.0, &mut self.bytecode);
                    self.bytecode.push_op(IlOp::Jump {
                        kind: *kind,
                        target: mapped,
                        loc: *loc,
                        hint: *hint,
                    });
                }
                IlOp::Entry {
                    kind,
                    arity,
                    target,
                    loc,
                } => {
                    if !allow_calls {
                        return false;
                    }
                    // Peel is an expression context: TailCall would replace the
                    // caller's frame and never yield a value back. Demote to Call.
                    let kind = match kind {
                        EntryKind::TailCall => EntryKind::Call,
                        other => *other,
                    };
                    self.bytecode.push_op(IlOp::Entry {
                        kind,
                        arity: *arity,
                        target: *target,
                        loc: *loc,
                    });
                    saw_value = true;
                }
                IlOp::Return { .. } => {
                    // Arm/function return → jump to join with value on stack.
                    self.bytecode.push_op(IlOp::Jump {
                        kind: IlJumpKind::Unconditional,
                        target: end_label,
                        loc: DebugLoc::unknown(),
                        hint: Default::default(),
                    });
                    saw_value = true;
                }
                IlOp::ConstReturnImm { imm, loc } => {
                    self.bytecode.push_op(IlOp::Const {
                        imm: *imm as i32,
                        loc: *loc,
                    });
                    self.bytecode.push_op(IlOp::Jump {
                        kind: IlJumpKind::Unconditional,
                        target: end_label,
                        loc: DebugLoc::unknown(),
                        hint: Default::default(),
                    });
                    saw_value = true;
                }
                IlOp::LoadReturnSlot { slot, loc } => {
                    let Some(&tmp) = temps.get(*slot as usize) else {
                        return false;
                    };
                    self.bytecode.push_op(IlOp::Load {
                        slot: tmp,
                        loc: *loc,
                    });
                    self.bytecode.push_op(IlOp::Jump {
                        kind: IlJumpKind::Unconditional,
                        target: end_label,
                        loc: DebugLoc::unknown(),
                        hint: Default::default(),
                    });
                    saw_value = true;
                }
                IlOp::BinReturn { op: bin_op, loc } => {
                    for &tmp in temps {
                        self.bytecode.push_op(IlOp::Load {
                            slot: tmp,
                            loc: *loc,
                        });
                    }
                    self.bytecode.push_op(IlOp::Bin {
                        op: *bin_op,
                        loc: *loc,
                    });
                    self.bytecode.push_op(IlOp::Jump {
                        kind: IlJumpKind::Unconditional,
                        target: end_label,
                        loc: DebugLoc::unknown(),
                        hint: Default::default(),
                    });
                    saw_value = true;
                }
                IlOp::Load { slot, loc } => {
                    let Some(&tmp) = temps.get(*slot as usize) else {
                        return false;
                    };
                    self.bytecode.push_op(IlOp::Load {
                        slot: tmp,
                        loc: *loc,
                    });
                }
                IlOp::StorePop { slot, loc } => {
                    let Some(&tmp) = temps.get(*slot as usize) else {
                        return false;
                    };
                    self.bytecode.push_op(IlOp::StorePop {
                        slot: tmp,
                        loc: *loc,
                    });
                }
                IlOp::BinSlotImm {
                    op: bin_op,
                    slot,
                    imm,
                    loc,
                } => {
                    let Some(&tmp) = temps.get(*slot as usize) else {
                        return false;
                    };
                    if tmp > u8::MAX as u32 {
                        return false;
                    }
                    self.bytecode.push_op(IlOp::BinSlotImm {
                        op: *bin_op,
                        slot: tmp as u8,
                        imm: *imm,
                        loc: *loc,
                    });
                }
                IlOp::BinSlotSlot {
                    op: bin_op,
                    a,
                    b,
                    loc,
                } => {
                    let Some(&ta) = temps.get(*a as usize) else {
                        return false;
                    };
                    let Some(&tb) = temps.get(*b as usize) else {
                        return false;
                    };
                    if ta > u8::MAX as u32 || tb > u8::MAX as u32 {
                        return false;
                    }
                    self.bytecode.push_op(IlOp::BinSlotSlot {
                        op: *bin_op,
                        a: ta as u8,
                        b: tb as u8,
                        loc: *loc,
                    });
                }
                other => {
                    // Plain producers / residual bytes — remap LOAD/STORE/BinSlot*.
                    if let Some(b) = other.as_plain_byte() {
                        match *b.bytecode() {
                            Instruction::RETURN => {
                                self.bytecode.push_op(IlOp::Jump {
                                    kind: IlJumpKind::Unconditional,
                                    target: end_label,
                                    loc: DebugLoc::unknown(),
                                    hint: Default::default(),
                                });
                                saw_value = true;
                            }
                            Instruction::ConstReturnImm => {
                                self.bytecode.push_const(b.operand_u32() as i32);
                                self.bytecode.push_op(IlOp::Jump {
                                    kind: IlJumpKind::Unconditional,
                                    target: end_label,
                                    loc: DebugLoc::unknown(),
                                    hint: Default::default(),
                                });
                                saw_value = true;
                            }
                            Instruction::LoadReturnSlot => {
                                let Some(&tmp) = temps.get(b.operand_u32() as usize) else {
                                    return false;
                                };
                                self.bytecode.push_load(tmp);
                                self.bytecode.push_op(IlOp::Jump {
                                    kind: IlJumpKind::Unconditional,
                                    target: end_label,
                                    loc: DebugLoc::unknown(),
                                    hint: Default::default(),
                                });
                                saw_value = true;
                            }
                            Instruction::BinReturn => {
                                let op: Instruction = b.bin_return_op().into();
                                for &tmp in temps {
                                    self.bytecode.push_load(tmp);
                                }
                                self.bytecode.push(Byte::new(op));
                                self.bytecode.push_op(IlOp::Jump {
                                    kind: IlJumpKind::Unconditional,
                                    target: end_label,
                                    loc: DebugLoc::unknown(),
                                    hint: Default::default(),
                                });
                                saw_value = true;
                            }
                            Instruction::LOAD => {
                                let Some(slot) = b.load_store_single_slot() else {
                                    return false;
                                };
                                let Some(&tmp) = temps.get(slot as usize) else {
                                    return false;
                                };
                                self.bytecode.push_load(tmp);
                            }
                            Instruction::STORE => {
                                let Some(slot) = b.load_store_single_slot() else {
                                    return false;
                                };
                                let Some(&tmp) = temps.get(slot as usize) else {
                                    return false;
                                };
                                self.bytecode.push_store_pop(tmp);
                            }
                            Instruction::BinSlotImm | Instruction::BinSlotSlot => {
                                let Some(remapped) = Self::remap_bin_slot_for_inline(&b, temps)
                                else {
                                    return false;
                                };
                                self.bytecode.push(remapped);
                            }
                            Instruction::CALL | Instruction::TailCall => {
                                if !allow_calls {
                                    return false;
                                }
                                // Expression peel: TailCall → CALL so the value returns.
                                let (arity, target) = b.call_parts();
                                self.bytecode.push(
                                    Byte::new(Instruction::CALL)
                                        .with_call_packed(arity as u32, target as u32),
                                );
                                saw_value = true;
                            }
                            _ => {
                                if Self::inline_forbidden_op(other) && !allow_calls {
                                    return false;
                                }
                                self.bytecode.push_op(other.clone());
                            }
                        }
                    } else {
                        self.bytecode.push_op(other.clone());
                    }
                }
            }
        }
        self.bytecode.bind_label(end_label);
        saw_value
    }
}

impl Compiler {
    pub fn get_function(&self, name: &str) -> Option<usize> {
        self.functions.get(name).copied()
    }

    pub fn function_offset(&self, name: &str) -> Option<usize> {
        self.functions.get(name).copied()
    }

    /// Bind a fresh entry label at the current PC and register `name`.
    fn bind_function_entry(&mut self, name: String) -> (usize, IlLabel) {
        let offset = self.bytecode.len();
        let label = if let Some(existing) = self.fn_entry_labels.get(&name).copied() {
            self.bytecode.bind_reserved_entry(existing);
            existing
        } else {
            self.bytecode.bind_fresh_entry()
        };
        self.functions.insert(name.clone(), offset);
        self.fn_entry_labels.insert(name, label);
        (offset, label)
    }

    /// Allocate an unbound entry label so later methods in the same `impl` can
    /// be called before their bodies are emitted.
    fn reserve_function_entry(&mut self, name: String) {
        if self.fn_entry_labels.contains_key(&name) {
            return;
        }
        let label = self.bytecode.fresh_label();
        self.fn_entry_labels.insert(name, label);
    }

    fn impl_method_name<'a>(method: &Output<'a>) -> Option<&'a str> {
        match method.1.as_ref() {
            Expression::Function { name, .. } => Some(*name),
            Expression::Method(_, body) => match body.1.as_ref() {
                Expression::Function { name, .. } => Some(*name),
                _ => None,
            },
            _ => None,
        }
    }

    /// Reserve CALL/CodePtr labels for every callable in this program before
    /// bodies are emitted, so later `impl` methods are never packed as PC 0.
    fn reserve_program_callable_entries(&mut self, children: &[Output]) {
        for child in children {
            match child.1.as_ref() {
                Expression::Function { name, .. } => {
                    let qualified = if self.namespace.is_empty() {
                        name.to_string()
                    } else {
                        format!("{}::{}", self.namespace, name)
                    };
                    self.reserve_function_entry(qualified);
                }
                Expression::Implementation { owner, methods, .. } => {
                    let owner_key = self.resolve_class_ident(owner);
                    for method in methods {
                        if let Some(name) = Self::impl_method_name(method) {
                            self.reserve_function_entry(format!("{}::{}", owner_key, name));
                        }
                    }
                }
                Expression::TypeClassImpl {
                    class,
                    args,
                    methods,
                } => {
                    let arg_tys: Vec<Ty> = args
                        .iter()
                        .map(|arg| self.codegen_instance_head_ty(arg))
                        .collect();
                    let ty_part = arg_tys
                        .iter()
                        .map(|ty| ty.to_string())
                        .collect::<Vec<_>>()
                        .join("_");
                    for method in methods {
                        if let Some(method_name) = Self::impl_method_name(method) {
                            self.reserve_function_entry(format!(
                                "{}__{}__{}",
                                class, ty_part, method_name
                            ));
                        }
                    }
                }
                _ => {}
            }
        }
    }


    /// Entry label for a registered function, if bound.
    #[allow(dead_code)] // call-site Entry emit / step-5 assert helpers
    fn fn_entry_label(&self, name: &str) -> Option<IlLabel> {
        self.fn_entry_labels.get(name).copied()
    }

    /// Bytecode offset WHERE the prologue (CALL+JMP+HALT)
    /// ENDS and user-program code BEGINS. Used by the runtime
    /// pipeline to patch the prologue's JMP operand so that
    /// any module-level `extern` block bytes (appended to
    /// `self.bytecode` before `main`) execute before main.
    /// Without this, the prologue would skip past the extern
    /// block entirely (because `main_offset` lands past it).
    pub fn program_start_offset(&self) -> u32 {
        self.program_start_offset
    }

    /// True iff at least one `extern` block was emitted in
    /// the last `compile`. The pipeline uses this to decide
    /// whether to JMP to `program_start_offset` (which would
    /// execute `extern` block bytes first) or directly to
    /// `main` (which is correct when no extern was used).
    pub fn has_extern_block(&self) -> bool {
        !self.extern_runtime_functions.is_empty()
    }

    /// Record a user-visible local/param for `coil debug` / dissect.
    ///
    /// Skips synthetic `__pad*` / `__dict*` names. `__shadow_name_N` is stored
    /// under the user-facing `name`.
    fn record_debug_local(&mut self, name: &str, slot: u32) {
        if name.starts_with("__pad") || name.starts_with("__dict") {
            return;
        }
        let display = if let Some(rest) = name.strip_prefix("__shadow_") {
            rest.rsplit_once('_').map(|(n, _)| n).unwrap_or(rest)
        } else {
            name
        };
        let Some(key) = self.current_function_table_key.clone() else {
            return;
        };
        self.fn_debug_locals
            .entry(key)
            .or_default()
            .insert(display.to_string(), slot);
    }

    /// Look up the slot for a name used in an arm body. First
    /// checks the nested `match_bindings` map (inner names shadow
    /// outer ones). Falls back to block overlays, then `variables`.
    ///
    /// Returns the slot ID (u32) if the name is found, `None`
    /// otherwise.
    fn lookup_slot(&self, name: &str) -> Option<u32> {
        if let Some(map) = &self.context.match_bindings
            && let Some(&slot) = map.get(name)
        {
            return Some(slot);
        }
        // Innermost block overlay first (walk `prev` for nested blocks).
        let mut ctx = Some(&self.context);
        while let Some(c) = ctx {
            if let Some(map) = &c.block_bindings
                && let Some(&slot) = map.get(name)
            {
                return Some(slot);
            }
            ctx = c.prev.as_deref();
        }
        self.context
            .variables
            .key(&name.to_string())
            .map(|s| s as u32)
    }

    /// Install `inner` on top of any enclosing arm's bindings (inner names
    /// shadow). Returns the previous map so the caller can restore it.
    fn push_match_bindings(
        &mut self,
        inner: HashMap<String, u32>,
    ) -> Option<HashMap<String, u32>> {
        let saved = self.context.match_bindings.take();
        let mut merged = saved.clone().unwrap_or_default();
        merged.extend(inner);
        self.context.match_bindings = Some(merged);
        saved
    }

    /// Allocate a locals slot for a `let` / destructure binder.
    ///
    /// Inside a block (`block_bindings = Some`), re-binding a name that is
    /// already visible in an outer scope gets a **fresh** slot so the outer
    /// value is not overwritten.
    fn alloc_binding_slot(&mut self, name: &str) -> u32 {
        if let Some(map) = &self.context.block_bindings
            && let Some(&slot) = map.get(name)
        {
            self.record_debug_local(name, slot);
            return slot;
        }
        if self.context.block_bindings.is_none() {
            let slot = self.context.variables.intern(name.to_string()) as u32;
            self.record_debug_local(name, slot);
            return slot;
        }
        let shadows_outer = {
            let in_vars = self.context.variables.key(&name.to_string()).is_some();
            let mut in_ancestor = false;
            let mut ctx = self.context.prev.as_deref();
            while let Some(c) = ctx {
                if let Some(map) = &c.block_bindings
                    && map.contains_key(name)
                {
                    in_ancestor = true;
                    break;
                }
                ctx = c.prev.as_deref();
            }
            in_vars || in_ancestor
        };
        if shadows_outer {
            self.temp_counter += 1;
            let synthetic = format!("__shadow_{}_{}", name, self.temp_counter);
            let slot = self.context.variables.intern(synthetic) as u32;
            self.context
                .block_bindings
                .as_mut()
                .expect("block_bindings checked above")
                .insert(name.to_string(), slot);
            self.record_debug_local(name, slot);
            slot
        } else {
            let slot = self.context.variables.intern(name.to_string()) as u32;
            self.record_debug_local(name, slot);
            slot
        }
    }

    /// Static length of a fixed `[T; N]` type, if any.
    fn fixed_array_len(ty: &crate::typechecking::Ty) -> Option<usize> {
        use crate::typechecking::{Ty, ty::ArrayLength};
        match ty {
            Ty::Array {
                length: ArrayLength::Static(n),
                ..
            } => Some(*n),
            Ty::Readonly(inner) => Self::fixed_array_len(inner),
            _ => None,
        }
    }

    fn stack_array_info(&self, name: &str) -> Option<(u32, usize)> {
        self.context.stack_array_locals.get(name).copied()
    }

    /// Whether a fixed `[T; N]` local should use multi-slot stack layout.
    ///
    /// Any `N >= 1` qualifies: each element is one `Value` (immediate scalar or
    /// heap pointer). Nested array *elements* still compile to heap `MakeArray`
    /// and occupy a pointer slot in the outer spine.
    fn stack_array_bind_len(ty: &crate::typechecking::Ty) -> Option<usize> {
        Self::fixed_array_len(ty).filter(|&n| n >= 1)
    }

    /// Reserve `n` consecutive locals for a fixed array binding `name`.
    fn alloc_stack_array_slots(&mut self, name: &str, n: usize) -> u32 {
        let base = self.alloc_binding_slot(name);
        for i in 1..n {
            let pad = format!("__arrpad_{name}_{i}");
            let slot = self.context.variables.intern(pad) as u32;
            debug_assert_eq!(slot, base + i as u32, "stack array slots must be consecutive");
        }
        self.context
            .stack_array_locals
            .insert(name.to_string(), (base, n));
        base
    }

    /// Push elements of a multi-slot local then `MakeArray` (escape to heap).
    fn emit_box_stack_array(&mut self, bytecode: &mut CodeBuf, base: u32, n: usize) {
        for i in 0..n {
            bytecode.push_load(base + i as u32);
        }
        bytecode.push_make_array(n as u32);
    }

    /// Copy heap-array elements at `arr_slot` back into multi-slot locals `base..base+n`.
    ///
    /// Dynamic `StoreIndex` mutates an escaped `MakeArray` temporary; without
    /// writeback, stack-array slots stay stale (e.g. `arr[i % n] += 1` loops).
    fn emit_unbox_stack_array(
        &mut self,
        bytecode: &mut CodeBuf,
        arr_slot: u32,
        base: u32,
        n: usize,
    ) {
        for i in 0..n {
            bytecode.push_load(arr_slot);
            bytecode.push_const(i as i32);
            bytecode.push_index();
            bytecode.push_store_pop(base + i as u32);
        }
    }

    /// Advance `emit_idx` through wrapper nodes and the unwrapped head, without
    /// emitting. Used when a parent is lowered specially (stack-array init)
    /// but children are still `do_compile`'d.
    fn skip_emit_ids_to_unwrapped(&mut self, expr: &Output<'_>) {
        let mut cur = expr;
        loop {
            let _ = self.next_emit_id();
            match cur.1.as_ref() {
                Expression::Expr(inner)
                | Expression::Group(inner)
                | Expression::Statement(inner)
                | Expression::ExprStatement(inner) => cur = inner,
                Expression::Fragment(items) if items.len() == 1 => cur = &items[0],
                _ => break,
            }
        }
    }

    /// Emit a multi-slot stack-array init: one Value per slot, store immediately.
    ///
    /// Forward `emit; STORE` (not reverse bulk store) so values never pile into
    /// the destination slot range. Shared-stack + post-STORE seek would otherwise
    /// re-pop those slots when lower packs adjacent STOREs.
    ///
    /// Returns `true` for array literals and copies from another multi-slot local.
    fn try_emit_stack_array_init(
        &mut self,
        bytecode: &mut CodeBuf,
        rhs: &Output,
        base: u32,
        n: usize,
    ) -> bool {
        let rhs_node = unwrap_expr_output(rhs);
        match rhs_node.1.as_ref() {
            Expression::Array(items) if items.len() == n => {
                // Infer assigned a NodeId to the array (and any wrappers) in
                // pre-order before the items. This path does not `do_compile`
                // the array node (no MakeArray), so skip those ids or later
                // literals in the same fragment steal them (byte_string_lit.hy).
                self.skip_emit_ids_to_unwrapped(rhs);
                for (i, item) in items.iter().enumerate() {
                    let mut bc = self.do_compile(item);
                    bytecode.append(&mut bc);
                    bytecode.push_store_pop(base + i as u32);
                }
                true
            }
            Expression::Identifier(src) => {
                if let Some((src_base, src_n)) = self.stack_array_info(src)
                    && src_n == n
                {
                    self.skip_emit_ids_to_unwrapped(rhs);
                    for i in 0..n {
                        bytecode.push_load(src_base + i as u32);
                        bytecode.push_store_pop(base + i as u32);
                    }
                    true
                } else {
                    false
                }
            }
            _ => false,
        }
    }

    /// Flatten `...expr` spread nodes for codegen using inferred types.
    fn flatten_call_args_for_emit<'a>(&self, args: &[Output<'a>]) -> Vec<Output<'a>> {
        use crate::typechecking::subst::apply_ty_prune;
        use crate::typechecking::ty::{ArrayLength, Ty};
        let mut out = Vec::new();
        for arg in args {
            if let Expression::Spread(inner) = arg.1.as_ref() {
                if let Expression::Array(items) = inner.1.as_ref() {
                    for i in 0..items.len() {
                        let span = inner.0;
                        out.push((
                            span,
                            Box::new(Expression::Index(
                                inner.clone(),
                                Some((span, Box::new(Expression::Integer(i as i64)))),
                            )),
                        ));
                    }
                    continue;
                }
                if let Expression::Tuple(items) = inner.1.as_ref() {
                    for i in 0..items.len() {
                        let span = inner.0;
                        out.push((
                            span,
                            Box::new(Expression::Index(
                                inner.clone(),
                                Some((span, Box::new(Expression::Integer(i as i64)))),
                            )),
                        ));
                    }
                    continue;
                }
                let ty = self.codegen_expr_ty(inner);
                let Some(ty) = ty else {
                    out.push(arg.clone());
                    continue;
                };
                let resolved = apply_ty_prune(self.checker.subst(), &ty);
                match resolved {
                    Ty::Tuple(elems) => {
                        for i in 0..elems.len() {
                            let span = inner.0;
                            out.push((
                                span,
                                Box::new(Expression::Index(
                                    inner.clone(),
                                    Some((span, Box::new(Expression::Integer(i as i64)))),
                                )),
                            ));
                        }
                    }
                    Ty::Array {
                        length: ArrayLength::Static(n),
                        ..
                    } => {
                        for i in 0..n {
                            let span = inner.0;
                            out.push((
                                span,
                                Box::new(Expression::Index(
                                    inner.clone(),
                                    Some((span, Box::new(Expression::Integer(i as i64)))),
                                )),
                            ));
                        }
                    }
                    _ => out.push(arg.clone()),
                }
            } else {
                out.push(arg.clone());
            }
        }
        out
    }

    /// Split call args into fixed formals + rest elements (P4).
    ///
    /// Returns `(fixed, rest_elems, pack_rest)`. When `pack_rest` is true,
    /// codegen must emit `MakeArray` even if `rest_elems` is empty.
    fn split_call_args_for_rest<'a>(
        &self,
        fn_name: &str,
        args: &[Output<'a>],
    ) -> (Vec<Output<'a>>, Vec<Output<'a>>, bool) {
        let args = self.flatten_call_args_for_emit(args);
        let fn_name = strip_overload_key(fn_name);
        let has_named = args
            .iter()
            .any(|a| matches!(a.1.as_ref(), Expression::NamedArg(..)));
        let has_rest = self.checker.fn_has_rest(fn_name);
        if !has_named && !has_rest {
            return (args, Vec::new(), false);
        }
        let Some(param_names) = self.checker.fn_param_names(fn_name) else {
            return (args, Vec::new(), false);
        };
        let fixed_count = if has_rest {
            param_names.len().saturating_sub(1)
        } else {
            param_names.len()
        };
        let rest_name = if has_rest {
            param_names.get(fixed_count).map(|s| s.as_str())
        } else {
            None
        };
        let mut slots: Vec<Option<Output<'a>>> = vec![None; fixed_count];
        let mut rest = Vec::new();
        let mut next_pos = 0usize;
        for arg in &args {
            match arg.1.as_ref() {
                Expression::NamedArg(name, value) => {
                    if rest_name == Some(*name) {
                        rest.push(value.clone());
                        continue;
                    }
                    if let Some(idx) = param_names[..fixed_count].iter().position(|p| p == *name) {
                        slots[idx] = Some(value.clone());
                    }
                }
                _ => {
                    while next_pos < fixed_count && slots[next_pos].is_some() {
                        next_pos += 1;
                    }
                    if next_pos < fixed_count {
                        slots[next_pos] = Some(arg.clone());
                        next_pos += 1;
                    } else if has_rest {
                        rest.push(arg.clone());
                        next_pos += 1;
                    } else {
                        next_pos += 1;
                    }
                }
            }
        }
        let pack_rest = has_rest
            && (has_named
                || next_pos >= fixed_count
                || args.len() >= fixed_count
                || fixed_count == 0);
        let fixed: Vec<_> = slots.into_iter().flatten().collect();
        if pack_rest {
            (fixed, rest, true)
        } else {
            (fixed, Vec::new(), false)
        }
    }

    /// Consume pre-walk IDs for `Spread` nodes (flattened at call sites).
    fn consume_spread_emit_ids(&mut self, args: &[Output<'_>]) {
        for arg in args {
            if matches!(arg.1.as_ref(), Expression::Spread(_)) {
                let _ = self.next_emit_id();
            }
        }
    }

    /// Emit value args for a call, packing rest into `MakeArray` when needed.
    /// Returns the CALL arity (fixed + 1 if rest packed).
    ///
    /// When args mix pure and effectful expressions, evaluates pure args into
    /// temps first, then effectful args, then restores original CALL order via
    /// `LOAD`s (2A pure-arg reorder).
    fn emit_call_args_with_rest(
        &mut self,
        fn_name: &str,
        args: &[Output<'_>],
        bytecode: &mut CodeBuf,
        box_generic: bool,
    ) -> u32 {
        self.consume_spread_emit_ids(args);
        let (fixed, rest, pack_rest) = self.split_call_args_for_rest(fn_name, args);

        if !pack_rest && Self::should_reorder_pure_call_args(&fixed) {
            return self.emit_call_args_pure_first(&fixed, bytecode, box_generic);
        }
        // Two+ HostInvoke/format/match args leave values on the shared
        // operand/local stack; the next self-bytecode emit clobbers the prior
        // result. Stage those onto temps; leave identifiers in the Call vec.
        if !pack_rest
            && fixed
                .iter()
                .filter(|a| self.arg_emits_on_self_bytecode(a))
                .count()
                >= 2
        {
            return self.emit_call_args_stage_self_bc(&fixed, bytecode, box_generic);
        }
        // Binary staging (`at + len(n)`) STORE-seeks past live prior args and
        // buries them under the high-water mark. Stage every arg when any may
        // clobber so CALL pops a clean [a0, a1, …] reload.
        if !pack_rest
            && fixed
                .iter()
                .any(|a| self.expr_may_clobber_operand_stack(a))
        {
            return self.emit_call_args_stage_all(&fixed, bytecode, box_generic);
        }

        for arg in &fixed {
            self.append_with_existential_pack(bytecode, arg);
            if box_generic {
                if let Some(arg_ty) = self.codegen_expr_ty(arg) {
                    self.emit_generic_arg_box(bytecode, &arg_ty);
                }
            }
        }
        if pack_rest {
            for arg in &rest {
                self.append_with_existential_pack(bytecode, arg);
                if box_generic {
                    if let Some(arg_ty) = self.codegen_expr_ty(arg) {
                        self.emit_generic_arg_box(bytecode, &arg_ty);
                    }
                }
            }
            if self.checker.fn_tuple_rest(fn_name) {
                bytecode.push_make_tuple(rest.len() as u32);
            } else {
                bytecode.push_make_array(rest.len() as u32);
            }
            return (fixed.len() + 1) as u32;
        }
        fixed.len() as u32
    }

    /// True when multi-arg calls must evaluate into temps before the CALL.
    ///
    /// Mixed pure/effectful args need reordering (2A pure-first). Two or more
    /// HostInvoke/`format`/`match` args are handled via
    /// [`Self::emit_call_args_stage_self_bc`]. Bare identifiers stay unstaged
    /// (see [`Self::call_arg_is_pure`]).
    fn should_reorder_pure_call_args(args: &[Output<'_>]) -> bool {
        if args.len() < 2 {
            return false;
        }
        if args
            .iter()
            .any(|a| matches!(a.1.as_ref(), Expression::Identifier(_)))
        {
            return false;
        }
        let mut saw_pure = false;
        let mut saw_effect = false;
        for arg in args {
            if Self::call_arg_is_pure(arg) {
                saw_pure = true;
            } else {
                saw_effect = true;
            }
            if saw_pure && saw_effect {
                return true;
            }
        }
        false
    }

    /// True when compiling `expr` writes into [`Self::bytecode`] (HostInvoke,
    /// `string::format`, `match`, …) rather than only returning a local `Vec`.
    fn arg_emits_on_self_bytecode(&self, expr: &Output<'_>) -> bool {
        match expr.1.as_ref() {
            Expression::NamedArg(_, v) | Expression::Group(v) | Expression::Expr(v) => {
                self.arg_emits_on_self_bytecode(v)
            }
            Expression::Match { .. } => true,
            Expression::Call { name, .. } => {
                if let Expression::Identifier(fname) = name.1.as_ref() {
                    if self.string_builtin_for_call(fname).is_some() {
                        return true;
                    }
                    if self.checker.io_fn_in_scope(fname).is_some() {
                        return true;
                    }
                    if self.checker.thread_fn_in_scope(fname).is_some() {
                        return true;
                    }
                    if self.checker.gc_fn_in_scope(fname).is_some() {
                        return true;
                    }
                    if self.checker.host_fn_in_scope(fname).is_some() {
                        return true;
                    }
                    if let Some(kind) = self.checker.prelude_fn_in_scope(fname) {
                        return kind.math_native_name().is_some()
                            || matches!(
                                kind,
                                crate::typechecking::PreludeFn::Ord
                                    | crate::typechecking::PreludeFn::Char
                                    | crate::typechecking::PreludeFn::Assert
                                    | crate::typechecking::PreludeFn::BlockOn
                            );
                    }
                    if self.checker.ffi_fn_in_scope(fname).is_some() {
                        return true;
                    }
                } else if let Expression::QualifiedAccess { owner, member } = name.1.as_ref() {
                    let fqn = format!("{}::{}", owner, member);
                    if self.string_builtin_for_call(&fqn).is_some() {
                        return true;
                    }
                }
                false
            }
            _ => false,
        }
    }

    /// True when compiling `expr` may `STORE` into a high temp that aliases the
    /// live expression-stack cursor (CALL receiver temps, HostInvoke, match).
    /// Used so binary operands stage instead of leaving a value under a call
    /// that is not [`Self::expr_is_stackable_direct_call`].
    fn expr_may_clobber_operand_stack(&self, expr: &Output<'_>) -> bool {
        if self.arg_emits_on_self_bytecode(expr) {
            return true;
        }
        match expr.1.as_ref() {
            Expression::NamedArg(_, v) | Expression::Group(v) | Expression::Expr(v) => {
                self.expr_may_clobber_operand_stack(v)
            }
            Expression::Call { .. } | Expression::Match { .. } => true,
            // `new Class(...)` StorePops the instance then Seek(tmp+1), which
            // clears any live operand left under the constructor (e.g. the
            // receiver of an inlined `Vec::push`).
            Expression::Instantiate(_, _) => true,
            Expression::Construct { fields, .. } => {
                use parser::ast::EnumConstructPayload;
                match fields {
                    EnumConstructPayload::Unit => false,
                    EnumConstructPayload::Tuple(args) => args
                        .iter()
                        .any(|a| self.expr_may_clobber_operand_stack(a)),
                    EnumConstructPayload::Record(parts) => parts
                        .iter()
                        .any(|p| self.expr_may_clobber_operand_stack(&p.value)),
                }
            }
            Expression::Negate(e)
            | Expression::Not(e)
            | Expression::LogicalNot(e)
            | Expression::Positive(e)
            | Expression::Cast(e, _)
            | Expression::Try(e) => self.expr_may_clobber_operand_stack(e),
            Expression::Add(a, b)
            | Expression::Sub(a, b)
            | Expression::Mul(a, b)
            | Expression::Div(a, b)
            | Expression::Mod(a, b)
            | Expression::Pow(a, b)
            | Expression::Shl(a, b)
            | Expression::Shr(a, b)
            | Expression::Xor(a, b)
            | Expression::And(a, b)
            | Expression::BitAnd(a, b)
            | Expression::Or(a, b)
            | Expression::BitOr(a, b)
            | Expression::Eq(a, b)
            | Expression::Neq(a, b)
            | Expression::Le(a, b)
            | Expression::Leq(a, b)
            | Expression::Gt(a, b)
            | Expression::Geq(a, b) => {
                self.expr_may_clobber_operand_stack(a) || self.expr_may_clobber_operand_stack(b)
            }
            Expression::Access(recv, _) | Expression::OptionalAccess(recv, _) => {
                self.expr_may_clobber_operand_stack(recv)
            }
            Expression::Index(recv, idx) => {
                self.expr_may_clobber_operand_stack(recv)
                    || idx
                        .as_ref()
                        .is_some_and(|i| self.expr_may_clobber_operand_stack(i))
            }
            _ => false,
        }
    }

    /// Leaf arg for a stackable call: no nested call/match/host and no STORE
    /// during emit (identifiers, literals, and pure arith/cmp over those).
    fn expr_is_call_arg_stack_leaf(&self, expr: &Output<'_>) -> bool {
        if self.arg_emits_on_self_bytecode(expr) || self.expr_may_clobber_operand_stack(expr) {
            return false;
        }
        match expr.1.as_ref() {
            Expression::NamedArg(_, v) | Expression::Group(v) | Expression::Expr(v) => {
                self.expr_is_call_arg_stack_leaf(v)
            }
            Expression::Identifier(_)
            | Expression::Integer(_)
            | Expression::Float(_)
            | Expression::Bool(_)
            | Expression::String(_) => true,
            Expression::Negate(e)
            | Expression::Not(e)
            | Expression::LogicalNot(e)
            | Expression::Positive(e)
            | Expression::Cast(e, _) => self.expr_is_call_arg_stack_leaf(e),
            Expression::Add(a, b)
            | Expression::Sub(a, b)
            | Expression::Mul(a, b)
            | Expression::Div(a, b)
            | Expression::Mod(a, b)
            | Expression::Pow(a, b)
            | Expression::Shl(a, b)
            | Expression::Shr(a, b)
            | Expression::Xor(a, b)
            | Expression::And(a, b)
            | Expression::BitAnd(a, b)
            | Expression::Or(a, b)
            | Expression::BitOr(a, b)
            | Expression::Eq(a, b)
            | Expression::Neq(a, b)
            | Expression::Le(a, b)
            | Expression::Leq(a, b)
            | Expression::Gt(a, b)
            | Expression::Geq(a, b) => {
                self.expr_is_call_arg_stack_leaf(a) && self.expr_is_call_arg_stack_leaf(b)
            }
            _ => false,
        }
    }

    /// Resolve a direct call's FQN the same way pair/call helpers do.
    fn direct_call_fqn(&self, name: &Output<'_>) -> Option<String> {
        match name.1.as_ref() {
            Expression::Identifier(n) => {
                let resolved = self.resolve_free_fn(n);
                Some(resolved)
            }
            Expression::QualifiedAccess { owner, member } => Some(format!("{owner}::{member}")),
            _ => None,
        }
    }

    /// True when `fqn`'s completed body is eligible for tiny-inline (which
    /// always `STORE`s args into temps and can bury a sibling operand).
    fn callee_is_tiny_inlineable(&self, fqn: &str) -> bool {
        let Some((start, end, provisional)) = self.resolve_fn_span(fqn) else {
            return false;
        };
        if provisional {
            return false;
        }
        let ops = self.bytecode.code_slice_ops(start, end);
        Self::is_tiny_inline_il(&ops)
    }

    /// Pure user `CALL` with leaf args that will emit a real `CALL` (not
    /// tiny-inline / host). Safe to leave a sibling value under the args: the
    /// VM preserves slots below the callee frame, and arg emit does not STORE.
    fn expr_is_stackable_direct_call(&self, expr: &Output<'_>) -> bool {
        let Expression::Call { name, args } = expr.1.as_ref() else {
            return false;
        };
        if self.arg_emits_on_self_bytecode(expr) {
            return false;
        }
        let arg_slice = args.as_deref().unwrap_or(&[]);
        if !arg_slice
            .iter()
            .all(|a| self.expr_is_call_arg_stack_leaf(a))
        {
            return false;
        }
        let Some(fqn) = self.direct_call_fqn(name) else {
            return false;
        };
        if !self.functions.contains_key(&fqn) && !self.functions.contains_key(strip_overload_key(&fqn))
        {
            // Method / unresolved — keep staging.
            return false;
        }
        !self.callee_is_tiny_inlineable(&fqn)
    }

    /// Pure call arg: literals and pure arith/cmp/logic — no Call / HostInvoke /
    /// IO / mutation / control side effects.
    ///
    /// Bare [`Expression::Identifier`] is intentionally **not** pure here: copying
    /// locals through temps on the shared operand/local stack (STORE extends
    /// `tell`) before effectful args corrupted frames in large functions
    /// (`parse_url` → Url field SEGV). Literals still reorder ahead of effects.
    fn call_arg_is_pure(expr: &Output<'_>) -> bool {
        match expr.1.as_ref() {
            Expression::NamedArg(_, v) | Expression::Group(v) | Expression::Expr(v) => {
                Self::call_arg_is_pure(v)
            }
            Expression::Integer(_)
            | Expression::Float(_)
            | Expression::String(_)
            | Expression::Bool(_)
            | Expression::Default(_)
            | Expression::TypeOf(_) => true,
            Expression::Identifier(_) => false,
            Expression::Negate(e)
            | Expression::Not(e)
            | Expression::LogicalNot(e)
            | Expression::Positive(e)
            | Expression::Cast(e, _) => Self::call_arg_is_pure(e),
            Expression::Add(a, b)
            | Expression::Sub(a, b)
            | Expression::Mul(a, b)
            | Expression::Div(a, b)
            | Expression::Mod(a, b)
            | Expression::Pow(a, b)
            | Expression::Shl(a, b)
            | Expression::Shr(a, b)
            | Expression::Xor(a, b)
            | Expression::And(a, b)
            | Expression::BitAnd(a, b)
            | Expression::Or(a, b)
            | Expression::BitOr(a, b)
            | Expression::Eq(a, b)
            | Expression::Neq(a, b)
            | Expression::Leq(a, b)
            | Expression::Geq(a, b)
            | Expression::Le(a, b)
            | Expression::Gt(a, b) => Self::call_arg_is_pure(a) && Self::call_arg_is_pure(b),
            Expression::Array(items) | Expression::Tuple(items) | Expression::List(items) => {
                items.iter().all(Self::call_arg_is_pure)
            }
            _ => false,
        }
    }

    /// Evaluate pure args into temps, then effectful args, then `LOAD` in order.
    fn emit_call_args_pure_first(
        &mut self,
        args: &[Output<'_>],
        bytecode: &mut CodeBuf,
        box_generic: bool,
    ) -> u32 {
        let mut temps = vec![0u32; args.len()];
        for (i, arg) in args.iter().enumerate() {
            if !Self::call_arg_is_pure(arg) {
                continue;
            }
            self.append_with_existential_pack(bytecode, arg);
            if box_generic {
                if let Some(arg_ty) = self.codegen_expr_ty(arg) {
                    self.emit_generic_arg_box(bytecode, &arg_ty);
                }
            }
            let tmp = self.alloc_temp_slot();
            bytecode.push_store_pop(tmp);
            temps[i] = tmp;
        }
        for (i, arg) in args.iter().enumerate() {
            if Self::call_arg_is_pure(arg) {
                continue;
            }
            // HostInvoke/format/match emit onto self.bytecode — StorePop must
            // follow immediately there, not in the Call local vec.
            if self.arg_emits_on_self_bytecode(arg) {
                self.stage_call_arg_to_temp(arg, box_generic, &mut temps[i]);
            } else {
                self.append_with_existential_pack(bytecode, arg);
                if box_generic {
                    if let Some(arg_ty) = self.codegen_expr_ty(arg) {
                        self.emit_generic_arg_box(bytecode, &arg_ty);
                    }
                }
                let tmp = self.alloc_temp_slot();
                bytecode.push_store_pop(tmp);
                temps[i] = tmp;
            }
        }
        for &tmp in &temps {
            bytecode.push_load(tmp);
        }
        args.len() as u32
    }

    /// Stage every arg into a temp, then `LOAD` in order. Used when an arg's
    /// codegen may STORE-seek past live prior args (binary staging, nested CALL).
    fn emit_call_args_stage_all(
        &mut self,
        args: &[Output<'_>],
        bytecode: &mut CodeBuf,
        box_generic: bool,
    ) -> u32 {
        let mut temps = vec![0u32; args.len()];
        for (i, arg) in args.iter().enumerate() {
            if self.arg_emits_on_self_bytecode(arg) {
                self.stage_call_arg_to_temp(arg, box_generic, &mut temps[i]);
            } else {
                self.append_with_existential_pack(bytecode, arg);
                if box_generic {
                    if let Some(arg_ty) = self.codegen_expr_ty(arg) {
                        self.emit_generic_arg_box(bytecode, &arg_ty);
                    }
                }
                let tmp = self.alloc_temp_slot();
                bytecode.push_store_pop(tmp);
                temps[i] = tmp;
            }
        }
        for &tmp in &temps {
            bytecode.push_load(tmp);
        }
        args.len() as u32
    }

    /// Stage HostInvoke/`format`/`match` args on [`Self::bytecode`]; emit other
    /// args (identifiers, etc.) into the Call local vec in original order.
    fn emit_call_args_stage_self_bc(
        &mut self,
        args: &[Output<'_>],
        bytecode: &mut CodeBuf,
        box_generic: bool,
    ) -> u32 {
        let mut temps: Vec<Option<u32>> = vec![None; args.len()];
        for (i, arg) in args.iter().enumerate() {
            if !self.arg_emits_on_self_bytecode(arg) {
                continue;
            }
            let mut slot = 0u32;
            self.stage_call_arg_to_temp(arg, box_generic, &mut slot);
            temps[i] = Some(slot);
        }
        for (i, arg) in args.iter().enumerate() {
            if let Some(tmp) = temps[i] {
                bytecode.push_load(tmp);
            } else {
                self.append_with_existential_pack(bytecode, arg);
                if box_generic {
                    if let Some(arg_ty) = self.codegen_expr_ty(arg) {
                        self.emit_generic_arg_box(bytecode, &arg_ty);
                    }
                }
            }
        }
        args.len() as u32
    }

    /// Compile `arg` onto [`Self::bytecode`] and `StorePop` into a fresh temp.
    fn stage_call_arg_to_temp(&mut self, arg: &Output<'_>, box_generic: bool, tmp_out: &mut u32) {
        let mut staged = CodeBuf::new();
        self.append_with_existential_pack(&mut staged, arg);
        self.bytecode.append(&mut staged);
        if box_generic {
            if let Some(arg_ty) = self.codegen_expr_ty(arg) {
                let mut box_bc = CodeBuf::new();
                self.emit_generic_arg_box(&mut box_bc, &arg_ty);
                self.bytecode.append(&mut box_bc);
            }
        }
        let tmp = self.alloc_temp_slot();
        self.bytecode.push_store_pop(tmp);
        *tmp_out = tmp;
    }

    fn next_emit_id(&mut self) -> Option<crate::typechecking::id::NodeId> {
        let id = self.checker.id_table().ids().get(self.emit_idx).copied();
        if id.is_some() {
            self.emit_idx += 1;
        }
        id
    }

    fn node_id_of(&self, node: &Output<'_>) -> Option<crate::typechecking::id::NodeId> {
        self.checker.id_table().id_of_output(node)
    }

    /// Type from the B2 sidecar (NodeId), falling back to the checker cache.
    fn sidecar_ty(&self, id: crate::typechecking::id::NodeId) -> Option<Ty> {
        self.typed_sidecar
            .ty(id)
            .cloned()
            .or_else(|| self.checker.lookup_at(id))
    }

    fn sidecar_ty_of(&self, node: &Output<'_>) -> Option<Ty> {
        self.typed_sidecar
            .ty_at_span(node.0.start, node.0.end)
            .cloned()
            .or_else(|| self.node_id_of(node).and_then(|id| self.sidecar_ty(id)))
    }

    /// Extern setup is keyed by the declaration's short name (and, after
    /// B3, sometimes the module FQN). Call meaning is FQN via DefId.
    fn lookup_extern_runtime(&self, n: &str) -> Option<(u32, u32)> {
        if let Some(&hit) = self.extern_runtime_functions.get(n) {
            return Some(hit);
        }
        let stripped = strip_overload_key(n);
        if stripped != n
            && let Some(&hit) = self.extern_runtime_functions.get(stripped)
        {
            return Some(hit);
        }
        let simple = stripped.rsplit("::").next().unwrap_or(stripped);
        if simple != n && simple != stripped {
            self.extern_runtime_functions.get(simple).copied()
        } else {
            None
        }
    }

    /// Free-fn FQN from interned [`DefId`], not `Compiler.aliases`.
    fn resolve_free_fn(&self, name: &str) -> String {
        if let Some(def) = self.checker.def_id_of(name) {
            return self.fqn_of_def(def);
        }
        if name.contains("::") {
            return name.to_string();
        }
        if !self.namespace.is_empty() {
            if let Some(def) = self.checker.interned_def(&self.namespace, name) {
                return self.fqn_of_def(def);
            }
            let qualified = format!("{}::{}", self.namespace, name);
            if self.functions.contains_key(&qualified)
                || self.fn_entry_labels.contains_key(&qualified)
            {
                return qualified;
            }
        }
        name.to_string()
    }

    fn fqn_of_def(&self, def: crate::typechecking::DefId) -> String {
        let Some(info) = self.checker.def_interner().info(def) else {
            return String::new();
        };
        match self.checker.def_interner().module_path(info.module) {
            Some(path) if !path.is_empty() => format!("{path}::{}", info.name),
            _ => info.name.clone(),
        }
    }

    fn sidecar_overload(
        &self,
        node: Option<crate::typechecking::id::NodeId>,
        start: usize,
        end: usize,
    ) -> Option<(usize, bool, u32)> {
        if let Some(id) = node {
            if let Some(o) = self.typed_sidecar.overload(id) {
                return Some((o.fixed_arity, o.is_rest, o.candidate_id));
            }
            if let Some(o) = self.checker.selected_overload_at_id(id) {
                return Some(o);
            }
        }
        self.checker.selected_overload_span(start, end)
    }

    fn sidecar_for_in(
        &self,
        node: Option<crate::typechecking::id::NodeId>,
        start: usize,
        end: usize,
    ) -> Option<ForInInfo> {
        if let Some(id) = node {
            if let Some(info) = self.typed_sidecar.for_in(id).cloned() {
                return Some(info);
            }
            if let Some(info) = self.checker.for_in_info_at(id).cloned() {
                return Some(info);
            }
        }
        self.checker.for_in_info_span(start, end).cloned()
    }

    fn sidecar_dicts(
        &self,
        node: Option<crate::typechecking::id::NodeId>,
        start: usize,
        end: usize,
    ) -> Option<&[crate::typechecking::generics::InstanceDef]> {
        if let Some(id) = node {
            if let Some(dicts) = self.typed_sidecar.dicts(id) {
                return Some(dicts);
            }
            if let Some(dicts) = self.checker.call_dicts_at(id) {
                return Some(dicts);
            }
        }
        self.checker.call_dicts_span(start, end)
    }

    fn bound_operator_hint(
        &self,
        node: Option<crate::typechecking::id::NodeId>,
        start: usize,
        end: usize,
    ) -> Option<crate::typechecking::infer::BoundOperatorCall> {
        node.and_then(|id| self.checker.bound_operator_call_at(id))
            .cloned()
            .or_else(|| self.checker.bound_operator_call_span(start, end).cloned())
    }

    fn bound_method_hint(
        &self,
        node: Option<crate::typechecking::id::NodeId>,
        start: usize,
        end: usize,
    ) -> Option<crate::typechecking::infer::BoundMethodCall> {
        node.and_then(|id| self.checker.bound_method_call_at(id))
            .cloned()
            .or_else(|| self.checker.bound_method_call_span(start, end).cloned())
    }

    fn bound_display_hint(
        &self,
        node: Option<crate::typechecking::id::NodeId>,
        start: usize,
        end: usize,
    ) -> Option<crate::typechecking::infer::BoundDisplayCall> {
        node.and_then(|id| self.checker.bound_display_call_at(id))
            .cloned()
            .or_else(|| self.checker.bound_display_call_span(start, end).cloned())
    }

    fn existential_pack_hint(
        &self,
        node: Option<crate::typechecking::id::NodeId>,
        expr: &Output<'_>,
    ) -> Option<crate::typechecking::infer::ExistentialPack> {
        node.and_then(|id| self.checker.existential_pack_at(id))
            .cloned()
            .or_else(|| {
                self.checker
                    .existential_pack_span(expr.0.start, expr.0.end)
                    .cloned()
            })
    }

    fn existential_method_hint(
        &self,
        node: Option<crate::typechecking::id::NodeId>,
        start: usize,
        end: usize,
    ) -> Option<crate::typechecking::infer::ExistentialMethodCall> {
        node.and_then(|id| self.checker.existential_method_call_at(id))
            .cloned()
            .or_else(|| {
                self.checker
                    .existential_method_call_span(start, end)
                    .cloned()
            })
    }

    fn forwarded_dicts_hint(
        &self,
        node: Option<crate::typechecking::id::NodeId>,
        start: usize,
        end: usize,
    ) -> Option<Vec<usize>> {
        node.and_then(|id| self.checker.forwarded_dicts_at(id))
            .map(<[usize]>::to_vec)
            .or_else(|| {
                self.checker
                    .forwarded_dicts_span(start, end)
                    .map(<[usize]>::to_vec)
            })
    }

    fn sidecar_pair_niche(&self, name: &str) -> Option<PairNicheAbi> {
        if let Some((base, cand)) = super::overload_key_parts(name) {
            if let Some(def) = self.checker.interned_overload_def(base, cand) {
                if let Some(abi) = self.typed_sidecar.pair_niche(def) {
                    return Some(abi);
                }
            }
        }
        let bare = super::strip_overload_key(name);
        self.typed_sidecar.pair_niche(self.def_id_for_name(bare)?)
    }

    fn sidecar_ffi_tags(&self, name: &str) -> Option<&[u32]> {
        self.typed_sidecar.ffi_tags(self.def_id_for_name(name)?)
    }

    fn def_id_for_name(&self, name: &str) -> Option<crate::typechecking::DefId> {
        if let Some((module, simple)) = name.rsplit_once("::") {
            self.checker
                .interned_def(module, simple)
                .or_else(|| self.checker.def_id_of(simple))
        } else {
            self.checker
                .def_id_of(name)
                .or_else(|| self.checker.interned_def(&self.namespace, name))
                .or_else(|| self.checker.interned_def("", name))
        }
    }

    fn resolve_call_ffi_tags(
        &mut self,
        callee: Option<&str>,
        span: (usize, usize),
        args: &[&Output],
    ) -> Option<Vec<(u32, u32)>> {
        if let Some(name) = callee
            && let Some(tags) = self.sidecar_ffi_tags(name)
            && tags.len() == args.len()
        {
            return Some(tags.iter().copied().map(|tag| (tag, 0u32)).collect());
        }
        resolve_variadic_ffi_tags(&self.checker, span, args, &mut self.messages)
    }

    /// Identifier type for codegen: mono arm overrides, then span cache, then
    /// scoped checker bindings. Preferring span avoids later functions' `let x`
    /// overwriting earlier `x` entries used by `static_len_of` / arith.
    fn codegen_ident_ty(&self, node: &Output) -> Option<Ty> {
        use crate::typechecking::subst::apply_ty_prune;
        let Expression::Identifier(name) = node.1.as_ref() else {
            return None;
        };
        for frame in self.mono_codegen_var_types.iter().rev() {
            if let Some(ty) = frame.get(*name) {
                return Some(apply_ty_prune(self.checker.subst(), ty));
            }
        }
        if let Some(ty) = self.sidecar_ty_of(node) {
            return Some(ty);
        }
        self.checker
            .codegen_var_type(name)
            .map(|t| apply_ty_prune(self.checker.subst(), t))
    }

    fn discard_statement_value(bytecode: &mut CodeBuf) {
        if matches!(
            bytecode.last_byte().map(|b| *b.bytecode()),
            Some(Instruction::DUPLICATE)
        ) {
            // If it was supposed to add `POP` but prev is `DUP`
            // then remove the DUP as well
            bytecode.pop_last_emitting();
        } else if matches!(
            bytecode.last_byte().map(|b| *b.bytecode()),
            Some(
                Instruction::STORE
                    | Instruction::StorePop
                    | Instruction::StoreStatic
                    | Instruction::POP
            )
        ) {
            // Slot/static stores consume the RHS; a prior POP already discarded.
            // SetField / StoreIndex push the value back — still need POP when
            // they are last (handled by the final branch).
        } else if !matches!(
            bytecode.last_byte().map(|b| *b.bytecode()),
            Some(Instruction::YieldCoro | Instruction::YieldFromCoro)
        ) {
            bytecode.push_pop();
        }
    }


    /// Lower an operator selected by HM inference to the uniform dictionary
    /// calling convention: two boxed values, the hidden trailing dictionary,
    /// then its method entry loaded from the dictionary tuple.
    fn emit_bound_operator_call(
        &mut self,
        bytecode: &mut CodeBuf,
        lhs: &Output,
        rhs: &Output,
        dict_index: usize,
        method_slot: usize,
    ) -> bool {
        let dict_name = format!("__dict{}", dict_index);
        let Some(dict_slot) = self.lookup_slot(&dict_name) else {
            return false;
        };
        bytecode.append(&mut self.do_compile(lhs));
        bytecode.append(&mut self.do_compile(rhs));
        bytecode.push_load(dict_slot);
        bytecode.push_load(dict_slot);
        bytecode.push_const(method_slot as i32);
        bytecode.push_index();
        bytecode.push(Byte::new(Instruction::CallIndirect).with_operand_u32(3));
        true
    }

    /// Direct `CALL` (or structural `ArrayLen`) for a bound method inside a
    /// monomorphized clone, where `__dictN` is not in the frame.
    fn try_emit_ground_bound_method(
        &mut self,
        bytecode: &mut CodeBuf,
        name: &Output,
        args: Option<&Vec<Output>>,
        hint: &crate::typechecking::infer::BoundMethodCall,
    ) -> bool {
        use crate::typechecking::subst::apply_ty_prune;
        let method = match name.1.as_ref() {
            Expression::Identifier(n) => *n,
            Expression::Access(_, m) => *m,
            Expression::QualifiedAccess { member, .. } => *member,
            _ => return false,
        };
        let mut arg_nodes: Vec<&Output> = Vec::new();
        if hint.has_receiver
            && let Expression::Access(recv, _) = name.1.as_ref()
        {
            arg_nodes.push(recv);
        }
        if let Some(items) = args {
            for arg in items {
                arg_nodes.push(arg);
            }
        }
        let mut arg_tys = Vec::with_capacity(arg_nodes.len());
        for node in &arg_nodes {
            let Some(ty) = self.codegen_expr_ty(node) else {
                return false;
            };
            arg_tys.push(apply_ty_prune(self.checker.subst(), &ty));
        }
        if method == "len"
            && arg_tys
                .first()
                .is_some_and(Checker::is_structural_len_ty_for_codegen)
        {
            bytecode.append(&mut self.do_compile(arg_nodes[0]));
            bytecode.push(Byte::new(Instruction::ArrayLen));
            return true;
        }
        let Some(class_def) = self.checker.generics().typeclass(&hint.class) else {
            return false;
        };
        let nparams = class_def.type_params.len().max(1);
        let lookup_n = nparams.min(arg_tys.len());
        if lookup_n == 0 {
            return false;
        }
        let lookup: Vec<Ty> = arg_tys[..lookup_n]
            .iter()
            .map(Self::show_lookup_ty_for_instance)
            .collect();
        let Some(instance) = self
            .checker
            .generics()
            .find_instance_relaxed(&hint.class, &lookup)
            .cloned()
        else {
            return false;
        };
        let Some(fqn) = instance.method_fqns.get(method).cloned() else {
            return false;
        };
        if !self.functions.contains_key(&fqn) && !self.fn_entry_labels.contains_key(&fqn) {
            return false;
        }
        let mut temps = Vec::with_capacity(arg_nodes.len());
        for (node, ty) in arg_nodes.iter().zip(arg_tys.iter()) {
            bytecode.append(&mut self.do_compile(node));
            Self::emit_box_if_needed(bytecode, ty);
            let tmp = self.alloc_temp_slot();
            bytecode.push_store_pop(tmp);
            temps.push(tmp);
        }
        for tmp in &temps {
            bytecode.push_load(*tmp);
        }
        self.emit_direct_fn_call(bytecode, &fqn, temps.len() as u32)
    }

    /// Emit element-wise / broadcast aggregate arithmetic when the typechecker
    /// recorded an [`AggregateArithInfo`] for this node (or we can recover the
    /// shape from mono/codegen var types).
    fn try_emit_aggregate_arith(
        &mut self,
        bytecode: &mut CodeBuf,
        self_id: Option<crate::typechecking::id::NodeId>,
        span_start: usize,
        span_end: usize,
        lhs: &Output,
        rhs: Option<&Output>,
        fallback_op: crate::typechecking::AggregateOp,
    ) -> bool {
        use crate::typechecking::{AggregateArithKind, AggregateOp, ScalarSide};

        let info = self_id
            .and_then(|id| self.checker.aggregate_arith_at(id))
            .cloned()
            .or_else(|| self.checker.aggregate_arith_span(span_start, span_end).cloned())
            .or_else(|| self.recover_aggregate_arith(lhs, rhs, fallback_op));
        let Some(info) = info else {
            return false;
        };

        if self.try_emit_packed_aggregate_arith(bytecode, &info, lhs, rhs) {
            return true;
        }

        let scalar_instr = |op: AggregateOp, is_float: bool| -> Instruction {
            match (op, is_float) {
                (AggregateOp::Add, false) => Instruction::ADD,
                (AggregateOp::Add, true) => Instruction::ADDF,
                (AggregateOp::Sub, false) => Instruction::SUB,
                (AggregateOp::Sub, true) => Instruction::SUBF,
                (AggregateOp::Mul, false) => Instruction::MUL,
                (AggregateOp::Mul, true) => Instruction::MULF,
                (AggregateOp::Div, false) => Instruction::DIV,
                (AggregateOp::Div, true) => Instruction::DIVF,
                (AggregateOp::Mod, false) => Instruction::MOD,
                (AggregateOp::Mod, true) => Instruction::MODF,
                (AggregateOp::Pow, false) => Instruction::Pow,
                (AggregateOp::Pow, true) => Instruction::PowF,
                // Neg is handled by `emit_neg_tos` (float uses NEGF).
                (AggregateOp::Neg, _) => Instruction::NEG,
            }
        };

        match info.kind {
            AggregateArithKind::NegTuple {
                arity,
                elem_is_float,
            } => {
                let t0 = self.alloc_temp_slot();
                bytecode.append(&mut self.do_compile(lhs));
                bytecode.push_store_pop(t0);
                self.emit_zip_loop(
                    bytecode,
                    arity,
                    |c, bc, i| {
                        bc.push_load(t0);
                        bc.push_const(i as i32);
                        bc.push_index();
                        c.emit_neg_tos(bc, elem_is_float);
                    },
                    true,
                );
                true
            }
            AggregateArithKind::NegArray {
                length,
                elem_is_float,
            } => {
                let t0 = self.alloc_temp_slot();
                bytecode.append(&mut self.do_compile(lhs));
                bytecode.push_store_pop(t0);
                match length {
                    Some(n) => {
                        self.emit_zip_loop(
                            bytecode,
                            n,
                            |c, bc, i| {
                                bc.push_load(t0);
                                bc.push_const(i as i32);
                                bc.push_index();
                                c.emit_neg_tos(bc, elem_is_float);
                            },
                            false,
                        );
                    }
                    None => {
                        // Flush setup into CodeBuf so loop labels join the main IL.
                        self.bytecode.append(bytecode);
                        self.emit_dynamic_unary_array(t0, elem_is_float);
                    }
                }
                true
            }
            AggregateArithKind::ZipTuple {
                arity,
                elem_is_float,
            } => {
                let Some(rhs) = rhs else {
                    return false;
                };
                let t0 = self.alloc_temp_slot();
                let t1 = self.alloc_temp_slot();
                bytecode.append(&mut self.do_compile(lhs));
                bytecode.push_store_pop(t0);
                bytecode.append(&mut self.do_compile(rhs));
                bytecode.push_store_pop(t1);
                let op = info.op;
                self.emit_zip_loop(
                    bytecode,
                    arity,
                    |_c, bc, i| {
                        bc.push_load(t0);
                        bc.push_const(i as i32);
                        bc.push_index();
                        bc.push_load(t1);
                        bc.push_const(i as i32);
                        bc.push_index();
                        bc.push(Byte::new(scalar_instr(op, elem_is_float)));
                    },
                    true,
                );
                true
            }
            AggregateArithKind::ZipArray {
                length,
                elem_is_float,
            } => {
                let Some(rhs) = rhs else {
                    return false;
                };
                let t0 = self.alloc_temp_slot();
                let t1 = self.alloc_temp_slot();
                bytecode.append(&mut self.do_compile(lhs));
                bytecode.push_store_pop(t0);
                bytecode.append(&mut self.do_compile(rhs));
                bytecode.push_store_pop(t1);
                let op = info.op;
                self.emit_zip_loop(
                    bytecode,
                    length,
                    |_c, bc, i| {
                        bc.push_load(t0);
                        bc.push_const(i as i32);
                        bc.push_index();
                        bc.push_load(t1);
                        bc.push_const(i as i32);
                        bc.push_index();
                        bc.push(Byte::new(scalar_instr(op, elem_is_float)));
                    },
                    false,
                );
                true
            }
            AggregateArithKind::BroadcastTuple {
                arity,
                scalar_on,
                elem_is_float,
            } => {
                let Some(rhs) = rhs else {
                    return false;
                };
                let t_vec = self.alloc_temp_slot();
                let t_sc = self.alloc_temp_slot();
                match scalar_on {
                    ScalarSide::Right => {
                        bytecode.append(&mut self.do_compile(lhs));
                        bytecode.push_store_pop(t_vec);
                        bytecode.append(&mut self.do_compile(rhs));
                        bytecode.push_store_pop(t_sc);
                    }
                    ScalarSide::Left => {
                        bytecode.append(&mut self.do_compile(lhs));
                        bytecode.push_store_pop(t_sc);
                        bytecode.append(&mut self.do_compile(rhs));
                        bytecode.push_store_pop(t_vec);
                    }
                }
                let op = info.op;
                self.emit_zip_loop(
                    bytecode,
                    arity,
                    |_c, bc, i| {
                        match scalar_on {
                            ScalarSide::Right => {
                                bc.push_load(t_vec);
                                bc.push_const(i as i32);
                                bc.push_index();
                                bc.push_load(t_sc);
                            }
                            ScalarSide::Left => {
                                bc.push_load(t_sc);
                                bc.push_load(t_vec);
                                bc.push_const(i as i32);
                                bc.push_index();
                            }
                        }
                        bc.push(Byte::new(scalar_instr(op, elem_is_float)));
                    },
                    true,
                );
                true
            }
            AggregateArithKind::BroadcastArray {
                length,
                scalar_on,
                elem_is_float,
            } => {
                let Some(rhs) = rhs else {
                    return false;
                };
                let t_vec = self.alloc_temp_slot();
                let t_sc = self.alloc_temp_slot();
                match scalar_on {
                    ScalarSide::Right => {
                        bytecode.append(&mut self.do_compile(lhs));
                        bytecode.push_store_pop(t_vec);
                        bytecode.append(&mut self.do_compile(rhs));
                        bytecode.push_store_pop(t_sc);
                    }
                    ScalarSide::Left => {
                        bytecode.append(&mut self.do_compile(lhs));
                        bytecode.push_store_pop(t_sc);
                        bytecode.append(&mut self.do_compile(rhs));
                        bytecode.push_store_pop(t_vec);
                    }
                }
                let op = info.op;
                match length {
                    Some(n) => {
                        self.emit_zip_loop(
                            bytecode,
                            n,
                            |_c, bc, i| {
                                match scalar_on {
                                    ScalarSide::Right => {
                                        bc.push_load(t_vec);
                                        bc.push_const(i as i32);
                                        bc.push_index();
                                        bc.push_load(t_sc);
                                    }
                                    ScalarSide::Left => {
                                        bc.push_load(t_sc);
                                        bc.push_load(t_vec);
                                        bc.push_const(i as i32);
                                        bc.push_index();
                                    }
                                }
                                bc.push(Byte::new(scalar_instr(op, elem_is_float)));
                            },
                            false,
                        );
                    }
                    None => {
                        // Flush setup into CodeBuf so loop labels join the main IL.
                        self.bytecode.append(bytecode);
                        self.emit_dynamic_broadcast_array(
                            t_vec,
                            t_sc,
                            scalar_on,
                            op,
                            elem_is_float,
                        );
                    }
                }
                true
            }
        }
    }

    /// HostInvoke packed path for 1-D aggregate zip / broadcast / neg.
    ///
    /// Used when static length ≥ 8 so SIMD kernels amortize HostInvoke cost;
    /// smaller shapes keep the existing scalar unroll.
    fn try_emit_packed_aggregate_arith(
        &mut self,
        bytecode: &mut CodeBuf,
        info: &crate::typechecking::AggregateArithInfo,
        lhs: &Output,
        rhs: Option<&Output>,
    ) -> bool {
        use crate::typechecking::{AggregateArithKind, AggregateOp, ScalarSide};

        const MIN_PACKED: usize = 8;

        let op_code: u32 = match info.op {
            AggregateOp::Add => 0,
            AggregateOp::Sub => 1,
            AggregateOp::Mul => 2,
            AggregateOp::Div => 3,
            AggregateOp::Neg => 4,
            AggregateOp::Mod | AggregateOp::Pow => return false,
        };

        let (len, is_tuple, elem_is_float, broadcast, scalar_left) = match &info.kind {
            AggregateArithKind::ZipTuple {
                arity,
                elem_is_float,
            } => (*arity, true, *elem_is_float, false, false),
            AggregateArithKind::ZipArray {
                length,
                elem_is_float,
            } => (*length, false, *elem_is_float, false, false),
            AggregateArithKind::BroadcastTuple {
                arity,
                scalar_on,
                elem_is_float,
            } => (
                *arity,
                true,
                *elem_is_float,
                true,
                matches!(scalar_on, ScalarSide::Left),
            ),
            AggregateArithKind::BroadcastArray {
                length: Some(n),
                scalar_on,
                elem_is_float,
            } => (
                *n,
                false,
                *elem_is_float,
                true,
                matches!(scalar_on, ScalarSide::Left),
            ),
            AggregateArithKind::NegTuple {
                arity,
                elem_is_float,
            } => (*arity, true, *elem_is_float, false, false),
            AggregateArithKind::NegArray {
                length: Some(n),
                elem_is_float,
            } => (*n, false, *elem_is_float, false, false),
            AggregateArithKind::BroadcastArray { length: None, .. }
            | AggregateArithKind::NegArray { length: None, .. } => return false,
        };

        if len < MIN_PACKED || len > u16::MAX as usize {
            return false;
        }

        let is_neg = matches!(info.op, AggregateOp::Neg);
        if !is_neg && rhs.is_none() {
            return false;
        }

        let Some(native_id) = self.native_id(common::PACKED_VEC_ARITH) else {
            return false;
        };

        let mut meta = (len as u32) & 0xFFFF;
        meta |= op_code << 16;
        if elem_is_float {
            meta |= 1 << 24;
        }
        if is_tuple {
            meta |= 1 << 25;
        }
        if broadcast {
            meta |= 1 << 26;
        }
        if scalar_left {
            meta |= 1 << 27;
        }

        let depth_on_entry = self.expr_depth;
        bytecode.push(Byte::new(Instruction::CONST).with_operand_u32(native_id as u32));
        self.expr_depth = depth_on_entry + 1;
        bytecode.append(&mut self.do_compile(lhs));
        self.expr_depth += 1;
        let arity = if is_neg {
            2 // vec + meta
        } else {
            bytecode.append(&mut self.do_compile(rhs.unwrap()));
            self.expr_depth += 1;
            3 // lhs + rhs + meta
        };
        bytecode.push(Byte::new(Instruction::CONST).with_operand_u32(meta));
        self.expr_depth += 1;
        bytecode.push_make_tuple(arity as u32);
        bytecode.push_host_invoke(arity as u32);
        self.expr_depth = depth_on_entry;
        true
    }

    /// Negate TOS: int via `NEG`; float via `NEGF`.
    fn emit_neg_tos(&mut self, bytecode: &mut CodeBuf, is_float: bool) {
        if is_float {
            bytecode.push(Byte::new(Instruction::NEGF));
        } else {
            bytecode.push(Byte::new(Instruction::NEG));
        }
    }

    /// Always unrolls static arities at compile time in v1 (including N > 4).
    fn emit_zip_loop<F>(
        &mut self,
        bytecode: &mut CodeBuf,
        n: usize,
        mut emit_elem: F,
        as_tuple: bool,
    ) where
        F: FnMut(&mut Self, &mut CodeBuf, usize),
    {
        for i in 0..n {
            emit_elem(self, bytecode, i);
        }
        if as_tuple {
            bytecode.push_make_tuple(n as u32);
        } else {
            bytecode.push_make_array(n as u32);
        }
    }

    fn emit_dynamic_unary_array(&mut self, src: u32, elem_is_float: bool) {
        let len_slot = self.alloc_temp_slot();
        let idx = self.alloc_temp_slot();
        let out = self.alloc_temp_slot();
        self.bytecode.push_load(src);
        self.bytecode.push(Byte::new(Instruction::ArrayLen));
        self.bytecode.push_store_pop(len_slot);
        self.bytecode.push_make_array(0);
        self.bytecode.push_store_pop(out);
        self.bytecode.push_const(0);
        self.bytecode.push_store_pop(idx);

        let mut bb = BlockBuilder::new();
        let loop_top = bb.fresh_label(self.bytecode.il_mut());
        let end = bb.fresh_label(self.bytecode.il_mut());
        bb.bind_label(loop_top, self.bytecode.il_mut());

        self.bytecode.push_load(idx);
        self.bytecode.push_load(len_slot);
        self.bytecode.push(Byte::new(Instruction::LE));
        bb.emit_jump_to(end, BbJumpKind::JumpIfFalse, self.bytecode.il_mut());

        self.bytecode.push_load(out);
        self.bytecode.push_load(src);
        self.bytecode.push_load(idx);
        self.bytecode.push_index();
        {
            let mut neg_bc = CodeBuf::new();
            self.emit_neg_tos(&mut neg_bc, elem_is_float);
            self.bytecode.append(&mut neg_bc);
        }
        self.bytecode.push(Byte::new(Instruction::ArrayPush));
        self.bytecode.push_store_pop(out);
        self.bytecode.push_load(idx);
        self.bytecode.push_const(1);
        self.bytecode.push(Byte::new(Instruction::ADD));
        self.bytecode.push_store_pop(idx);

        bb.emit_jump_to(loop_top, BbJumpKind::Unconditional, self.bytecode.il_mut());
        bb.bind_label(end, self.bytecode.il_mut());
        self.bytecode.push_load(out);
    }

    fn emit_dynamic_broadcast_array(
        &mut self,
        t_vec: u32,
        t_sc: u32,
        scalar_on: crate::typechecking::ScalarSide,
        op: crate::typechecking::AggregateOp,
        elem_is_float: bool,
    ) {
        use crate::typechecking::{AggregateOp, ScalarSide};
        let scalar_instr = match (op, elem_is_float) {
            (AggregateOp::Add, false) => Instruction::ADD,
            (AggregateOp::Add, true) => Instruction::ADDF,
            (AggregateOp::Sub, false) => Instruction::SUB,
            (AggregateOp::Sub, true) => Instruction::SUBF,
            (AggregateOp::Mul, false) => Instruction::MUL,
            (AggregateOp::Mul, true) => Instruction::MULF,
            (AggregateOp::Div, false) => Instruction::DIV,
            (AggregateOp::Div, true) => Instruction::DIVF,
            (AggregateOp::Mod, false) => Instruction::MOD,
            (AggregateOp::Mod, true) => Instruction::MODF,
            (AggregateOp::Pow, false) => Instruction::Pow,
            (AggregateOp::Pow, true) => Instruction::PowF,
            (AggregateOp::Neg, _) => Instruction::NEG, // unused
        };
        let len_slot = self.alloc_temp_slot();
        let idx = self.alloc_temp_slot();
        let out = self.alloc_temp_slot();
        self.bytecode.push_load(t_vec);
        self.bytecode.push(Byte::new(Instruction::ArrayLen));
        self.bytecode.push_store_pop(len_slot);
        self.bytecode.push_make_array(0);
        self.bytecode.push_store_pop(out);
        self.bytecode.push_const(0);
        self.bytecode.push_store_pop(idx);

        let mut bb = BlockBuilder::new();
        let loop_top = bb.fresh_label(self.bytecode.il_mut());
        let end = bb.fresh_label(self.bytecode.il_mut());
        bb.bind_label(loop_top, self.bytecode.il_mut());

        self.bytecode.push_load(idx);
        self.bytecode.push_load(len_slot);
        self.bytecode.push(Byte::new(Instruction::LE));
        bb.emit_jump_to(end, BbJumpKind::JumpIfFalse, self.bytecode.il_mut());

        self.bytecode.push_load(out);
        match scalar_on {
            ScalarSide::Right => {
                self.bytecode.push_load(t_vec);
                self.bytecode.push_load(idx);
                self.bytecode.push_index();
                self.bytecode.push_load(t_sc);
            }
            ScalarSide::Left => {
                self.bytecode.push_load(t_sc);
                self.bytecode.push_load(t_vec);
                self.bytecode.push_load(idx);
                self.bytecode.push_index();
            }
        }
        self.bytecode.push(Byte::new(scalar_instr));
        self.bytecode.push(Byte::new(Instruction::ArrayPush));
        self.bytecode.push_store_pop(out);
        self.bytecode.push_load(idx);
        self.bytecode.push_const(1);
        self.bytecode.push(Byte::new(Instruction::ADD));
        self.bytecode.push_store_pop(idx);

        bb.emit_jump_to(loop_top, BbJumpKind::Unconditional, self.bytecode.il_mut());
        bb.bind_label(end, self.bytecode.il_mut());
        self.bytecode.push_load(out);
    }

    /// Recover aggregate arith info from mono/codegen var types when the
    /// side-table miss (specialized clones).
    ///
    /// Requires the same homogeneous-element rule as the typechecker: mixed
    /// element types (e.g. `(int, float)`) must not recover as zip candidates.
    fn recover_aggregate_arith(
        &self,
        lhs: &Output,
        rhs: Option<&Output>,
        op: crate::typechecking::AggregateOp,
    ) -> Option<crate::typechecking::AggregateArithInfo> {
        use crate::typechecking::aggregate_arith::{elem_is_float, homogeneous_aggregate_elem};
        use crate::typechecking::subst::apply_ty_prune;
        use crate::typechecking::ty::{ArrayLength, Ty};
        use crate::typechecking::{
            AggregateArithInfo, AggregateArithKind, AggregateOp, ScalarSide,
        };

        let lty = self.expr_codegen_ty(lhs)?;
        let lty = apply_ty_prune(self.checker.subst(), &lty);
        match (op, rhs) {
            (AggregateOp::Neg, None) => {
                let elem = homogeneous_aggregate_elem(&lty)?;
                let float = elem_is_float(&elem);
                match lty {
                    Ty::Tuple(elems) => Some(AggregateArithInfo {
                        kind: AggregateArithKind::NegTuple {
                            arity: elems.len(),
                            elem_is_float: float,
                        },
                        op,
                    }),
                    Ty::Array { length, .. } => Some(AggregateArithInfo {
                        kind: AggregateArithKind::NegArray {
                            length: match length {
                                ArrayLength::Static(n) => Some(n),
                                ArrayLength::Dynamic => None,
                            },
                            elem_is_float: float,
                        },
                        op,
                    }),
                    _ => None,
                }
            }
            (_, Some(rhs)) => {
                let rty = self.expr_codegen_ty(rhs)?;
                let rty = apply_ty_prune(self.checker.subst(), &rty);
                use crate::typechecking::aggregate_arith::is_numeric_elem;
                match (&lty, &rty) {
                    (Ty::Tuple(a), Ty::Tuple(b)) if a.len() == b.len() && !a.is_empty() => {
                        let le = homogeneous_aggregate_elem(&lty)?;
                        let re = homogeneous_aggregate_elem(&rty)?;
                        if le != re {
                            return None;
                        }
                        Some(AggregateArithInfo {
                            kind: AggregateArithKind::ZipTuple {
                                arity: a.len(),
                                elem_is_float: elem_is_float(&le),
                            },
                            op,
                        })
                    }
                    (
                        Ty::Array {
                            element,
                            length: ArrayLength::Static(n),
                        },
                        Ty::Array {
                            length: ArrayLength::Static(m),
                            ..
                        },
                    ) if n == m => Some(AggregateArithInfo {
                        kind: AggregateArithKind::ZipArray {
                            length: *n,
                            elem_is_float: elem_is_float(element),
                        },
                        op,
                    }),
                    (Ty::Tuple(a), r) if !a.is_empty() && is_numeric_elem(r) => {
                        let elem = homogeneous_aggregate_elem(&lty)?;
                        Some(AggregateArithInfo {
                            kind: AggregateArithKind::BroadcastTuple {
                                arity: a.len(),
                                scalar_on: ScalarSide::Right,
                                elem_is_float: elem_is_float(&elem),
                            },
                            op,
                        })
                    }
                    (l, Ty::Tuple(b)) if !b.is_empty() && is_numeric_elem(l) => {
                        let elem = homogeneous_aggregate_elem(&rty)?;
                        Some(AggregateArithInfo {
                            kind: AggregateArithKind::BroadcastTuple {
                                arity: b.len(),
                                scalar_on: ScalarSide::Left,
                                elem_is_float: elem_is_float(&elem),
                            },
                            op,
                        })
                    }
                    (Ty::Array { element, length }, r) if is_numeric_elem(r) => {
                        Some(AggregateArithInfo {
                            kind: AggregateArithKind::BroadcastArray {
                                length: match length {
                                    ArrayLength::Static(n) => Some(*n),
                                    ArrayLength::Dynamic => None,
                                },
                                scalar_on: ScalarSide::Right,
                                elem_is_float: elem_is_float(element),
                            },
                            op,
                        })
                    }
                    (l, Ty::Array { element, length }) if is_numeric_elem(l) => {
                        Some(AggregateArithInfo {
                            kind: AggregateArithKind::BroadcastArray {
                                length: match length {
                                    ArrayLength::Static(n) => Some(*n),
                                    ArrayLength::Dynamic => None,
                                },
                                scalar_on: ScalarSide::Left,
                                elem_is_float: elem_is_float(element),
                            },
                            op,
                        })
                    }
                    _ => None,
                }
            }
            _ => None,
        }
    }

    fn expr_codegen_ty(&self, expr: &Output) -> Option<Ty> {
        match expr.1.as_ref() {
            Expression::Identifier(_) => self.codegen_ident_ty(expr),
            Expression::Integer(_) => Some(Ty::Con("int".into())),
            Expression::Float(_) => Some(Ty::Con("float".into())),
            Expression::Tuple(items) => {
                let mut tys = Vec::with_capacity(items.len());
                for it in items {
                    tys.push(self.expr_codegen_ty(it)?);
                }
                Some(Ty::Tuple(tys))
            }
            Expression::Array(items) => {
                if items.is_empty() {
                    return None;
                }
                let elem = self.expr_codegen_ty(&items[0])?;
                Some(crate::typechecking::ty::array_fixed(elem, items.len()))
            }
            Expression::Group(inner) | Expression::Expr(inner) | Expression::Statement(inner) => {
                self.expr_codegen_ty(inner)
            }
            _ => None,
        }
    }

    /// Emit a direct call to a concrete `Eq` / `Ord` instance method when
    /// the operands are a user type with a registered instance.
    ///
    /// Primitive `int`/`float`/`string`/`bool` keep the hardwired opcode
    /// path (caller falls through when this returns `false`).
    fn emit_concrete_operator_call(
        &mut self,
        bytecode: &mut CodeBuf,
        lhs: &Output,
        rhs: &Output,
        class: &str,
        method: &str,
    ) -> bool {
        let arg_ty = self
            .codegen_expr_ty(lhs)
            .or_else(|| self.codegen_expr_ty(rhs));
        let Some(ty) = arg_ty else {
            return false;
        };
        let resolved = crate::typechecking::subst::apply_ty_prune(self.checker.subst(), &ty);
        let lookup_ty = Self::show_lookup_ty_for_instance(&resolved);
        // Only dispatch for nominal *user* enums/classes. Open `Ty::Var`s
        // unify against the first builtin `Eq`/`Ord` instance under
        // `find_instance_relaxed`, which would incorrectly replace
        // hardwired `EQ`/`LT` opcodes (and box immediates into garbage).
        let nominal = match &lookup_ty {
            Ty::Con(name) => Some(name.as_str()),
            Ty::App(head, _) => match head.as_ref() {
                Ty::Con(name) => Some(name.as_str()),
                _ => None,
            },
            _ => None,
        };
        let Some(name) = nominal else {
            return false;
        };
        if matches!(
            name,
            "int" | "float" | "string" | "bool" | "unit" | "Option" | "Result"
        ) {
            return false;
        }
        if self.checker.enum_variants(name).is_none() && !self.checker.is_class(name) {
            return false;
        }
        let Some(instance) = self
            .checker
            .generics()
            .find_instance_relaxed(class, std::slice::from_ref(&lookup_ty))
            .cloned()
        else {
            return false;
        };
        let Some(fqn) = instance.method_fqns.get(method).cloned() else {
            return false;
        };
        if !self.functions.contains_key(&fqn) && !self.fn_entry_labels.contains_key(&fqn) {
            return false;
        }
        // Instance methods use the dictionary ABI: value args are boxed at
        // the call site and unboxed in the method prologue (see
        // `instance_method_unbox_tys` + `compile_function_output_with_name`).
        // Without boxing here, `UnboxValue` on a raw enum/class pointer
        // yields `Value::default()` and comparisons always fail.
        //
        // Stash each boxed operand in a temp before compiling the other
        // side: `new Class(...)` (Instantiate) uses `StorePop` into temps
        // and would otherwise steal a pending boxed arg off the operand
        // stack mid-call.
        bytecode.append(&mut self.do_compile(lhs));
        Self::emit_box_if_needed(bytecode, &lookup_ty);
        let lhs_slot = self.alloc_temp_slot();
        bytecode.push_store_pop(lhs_slot);
        bytecode.append(&mut self.do_compile(rhs));
        Self::emit_box_if_needed(bytecode, &lookup_ty);
        let rhs_slot = self.alloc_temp_slot();
        bytecode.push_store_pop(rhs_slot);
        bytecode.push_load(lhs_slot);
        bytecode.push_load(rhs_slot);
        self.emit_direct_fn_call(bytecode, &fqn, 2)
    }

    /// Emit a string literal as a table-indexed `STRING` byte into `self.bytecode`.
    /// Applies the same escape processing as `Expression::String` codegen.
    fn emit_string_literal(&mut self, s: &str) {
        let escaped = unescape_coil_string(s);
        let idx = self.intern_string(&escaped);
        self.bytecode.push_string(idx);
    }

    /// Rewrite `%v` → `%s` in a format literal (leave `%%` alone).
    fn rewrite_format_v_to_s(fmt: &str) -> String {
        let mut out = String::with_capacity(fmt.len());
        let mut chars = fmt.chars().peekable();
        while let Some(ch) = chars.next() {
            if ch == '%' {
                match chars.next() {
                    Some('%') => {
                        out.push('%');
                        out.push('%');
                    }
                    Some('v') => {
                        out.push('%');
                        out.push('s');
                    }
                    Some(other) => {
                        out.push('%');
                        out.push(other);
                    }
                    None => out.push('%'),
                }
            } else {
                out.push(ch);
            }
        }
        out
    }

    /// Consuming format specifiers in source order (`%%` skipped).
    fn format_consuming_specs(fmt: &str) -> Vec<char> {
        let mut specs = Vec::new();
        let mut chars = fmt.chars().peekable();
        while let Some(ch) = chars.next() {
            if ch == '%' {
                match chars.next() {
                    Some('%') => {}
                    Some(spec) => specs.push(spec),
                    None => break,
                }
            }
        }
        specs
    }

    /// Emit `string::format` body: format string, then args (with `%v`
    /// lowered through `Show`), then `FORMAT`.
    fn emit_format_expression(&mut self, format: &Output, params: Option<&Vec<Output>>) {
        let fmt_lit = match format.1.as_ref() {
            Expression::String(s) => Some(s.to_string()),
            _ => None,
        };

        if let (Some(fmt), Some(params)) = (fmt_lit.as_deref(), params) {
            let rewritten = Self::rewrite_format_v_to_s(fmt);
            let specs = Self::format_consuming_specs(fmt);
            // Evaluate args into temps first. Emitting the format string
            // before args leaves it under CALL/STORE frames (self-unroll /
            // nested calls) and corrupts the shared stack.
            let mut arg_slots = Vec::with_capacity(params.len());
            let mut emitted = 0usize;
            for (param, spec) in params.iter().zip(specs.iter()) {
                if *spec == 'v' {
                    self.emit_show_for_format_arg(param);
                } else {
                    let mut bc = self.do_compile(param);
                    self.bytecode.append(&mut bc);
                }
                let slot = self.alloc_temp_slot();
                self.bytecode.push_store_pop(slot);
                arg_slots.push(slot);
                emitted += 1;
            }
            for param in params.iter().skip(emitted) {
                let mut bc = self.do_compile(param);
                self.bytecode.append(&mut bc);
                let slot = self.alloc_temp_slot();
                self.bytecode.push_store_pop(slot);
                arg_slots.push(slot);
            }
            self.emit_string_literal(&rewritten);
            for slot in arg_slots {
                self.bytecode.push_load(slot);
            }
            self.bytecode
                .push(Byte::new(Instruction::FORMAT).with_operand_u32(params.len() as u32));
        } else {
            let mut format_bc = self.do_compile(format);
            self.bytecode.append(&mut format_bc);
            let mut params_len = 0u32;
            if let Some(params) = params {
                params_len = params.len() as u32;
                for param in params {
                    let mut bc = self.do_compile(param);
                    self.bytecode.append(&mut bc);
                }
            }
            self.bytecode
                .push(Byte::new(Instruction::FORMAT).with_operand_u32(params_len));
        }
    }

    fn string_builtin_for_call(&self, ident: &str) -> Option<crate::typechecking::StringBuiltin> {
        self.checker.string_fn_in_scope(ident).or_else(|| {
            ident
                .strip_prefix("string::")
                .and_then(crate::typechecking::StringBuiltin::from_name)
        })
    }

    fn show_format_arg_ty(&self, arg: &Output) -> Option<Ty> {
        match self.sidecar_ty_of(arg) {
            Some(Ty::Var(_)) | None => self.codegen_expr_ty(arg),
            Some(other) => Some(other),
        }
    }

    fn emit_ffi_declare(&mut self, span: SimpleSpan, args: &[Output]) {
        if args.len() != 4 && args.len() != 5 {
            let mut m = Message::error(
                ErrorCode::DeclareArity,
                "declare requires arguments as a tuple in position 3 (use (T1, T2, ...) syntax)"
                    .to_string(),
                span.into_range(),
            );
            m.push(DiagLabel::new(
                format!(
                    "expected 4 or 5 arguments (lib, name, args_tuple, ret_type[, variadic]); got {}",
                    args.len()
                ),
                span.into_range(),
            ));
            self.messages.push(m);
            self.bytecode
                .push(Byte::new(Instruction::DeclareFFI).with_operand_u32(0));
            return;
        }
        let lib = &args[0];
        let name = &args[1];
        let args_tuple = &args[2];
        let ret_type = &args[3];
        let variadic = if args.len() == 5 {
            match args[4].1.as_ref() {
                Expression::Bool(b) => *b,
                _ => {
                    let mut m = Message::error(
                        ErrorCode::DeclareArity,
                        "declare(...) 5th argument (variadic) must be a bool literal".to_string(),
                        args[4].0.into_range(),
                    );
                    m.push(DiagLabel::new(
                        "use `true` or `false`".to_string(),
                        args[4].0.into_range(),
                    ));
                    self.messages.push(m);
                    false
                }
            }
        } else {
            false
        };

        let tuple_elements: Vec<_> = match args_tuple.1.as_ref() {
            Expression::Tuple(items) => items.to_vec(),
            _ => {
                let mut m = Message::error(
                    ErrorCode::DeclareArity,
                    "declare(...) arguments tuple must be (T1, T2, ...) syntax".to_string(),
                    args_tuple.0.into_range(),
                );
                m.push(DiagLabel::new(
                    "wrap the arg types in parentheses — (Int, Float) after `use ffi::types::{Int, Float, …}`"
                        .to_string(),
                    args_tuple.0.into_range(),
                ));
                self.messages.push(m);
                Vec::new()
            }
        };

        let mut lib_bc = self.do_compile(lib);
        self.bytecode.append(&mut lib_bc);
        let mut name_bc = self.do_compile(name);
        self.bytecode.append(&mut name_bc);

        for elem in &tuple_elements {
            if let Some((tag, aux)) = ffi_type_tag_from_output(&self.checker, elem) {
                emit_ffi_type_const(&mut self.bytecode, tag, aux);
            } else {
                let mut bc = self.do_compile(elem);
                self.bytecode.append(&mut bc);
            }
        }
        let arity = tuple_elements.len() as u32;
        self.bytecode.push_make_tuple(arity);

        if let Some((tag, aux)) = ffi_type_tag_from_output(&self.checker, ret_type) {
            emit_ffi_type_const(&mut self.bytecode, tag, aux);
        } else {
            let mut ret_bc = self.do_compile(ret_type);
            self.bytecode.append(&mut ret_bc);
        }

        let mut operand = arity & 0xFFFF;
        if variadic {
            operand |= 1 << 16;
        }
        self.bytecode
            .push(Byte::new(Instruction::DeclareFFI).with_operand_u32(operand));
    }

    fn emit_ffi_invoke(&mut self, span: SimpleSpan, args: &[Output]) {
        if args.len() != 3 {
            let mut m = Message::error(
                ErrorCode::InvokeArity,
                "invoke requires arguments as a tuple in position 3 (use (a, b, ...) syntax)"
                    .to_string(),
                span.into_range(),
            );
            m.push(DiagLabel::new(
                format!(
                    "expected 3 arguments (lib, fn_id, args_tuple); got {}",
                    args.len()
                ),
                span.into_range(),
            ));
            self.messages.push(m);
            self.bytecode
                .push(Byte::new(Instruction::FfiInvoke).with_operand_u32(0));
            return;
        }
        let lib = &args[0];
        let fn_id = &args[1];
        let args_tuple = &args[2];

        let variadic = self.checker.is_ffi_declare_variadic_for_fn_id(fn_id);

        let tuple_elements: Vec<_> = match args_tuple.1.as_ref() {
            Expression::Tuple(items) => items.to_vec(),
            _ => {
                let mut m = Message::error(
                    ErrorCode::InvokeArity,
                    "invoke(...) arguments must be a tuple in position 3".to_string(),
                    args_tuple.0.into_range(),
                );
                m.push(DiagLabel::new(
                    "wrap the arg values in parentheses — (40, 2)".to_string(),
                    args_tuple.0.into_range(),
                ));
                self.messages.push(m);
                Vec::new()
            }
        };

        let mut lib_bc = self.do_compile(lib);
        self.bytecode.append(&mut lib_bc);
        let mut fn_bc = self.do_compile(fn_id);
        self.bytecode.append(&mut fn_bc);

        for elem in &tuple_elements {
            if let Expression::Identifier(name) = elem.1.as_ref()
                && let Some(&offset) = self.functions.get(*name)
            {
                self.bytecode
                    .push(Byte::new(Instruction::CodePtr).with_operand_u32(offset as u32));
                continue;
            }
            let mut bc = self.do_compile(elem);
            self.bytecode.append(&mut bc);
        }
        let arity = tuple_elements.len() as u32;
        self.bytecode.push_make_tuple(arity);

        let mut operand = arity & 0xFFFF;
        if variadic {
            let args: Vec<_> = tuple_elements.iter().collect();
            if let Some(tags) = self.resolve_call_ffi_tags(
                None,
                (span.start, span.end),
                &args,
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
    }

    /// Unwrap a `Result` on top of the stack: on `Ok`, leave the payload;
    /// on `Err(ffi::Error)`, panic with the error's `message` string.
    /// Used by `extern` lowering so failed `dload`/`declare`/`invoke`
    /// never reach unsafe FFI calls.
    fn emit_result_unwrap_or_panic(&mut self) {
        // Result::Ok = tag 0 (arity 1), Result::Err = tag 1 (arity 1).
        let mut bb = BlockBuilder::new();
        let success = bb.fresh_label(self.bytecode.il_mut());
        bb.emit_jump_to(
            success,
            BbJumpKind::JumpIfMatch { tag: 0, arity: 1 },
            self.bytecode.il_mut(),
        );
        // Miss: Err still on stack — unpack `ffi::Error`, then LoadField
        // message (field index 1: kind=0, message=1) and Panic.
        self.bytecode
            .push(Byte::new(Instruction::Unpack).with_operand_u32(1));
        self.bytecode.push_load_field(1);
        self.bytecode.push(Byte::new(Instruction::Panic));
        bb.bind_label(success, self.bytecode.il_mut());
    }

    /// True when `expr` is a `match` (after peeling `Expr` wrappers).
    fn rhs_is_match_expr(expr: &Output<'_>) -> bool {
        matches!(
            unwrap_expr_output(expr).1.as_ref(),
            Expression::Match { .. }
        )
    }

    /// True when a free `fn` body calls a method declared in a user `impl`
    /// block in the same file (must emit after those `impl`s). Builtin
    /// methods such as `Vec::push` do not count.
    fn function_body_calls_user_impl_method(
        body: &Output<'_>,
        user_impl_methods: &std::collections::HashSet<String>,
    ) -> bool {
        if user_impl_methods.is_empty() {
            return false;
        }
        Self::walk_expr_calls(body, &mut |name, recv_is_value_method| {
            recv_is_value_method && user_impl_methods.contains(name)
        })
    }

    /// True when `body` calls any free function whose short name is in `names`.
    fn function_body_calls_free_fn(
        body: &Output<'_>,
        names: &std::collections::HashSet<String>,
    ) -> bool {
        if names.is_empty() {
            return false;
        }
        Self::walk_expr_calls(body, &mut |name, recv_is_value_method| {
            !recv_is_value_method && names.contains(name)
        })
    }

    /// Walk call sites in `node`. `pred(callee_name, is_value_method)` —
    /// `is_value_method` is true for `recv.method(...)` on an identifier/
    /// variable receiver.
    fn walk_expr_calls(
        node: &Output<'_>,
        pred: &mut dyn FnMut(&str, bool) -> bool,
    ) -> bool {
        if let Expression::Call { name, args } = node.1.as_ref() {
            match name.1.as_ref() {
                Expression::Access(recv, method)
                    if matches!(
                        recv.1.as_ref(),
                        Expression::Identifier(_) | Expression::Variable(_, _)
                    ) =>
                {
                    if pred(method, true) {
                        return true;
                    }
                }
                Expression::Identifier(callee) => {
                    if pred(callee, false) {
                        return true;
                    }
                }
                _ => {}
            }
            if Self::walk_expr_calls(name, pred) {
                return true;
            }
            if let Some(args) = args {
                if args.iter().any(|a| Self::walk_expr_calls(a, pred)) {
                    return true;
                }
            }
        }
        match node.1.as_ref() {
            Expression::Block(children)
            | Expression::Fragment(children)
            | Expression::If(children) => {
                children.iter().any(|c| Self::walk_expr_calls(c, pred))
            }
            Expression::Branch(cond, body) => {
                cond.as_ref()
                    .is_some_and(|c| Self::walk_expr_calls(c, pred))
                    || Self::walk_expr_calls(body, pred)
            }
            Expression::Loop {
                iterable,
                body,
                identifier,
            } => {
                Self::walk_expr_calls(iterable, pred)
                    || identifier
                        .as_ref()
                        .is_some_and(|id| Self::walk_expr_calls(id, pred))
                    || Self::walk_expr_calls(body, pred)
            }
            Expression::Expr(inner)
            | Expression::Group(inner)
            | Expression::Statement(inner)
            | Expression::ExprStatement(inner)
            | Expression::Return(inner)
            | Expression::ImplicitReturn(inner)
            | Expression::Raise(inner)
            | Expression::Yield(inner)
            | Expression::Try(inner)
            | Expression::NamedArg(_, inner) => Self::walk_expr_calls(inner, pred),
            Expression::Construct {
                variant_name,
                fields,
                ..
            } => {
                // `Class::static_method(...)` shares Construct surface with enums.
                if pred(variant_name, true) {
                    return true;
                }
                match fields {
                    parser::ast::EnumConstructPayload::Unit => false,
                    parser::ast::EnumConstructPayload::Tuple(args) => {
                        args.iter().any(|a| Self::walk_expr_calls(a, pred))
                    }
                    parser::ast::EnumConstructPayload::Record(parts) => parts
                        .iter()
                        .any(|p| Self::walk_expr_calls(&p.value, pred)),
                }
            },
            Expression::List(items)
            | Expression::Tuple(items)
            | Expression::Array(items)
            | Expression::Declare(items)
            | Expression::Invoke(items) => {
                items.iter().any(|i| Self::walk_expr_calls(i, pred))
            }
            Expression::Match { scrutinee, arms } => {
                Self::walk_expr_calls(scrutinee, pred)
                    || arms
                        .iter()
                        .any(|arm| Self::walk_expr_calls(&arm.body, pred))
            }
            Expression::Access(recv, _) | Expression::OptionalAccess(recv, _) => {
                Self::walk_expr_calls(recv, pred)
            }
            Expression::Instantiate(class, args) => {
                Self::walk_expr_calls(class, pred)
                    || args.as_ref().is_some_and(|a| {
                        a.iter().any(|arg| Self::walk_expr_calls(arg, pred))
                    })
            }
            _ => false,
        }
    }

    /// Free fns that must emit after `impl`s: those that call user methods,
    /// plus the transitive callers of that set (so phase-1 never forward-calls
    /// a deferred callee via Entry into `self.bytecode`).
    fn deferred_post_impl_free_fns(
        children: &[Output<'_>],
        user_impl_methods: &std::collections::HashSet<String>,
    ) -> std::collections::HashSet<String> {
        use std::collections::HashSet;
        let mut deferred = HashSet::new();
        for child in children {
            if let Expression::Function { name, body, .. } = child.1.as_ref() {
                if *name == "main" {
                    continue;
                }
                if body.as_ref().is_some_and(|b| {
                    Self::function_body_calls_user_impl_method(b, user_impl_methods)
                }) {
                    deferred.insert(name.to_string());
                }
            }
        }
        loop {
            let mut grew = false;
            for child in children {
                if let Expression::Function { name, body, .. } = child.1.as_ref() {
                    if *name == "main" || deferred.contains(*name) {
                        continue;
                    }
                    if body.as_ref().is_some_and(|b| {
                        Self::function_body_calls_free_fn(b, &deferred)
                    }) {
                        deferred.insert(name.to_string());
                        grew = true;
                    }
                }
            }
            if !grew {
                break;
            }
        }
        deferred
    }

    fn collect_user_impl_method_names(children: &[Output<'_>]) -> std::collections::HashSet<String> {
        use std::collections::HashSet;
        let mut names = HashSet::new();
        for child in children {
            let methods = match child.1.as_ref() {
                Expression::Implementation { methods, .. } => methods.as_slice(),
                _ => continue,
            };
            for method in methods {
                let inner = match method.1.as_ref() {
                    Expression::Method(_, inner) => inner,
                    _ => method,
                };
                if let Expression::Function { name, .. } = inner.1.as_ref() {
                    names.insert(name.to_string());
                }
            }
        }
        names
    }

    fn top_level_free_fn_positions(children: &[Output<'_>]) -> std::collections::HashMap<String, usize> {
        use std::collections::HashMap;
        let mut pos = HashMap::new();
        for (idx, child) in children.iter().enumerate() {
            if let Expression::Function { name, .. } = child.1.as_ref() {
                pos.insert(name.to_string(), idx);
            }
        }
        pos
    }

    /// True when an `impl` method calls a module-level `fn` defined later in
    /// the same file (COI-109 codegen ordering).
    fn impl_calls_later_free_fn(
        children: &[Output<'_>],
        free_fn_pos: &std::collections::HashMap<String, usize>,
    ) -> bool {
        fn body_calls_later_fn(
            node: &Output<'_>,
            impl_idx: usize,
            free_fn_pos: &std::collections::HashMap<String, usize>,
        ) -> bool {
            if let Expression::Call { name, args } = node.1.as_ref() {
                if let Expression::Identifier(callee) = name.1.as_ref() {
                    if free_fn_pos
                        .get(*callee)
                        .is_some_and(|fn_idx| *fn_idx > impl_idx)
                    {
                        return true;
                    }
                }
                if body_calls_later_fn(name, impl_idx, free_fn_pos) {
                    return true;
                }
                if let Some(args) = args {
                    return args
                        .iter()
                        .any(|a| body_calls_later_fn(a, impl_idx, free_fn_pos));
                }
            }
            match node.1.as_ref() {
                Expression::Block(children)
                | Expression::Fragment(children)
                | Expression::If(children) => children
                    .iter()
                    .any(|c| body_calls_later_fn(c, impl_idx, free_fn_pos)),
                Expression::Branch(cond, body) => {
                    cond.as_ref()
                        .is_some_and(|c| body_calls_later_fn(c, impl_idx, free_fn_pos))
                        || body_calls_later_fn(body, impl_idx, free_fn_pos)
                }
                Expression::Loop {
                    iterable,
                    body,
                    identifier,
                } => {
                    body_calls_later_fn(iterable, impl_idx, free_fn_pos)
                        || identifier.as_ref().is_some_and(|id| {
                            body_calls_later_fn(id, impl_idx, free_fn_pos)
                        })
                        || body_calls_later_fn(body, impl_idx, free_fn_pos)
                }
                Expression::Expr(inner)
                | Expression::Group(inner)
                | Expression::Statement(inner)
                | Expression::ExprStatement(inner)
                | Expression::Return(inner)
                | Expression::Raise(inner)
                | Expression::Yield(inner)
                | Expression::Try(inner) => {
                    body_calls_later_fn(inner, impl_idx, free_fn_pos)
                }
                Expression::Match { scrutinee, arms } => {
                    body_calls_later_fn(scrutinee, impl_idx, free_fn_pos)
                        || arms.iter().any(|arm| {
                            body_calls_later_fn(&arm.body, impl_idx, free_fn_pos)
                        })
                }
                Expression::Access(recv, _) => body_calls_later_fn(recv, impl_idx, free_fn_pos),
                _ => false,
            }
        }

        for (impl_idx, child) in children.iter().enumerate() {
            let methods = match child.1.as_ref() {
                Expression::Implementation { what, methods, .. } if what.is_empty() => {
                    methods.as_slice()
                }
                _ => continue,
            };
            for method in methods {
                let body = match method.1.as_ref() {
                    Expression::Method(_, inner) => match inner.1.as_ref() {
                        Expression::Function { body, .. } => body.as_ref(),
                        _ => None,
                    },
                    _ => None,
                };
                if let Some(body) = body
                    && body_calls_later_fn(body, impl_idx, free_fn_pos)
                {
                    return true;
                }
            }
        }
        false
    }

    fn reserve_phased_free_fn_entries(&mut self, children: &[Output<'_>]) {
        for child in children {
            if let Expression::Function { name, .. } = child.1.as_ref() {
                let qualified = if self.namespace.is_empty() {
                    name.to_string()
                } else {
                    format!("{}::{}", self.namespace, name)
                };
                self.reserve_function_entry(qualified);
            }
        }
    }

    fn program_needs_phased_emit(children: &[Output<'_>]) -> bool {
        let user_impl_methods = Self::collect_user_impl_method_names(children);
        let free_fn_pos = Self::top_level_free_fn_positions(children);
        !Self::deferred_post_impl_free_fns(children, &user_impl_methods).is_empty()
            || Self::impl_calls_later_free_fn(children, &free_fn_pos)
    }

    /// True when `body` is just the sole pattern binding (e.g. `Ok(x) => x`).
    fn match_arm_body_is_identity_binding<'a>(
        pattern: &parser::ast::Pattern<'a>,
        body: &Output<'a>,
    ) -> bool {
        use parser::ast::{Expression, Pattern, PatternPayload};
        let body_name = match unwrap_expr_output(body).1.as_ref() {
            Expression::Identifier(n) | Expression::Variable(n, _) => *n,
            _ => return false,
        };
        fn sole_binding<'a>(pattern: &Pattern<'a>) -> Option<&'a str> {
            match pattern {
                Pattern::Binding { name } => Some(name),
                Pattern::Constructor { payload, .. } => match payload {
                    PatternPayload::Tuple(items) if items.len() == 1 => sole_binding(&items[0].1),
                    _ => None,
                },
                _ => None,
            }
        }
        sole_binding(pattern) == Some(body_name)
    }

    /// Lower one `%v` argument to a string via the `Show` dictionary /
    /// concrete instance method, leaving an `ObjString` on the stack.
    fn emit_show_for_format_arg(&mut self, arg: &Output) {
        if let Some((dict_index, method_slot)) = self
            .bound_display_hint(self.node_id_of(arg), arg.0.start, arg.0.end)
            .map(|h| (h.dict_index, h.method_slot))
        {
            let dict_name = format!("__dict{}", dict_index);
            if let Some(dict_slot) = self.lookup_slot(&dict_name) {
                let mut arg_bc = self.do_compile(arg);
                self.bytecode.append(&mut arg_bc);
                self.bytecode.push_load(dict_slot);
                self.bytecode.push_load(dict_slot);
                self.bytecode.push_const(method_slot as i32);
                self.bytecode.push_index();
                self.bytecode
                    .push(Byte::new(Instruction::CallIndirect).with_operand_u32(2));
                return;
            }
        }

        // Concrete Show instance at the call site.
        // Prefer a fully resolved type: span cache from a shared generic body
        // may still be an open `Ty::Var` even when mono/codegen side-tables
        // know the ground type (or when the arg is a literal / construct).
        let arg_ty = self.show_format_arg_ty(arg);

        if let Some(ty) = arg_ty.as_ref() {
            let resolved = crate::typechecking::subst::apply_ty_prune(self.checker.subst(), ty);
            if matches!(resolved, Ty::Tuple(_) | Ty::Record { .. }) {
                let mut arg_bc = self.do_compile(arg);
                self.bytecode.append(&mut arg_bc);
                self.emit_show_for_stack_value(&resolved);
                return;
            }

            // Instance heads use `Ty::Con("Point")`; construct sites often
            // produce `Constructor` / `Sum` — peel to the enum name.
            let lookup_ty = Self::show_lookup_ty_for_instance(&resolved);
            if let Some(instance) = self
                .checker
                .generics()
                .find_instance("Show", std::slice::from_ref(&lookup_ty))
                .cloned()
                && let Some(fqn) = instance.method_fqns.get("show").cloned()
                && (self.functions.contains_key(&fqn) || self.fn_entry_labels.contains_key(&fqn))
            {
                let mut arg_bc = self.do_compile(arg);
                self.bytecode.append(&mut arg_bc);
                // Box using the lookup head so enum Constructs get Enum tag.
                Self::emit_box_if_needed(&mut self.bytecode, &lookup_ty);
                let _ = self.emit_named_entry_on_module(&fqn, 1, crate::il::EntryKind::Call);
                return;
            }
        }

        // Typechecker should have rejected; keep bytecode well-formed.
        let mut arg_bc = self.do_compile(arg);
        self.bytecode.append(&mut arg_bc);
        self.bytecode.push(Byte::new(Instruction::STRINGIFY));
    }

    fn show_lookup_ty_for_instance(ty: &Ty) -> Ty {
        match ty {
            Ty::Sum { name, .. } => Ty::Con(name.clone()),
            Ty::Constructor { owner, .. } => Self::show_lookup_ty_for_instance(owner),
            other => other.clone(),
        }
    }

    fn tuple_show_format(len: usize) -> String {
        match len {
            0 => "()".to_string(),
            1 => "(%s,)".to_string(),
            _ => format!("({})", vec!["%s"; len].join(", ")),
        }
    }

    fn record_show_format(fields: &[(String, Ty)]) -> String {
        if fields.is_empty() {
            return "{}".to_string();
        }
        let parts = fields
            .iter()
            .map(|(name, _)| format!("{name}: %s"))
            .collect::<Vec<_>>()
            .join(", ");
        format!("{{ {parts} }}")
    }

    fn emit_show_for_stack_value(&mut self, ty: &Ty) {
        let resolved = crate::typechecking::subst::apply_ty_prune(self.checker.subst(), ty);
        match resolved {
            Ty::Tuple(items) => self.emit_tuple_show_for_stack_value(&items),
            Ty::Record { fields } => self.emit_record_show_for_stack_value(&fields),
            other => {
                let lookup_ty = Self::show_lookup_ty_for_instance(&other);
                if let Some(instance) = self
                    .checker
                    .generics()
                    .find_instance("Show", std::slice::from_ref(&lookup_ty))
                    .cloned()
                    && let Some(fqn) = instance.method_fqns.get("show").cloned()
                    && (self.functions.contains_key(&fqn)
                        || self.fn_entry_labels.contains_key(&fqn))
                {
                    Self::emit_box_if_needed(&mut self.bytecode, &lookup_ty);
                    let _ = self.emit_named_entry_on_module(&fqn, 1, crate::il::EntryKind::Call);
                } else {
                    self.bytecode.push(Byte::new(Instruction::STRINGIFY));
                }
            }
        }
    }

    fn emit_tuple_show_for_stack_value(&mut self, items: &[Ty]) {
        let tuple_slot = self.alloc_temp_slot();
        self.bytecode.push_store_pop(tuple_slot);

        let mut element_slots = Vec::with_capacity(items.len());
        for (idx, item_ty) in items.iter().enumerate() {
            self.bytecode.push_load(tuple_slot);
            self.bytecode.push_const(idx as i32);
            self.bytecode.push_index();
            self.emit_show_for_stack_value(item_ty);
            let slot = self.alloc_temp_slot();
            self.bytecode.push_store_pop(slot);
            element_slots.push(slot);
        }

        self.emit_string_literal(&Self::tuple_show_format(items.len()));
        for slot in element_slots {
            self.bytecode.push_load(slot);
        }
        self.bytecode
            .push(Byte::new(Instruction::FORMAT).with_operand_u32(items.len() as u32));
    }

    fn emit_record_show_for_stack_value(&mut self, fields: &[(String, Ty)]) {
        let record_slot = self.alloc_temp_slot();
        self.bytecode.push_store_pop(record_slot);

        let mut field_slots = Vec::with_capacity(fields.len());
        for (name, field_ty) in fields {
            self.bytecode.push_load(record_slot);
            let idx = self.intern_string(name);
            self.bytecode.push_string(idx);
            self.bytecode.push_get_field();
            self.emit_show_for_stack_value(field_ty);
            let slot = self.alloc_temp_slot();
            self.bytecode.push_store_pop(slot);
            field_slots.push(slot);
        }

        self.emit_string_literal(&Self::record_show_format(fields));
        for slot in field_slots {
            self.bytecode.push_load(slot);
        }
        self.bytecode
            .push(Byte::new(Instruction::FORMAT).with_operand_u32(fields.len() as u32));
    }

    /// Structurally bind scheme pattern variables to concrete call-site types.
    ///
    /// Used by [`emit_call_site_dicts`] so `F<A>` against `Option<int>` records
    /// both `F = Option` and `A = int` (Phase 5).
    fn bind_scheme_vars(
        pattern: &Ty,
        concrete: &Ty,
        map: &mut HashMap<crate::typechecking::ty::TyVarId, Ty>,
    ) {
        use crate::typechecking::ty::{option_inner, result_ok_err};

        match (pattern, concrete) {
            (Ty::Var(v), c) => {
                map.entry(*v).or_insert_with(|| c.clone());
            }
            (Ty::App(h1, a1), Ty::App(h2, a2)) if a1.len() == a2.len() => {
                Self::bind_scheme_vars(h1, h2, map);
                for (p, c) in a1.iter().zip(a2.iter()) {
                    Self::bind_scheme_vars(p, c, map);
                }
            }
            // `F<A>` vs builtin Option/Result constructor or structural sum:
            // bind `F` to the constructor constant and recurse into payloads.
            (Ty::App(head, args), other)
                if matches!(head.as_ref(), Ty::Var(_))
                    && (option_inner(other).is_some() || result_ok_err(other).is_some()) =>
            {
                if let Some(inner) = option_inner(other) {
                    if args.len() == 1 {
                        Self::bind_scheme_vars(
                            head,
                            &Ty::Con(common::BUILTIN_OPTION_ENUM.into()),
                            map,
                        );
                        Self::bind_scheme_vars(&args[0], &inner, map);
                    }
                } else if let Some((ok, err)) = result_ok_err(other) {
                    if args.len() == 2 {
                        Self::bind_scheme_vars(
                            head,
                            &Ty::Con(common::BUILTIN_RESULT_ENUM.into()),
                            map,
                        );
                        Self::bind_scheme_vars(&args[0], &ok, map);
                        Self::bind_scheme_vars(&args[1], &err, map);
                    }
                }
            }
            (Ty::App(h1, a1), Ty::Constructor { owner, .. }) => {
                Self::bind_scheme_vars(&Ty::App(h1.clone(), a1.clone()), owner.as_ref(), map);
            }
            (Ty::Fun(a1, r1), Ty::Fun(a2, r2)) => {
                Self::bind_scheme_vars(a1, a2, map);
                Self::bind_scheme_vars(r1, r2, map);
            }
            (Ty::Tuple(t1), Ty::Tuple(t2)) if t1.len() == t2.len() => {
                for (p, c) in t1.iter().zip(t2.iter()) {
                    Self::bind_scheme_vars(p, c, map);
                }
            }
            // Rest packs are `[T]` / `[T; N]` — bind `T` from the element.
            (Ty::Array { element: e1, .. }, Ty::Array { element: e2, .. }) => {
                Self::bind_scheme_vars(e1, e2, map);
            }
            // Rest params are typed as `Vec<T>` in schemes, while call sites
            // synthesize `Ty::Array` for the packed MakeArray — cross-bind.
            (Ty::App(head, args), Ty::Array { element, .. })
                if args.len() == 1
                    && matches!(
                        head.as_ref(),
                        Ty::Con(n) if n == common::BUILTIN_VEC_TYPE
                    ) =>
            {
                Self::bind_scheme_vars(&args[0], element, map);
            }
            (Ty::Array { element, .. }, Ty::App(head, args))
                if args.len() == 1
                    && matches!(
                        head.as_ref(),
                        Ty::Con(n) if n == common::BUILTIN_VEC_TYPE
                    ) =>
            {
                Self::bind_scheme_vars(element, &args[0], map);
            }
            _ => {}
        }
    }

    /// Emit one instance dictionary (`CodePtr`s + `MakeTuple`) for a
    /// trait constraint whose type arguments have already been resolved
    /// to concrete lookup types. Returns `true` when a dict was pushed.
    ///
    /// Layout (Phase 5): subclass methods first, then each superclass’s
    /// methods in declaration order (flattened). Superclass slots are filled
    /// from the matching superclass instance for the same type arguments.
    fn emit_instance_dict(
        &mut self,
        bytecode: &mut CodeBuf,
        class: &str,
        lookup: &[crate::typechecking::Ty],
    ) -> bool {
        let (fqns, diag_range) = {
            let Some(instance) = self.checker.generics().find_instance_relaxed(class, lookup) else {
                return false;
            };
            let Some(class_def) = self.checker.generics().typeclass(&instance.class) else {
                return false;
            };
            let flat = class_def.flattened_methods(self.checker.generics());
            let mut fqns = Vec::with_capacity(flat.len());
            for (owner_class, method_def) in &flat {
                let fqn = if *owner_class == instance.class.as_str() {
                    instance.method_fqns.get(&method_def.name).cloned()
                } else {
                    self.checker
                        .generics()
                        .find_instance_relaxed(owner_class, lookup)
                        .and_then(|super_inst| super_inst.method_fqns.get(&method_def.name).cloned())
                };
                let Some(name) = fqn else {
                    return false;
                };
                if !self.functions.contains_key(&name) && !self.fn_entry_labels.contains_key(&name)
                {
                    self.missing_call_target(&name, instance.range.clone());
                    return false;
                }
                fqns.push(name);
            }
            (fqns, instance.range.clone())
        };
        for name in &fqns {
            if !self.emit_named_entry(bytecode, name, 0, crate::il::EntryKind::CodePtr) {
                self.missing_call_target(name, diag_range.clone());
                return false;
            }
        }
        bytecode.push_make_tuple(fqns.len() as u32);
        true
    }

    fn emit_existential_pack_recipe(
        &mut self,
        bytecode: &mut CodeBuf,
        pack: &crate::typechecking::infer::ExistentialPack,
    ) {
        Self::emit_box_if_needed(bytecode, &pack.value_ty);
        if self.emit_instance_dict(bytecode, &pack.class, std::slice::from_ref(&pack.value_ty)) {
            bytecode.push_make_tuple(2);
        }
    }

    fn append_with_existential_pack(&mut self, bytecode: &mut CodeBuf, expr: &Output) {
        let pack = self.existential_pack_hint(self.node_id_of(expr), expr);
        bytecode.append(&mut self.do_compile(expr));
        if let Some(pack) = pack {
            self.emit_existential_pack_recipe(bytecode, &pack);
        }
    }

    /// Compile `expr` into [`Self::bytecode`] for an immediate store (`let` / `=`).
    fn emit_binding_rhs(&mut self, expr: &Output) {
        let prev = self.suppress_match_fusion_barrier;
        self.suppress_match_fusion_barrier = true;
        let pack = self.existential_pack_hint(self.node_id_of(expr), expr);
        let mut expr_bc = self.do_compile(expr);
        self.bytecode.append(&mut expr_bc);
        if let Some(pack) = pack {
            let mut pack_bc = CodeBuf::new();
            self.emit_existential_pack_recipe(&mut pack_bc, &pack);
            self.bytecode.append(&mut pack_bc);
        }
        self.suppress_match_fusion_barrier = prev;
    }

    /// Like [`Self::append_with_existential_pack`], but match expressions skip
    /// the join `DUPLICATE; POP` barrier because the value is stored immediately.
    fn append_binding_rhs(&mut self, bytecode: &mut CodeBuf, expr: &Output) {
        let prev = self.suppress_match_fusion_barrier;
        self.suppress_match_fusion_barrier = true;
        self.append_with_existential_pack(bytecode, expr);
        self.suppress_match_fusion_barrier = prev;
    }

    fn load_tuple_field(bytecode: &mut CodeBuf, tuple_slot: u32, index: i32) {
        bytecode.push_load(tuple_slot);
        bytecode.push_const(index);
        bytecode.push_index();
    }

    fn emit_existential_method_call(
        &mut self,
        bytecode: &mut CodeBuf,
        name: &Output,
        args: Option<&Vec<Output>>,
        hint: &crate::typechecking::infer::ExistentialMethodCall,
    ) -> bool {
        let (pack_expr, extra_args): (&Output, &[Output]) = if hint.has_receiver {
            let Expression::Access(recv, _) = name.1.as_ref() else {
                return false;
            };
            (recv, args.map(Vec::as_slice).unwrap_or(&[]))
        } else {
            let Some(items) = args else {
                return false;
            };
            let Some((first, rest)) = items.split_first() else {
                return false;
            };
            (first, rest)
        };

        let pack_slot = self.alloc_temp_slot();
        bytecode.append(&mut self.do_compile(pack_expr));
        bytecode.push_store_pop(pack_slot);

        // Pack layout: tuple[0] = boxed value, tuple[1] = dictionary tuple.
        Self::load_tuple_field(bytecode, pack_slot, 0);
        for arg in extra_args {
            self.append_with_existential_pack(bytecode, arg);
        }
        Self::load_tuple_field(bytecode, pack_slot, 1);
        Self::load_tuple_field(bytecode, pack_slot, 1);
        bytecode.push_const(hint.method_slot as i32);
        bytecode.push_index();
        bytecode.push(Byte::new(Instruction::CallIndirect).with_operand_u32(hint.arity as u32 + 1));
        true
    }

    /// Resolve a constraint's type arguments through `var_to_ty`, returning
    /// concrete lookup types when every argument is ground. `None` means at
    /// least one argument is still open (cannot synthesize yet).
    fn resolve_constraint_lookup(
        constraint: &crate::typechecking::ty::Constraint,
        var_to_ty: &HashMap<crate::typechecking::ty::TyVarId, crate::typechecking::Ty>,
        checker: &Checker,
    ) -> Option<Vec<crate::typechecking::Ty>> {
        use crate::typechecking::Ty;
        use crate::typechecking::subst::apply_ty_prune;
        use crate::typechecking::ty::ftv_ty;

        let mut resolved = Vec::with_capacity(constraint.args.len());
        for arg in &constraint.args {
            let concrete = match arg {
                Ty::Var(v) => apply_ty_prune(checker.subst(), var_to_ty.get(v)?),
                other => apply_ty_prune(checker.subst(), other),
            };
            if !ftv_ty(&concrete).is_empty() {
                return None;
            }
            resolved.push(concrete);
        }
        // Constructor-kinded class params look up by constructor head
        // (`Option`, `Result`), not applied types.
        let lookup = if let Some(class_def) = checker.generics().typeclass(&constraint.class) {
            resolved
                .iter()
                .enumerate()
                .map(|(i, concrete)| {
                    if class_def.is_constructor_kind_at(i) {
                        match concrete {
                            Ty::App(head, _) => head.as_ref().clone(),
                            other => other.clone(),
                        }
                    } else {
                        concrete.clone()
                    }
                })
                .collect()
        } else {
            resolved
        };
        Some(lookup)
    }

    /// Emit dictionary tuples for a non-monomorphized generic call site.
    ///
    /// Convention: after value args, one `MakeTuple` per typeclass
    /// constraint. Compiler-provided and source-provided instances use the
    /// same dictionary layout.
    /// Each tuple holds method entry offsets in flattened declaration order
    /// (subclass methods, then superclass methods — Phase 5)
    /// (`CodePtr` / `Entry` to the instance method).
    ///
    /// Instances are resolved from the callee's scheme + concrete argument
    /// types (not `NodeId`), because the pre-walk / infer ID table can be
    /// misaligned inside function bodies.
    ///
    /// Returns the number of dict tuples pushed (used to bump CALL arity).
    fn emit_call_site_dicts(
        &mut self,
        bytecode: &mut CodeBuf,
        fn_name: &str,
        arg_tys: &[crate::typechecking::Ty],
        ret_ty: Option<&crate::typechecking::Ty>,
    ) -> usize {
        use crate::typechecking::Ty;

        let Some(scheme) = self.checker.env().lookup(fn_name).cloned() else {
            return 0;
        };
        // Map quantified vars → concrete arg types by structurally matching
        // the curried function type against the call's argument types.
        // Phase 5: `F<A>` vs `Option<int>` binds both `F = Option` and `A = int`.
        // Do NOT apply the global subst to the scheme — those vars may have
        // been reused/unified later in the program.
        let mut var_to_ty: HashMap<crate::typechecking::ty::TyVarId, crate::typechecking::Ty> =
            HashMap::new();
        let mut fun = &scheme.ty;
        let mut arg_idx = 0usize;
        while let Ty::Fun(param, ret) = fun {
            if arg_idx >= arg_tys.len() {
                break;
            }
            Self::bind_scheme_vars(param.as_ref(), &arg_tys[arg_idx], &mut var_to_ty);
            fun = ret.as_ref();
            arg_idx += 1;
        }
        // Multi-param constraints often mention return-type vars
        // (`Convert<A, B>` with `A -> B`). Bind those from the call's result type.
        if let Some(ret_ty) = ret_ty {
            Self::bind_scheme_vars(fun, ret_ty, &mut var_to_ty);
        }

        let mut dict_count = 0;
        for constraint in &scheme.constraints {
            let Some(lookup) =
                Self::resolve_constraint_lookup(constraint, &var_to_ty, &self.checker)
            else {
                continue;
            };
            if self.emit_instance_dict(bytecode, &constraint.class, &lookup) {
                dict_count += 1;
            }
        }
        dict_count
    }

    /// Phase 4: push dictionary evidence for every constraint slot when a
    /// generic function escapes into a `PolyFn` value.
    ///
    /// Slot fill order per constraint index:
    /// 1. in-scope `__dictN` (open bound forwarded from the enclosing frame)
    /// 2. concrete instance synthesis when constraint args are ground
    /// 3. null sentinel (`CONST 0` → `None` in `MakePolyFnCapture`) when
    ///    evidence is truly unavailable (e.g. top-level `let f = show`)
    ///
    /// Returns the dict arity (number of stack slots pushed). Caller always
    /// emits `MakePolyFnCapture` when this is non-zero.
    fn emit_polyfn_escape_dicts(
        &mut self,
        bytecode: &mut CodeBuf,
        fn_name: &str,
        escape_ty: Option<&crate::typechecking::Ty>,
    ) -> usize {
        let dict_arity = self.checker.dict_arity_for(fn_name);
        if dict_arity == 0 {
            return 0;
        }

        let scheme = self.checker.env().lookup(fn_name).cloned();
        let mut var_to_ty: HashMap<crate::typechecking::ty::TyVarId, crate::typechecking::Ty> =
            HashMap::new();
        if let (Some(scheme), Some(escape_ty)) = (scheme.as_ref(), escape_ty) {
            // Bind scheme vars from the escape site's instantiated type so
            // ground specializations can synthesize instance dictionaries.
            Self::bind_scheme_vars(&scheme.ty, escape_ty, &mut var_to_ty);
        }

        for dict_index in 0..dict_arity {
            if let Some(slot) = self.lookup_slot(&format!("__dict{}", dict_index)) {
                bytecode.push_load(slot);
                continue;
            }

            let synthesized = scheme.as_ref().and_then(|s| {
                let constraint = s.constraints.get(dict_index)?;
                let lookup =
                    Self::resolve_constraint_lookup(constraint, &var_to_ty, &self.checker)?;
                self.emit_instance_dict(bytecode, &constraint.class, &lookup)
                    .then_some(())
            });
            if synthesized.is_none() {
                // Unresolved sentinel — CallIndirect fills from app evidence.
                bytecode.push_const(0);
            }
        }
        dict_arity
    }

    /// Emit `HostInvoke` for a virtual `io` free function.
    ///
    /// Nested IO calls (e.g. `read_to_end(stdin())`) also write directly to
    /// `self.bytecode` via this helper. Emit the native-id `CONST` **before**
    /// compiling arguments so the runtime stack is `[id, arg0, …]` — the order
    /// `HostInvoke` expects. Compiling args into a side buffer first left nested
    /// invokes *above* the id and `MakeTuple` packed the wrong values (piped
    /// stdin then looked empty).
    ///
    /// Always targets [`Self::bytecode`] so nested `format` / `match` (same
    /// buffer) stay contiguous with HostInvoke staging.
    fn emit_io_host_invoke(&mut self, kind: crate::typechecking::IoBuiltin, args: &[Output]) {
        self.emit_host_native_invoke(kind.native_name(), args);
    }

    fn emit_thread_host_invoke(
        &mut self,
        kind: crate::typechecking::ThreadBuiltin,
        args: &[Output],
    ) {
        self.emit_host_native_invoke(kind.native_name(), args);
    }

    /// Resolve the bare recursive-pure name used in [`par_shapes`].
    fn par_shape_key(fname: &str) -> &str {
        let short = strip_overload_key(fname);
        short.rsplit("::").next().unwrap_or(short)
    }

    /// Rewrite `f(N, …)` to `CALL __coil_par_f_N_…` when a specialization exists.
    fn try_emit_par_specialized_call(
        &mut self,
        fname: &str,
        args: Option<&[Output<'_>]>,
        bytecode: &mut CodeBuf,
    ) -> bool {
        let Some(args) = args else {
            return false;
        };
        if args.is_empty() {
            return false;
        }
        let mut vals = Vec::with_capacity(args.len());
        for arg in args {
            let Expression::Integer(n) = unwrap_expr_output(arg).1.as_ref() else {
                return false;
            };
            vals.push(*n);
        }
        let key = Self::par_shape_key(fname);
        if !crate::typechecking::args_worth_parallel(&self.par_shapes, key, &vals) {
            return false;
        }
        let spec = crate::typechecking::par_specialization_name(key, &vals);
        let Some(&offset) = self.functions.get(&spec) else {
            return false;
        };
        bytecode.push(Byte::new(Instruction::CALL).with_call_packed(0, offset as u32));
        true
    }

    /// Emit nullary `__coil_par_{fn}_{args…}` clones that always fork (no RT threshold).
    fn emit_par_specializations_for(&mut self, bare_name: &str, table_key: &str) {
        let Some(site) = self.par_shapes.get(bare_name).cloned() else {
            return;
        };
        let Some(arg_sets) = self.par_spec_args.get(bare_name).cloned() else {
            return;
        };
        let Some(&orig_offset) = self
            .functions
            .get(table_key)
            .or_else(|| self.functions.get(bare_name))
        else {
            return;
        };
        // Cheapest arg vectors first: arms shrink their args, so a parent then
        // finds its children's nullary clones already bound.
        let mut ordered: Vec<Vec<i64>> = arg_sets.into_iter().collect();
        ordered.sort_by(|a, b| (a.iter().sum::<i64>(), a).cmp(&(b.iter().sum::<i64>(), b)));
        for args in &ordered {
            self.emit_one_par_specialization(&site, args, orig_offset as u32);
        }
    }

    /// Emit one always-fork nullary clone of `site.fn_name` at `parent_args`.
    ///
    /// Arm 0 is spawned onto the work-stealing reactor, the remaining arms run
    /// inline, and the joined results are folded by the site's combine. A
    /// failed spawn falls back to evaluating every arm sequentially.
    fn emit_one_par_specialization(
        &mut self,
        site: &crate::typechecking::ParForkSite,
        parent_args: &[i64],
        orig_offset: u32,
    ) {
        if site.arms.len() < 2 || parent_args.len() != site.param_count {
            return;
        }
        let spec_name = crate::typechecking::par_specialization_name(&site.fn_name, parent_args);
        if self.functions.contains_key(&spec_name) {
            return;
        }
        let Some(child_args) = site
            .arms
            .iter()
            .map(|arm| crate::typechecking::eval_arm_args(arm, parent_args))
            .collect::<Option<Vec<Vec<i64>>>>()
        else {
            return;
        };
        if child_args[0].len() > common::MAX_THREAD_SPAWN_ARGS {
            return;
        }
        let Some((plan, push_order)) = self.par_combine_plan(site, orig_offset) else {
            return;
        };
        let Some(spawn_id) = self.native_id("thread_spawn") else {
            return;
        };
        let Some(join_id) = self.native_id("thread_join") else {
            return;
        };
        // Resolved before the clone is bound so an arm that reproduces
        // `parent_args` cannot bind to the clone itself (infinite CALL).
        let Some(callables) = site
            .arms
            .iter()
            .zip(child_args.iter())
            .map(|(arm, c)| self.par_arm_callable(arm, c))
            .collect::<Option<Vec<_>>>()
        else {
            return;
        };

        self.bind_function_entry(spec_name.clone());
        self.fn_arities.insert(spec_name.clone(), (0, false));

        let prev_fn_vars = std::mem::take(&mut self.context.variables);
        let prev_fn_table_key = self.current_function_table_key.take();
        self.current_function_table_key = Some(spec_name.clone());
        self.context.variables = Interner::default();
        let entry_sp = 0u32;

        let body_start = self.bytecode.len();

        // MakeFn for arm 0 (nullary child clone, or the original with args).
        let (entry0, arity0, push_args0) = callables[0];
        self.bytecode.push_const(0);
        self.bytecode
            .push(Byte::new(Instruction::CodePtr).with_operand_u32(entry0));
        self.bytecode.push(
            Byte::new(Instruction::MakeFn).with_operand_u32(make_fn_operand(0, 0, arity0, false)),
        );
        let fn_tmp = self.alloc_temp_slot();
        self.bytecode.push_store_pop(fn_tmp);

        let mut bb = BlockBuilder::new();
        let have_handle = bb.fresh_label(self.bytecode.il_mut());
        let seq = bb.fresh_label(self.bytecode.il_mut());
        let done = bb.fresh_label(self.bytecode.il_mut());

        // AlwaysPar: thread_spawn(fn[, child args…])
        self.bytecode
            .push(Byte::new(Instruction::CONST).with_value_u32(spawn_id as u32));
        self.bytecode.push_load(fn_tmp);
        let mut spawn_arity = 1;
        if push_args0 {
            for a in child_args[0].clone() {
                self.push_int_const(a);
                spawn_arity += 1;
            }
        }
        self.bytecode.push_make_tuple(spawn_arity);
        self.bytecode.push_host_invoke(spawn_arity);

        bb.emit_jump_to(
            have_handle,
            BbJumpKind::JumpIfMatch { tag: 0, arity: 1 },
            self.bytecode.il_mut(),
        );
        self.bytecode.push_pop();
        bb.emit_jump_to(seq, BbJumpKind::Unconditional, self.bytecode.il_mut());

        bb.bind_label(have_handle, self.bytecode.il_mut());
        let handle_tmp = self.alloc_temp_slot();
        self.bytecode.push_store_pop(handle_tmp);

        // Inline arms first, then join arm 0 — every result is parked in a
        // slot so the combine can push them in whatever order it needs.
        let mut arm_tmps = vec![0u32; callables.len()];
        for i in 1..callables.len() {
            self.emit_par_arm_call_args(callables[i], &child_args[i]);
            let slot = self.alloc_temp_slot();
            self.bytecode.push_store_pop(slot);
            arm_tmps[i] = slot;
        }

        self.bytecode
            .push(Byte::new(Instruction::CONST).with_value_u32(join_id as u32));
        self.bytecode.push_load(handle_tmp);
        self.bytecode.push_make_tuple(1);
        self.bytecode.push_host_invoke(1);
        // A failed join (worker result was not sendable, handle already taken)
        // redoes the whole site sequentially rather than propagating an error.
        let joined = bb.fresh_label(self.bytecode.il_mut());
        bb.emit_jump_to(
            joined,
            BbJumpKind::JumpIfMatch { tag: 0, arity: 1 },
            self.bytecode.il_mut(),
        );
        self.bytecode.push_pop();
        bb.emit_jump_to(seq, BbJumpKind::Unconditional, self.bytecode.il_mut());
        bb.bind_label(joined, self.bytecode.il_mut());
        arm_tmps[0] = self.alloc_temp_slot();
        self.bytecode.push_store_pop(arm_tmps[0]);

        for &idx in &push_order {
            self.bytecode.push_load(arm_tmps[idx]);
        }
        self.emit_par_combine(&plan);
        bb.emit_jump_to(done, BbJumpKind::Unconditional, self.bytecode.il_mut());

        // Spawn failed: arms are pure, so evaluating them straight into the
        // combine's push order is equivalent.
        bb.bind_label(seq, self.bytecode.il_mut());
        for &idx in &push_order {
            self.emit_par_arm_call_args(callables[idx], &child_args[idx]);
        }
        self.emit_par_combine(&plan);

        bb.bind_label(done, self.bytecode.il_mut());

        self.bytecode.push_return();

        let body_end = self.bytecode.len();
        self.record_fn_span(spec_name.clone(), body_start, body_end);
        let entry = self.fn_entry_labels.get(&spec_name).copied();
        self.bytecode
            .record_func_with_sp(spec_name, entry, body_start, body_end, entry_sp);
        self.current_function_table_key = prev_fn_table_key;
        self.context.variables = prev_fn_vars;
    }

    /// Lower `site.combine` to a fold instruction plus the arm push order it
    /// expects on the stack. `None` when the combine cannot be lowered here.
    fn par_combine_plan(
        &self,
        site: &crate::typechecking::ParForkSite,
        orig_offset: u32,
    ) -> Option<(ParCombinePlan, Vec<usize>)> {
        use crate::typechecking::{ParBinOp, ParCombine};
        let arms = site.arms.len();
        match &site.combine {
            ParCombine::BinOp(op) => {
                if arms != 2 {
                    return None;
                }
                let ins = match op {
                    ParBinOp::Add => Instruction::ADD,
                    ParBinOp::Sub => Instruction::SUB,
                    ParBinOp::Mul => Instruction::MUL,
                };
                Some((ParCombinePlan::Bin(ins), vec![0, 1]))
            }
            ParCombine::SelfCall => {
                if arms != site.param_count {
                    return None;
                }
                Some((
                    ParCombinePlan::Call {
                        entry: orig_offset,
                        arity: arms as u32,
                    },
                    (0..arms).collect(),
                ))
            }
            ParCombine::ApplyCall { fn_name } => {
                let entry = self.resolve_par_fn_entry(fn_name)? as u32;
                Some((
                    ParCombinePlan::Call {
                        entry,
                        arity: arms as u32,
                    },
                    (0..arms).collect(),
                ))
            }
            ParCombine::Tuple => Some((
                ParCombinePlan::Tuple {
                    arity: arms as u32,
                },
                (0..arms).collect(),
            )),
            ParCombine::EnumCtor {
                enum_name,
                variant_name,
            } => {
                let tag = self.checker.tag_for(enum_name, variant_name)?;
                if self.checker.arity_for(enum_name, variant_name) != Some(arms) {
                    return None;
                }
                // `MakeEnum` pops into declaration order, so arm 0 goes last.
                Some((
                    ParCombinePlan::Enum {
                        tag: u16::try_from(tag).ok()?,
                        arity: u16::try_from(arms).ok()?,
                    },
                    (0..arms).rev().collect(),
                ))
            }
        }
    }

    fn emit_par_combine(&mut self, plan: &ParCombinePlan) {
        match plan {
            ParCombinePlan::Bin(op) => self.bytecode.push(Byte::new(*op)),
            ParCombinePlan::Call { entry, arity } => self
                .bytecode
                .push(Byte::new(Instruction::CALL).with_call_packed(*arity, *entry)),
            ParCombinePlan::Tuple { arity } => self.bytecode.push_make_tuple(*arity),
            ParCombinePlan::Enum { tag, arity } => self.bytecode.push_make_enum(*tag, *arity),
        }
    }

    /// Look up a bare / FQN function entry used by IPA arms and combines.
    fn resolve_par_fn_entry(&self, name: &str) -> Option<usize> {
        self.functions
            .get(name)
            .copied()
            .or_else(|| {
                let fqn = format!("{}::{}", self.namespace, name);
                self.functions.get(&fqn).copied()
            })
            .or_else(|| {
                // Unnamespaced bare keys sometimes live next to FQNs.
                self.functions
                    .iter()
                    .find(|(k, _)| k.rsplit("::").next() == Some(name) && !k.starts_with("__coil_par_"))
                    .map(|(_, &off)| off)
            })
    }

    /// Callable for one arm: `(entry, arity, needs_push_args)`.
    ///
    /// A child level that is itself specialized is invoked as its nullary
    /// clone; otherwise the callee is called with concrete args.
    fn par_arm_callable(
        &self,
        arm: &crate::typechecking::ParArm,
        child_args: &[i64],
    ) -> Option<(u32, u32, bool)> {
        let callee = crate::typechecking::arm_callee(arm);
        if crate::typechecking::args_worth_parallel(&self.par_shapes, callee, child_args) {
            let spec = crate::typechecking::par_specialization_name(callee, child_args);
            if let Some(&off) = self.functions.get(&spec) {
                return Some((off as u32, 0, false));
            }
        }
        let entry = self.resolve_par_fn_entry(callee)? as u32;
        Some((entry, child_args.len() as u32, true))
    }

    fn emit_par_arm_call_args(&mut self, callable: (u32, u32, bool), child_args: &[i64]) {
        let (entry, arity, push_args) = callable;
        if push_args {
            for a in child_args.to_vec() {
                self.push_int_const(a);
            }
        }
        self.bytecode
            .push(Byte::new(Instruction::CALL).with_call_packed(arity, entry));
    }

    // -----------------------------------------------------------------------
    // Loop IPA (chunked fork-join over an induction range)
    // -----------------------------------------------------------------------

    /// Emit a 2-way chunked fork-join for the counted loop at `span`, replacing
    /// the sequential loop entirely. Returns `false` — with nothing emitted —
    /// when any precondition fails, so the caller falls back to the plain loop.
    ///
    /// The chunk worker is a private `(lo, hi, acc)` function holding the
    /// original body over `[lo, hi)`. `[mid, end)` is spawned onto the reactor
    /// seeded with the reduction's identity, `[begin, mid)` runs inline seeded
    /// with the live accumulator, and the partials fold with the site operator.
    fn try_emit_par_loop(
        &mut self,
        span: SimpleSpan,
        cond: &Output<'_>,
        body: &Output<'_>,
    ) -> bool {
        use crate::typechecking::LoopReduceOp;

        let Some(site) = self.loop_par_sites.get(&(span.start, span.end)).cloned() else {
            return false;
        };
        // Reassociating a float reduction changes results, so both the
        // induction variable and the per-iteration value must be `int`.
        if !self.ptr_ty_is_int(site.index_expr_ptr) || !self.ptr_ty_is_int(site.reduce_expr_ptr) {
            return false;
        }
        let (Some(index_slot), Some(acc_slot)) = (
            self.lookup_slot(&site.index),
            self.lookup_slot(&site.acc),
        ) else {
            return false;
        };
        let (Some(spawn_id), Some(join_id)) =
            (self.native_id("thread_spawn"), self.native_id("thread_join"))
        else {
            return false;
        };
        let (Ok(begin), Ok(mid), Ok(end)) = (
            i32::try_from(site.begin),
            i32::try_from(site.midpoint()),
            i32::try_from(site.end),
        ) else {
            return false;
        };
        let fold = match site.op {
            LoopReduceOp::Add => Instruction::ADD,
            LoopReduceOp::Mul => Instruction::MUL,
        };
        let identity = site.op.identity() as i32;

        // The chunk worker tests `i < hi` against its own bound, but the
        // condition's NodeIds still have to be consumed in walk order.
        self.discard_compile(cond);

        let mut bb = BlockBuilder::new();
        let after_worker = bb.fresh_label(self.bytecode.il_mut());
        bb.emit_jump_to(after_worker, BbJumpKind::Unconditional, self.bytecode.il_mut());
        let worker = self.emit_par_loop_worker(&site, body);
        bb.bind_label(after_worker, self.bytecode.il_mut());

        // MakeFn of the worker, then spawn the upper chunk.
        self.bytecode.push_const(0);
        self.bytecode
            .push(Byte::new(Instruction::CodePtr).with_operand_u32(worker));
        self.bytecode.push(
            Byte::new(Instruction::MakeFn).with_operand_u32(make_fn_operand(0, 0, 3, false)),
        );
        let fn_tmp = self.alloc_temp_slot();
        self.bytecode.push_store_pop(fn_tmp);

        let have_handle = bb.fresh_label(self.bytecode.il_mut());
        let seq = bb.fresh_label(self.bytecode.il_mut());
        let joined = bb.fresh_label(self.bytecode.il_mut());
        let done = bb.fresh_label(self.bytecode.il_mut());

        self.bytecode
            .push(Byte::new(Instruction::CONST).with_value_u32(spawn_id as u32));
        self.bytecode.push_load(fn_tmp);
        self.bytecode.push_const(mid);
        self.bytecode.push_const(end);
        self.bytecode.push_const(identity);
        self.bytecode.push_make_tuple(4);
        self.bytecode.push_host_invoke(4);
        bb.emit_jump_to(
            have_handle,
            BbJumpKind::JumpIfMatch { tag: 0, arity: 1 },
            self.bytecode.il_mut(),
        );
        self.bytecode.push_pop();
        bb.emit_jump_to(seq, BbJumpKind::Unconditional, self.bytecode.il_mut());

        bb.bind_label(have_handle, self.bytecode.il_mut());
        let handle_tmp = self.alloc_temp_slot();
        self.bytecode.push_store_pop(handle_tmp);

        // Lower chunk inline while the worker runs, seeded with the live `acc`.
        self.bytecode.push_const(begin);
        self.bytecode.push_const(mid);
        self.bytecode.push_load(acc_slot);
        self.bytecode
            .push(Byte::new(Instruction::CALL).with_call_packed(3, worker));
        let lower_tmp = self.alloc_temp_slot();
        self.bytecode.push_store_pop(lower_tmp);

        self.bytecode
            .push(Byte::new(Instruction::CONST).with_value_u32(join_id as u32));
        self.bytecode.push_load(handle_tmp);
        self.bytecode.push_make_tuple(1);
        self.bytecode.push_host_invoke(1);
        bb.emit_jump_to(
            joined,
            BbJumpKind::JumpIfMatch { tag: 0, arity: 1 },
            self.bytecode.il_mut(),
        );
        // A failed join redoes the whole range sequentially; the worker is pure,
        // so the discarded lower partial costs time but not correctness.
        self.bytecode.push_pop();
        bb.emit_jump_to(seq, BbJumpKind::Unconditional, self.bytecode.il_mut());

        bb.bind_label(joined, self.bytecode.il_mut());
        let upper_tmp = self.alloc_temp_slot();
        self.bytecode.push_store_pop(upper_tmp);
        self.bytecode.push_load(lower_tmp);
        self.bytecode.push_load(upper_tmp);
        self.bytecode.push(Byte::new(fold));
        bb.emit_jump_to(done, BbJumpKind::Unconditional, self.bytecode.il_mut());

        // Spawn / join failed: one worker call over the whole range.
        bb.bind_label(seq, self.bytecode.il_mut());
        self.bytecode.push_const(begin);
        self.bytecode.push_const(end);
        self.bytecode.push_load(acc_slot);
        self.bytecode
            .push(Byte::new(Instruction::CALL).with_call_packed(3, worker));

        bb.bind_label(done, self.bytecode.il_mut());
        self.bytecode.push_store_pop(acc_slot);
        // The loop exits with the induction variable one past its range;
        // later reads of it must not see the pre-loop value.
        self.bytecode.push_const(end);
        self.bytecode.push_store_pop(index_slot);

        true
    }

    /// Emit the chunk worker `(lo, hi, acc) -> acc'` and return its entry offset.
    ///
    /// Runs in a private frame with the induction variable, the chunk bound and
    /// the accumulator as slots 0..2 — sound only because the site analysis
    /// proved the body reads nothing else.
    fn emit_par_loop_worker(
        &mut self,
        site: &crate::typechecking::LoopParSite,
        body: &Output<'_>,
    ) -> u32 {
        const INDEX_SLOT: u32 = 0;
        const BOUND_SLOT: u32 = 1;
        const ACC_SLOT: u32 = 2;

        self.loop_par_helpers += 1;
        let name = format!("__coil_par_loop_{}", self.loop_par_helpers);
        let (entry, _) = self.bind_function_entry(name.clone());
        let entry = entry as u32;
        self.fn_arities.insert(name, (3, false));

        let prev_ctx = std::mem::take(&mut self.context);
        let prev_depth = std::mem::replace(&mut self.expr_depth, 0);
        self.context.variables.intern(site.index.clone());
        self.context.variables.intern("__coil_par_hi".to_string());
        self.context.variables.intern(site.acc.clone());

        let mut bb = BlockBuilder::new();
        let top = bb.fresh_label(self.bytecode.il_mut());
        let exit = bb.fresh_label(self.bytecode.il_mut());
        bb.bind_label(top, self.bytecode.il_mut());
        self.bytecode.push_load(INDEX_SLOT);
        self.bytecode.push_load(BOUND_SLOT);
        self.bytecode.push(Byte::new(Instruction::LE));
        bb.emit_jump_to(exit, BbJumpKind::JumpIfFalse, self.bytecode.il_mut());
        let mut body_bc = self.do_compile(body);
        self.bytecode.append(&mut body_bc);
        bb.emit_jump_to(top, BbJumpKind::Unconditional, self.bytecode.il_mut());
        bb.bind_label(exit, self.bytecode.il_mut());

        self.bytecode.push_load(ACC_SLOT);
        self.bytecode.push_return();

        self.context = prev_ctx;
        self.expr_depth = prev_depth;
        entry
    }

    /// Whether the checker inferred `int` for the expression at `ptr`.
    fn ptr_ty_is_int(&self, ptr: usize) -> bool {
        let Some(id) = self.checker.id_table().id_of_ptr(ptr) else {
            return false;
        };
        matches!(self.sidecar_ty(id), Some(Ty::Con(ref c)) if c == "int")
    }

    /// Push an `int` constant onto [`Self::bytecode`]; inline `CONST` cannot
    /// encode negatives (they collide with the pool flag) or values past `i32`.
    fn push_int_const(&mut self, n: i64) {
        if (0..=i32::MAX as i64).contains(&n) {
            self.bytecode.push_const(n as i32);
        } else {
            let bits = Value::from(n).raw() as u64;
            let idx = self.intern_constant(bits);
            self.bytecode.push_const_pool(idx);
        }
    }

    /// Emit `HostInvoke` for a pipeline-registered host native by registry name.
    fn emit_host_native_invoke(&mut self, native_name: &str, args: &[Output]) {
        let Some(native_id) = self.native_id(native_name) else {
            let range = args.first().map(|a| a.0.into_range()).unwrap_or(0..0);
            let mut message = Message::error(
                ErrorCode::UnknownFunction,
                format!("Host native `{native_name}` is not registered with the pipeline"),
                range.clone(),
            );
            if range.start >= range.end {
                message.with_help(
                    "host natives are wired in Pipeline::register_io_natives / register_thread_natives"
                        .to_string(),
                );
            } else {
                message.push(DiagLabel::new(
                    "host natives are wired in Pipeline::register_io_natives / register_thread_natives"
                        .to_string(),
                    range,
                ));
            }
            self.messages.push(message);
            return;
        };
        let depth_on_entry = self.expr_depth;
        let mut arg_slots = Vec::with_capacity(args.len());
        for arg in args {
            // Nested HostInvoke / format / match write to `self.bytecode`; also
            // fold any bytes returned in the local vec (non-host subexprs).
            let mut arg_bc = self.do_compile(arg);
            self.bytecode.append(&mut arg_bc);
            let slot = self.alloc_temp_slot();
            self.bytecode.push_store_pop(slot);
            arg_slots.push(slot);
        }
        // Native id first, then reload staged args — nested HostInvoke in
        // args must not sit above the id on the runtime stack.
        self.bytecode
            .push(Byte::new(Instruction::CONST).with_value_u32(native_id as u32));
        self.expr_depth = depth_on_entry + 1;
        for slot in &arg_slots {
            self.bytecode.push_load(*slot);
            self.expr_depth += 1;
        }
        let arity = args.len();
        self.bytecode.push_make_tuple(arity as u32);
        self.bytecode.push_host_invoke(arity as u32);
        // Result stays on the stack for the caller (ExprStatement POPs it).
        self.expr_depth = depth_on_entry;
    }

    fn emit_prelude_host_call(&mut self, args: &[Output], native_name: &str) {
        self.emit_host_native_invoke(native_name, args);
    }

    /// Emit bytecode thunks for compiler-provided primitive instances.
    ///
    /// Shared generic bodies receive boxed type-parameter values, so numeric
    /// thunks unbox their two arguments and re-box a type-parameter result.
    /// Comparison methods return their concrete `bool` result directly. Every
    /// thunk accepts the ordinary hidden trailing dictionary argument, even
    /// though primitive implementations do not need to inspect it.
    fn emit_builtin_dict_thunks(&mut self) {
        use crate::typechecking::generics::Generics;

        let emit = |compiler: &mut Self,
                    class: &str,
                    ty: &str,
                    method: &str,
                    tag: ValueTag,
                    op: Instruction,
                    boxes_result: bool| {
            let fqn = Generics::builtin_instance_fqn(class, ty, method);
            if compiler.functions.contains_key(&fqn) {
                return;
            }
            compiler.bind_function_entry(fqn);
            for slot in 0..2 {
                compiler.bytecode.push_load(slot);
                compiler.bytecode.push_unbox_value(tag as u32);
            }
            compiler.bytecode.push(Byte::new(op));
            if boxes_result {
                compiler.bytecode.push_box_value(tag as u32);
            }
            compiler.bytecode.push_return();
        };

        for (ty, tag, arithmetic, comparisons) in [
            (
                "int",
                ValueTag::Int,
                [
                    ("Add", "add", Instruction::ADD),
                    ("Sub", "sub", Instruction::SUB),
                    ("Mul", "mul", Instruction::MUL),
                    ("Div", "div", Instruction::DIV),
                ],
                [
                    ("Lt", "lt", Instruction::LE),
                    ("Le", "le", Instruction::LEQ),
                    ("Gt", "gt", Instruction::GT),
                    ("Ge", "ge", Instruction::GEQ),
                    ("Eq", "eq", Instruction::EQ),
                    ("Eq", "ne", Instruction::NEQ),
                ],
            ),
            (
                "float",
                ValueTag::Float,
                [
                    ("Add", "add", Instruction::ADDF),
                    ("Sub", "sub", Instruction::SUBF),
                    ("Mul", "mul", Instruction::MULF),
                    ("Div", "div", Instruction::DIVF),
                ],
                [
                    ("Lt", "lt", Instruction::LEF),
                    ("Le", "le", Instruction::LEQF),
                    ("Gt", "gt", Instruction::GTF),
                    ("Ge", "ge", Instruction::GEQF),
                    ("Eq", "eq", Instruction::EQ),
                    ("Eq", "ne", Instruction::NEQ),
                ],
            ),
        ] {
            for (class, method, op) in arithmetic {
                emit(self, class, ty, method, tag, op, true);
            }
            for (class, method, op) in comparisons {
                emit(self, class, ty, method, tag, op, false);
            }
        }
        for (ty, tag) in [
            ("string", ValueTag::String),
            ("bool", ValueTag::Bool),
            ("byte", ValueTag::Int),
        ] {
            emit(self, "Eq", ty, "eq", tag, Instruction::EQ, false);
            emit(self, "Eq", ty, "ne", tag, Instruction::NEQ, false);
        }

        // Show thunks: accept a boxed (or heap-string) argument at slot 0,
        // ignore the trailing dictionary, and return an ObjString via STRINGIFY.
        for ty in ["int", "float", "string", "bool", "unit", "byte"] {
            let fqn = Generics::builtin_instance_fqn("Show", ty, "show");
            if self.functions.contains_key(&fqn) {
                continue;
            }
            self.bind_function_entry(fqn);
            self.bytecode.push_load(0);
            self.bytecode.push(Byte::new(Instruction::STRINGIFY));
            self.bytecode.push_return();
        }

        // Length__string__len: unbox (dict ABI) then ArrayLen (byte length).
        {
            let fqn = Generics::builtin_instance_fqn("Length", "string", "len");
            if !self.functions.contains_key(&fqn) {
                self.bind_function_entry(fqn);
                self.bytecode.push_load(0);
                self.bytecode.push_unbox_value(ValueTag::String as u32);
                self.bytecode.push(Byte::new(Instruction::ArrayLen));
                self.bytecode.push_return();
            }
        }

        // Hash thunks: boxed receiver at slot 0 → int. int/byte/bool identity
        // after unbox; float returns the float `Value` bits read via
        // `Value::as_int()` (IEEE bit pattern in the current Value encoding);
        // unit is 0; string uses the intern FNV via HostInvoke `hash_string`.
        for (ty, tag) in [
            ("int", ValueTag::Int),
            ("byte", ValueTag::Int),
            ("bool", ValueTag::Bool),
            ("float", ValueTag::Float),
        ] {
            let fqn = Generics::builtin_instance_fqn("Hash", ty, "hash");
            if self.functions.contains_key(&fqn) {
                continue;
            }
            self.bind_function_entry(fqn);
            self.bytecode.push_load(0);
            self.bytecode.push_unbox_value(tag as u32);
            self.bytecode.push_return();
        }
        {
            let fqn = Generics::builtin_instance_fqn("Hash", "unit", "hash");
            if !self.functions.contains_key(&fqn) {
                self.bind_function_entry(fqn);
                self.bytecode.push_const(0);
                self.bytecode.push_return();
            }
        }
        if let Some(native_id) = self.native_id("hash_string") {
            let fqn = Generics::builtin_instance_fqn("Hash", "string", "hash");
            if !self.functions.contains_key(&fqn) {
                self.bind_function_entry(fqn);
                self.bytecode
                    .push(Byte::new(Instruction::CONST).with_value_u32(native_id as u32));
                self.bytecode.push_load(0);
                self.bytecode.push_unbox_value(ValueTag::String as u32);
                self.bytecode.push_make_tuple(1);
                self.bytecode.push_host_invoke(1);
                self.bytecode.push_return();
            }
        }

        // Read/Write for Stream — lower to the same HostInvoke natives as
        // free functions `read` / `write`. Args may arrive boxed via the
        // dictionary ABI; unbox then call. `Vec<byte>` shares the Array
        // carrier tag with dynamic arrays.
        for (class, method, native_name, arity) in [
            ("Read", "read", "read", 2u32),
            ("Write", "write", "write", 2u32),
        ] {
            let fqn = Generics::builtin_instance_fqn(class, "Stream", method);
            if self.functions.contains_key(&fqn) {
                continue;
            }
            let Some(native_id) = self.native_id(native_name) else {
                continue;
            };
            self.bind_function_entry(fqn);
            self.bytecode
                .push(Byte::new(Instruction::CONST).with_value_u32(native_id as u32));
            self.bytecode.push_load(0);
            self.bytecode.push_unbox_value(ValueTag::Instance as u32);
            self.bytecode.push_load(1);
            self.bytecode.push_unbox_value(ValueTag::Array as u32);
            self.bytecode.push_make_tuple(arity);
            self.bytecode.push_host_invoke(arity);
            self.bytecode.push_return();
        }

        let into_pairs = [
            (
                "int",
                "float",
                Instruction::CastIntToFloat,
                ValueTag::Int,
                ValueTag::Float,
            ),
            (
                "float",
                "int",
                Instruction::CastFloatToInt,
                ValueTag::Float,
                ValueTag::Int,
            ),
            (
                "int",
                "byte",
                Instruction::CastIntToByte,
                ValueTag::Int,
                ValueTag::Int,
            ),
            (
                "byte",
                "int",
                Instruction::CastByteToInt,
                ValueTag::Int,
                ValueTag::Int,
            ),
            (
                "int",
                "bool",
                Instruction::CastIntToBool,
                ValueTag::Int,
                ValueTag::Bool,
            ),
            (
                "bool",
                "int",
                Instruction::CastBoolToInt,
                ValueTag::Bool,
                ValueTag::Int,
            ),
        ];
        for (from, to, cast_op, from_tag, to_tag) in into_pairs {
            let fqn = into_primitive_fqn(from, to);
            if self.functions.contains_key(&fqn) {
                continue;
            }
            self.bind_function_entry(fqn);
            self.bytecode.push_load(0);
            self.bytecode.push_unbox_value(from_tag as u32);
            self.bytecode.push(Byte::new(cast_op));
            if from_tag != to_tag {
                self.bytecode.push_box_value(to_tag as u32);
            }
            self.bytecode.push_return();
        }
    }

    /// Emit intrinsic bodies for builtin `Vec<T>` methods and register
    /// them in the function / method tables so `v.push(x)` / `Vec::new()`
    /// lower to direct `CALL`s.
    fn emit_vec_method_thunks(&mut self) {
        let owner = common::BUILTIN_VEC_TYPE;
        let methods = self.context.methods.entry(owner.to_string()).or_default();
        for name in [
            "push",
            "pop",
            "insert",
            "remove",
            "clear",
            "reserve",
            "capacity",
            "len",
            "new",
            "with_capacity",
            "from",
        ] {
            methods.insert(name.to_string(), format!("{owner}::{name}"));
        }

        let emit_host = |compiler: &mut Self, fqn: String, native: &str, slots: &[u32]| {
            if compiler.functions.contains_key(&fqn) {
                return;
            }
            let Some(native_id) = compiler.native_id(native) else {
                return;
            };
            compiler.bind_function_entry(fqn);
            compiler
                .bytecode
                .push(Byte::new(Instruction::CONST).with_value_u32(native_id as u32));
            for &slot in slots {
                compiler.bytecode.push_load(slot);
            }
            compiler
                .bytecode
                .push_make_tuple(slots.len() as u32);
            compiler
                .bytecode
                .push_host_invoke(slots.len() as u32);
            compiler.bytecode.push_return();
        };

        // static fn new() -> Vec<T>
        {
            let fqn = format!("{owner}::new");
            if !self.functions.contains_key(&fqn) {
                self.bind_function_entry(fqn);
                self.bytecode.push_make_array(0);
                self.bytecode.push_return();
            }
        }

        // static fn with_capacity(n) -> Vec<T>
        emit_host(self, format!("{owner}::with_capacity"), "vec_with_capacity", &[0]);

        // static fn from(arr) -> Vec<T>
        emit_host(self, format!("{owner}::from"), "vec_from_array", &[0]);

        // fn push(x)
        {
            let fqn = format!("{owner}::push");
            if !self.functions.contains_key(&fqn) {
                self.bind_function_entry(fqn);
                self.bytecode.push_load(0);
                self.bytecode.push_load(1);
                self.bytecode
                    .push(Byte::new(Instruction::ArrayPush));
                self.bytecode.push_pop();
                self.bytecode.push_const(0);
                self.bytecode.push_return();
            }
        }

        // fn len() / capacity() / clear() / pop() / remove(i) / reserve(n) / insert(i, x)
        {
            let fqn = format!("{owner}::len");
            if !self.functions.contains_key(&fqn) {
                self.bind_function_entry(fqn);
                self.bytecode.push_load(0);
                self.bytecode
                    .push(Byte::new(Instruction::ArrayLen));
                self.bytecode.push_return();
            }
        }
        emit_host(self, format!("{owner}::capacity"), "vec_capacity", &[0]);
        emit_host(self, format!("{owner}::clear"), "vec_clear", &[0]);
        emit_host(self, format!("{owner}::pop"), "vec_pop", &[0]);
        emit_host(self, format!("{owner}::remove"), "vec_remove", &[0, 1]);
        emit_host(self, format!("{owner}::reserve"), "vec_reserve", &[0, 1]);
        emit_host(self, format!("{owner}::insert"), "vec_insert", &[0, 1, 2]);
        self.emit_range_method_thunks();
    }

    /// Inherent `Range::to_vec` / `RangeInclusive::to_vec` bodies.
    ///
    /// Unpacks the runtime dict `{start,end,inclusive}` and fills a `Vec`
    /// with the same step as `for` (`+1` / `+1.0`). Float uses a sibling
    /// `__float_to_vec` thunk selected at the call site.
    fn emit_range_method_thunks(&mut self) {
        for owner in ["Range", "RangeInclusive"] {
            let methods = self.context.methods.entry(owner.to_string()).or_default();
            methods.insert("to_vec".to_string(), format!("{owner}::to_vec"));
        }
        self.emit_range_to_vec_thunk("Range::to_vec".into(), false, false);
        self.emit_range_to_vec_thunk("Range::__float_to_vec".into(), false, true);
        self.emit_range_to_vec_thunk("RangeInclusive::to_vec".into(), true, false);
        self.emit_range_to_vec_thunk("RangeInclusive::__float_to_vec".into(), true, true);
    }

    /// Inherent `Stream::attach` / `Stream::park` bodies (HostInvoke thunks).
    fn emit_stream_method_thunks(&mut self) {
        let owner = crate::typechecking::ty::STREAM;
        let methods = self.context.methods.entry(owner.to_string()).or_default();
        methods.insert("attach".to_string(), format!("{owner}::attach"));
        methods.insert("park".to_string(), format!("{owner}::park"));

        let emit_host = |compiler: &mut Self, fqn: String, native: &str, slots: &[u32]| {
            if compiler.functions.contains_key(&fqn) {
                return;
            }
            let Some(native_id) = compiler.native_id(native) else {
                return;
            };
            compiler.bind_function_entry(fqn);
            compiler
                .bytecode
                .push(Byte::new(Instruction::CONST).with_value_u32(native_id as u32));
            for &slot in slots {
                compiler.bytecode.push_load(slot);
            }
            compiler.bytecode.push_make_tuple(slots.len() as u32);
            compiler.bytecode.push_host_invoke(slots.len() as u32);
            compiler.bytecode.push_return();
        };
        emit_host(
            self,
            format!("{owner}::attach"),
            common::STREAM_ATTACH_NATIVE,
            &[0, 1, 2, 3, 4, 5],
        );
        emit_host(
            self,
            format!("{owner}::park"),
            common::STREAM_PARK_NATIVE,
            &[0],
        );
    }

    fn emit_range_to_vec_thunk(&mut self, fqn: String, inclusive: bool, float: bool) {
        if self.functions.contains_key(&fqn) {
            return;
        }
        self.bind_function_entry(fqn);
        // slot 0 = self (range dict); 1 = cur; 2 = end; 3 = out vec
        let start_idx = self.intern_string("start");
        self.bytecode.push_load(0);
        self.bytecode.push_string(start_idx);
        self.bytecode.push_get_field();
        self.bytecode.push_store_pop(1);

        let end_idx = self.intern_string("end");
        self.bytecode.push_load(0);
        self.bytecode.push_string(end_idx);
        self.bytecode.push_get_field();
        self.bytecode.push_store_pop(2);

        self.bytecode.push_make_array(0);
        self.bytecode.push_store_pop(3);

        let mut bb = BlockBuilder::new();
        let top_label = bb.fresh_label(self.bytecode.il_mut());
        let exit_label = bb.fresh_label(self.bytecode.il_mut());
        bb.bind_label(top_label, self.bytecode.il_mut());

        self.bytecode.push_load(1);
        self.bytecode.push_load(2);
        self.bytecode.push(Byte::new(if float {
            if inclusive {
                Instruction::LEQF
            } else {
                Instruction::LEF
            }
        } else if inclusive {
            Instruction::LEQ
        } else {
            Instruction::LE
        }));
        bb.emit_jump_to(exit_label, BbJumpKind::JumpIfFalse, self.bytecode.il_mut());

        self.bytecode.push_load(3);
        self.bytecode.push_load(1);
        self.bytecode.push(Byte::new(Instruction::ArrayPush));
        self.bytecode.push_store_pop(3);

        self.bytecode.push_load(1);
        if float {
            let bits = Value::from(1.0_f64).raw() as u64;
            let idx = self.intern_constant(bits);
            self.bytecode.push_const_pool(idx);
            self.bytecode.push(Byte::new(Instruction::ADDF));
        } else {
            self.bytecode.push_const(1);
            self.bytecode.push(Byte::new(Instruction::ADD));
        }
        self.bytecode.push_store_pop(1);

        bb.emit_jump_to(top_label, BbJumpKind::Unconditional, self.bytecode.il_mut());
        bb.bind_label(exit_label, self.bytecode.il_mut());

        self.bytecode.push_load(3);
        self.bytecode.push_return();
    }

    /// Map a fully-resolved `Ty` to a `ValueTag` for box/unbox
    /// emission at generic call boundaries.
    fn ty_to_value_tag(ty: &crate::typechecking::Ty) -> Option<ValueTag> {
        use crate::typechecking::{
            Ty, ty::BOOL, ty::BYTE, ty::FLOAT, ty::INT, ty::STRING, ty::UNIT,
        };
        match ty {
            Ty::Con(name) => match name.as_str() {
                INT | BYTE => Some(ValueTag::Int),
                FLOAT => Some(ValueTag::Float),
                STRING => Some(ValueTag::String),
                BOOL => Some(ValueTag::Bool),
                UNIT => Some(ValueTag::Unit),
                _ => Some(ValueTag::Instance), // user-defined class / enum
            },
            // Same carrier ABI as `Con(enum)` — trait methods unbox Instance.
            Ty::Sum { .. } => Some(ValueTag::Instance),
            // Variant refinements box like their owning enum.
            Ty::Constructor { owner, .. } => Self::ty_to_value_tag(owner),
            Ty::Tuple(_) => Some(ValueTag::Tuple),
            Ty::Array { .. } => Some(ValueTag::Array),
            Ty::App(head, _) => match head.as_ref() {
                Ty::Con(n) if n == common::BUILTIN_VEC_TYPE => Some(ValueTag::Array),
                Ty::Con(n) if n == "Range" || n == "RangeInclusive" => Some(ValueTag::Record),
                // Option / Result / user ADT apps share the Instance box tag.
                Ty::Con(_) => Some(ValueTag::Instance),
                _ => None,
            },
            Ty::Record { .. } => Some(ValueTag::Record),
            // Open type vars — boxing is required but we don't know the tag yet
            Ty::Var(_) => None,
            _ => None,
        }
    }

    fn range_to_vec_elem_is_float(&self, recv_ty: Option<&Ty>) -> bool {
        recv_ty
            .map(|ty| crate::typechecking::subst::apply_ty_prune(self.checker.subst(), ty))
            .as_ref()
            .and_then(crate::typechecking::ty::range_app)
            .is_some_and(|(elem, _)| matches!(elem, Ty::Con(n) if n == "float"))
    }

    /// Peel `forall` / function arrows to the final return type.
    #[allow(dead_code)]
    fn peel_fn_return_ty(ty: &crate::typechecking::Ty) -> crate::typechecking::Ty {
        use crate::typechecking::Ty;
        let mut t = ty.clone();
        while let Ty::Forall { body, .. } = t {
            t = *body;
        }
        while let Ty::Fun(_, ret) = t {
            t = *ret;
        }
        t
    }

    /// Look up a function's scheme and peel to its return type.
    /// Lookup declared return type for diagnostics / tooling.
    #[allow(dead_code)]
    fn fn_return_ty(&self, name: &str) -> Option<crate::typechecking::Ty> {
        use crate::typechecking::subst::apply_ty_prune;
        let scheme = self
            .checker
            .env()
            .lookup(name)
            .or_else(|| {
                self.current_function_qualified
                    .as_deref()
                    .and_then(|q| self.checker.env().lookup(q))
            })
            .or_else(|| {
                self.current_function_table_key
                    .as_deref()
                    .and_then(|q| self.checker.env().lookup(q))
            })?;
        let applied = apply_ty_prune(self.checker.subst(), &scheme.ty);
        Some(Self::peel_fn_return_ty(&applied))
    }

    fn pair_value_ty_supported(ty: &Ty) -> bool {
        match ty {
            Ty::Var(_) | Ty::Fun(_, _) | Ty::Existential { .. } | Ty::Forall { .. } => false,
            Ty::Array {
                length: crate::typechecking::ty::ArrayLength::Static(_),
                ..
            } => false,
            Ty::Readonly(inner) | Ty::Constructor { owner: inner, .. } => {
                Self::pair_value_ty_supported(inner)
            }
            Ty::App(_, args) => args.iter().all(Self::pair_value_ty_supported),
            Ty::List(inner) => Self::pair_value_ty_supported(inner),
            Ty::Sum { variants, .. } => variants.iter().all(|(_, payload)| {
                payload
                    .field_types()
                    .into_iter()
                    .all(Self::pair_value_ty_supported)
            }),
            Ty::Tuple(items) => items.iter().all(Self::pair_value_ty_supported),
            Ty::Record { fields } => fields
                .iter()
                .all(|(_, field)| Self::pair_value_ty_supported(field)),
            Ty::Array {
                length: crate::typechecking::ty::ArrayLength::Dynamic,
                ..
            }
            | Ty::Con(_)
            | Ty::Never => true,
        }
    }

    /// Return `Some(is_option)` for a compiled function whose unary return
    /// can use the pair ABI. Pointer-niche options stay on the niche path.
    fn pair_return_kind(&self, name: &str) -> Option<bool> {
        if let Some(cached) = self.pair_return_kinds.borrow().get(name) {
            return *cached;
        }
        if let Some(abi) = self.sidecar_pair_niche(name) {
            let kind = match abi {
                PairNicheAbi::PairResult => Some(false),
                PairNicheAbi::PairOption => Some(true),
                PairNicheAbi::NicheOption => None,
            };
            self.pair_return_kinds
                .borrow_mut()
                .insert(name.to_string(), kind);
            return kind;
        }
        let kind = self.compute_pair_return_kind(name);
        self.pair_return_kinds
            .borrow_mut()
            .insert(name.to_string(), kind);
        kind
    }

    /// Pin a verdict so later queries cannot disagree with it. Definition sites
    /// use this for shapes only they can see (a coroutine body never returns a
    /// pair, however its return type reads).
    fn pin_pair_return_kind(&self, name: &str, kind: Option<bool>) {
        self.pair_return_kinds
            .borrow_mut()
            .insert(name.to_string(), kind);
    }

    fn compute_pair_return_kind(&self, _name: &str) -> Option<bool> {
        None
    }

    /// Emit `ReturnPair`, tagging it with the enclosing function's pair kind so
    /// the VM can re-box the two slots when the frame is a host entry.
    fn push_return_pair(&mut self) {
        self.bytecode.push(
            Byte::new(Instruction::ReturnPair)
                .with_operand_u32(u32::from(self.compiling_pair_is_option)),
        );
    }

    /// Box a unary `Option`/`Result` call return before storing or re-matching.
    fn emit_pair_to_heap_after_call(&self, bytecode: &mut CodeBuf, lookup_name: &str) {
        if let Some(is_option) = self.pair_return_kind(lookup_name)
            && !self.pair_value_context
        {
            bytecode.push(
                Byte::new(Instruction::PairToHeap)
                    .with_operand_u32(u32::from(is_option)),
            );
        }
    }

    fn pair_call_candidate(&self, callee: &Output) -> bool {
        self.pair_call_kind(callee).is_some()
    }

    /// `Some(is_option)` when calling `callee` leaves a pair on the stack.
    fn pair_call_kind(&self, callee: &Output) -> Option<bool> {
        let name = match callee.1.as_ref() {
            Expression::Identifier(name) => self.resolve_free_fn(name),
            Expression::QualifiedAccess { owner, member } => {
                format!("{}::{}", owner, member)
            }
            Expression::Access(receiver, method) => {
                let ty = self.receiver_type(receiver).or_else(|| self.codegen_expr_ty(receiver));
                let owner = ty
                    .as_ref()
                    .and_then(Checker::class_name_of_ty)
                    .map(str::to_string)?;
                self.context
                    .methods
                    .get(&owner)
                    .and_then(|m| m.get(*method))
                    .cloned()
                    .unwrap_or_else(|| format!("{}::{}", owner, method))
            }
            _ => return None,
        };
        if !self.functions.contains_key(&name) && !self.fn_entry_labels.contains_key(&name) {
            return None;
        }
        self.pair_return_kind(&name)
    }

    fn expr_is_pair_producer(&self, expr: &Output) -> bool {
        self.expr_pair_producer_kind(expr).is_some()
    }

    /// `Some(is_option)` when the expression is emitted in the pair ABI, i.e. it
    /// leaves `[payload, tag]` rather than a heap enum.
    fn expr_pair_producer_kind(&self, _expr: &Output) -> Option<bool> {
        None
    }

    /// True when the expression yields a pair that *is* the enclosing function's
    /// return enum, so its tag can serve as the `ReturnPair` tag. An enum of the
    /// other kind — or a same-kind enum nested one level down, as in
    /// `Result<Result<int, E>, E>` — is an ordinary payload and must be boxed.
    fn expr_pairs_with_return(&self, expr: &Output) -> bool {
        self.compiling_pair_mode
            && self.expr_pair_producer_kind(expr) == Some(self.compiling_pair_is_option)
            && self.expr_ty_is_return_ty(expr)
    }

    /// `Some(is_option)` when a heap `expr` holds the very enum the enclosing
    /// pair-mode function returns, so it can be split into `[payload, tag]`
    /// rather than boxed as an `Ok`/`Some` payload.
    fn expr_is_return_enum(&self, expr: &Output) -> Option<bool> {
        let kind = self.expr_pair_enum_kind(expr)?;
        (kind == self.compiling_pair_is_option && self.expr_ty_is_return_ty(expr)).then_some(kind)
    }

    /// Whether the expression's type is the enclosing function's return type.
    /// Unknown on either side counts as a match: the caller has already checked
    /// the enum kind, and that is then all the evidence there is.
    fn expr_ty_is_return_ty(&self, expr: &Output) -> bool {
        match (self.codegen_expr_ty(expr), self.compiling_fn_return_ty()) {
            (Some(expr_ty), Some(ret_ty)) => Self::enum_nesting_eq(&expr_ty, &ret_ty),
            _ => true,
        }
    }

    /// Compare two types by how deeply `Option`/`Result` nest in them, so that
    /// `Result<int, E>` and `Result<Result<int, E>, E>` are told apart. Peeling
    /// with the `ty` accessors keeps this blind to which shape (`App`, `Sum`,
    /// `Constructor`) each side happens to carry.
    fn enum_nesting_eq(a: &Ty, b: &Ty) -> bool {
        use crate::typechecking::ty::{option_inner, result_ok_err};
        match (result_ok_err(a), result_ok_err(b)) {
            (Some((a_ok, _)), Some((b_ok, _))) => return Self::enum_nesting_eq(&a_ok, &b_ok),
            (Some(_), None) | (None, Some(_)) => return false,
            (None, None) => {}
        }
        match (option_inner(a), option_inner(b)) {
            (Some(a_inner), Some(b_inner)) => Self::enum_nesting_eq(&a_inner, &b_inner),
            (Some(_), None) | (None, Some(_)) => false,
            (None, None) => a == b,
        }
    }

    /// Return type of the function whose body is being compiled.
    fn compiling_fn_return_ty(&self) -> Option<crate::typechecking::Ty> {
        let name = self
            .current_function_qualified
            .as_deref()
            .or(self.current_function_table_key.as_deref())?;
        self.checker
            .fn_return_ty(name)
            .or_else(|| self.fn_return_ty(name))
    }

    fn expr_pair_enum_kind(&self, _expr: &Output) -> Option<bool> {
        None
    }

    fn emit_host_option_boundary(&mut self, _expr: &Output) {}

    /// Emit defers + unit fall-through return when a body does not end in a return.
    ///
    /// Non-unit missing returns are diagnosed by HM (E0111). This epilogue only
    /// invents a unit/`0` sentinel (plus Result Ok-wrap in result-mode) so frames
    /// unwind and defers run. No Option/`None` invent.
    fn emit_fallthrough_return(&mut self, _name: &str, _span: SimpleSpan) {
        self.emit_run_defers();
        self.bytecode.push_const(0);
        if self.compiling_pair_mode {
            self.bytecode.push_const(0);
            self.push_return_pair();
        } else if self.compiling_result_mode {
            Self::emit_ok_or_some_wrap(&mut self.bytecode, false);
            self.bytecode.push_return();
        } else {
            self.bytecode.push_return();
        }
    }

    /// True when IL ops in `[op_start, ops.len())` end with a return terminator
    /// (labels skipped). `op_start` must be an index into [`CodeBuf::ops`], not
    /// an emitting-code length from [`CodeBuf::len`].
    fn region_ends_with_return(&self, op_start: usize) -> bool {
        let ops = self.bytecode.ops();
        let mut i = ops.len();
        while i > op_start {
            i -= 1;
            match &ops[i] {
                IlOp::Label(_) => continue,
                IlOp::Return { .. }
                | IlOp::LoadReturnSlot { .. }
                | IlOp::ConstReturnImm { .. }
                | IlOp::BinReturn { .. }
                | IlOp::Halt { .. } => return true,
                IlOp::Byte { byte, .. } if *byte.bytecode() == Instruction::ReturnPair => {
                    return true;
                }
                op if op.is_plain_return() => return true,
                _ => return false,
            }
        }
        false
    }

    /// Emit a `BoxValue` instruction for a concrete `Ty` at a generic
    /// call argument boundary (concrete→generic).  Does nothing when the
    /// type is already open (Ty::Var), or if a tag cannot be determined.
    fn emit_box_if_needed(bytecode: &mut impl EmitBuf, ty: &crate::typechecking::Ty) {
        if let Some(tag) = Self::ty_to_value_tag(ty) {
            bytecode.push_box_value(tag as u32);
        }
    }

    fn emit_generic_arg_box(&self, bytecode: &mut impl EmitBuf, ty: &Ty) {
        Self::emit_box_if_needed(bytecode, ty);
    }

    /// Emit an `UnboxValue` instruction for a concrete `Ty` at a generic
    /// call return boundary (generic→concrete).  Does nothing when the
    /// type is open (`Ty::Var`) — the caller can't know the tag at compile
    /// time in that case (the boxed value stays boxed).
    fn emit_unbox_if_needed(bytecode: &mut CodeBuf, ty: &crate::typechecking::Ty) {
        if let Some(tag) = Self::ty_to_value_tag(ty) {
            // UnboxValue operand: [15:0] = ValueTag as u16.
            bytecode.push_unbox_value(tag as u32);
        }
    }

    fn compile_function_output_with_name<'compiler>(
        &mut self,
        method: &Output<'compiler>,
        qualified: String,
        argument_unbox_tys: &[Option<Ty>],
        dict_arity: usize,
    ) {
        let _method_id = self.next_emit_id();
        let Expression::Function {
            docs: _,
            name,
            is_coro,
            args,
            body,
            ..
        } = method.1.as_ref()
        else {
            let mut bc = self.do_compile(method);
            self.bytecode.append(&mut bc);
            return;
        };
        let Some(body) = body else {
            self.consume_function_signature_output(method);
            return;
        };

        self.bind_function_entry(qualified.clone());
        if *is_coro {
            self.coroutine_fns.insert(qualified.clone());
        }

        let prev_vars = std::mem::take(&mut self.context.variables);
        let prev_polyfn_vars = std::mem::take(&mut self.polyfn_vars);
        let prev_polyfn_sources = std::mem::take(&mut self.polyfn_sources);
        let prev_fn_table_key = self.current_function_table_key.take();
        self.current_function_table_key = Some(qualified.clone());
        self.context.variables = Interner::default();
        if self.compiling_method {
            let slot = self.context.variables.intern("self".to_string()) as u32;
            self.record_debug_local("self", slot);
        }

        let prev_result_mode = self.compiling_result_mode;
        let prev_result_ok_is_result = self.compiling_result_ok_is_result;
        self.compiling_result_mode = self.checker.fn_is_result_mode(name);
        self.compiling_result_ok_is_result = self.checker.fn_result_ok_is_result(name);
        let prev_pair_mode = self.compiling_pair_mode;
        let prev_pair_is_option = self.compiling_pair_is_option;
        let pair_kind = if *is_coro {
            self.pin_pair_return_kind(&qualified, None);
            None
        } else {
            self.pair_return_kind(&qualified)
        };
        self.compiling_pair_mode = pair_kind.is_some();
        self.compiling_pair_is_option = pair_kind.unwrap_or(false);
        let prev_fn_defers = std::mem::take(&mut self.fn_defers);

        let mut a = self.do_compile(args);
        self.bytecode.append(&mut a);
        for (slot, ty) in argument_unbox_tys.iter().enumerate() {
            if let Some(tag) = ty.as_ref().and_then(Self::ty_to_value_tag) {
                self.bytecode.push_load(slot as u32);
                self.bytecode.push_unbox_value(tag as u32);
                self.bytecode.push_store_pop(slot as u32);
            }
        }
        for dict_idx in 0..dict_arity {
            self.context.variables.intern(format!("__dict{}", dict_idx));
        }
        let body_op_start = self.bytecode.ops().len();
        let mut c = self.do_compile(body);
        self.bytecode.append(&mut c);

        if !self.region_ends_with_return(body_op_start) {
            self.emit_fallthrough_return(name, body.0);
        }

        self.fn_defers = prev_fn_defers;
        self.compiling_result_mode = prev_result_mode;
        self.compiling_result_ok_is_result = prev_result_ok_is_result;
        self.compiling_pair_mode = prev_pair_mode;
        self.compiling_pair_is_option = prev_pair_is_option;
        self.context.variables = prev_vars;
        self.polyfn_vars = prev_polyfn_vars;
        self.polyfn_sources = prev_polyfn_sources;
        self.current_function_table_key = prev_fn_table_key;
    }

    fn instance_method_unbox_tys(
        &self,
        class: &str,
        method: &str,
        instance_args: &[Ty],
    ) -> Vec<Option<Ty>> {
        let Some(scheme) = self.checker.typeclass_method_scheme(class, method) else {
            return Vec::new();
        };
        let mut result = Vec::new();
        let mut current = &scheme.ty;
        while let Ty::Fun(param, ret) = current {
            let concrete = match param.as_ref() {
                Ty::Var(var) => scheme
                    .bounds
                    .iter()
                    .position(|bound| bound == var)
                    .and_then(|index| instance_args.get(index))
                    .cloned(),
                _ => None,
            };
            result.push(concrete);
            current = ret;
        }
        result
    }

    fn generic_return_depends_on_type_param(&self, name: &str) -> bool {
        let Some(scheme) = self.checker.env().lookup(name) else {
            return false;
        };
        let mut result = &scheme.ty;
        while let Ty::Fun(_, ret) = result {
            result = ret;
        }
        let free = crate::typechecking::subst::ftv(result);
        scheme.bounds.iter().any(|bound| free.contains(bound))
    }

    /// Whether a generic function has at least one bare type-parameter
    /// argument (`T` rather than `[T]` / `F<T>`). Those positions use the
    /// boxed shared-body ABI; nested ADT params do not.
    fn generic_has_toplevel_type_param_args(&self, name: &str) -> bool {
        let Some(scheme) = self.checker.env().lookup(name) else {
            return false;
        };
        let mut current = &scheme.ty;
        while let crate::typechecking::Ty::Fun(param, ret) = current {
            if let crate::typechecking::Ty::Var(v) = param.as_ref() {
                if scheme.bounds.iter().any(|b| b == v) {
                    return true;
                }
            }
            current = ret;
        }
        false
    }

    /// Whether a generic call's return value is boxed at the ABI boundary.
    ///
    /// Direct type-parameter arguments (`id<T>(T x) -> T`) are boxed at the
    /// call site, so the matching return must be unboxed. Type parameters that
    /// only appear nested under ADTs / HKT apps (`get<F, A>(F<A>) -> A`) keep
    /// the payload's native representation (e.g. a raw `int` inside
    /// `Option::Some`), so emitting `UnboxValue` would turn a valid immediate
    /// into `Value::default()`.
    fn generic_return_is_boxed(&self, name: &str) -> bool {
        let Some(scheme) = self.checker.env().lookup(name) else {
            return false;
        };
        let mut top_level_vars = std::collections::HashSet::new();
        let mut current = &scheme.ty;
        while let Ty::Fun(param, ret) = current {
            if let Ty::Var(v) = param.as_ref() {
                top_level_vars.insert(*v);
            }
            current = ret;
        }
        // Only a bare type-param return (`id<T>(T) -> T`) is boxed at the ABI
        // boundary. Nested ADTs (`Option<T>`, `Vec<T>`) keep their native
        // representation — UnboxValue would corrupt the heap object.
        match current {
            Ty::Var(v) => scheme.bounds.iter().any(|b| b == v) && top_level_vars.contains(v),
            _ => false,
        }
    }

    /// Whether CallIndirect args through `local` must be `BoxValue`'d.
    ///
    /// Bare `let f = show` sets [`Self::polyfn_sources`]. Locals assigned from a
    /// *call* that returns a PolyFn (`let f = capture_show(0)`) are seeded into
    /// [`Self::polyfn_vars`] from the binder's span in
    /// [`Checker::is_polyfn_binding_at`] when the let is emitted. Both sets are
    /// snapshotted around `{ … }` blocks so an inner PolyFn cannot poison an
    /// outer same-named ObjFn. Mono partials / lambdas stay unboxed.
    fn local_call_needs_arg_boxing(&self, local: &str) -> bool {
        // Only the codegen-scoped sets — never a flat name table (that would
        // leak across `{ … }` block shadows).
        self.polyfn_sources.contains_key(local) || self.polyfn_vars.contains(local)
    }

    /// Whether a CallIndirect through a local needs a post-call `UnboxValue`.
    ///
    /// Direct `polyfn_sources` mappings consult the original generic scheme.
    /// Returned/captured PolyFns and rank-n parameters fall back to the local's
    /// recorded type: unbox only when that type's result is still a type
    /// parameter (boxed at runtime) and the call site resolved it concretely.
    fn local_polyfn_var_ty(&self, local: &str, span: Option<(usize, usize)>) -> Option<Ty> {
        use crate::typechecking::subst::apply_ty_prune;
        if let Some((start, end)) = span {
            let _ = (start, end);
        }
        for frame in self.mono_codegen_var_types.iter().rev() {
            if let Some(ty) = frame.get(local) {
                return Some(apply_ty_prune(self.checker.subst(), ty));
            }
        }
        self.checker
            .codegen_var_type(local)
            .map(|ty| apply_ty_prune(self.checker.subst(), ty))
    }

    fn local_polyfn_call_needs_unbox(&self, local: &str, span: Option<(usize, usize)>) -> bool {
        use crate::typechecking::subst::apply_ty_prune;

        if let Some(source) = self.polyfn_sources.get(local) {
            return self.generic_return_depends_on_type_param(source);
        }
        // Prefer the binder / env type over the call-site identifier span:
        // rank-n `f` at `f(x)` may already be recorded as `int -> int`, which
        // would hide that the PolyFn ABI still returns a boxed type param.
        let binder_ty = {
            let mut found = None;
            for frame in self.mono_codegen_var_types.iter().rev() {
                if let Some(ty) = frame.get(local) {
                    found = Some(apply_ty_prune(self.checker.subst(), ty));
                    break;
                }
            }
            found.or_else(|| {
                self.checker
                    .codegen_var_type(local)
                    .map(|ty| apply_ty_prune(self.checker.subst(), ty))
            })
        };
        let var_ty = binder_ty.or_else(|| self.local_polyfn_var_ty(local, span));
        let Some(var_ty) = var_ty else {
            return false;
        };
        let pruned = apply_ty_prune(self.checker.subst(), &var_ty);
        let result_ty = match &pruned {
            Ty::Forall { body, .. } => {
                let mut result = body.as_ref();
                while let Ty::Fun(_, ret) = result {
                    result = ret.as_ref();
                }
                result.clone()
            }
            other => {
                let mut result = other;
                while let Ty::Fun(_, ret) = result {
                    result = ret.as_ref();
                }
                result.clone()
            }
        };
        matches!(result_ty, Ty::Var(_))
    }

    /// Instantiate a (possibly `forall`) function type against concrete arg
    /// types and return the result type after binding.
    fn instantiate_polyfn_app_result(fun_ty: &Ty, arg_tys: &[Ty]) -> Option<Ty> {
        let mut peeled = fun_ty;
        while let Ty::Forall { body, .. } = peeled {
            peeled = body.as_ref();
        }
        let mut map: HashMap<crate::typechecking::ty::TyVarId, Ty> = HashMap::new();
        let mut current = peeled;
        for arg in arg_tys {
            match current {
                Ty::Fun(param, ret) => {
                    Self::bind_scheme_vars(param.as_ref(), arg, &mut map);
                    current = ret.as_ref();
                }
                _ => return None,
            }
        }
        Some(Self::apply_ty_var_map(current, &map))
    }

    fn apply_ty_var_map(
        ty: &Ty,
        map: &HashMap<crate::typechecking::ty::TyVarId, Ty>,
    ) -> Ty {
        match ty {
            Ty::Var(v) => map.get(v).cloned().unwrap_or_else(|| ty.clone()),
            Ty::Fun(a, r) => Ty::Fun(
                Box::new(Self::apply_ty_var_map(a, map)),
                Box::new(Self::apply_ty_var_map(r, map)),
            ),
            Ty::App(h, args) => Ty::App(
                Box::new(Self::apply_ty_var_map(h, map)),
                args.iter().map(|a| Self::apply_ty_var_map(a, map)).collect(),
            ),
            Ty::Tuple(items) => {
                Ty::Tuple(items.iter().map(|t| Self::apply_ty_var_map(t, map)).collect())
            }
            Ty::Array { element, length } => Ty::Array {
                element: Box::new(Self::apply_ty_var_map(element, map)),
                length: length.clone(),
            },
            Ty::Forall { body, .. } => Self::apply_ty_var_map(body, map),
            Ty::Readonly(inner) => Ty::Readonly(Box::new(Self::apply_ty_var_map(inner, map))),
            other => other.clone(),
        }
    }

    fn emit_mono_specializations_for_function<'compiler>(
        &mut self,
        qualified: &str,
        type_params: &[parser::ast::TypeParam<'compiler>],
        args: &Output<'compiler>,
        body: Option<&Output<'compiler>>,
        source_name: &str,
    ) {
        let Some(body) = body else {
            return;
        };
        if type_params.is_empty() || self.mono_plan.is_empty() {
            return;
        }

        let def_id = self
            .checker
            .def_id_of(source_name)
            .or_else(|| self.checker.interned_def(&self.namespace, source_name));
        let specializations = if let Some(id) = def_id {
            self.mono_plan
                .specializations_for_def(id)
                .cloned()
                .collect::<Vec<_>>()
        } else {
            self.mono_plan
                .specializations_for_fn(qualified)
                .chain(self.mono_plan.specializations_for_fn(source_name))
                .cloned()
                .collect::<Vec<_>>()
        };
        if specializations.is_empty() {
            return;
        }

        for specialization in specializations {
            if self.mono_offsets.contains_key(&specialization.key) {
                continue;
            }

            let overrides = self.mono_overrides_for_args(type_params, args, &specialization.key);
            if overrides.is_empty() {
                continue;
            }

            let subst_ids = specialization
                .key
                .subst
                .iter()
                .map(|id| id.0.to_string())
                .collect::<Vec<_>>()
                .join("$");
            let mono_name = format!(
                "{}$mono${}${}",
                qualified,
                specialization.key.def_id.raw(),
                subst_ids
            );
            let (clone_offset, _) = self.bind_function_entry(mono_name);
            self.mono_offsets
                .insert(specialization.key.clone(), clone_offset);

            let prev_fn_vars = std::mem::take(&mut self.context.variables);
            let prev_fn_polyfn_vars = std::mem::take(&mut self.polyfn_vars);
            let prev_fn_polyfn_sources = std::mem::take(&mut self.polyfn_sources);
            let prev_result_mode = self.compiling_result_mode;
            let prev_result_ok_is_result = self.compiling_result_ok_is_result;
            let prev_mono_clone = self.compiling_mono_clone;
            self.context.variables = Interner::default();
            self.compiling_result_mode = self.checker.fn_is_result_mode(source_name);
            self.compiling_result_ok_is_result =
                self.checker.fn_result_ok_is_result(source_name);
            self.compiling_mono_clone = true;
            self.mono_codegen_var_types.push(overrides);

            let prev_fn_defers = std::mem::take(&mut self.fn_defers);
            let mut a = self.do_compile(args);
            self.bytecode.append(&mut a);
            let body_op_start = self.bytecode.ops().len();
            let prev_field_keys = std::mem::take(&mut self.field_key_slots);
            self.emit_field_key_prologue(body);
            let mut c = self.do_compile(body);
            self.bytecode.append(&mut c);

            if !self.region_ends_with_return(body_op_start) {
                self.emit_fallthrough_return(source_name, body.0);
            }

            self.fn_defers = prev_fn_defers;
            self.mono_codegen_var_types.pop();
            self.compiling_result_mode = prev_result_mode;
            self.compiling_result_ok_is_result = prev_result_ok_is_result;
            self.compiling_mono_clone = prev_mono_clone;
            self.field_key_slots = prev_field_keys;
            self.context.variables = prev_fn_vars;
            self.polyfn_vars = prev_fn_polyfn_vars;
            self.polyfn_sources = prev_fn_polyfn_sources;
        }
    }

    fn mono_overrides_for_args<'compiler>(
        &self,
        type_params: &[parser::ast::TypeParam<'compiler>],
        args: &Output<'compiler>,
        key: &MonoKey,
    ) -> HashMap<String, Ty> {
        let mut type_param_tys = HashMap::new();
        for (idx, tp) in type_params.iter().enumerate() {
            if let Some(&ty_id) = key.subst.get(idx)
                && let Some(ty) = self.mono_plan.intern.get(ty_id)
            {
                type_param_tys.insert(tp.name, ty.clone());
            }
        }

        let mut overrides = HashMap::new();
        if let Expression::Fragment(children) = args.1.as_ref() {
            for child in children {
                if let Expression::Argument {
                    ty,
                    name,
                    is_rest,
                    ..
                } = child.1.as_ref()
                    && let Some(ty) = ty
                    && let Expression::Type(tp_name) | Expression::Identifier(tp_name) =
                        ty.1.as_ref()
                    && let Some(concrete) = type_param_tys.get(tp_name)
                {
                    // Rest formals are packed arrays at runtime (`MakeArray`).
                    let ty = if *is_rest {
                        crate::typechecking::ty::array(concrete.clone())
                    } else {
                        concrete.clone()
                    };
                    overrides.insert(name.to_string(), ty);
                }
            }
        }
        overrides
    }

    fn mono_call_offset(&self, fn_name: &str, args: Option<&Vec<Output<'_>>>) -> Option<usize> {
        let args = args?;
        // Keep keying in sync with `crate::monomorphize::candidate_for_call`: one
        // ground type per formal, with rest contributing its *element* type.
        let (fixed, rest, pack_rest) = self.split_call_args_for_rest(fn_name, args);
        let mut arg_types = Vec::with_capacity(fixed.len() + usize::from(pack_rest));
        for arg in &fixed {
            arg_types.push(crate::monomorphize::ground_ty(&self.checker, arg)?);
        }
        if pack_rest {
            if rest.is_empty() {
                // Empty rest: only match when a fixed formal already pinned T.
                // Without a rest element we can't invent a ground type here;
                // specializations with empty rest still key the rest slot from
                // subst — look up by trying each specialization's arg_types
                // prefix match below via exact equality, so require the rest
                // element type from the first fixed arg that shares the rest
                // type param. Fallback: skip mono (shared body).
                return None;
            }
            let elem = crate::monomorphize::ground_ty(&self.checker, &rest[0])?;
            for arg in rest.iter().skip(1) {
                if crate::monomorphize::ground_ty(&self.checker, arg)? != elem {
                    return None;
                }
            }
            arg_types.push(elem);
        }
        let spec = self
            .mono_plan
            .specialization_for_call(fn_name, &arg_types)?;
        self.mono_offsets.get(&spec.key).copied()
    }

    fn consume_function_signature_output<'compiler>(&mut self, method: &Output<'compiler>) {
        let _method_id = self.next_emit_id();
        if let Expression::Function { args, body, .. } = method.1.as_ref() {
            let mut args_bc = self.do_compile(args);
            self.bytecode.append(&mut args_bc);
            if let Some(body) = body {
                let mut body_bc = self.do_compile(body);
                self.bytecode.append(&mut body_bc);
            }
        } else {
            let mut bc = self.do_compile(method);
            self.bytecode.append(&mut bc);
        }
    }

    pub fn get_messages(&self) -> &Vec<Message> {
        &self.messages
    }

    /// Append a diagnostic produced outside the typechecker/codegen
    /// path (e.g. pipeline discovery parse errors). Callers that also
    /// emit via the reporting sink must bump their own
    /// `messages_emitted` cursor so [`Pipeline::emit_new_messages`]
    /// does not re-forward the same message.
    pub fn push_message(&mut self, message: Message) {
        self.messages.push(message);
    }

    pub fn c_structs(&self) -> &[CStructDef] {
        self.checker.c_structs()
    }

    pub fn register(&mut self, name: &str, params: &[Ty], returns: &Ty) -> &mut Self {
        let idx = self.native.len();
        self.native.insert(name.to_string(), idx);
        self.checker.register_native(name, params, returns);

        self
    }

    /// Bind a host-native name to a stable id for [`Instruction::HostInvoke`]
    /// without inserting a type into the HM env (virtual `io::*` schemes
    /// are bound via `use` instead).
    pub fn register_native_id(&mut self, name: &str, id: usize) {
        self.native.insert(name.to_string(), id);
    }

    /// Look up a registered host-native id by export name.
    pub fn native_id(&self, name: &str) -> Option<usize> {
        self.native.get(name).copied()
    }

    fn resolve_variable<'compiler>(
        &self,
        variable: &(SimpleSpan, Box<Expression<'compiler>>),
    ) -> String {
        match variable.1.borrow() {
            Expression::Identifier(n) => n.to_string(),
            _ => String::new(),
        }
    }

    /// Like `resolve_variable`, but records a diagnostic when the
    /// expression is not an identifier (replaces the old `todo!`).
    fn resolve_variable_checked<'compiler>(
        &mut self,
        variable: &(SimpleSpan, Box<Expression<'compiler>>),
    ) -> String {
        match variable.1.borrow() {
            Expression::Identifier(n) => n.to_string(),
            other => {
                let span = variable.0;
                let mut m = Message::error(
                    ErrorCode::InvalidAssignment,
                    "Cannot use this expression as a variable name".to_string(),
                    span.into_range(),
                );
                m.push(DiagLabel::new(
                    format!("expected an identifier, found `{other}`"),
                    span.into_range(),
                ));
                self.messages.push(m);
                String::new()
            }
        }
    }

    /// Look up the inferred type of the `lhs` we're about to use as
    /// the operand of a binary operator. The ID for `lhs` lives at
    /// the current `emit_idx` (since `lhs` is the next AST node to be
    /// visited). Returns true iff that type is the float constructor.
    ///
    fn string_concat_needs_staging(&self, lhs: &Output, rhs: &Output) -> bool {
        if self.arg_emits_on_self_bytecode(lhs) || self.arg_emits_on_self_bytecode(rhs) {
            return true;
        }
        matches!(lhs.1.as_ref(), Expression::Add(_, _))
            || matches!(rhs.1.as_ref(), Expression::Add(_, _))
    }

    /// Recurses into `lhs` and `rhs`, appending their bytecodes to
    /// `bytecode` in the same order as the legacy emitter. The caller
    /// is then responsible for emitting the operator-specific
    /// instruction.
    fn compile_binary_operands(
        &mut self,
        bytecode: &mut CodeBuf,
        lhs: &Output,
        rhs: &Output,
    ) -> bool {
        // Capture lhs's ID before recursing — `do_compile(lhs)`
        // advances `emit_idx` past lhs's entire subtree.
        let lhs_ty = self.codegen_expr_ty(lhs);
        let lhs_id = self.checker.id_table().ids().get(self.emit_idx).copied();
        if self.expr_is_stackable_direct_call(lhs) && self.expr_is_stackable_direct_call(rhs) {
            // Pure user `CALL`s with leaf args emit `push args; CALL` and leave
            // the sibling below the callee frame. Raise `expr_depth` so any
            // unexpected temp pads above the stacked lhs — enabling
            // `CALL; CALL; ADD; RETURN` → `BinReturn` (fib).
            let depth_on_entry = self.expr_depth;
            self.append_with_existential_pack(bytecode, lhs);
            self.expr_depth = depth_on_entry + 1;
            self.append_with_existential_pack(bytecode, rhs);
            self.expr_depth = depth_on_entry;
        } else if self.expr_may_clobber_operand_stack(lhs)
            || self.expr_may_clobber_operand_stack(rhs)
        {
            // HostInvoke / match / tiny-inline / nested calls: stage into *this*
            // buffer so a stacked lhs cannot be buried by temp STOREs.
            let lhs_slot;
            let rhs_slot;
            self.append_with_existential_pack(bytecode, lhs);
            lhs_slot = self.alloc_temp_slot();
            bytecode.push_store_pop(lhs_slot);
            self.append_with_existential_pack(bytecode, rhs);
            rhs_slot = self.alloc_temp_slot();
            bytecode.push_store_pop(rhs_slot);
            bytecode.push_load(lhs_slot);
            bytecode.push_load(rhs_slot);
        } else {
            bytecode.append(&mut self.do_compile(lhs));
            bytecode.append(&mut self.do_compile(rhs));
        }
        if matches!(
            lhs_ty,
            Some(crate::typechecking::ty::Ty::Con(ref name))
                if name == crate::typechecking::ty::FLOAT
        ) {
            return true;
        }
        matches!(
            lhs_id.and_then(|id| self.checker.lookup_at(id)),
            Some(crate::typechecking::ty::Ty::Con(ref name))
                if name == crate::typechecking::ty::FLOAT
        )
    }

    /// True when `operand` resolves to an open type variable (generic param).
    fn operand_is_open_ty(&self, operand: &Output) -> bool {
        match operand.1.as_ref() {
            Expression::Identifier(_) => match self.codegen_ident_ty(operand) {
                Some(ty) => matches!(ty, Ty::Var(_)),
                None => false,
            },
            _ => false,
        }
    }

    fn alloc_temp_slot(&mut self) -> u32 {
        self.temp_counter += 1;
        // Operand stack and locals share one buffer. A `CONST` left on
        // the stack by `emit_host_native_invoke` (native id before args)
        // occupies index `variables.len()` without being interned. If we
        // `StorePop` into that index from `new Class(...)`, we overwrite
        // the id and `HostInvoke` sees a heap address instead.
        let min_slot = self.context.variables.len() as u32 + self.expr_depth;
        while (self.context.variables.len() as u32) < min_slot {
            let pad = format!("__pad{}", self.context.variables.len());
            let _ = self.context.variables.intern(pad);
        }
        let name = format!("__tmp{}", self.temp_counter);
        self.context.variables.intern(name) as u32
    }

    /// True when compiling `idx` will only push (no `STORE` that seeks `tell`
    /// past a live operand). Safe to leave the array under the index on the
    /// shared locals/operand buffer; call/inline index exprs are not.
    fn index_keeps_array_on_stack_safe(idx: &Expression<'_>) -> bool {
        matches!(
            idx,
            Expression::Identifier(_) | Expression::Integer(_)
        )
    }

    /// Synthesize the packed rest-array type for a call's trailing args
    /// (`[T]` / `[T; N]`), mirroring typechecker `infer_and_reorder_call_args`.
    fn synthesize_rest_array_ty(&self, rest: &[Output<'_>]) -> crate::typechecking::Ty {
        use crate::typechecking::ty::vec_app_ty;
        let mut elem: Option<crate::typechecking::Ty> = None;
        for arg in rest {
            if let Some(t) = self.codegen_expr_ty(arg) {
                match &elem {
                    None => elem = Some(t),
                    Some(prev) if prev != &t => {
                        // Prefer the first element type; unify already ran in TC.
                    }
                    _ => {}
                }
            }
        }
        let element = elem.unwrap_or_else(crate::typechecking::ty::int);
        // Rest params are `Vec<T>` in schemes (`parse_arg_list`); match that
        // shape so `emit_call_site_dicts` can bind constraint vars.
        vec_app_ty(element)
    }

    /// Bind names from an irrefutable `let` pattern by reading from
    /// `src_slot` (tuple via `Index`, record via `GetField`).
    fn emit_let_pattern_binds(
        &mut self,
        pattern: &parser::ast::LetPattern<'_>,
        src_slot: u32,
        bytecode: &mut CodeBuf,
    ) {
        use parser::ast::LetPattern;
        match pattern {
            LetPattern::Wildcard => {}
            LetPattern::Binding { name } => {
                bytecode.push_load(src_slot);
                let slot = self.alloc_binding_slot(name);
                bytecode.push_store_pop(slot);
            }
            LetPattern::Tuple(parts) => {
                for (idx, part) in parts.iter().enumerate() {
                    match part {
                        LetPattern::Wildcard => {}
                        LetPattern::Binding { name } => {
                            bytecode.push_load(src_slot);
                            bytecode.push_const(idx as i32);
                            bytecode.push_index();
                            let slot = self.alloc_binding_slot(name);
                            bytecode.push_store_pop(slot);
                        }
                        nested @ (LetPattern::Tuple(_) | LetPattern::Record(_)) => {
                            bytecode.push_load(src_slot);
                            bytecode.push_const(idx as i32);
                            bytecode.push_index();
                            let nested_slot = self.alloc_temp_slot();
                            bytecode.push_store_pop(nested_slot);
                            self.emit_let_pattern_binds(nested, nested_slot, bytecode);
                        }
                    }
                }
            }
            LetPattern::Record(fields) => {
                for pf in fields {
                    match &pf.pattern {
                        LetPattern::Wildcard => {}
                        LetPattern::Binding { name } => {
                            bytecode.push_load(src_slot);
                            self.emit_raw_string_literal(bytecode, pf.name);
                            bytecode.push_get_field();
                            let slot = self.alloc_binding_slot(name);
                            bytecode.push_store_pop(slot);
                        }
                        nested @ (LetPattern::Tuple(_) | LetPattern::Record(_)) => {
                            bytecode.push_load(src_slot);
                            self.emit_raw_string_literal(bytecode, pf.name);
                            bytecode.push_get_field();
                            let nested_slot = self.alloc_temp_slot();
                            bytecode.push_store_pop(nested_slot);
                            self.emit_let_pattern_binds(nested, nested_slot, bytecode);
                        }
                    }
                }
            }
        }
    }

    /// `for x in` over an array already on the operand stack (or just
    /// compiled). Observationally identical to `ArrayIter::next`.
    ///
    /// Layout: StorePop arr; idx=0; [top] idx < len → else exit;
    /// `x = arr[idx]`; body; [continue] idx++; JMP top; [exit].
    fn emit_for_in_array_loop(
        &mut self,
        body: &Output<'_>,
        binding_name: &str,
        array_already_on_stack: bool,
        iterable: Option<&Output<'_>>,
    ) {
        let arr_slot = self.alloc_temp_slot();
        let idx_slot = self.alloc_temp_slot();
        if !array_already_on_stack {
            let mut iter_bc = self.do_compile(iterable.expect("iterable required when not on stack"));
            self.bytecode.append(&mut iter_bc);
        }
        self.bytecode.push_store_pop(arr_slot);
        self.bytecode.push_const(0);
        self.bytecode.push_store_pop(idx_slot);

        // Hoist ArrayLen once — the array slot is not mutated by for-in.
        let len_slot = self.alloc_temp_slot();
        self.bytecode.push_load(arr_slot);
        self.bytecode.push(Byte::new(Instruction::ArrayLen));
        self.bytecode.push_store_pop(len_slot);

        // Consume binding Identifier NodeId (iterable → binding → body).
        let _ = self.next_emit_id();
        let binding_slot = self.alloc_binding_slot(binding_name);

        let mut bb = BlockBuilder::new();
        let top_label = bb.fresh_label(self.bytecode.il_mut());
        let continue_label = bb.fresh_label(self.bytecode.il_mut());
        let exit_label = bb.fresh_label(self.bytecode.il_mut());
        bb.bind_label(top_label, self.bytecode.il_mut());

        // cond: idx < len  (LE is `<`)
        self.bytecode.push_load(idx_slot);
        self.bytecode.push_load(len_slot);
        self.bytecode.push(Byte::new(Instruction::LE));
        bb.emit_jump_to(exit_label, BbJumpKind::JumpIfFalse, self.bytecode.il_mut());

        // x = arr[idx]
        self.bytecode.push_load(arr_slot);
        self.bytecode.push_load(idx_slot);
        self.bytecode.push_index();
        self.bytecode.push_store_pop(binding_slot);

        self.loop_stack.push((continue_label, exit_label));
        self.loop_bbs.push(bb);
        let mut body_bc = self.do_compile(body);
        self.bytecode.append(&mut body_bc);
        let mut bb = self
            .loop_bbs
            .pop()
            .expect("loop builder stack balanced for for-in array");
        self.loop_stack
            .pop()
            .expect("loop label stack balanced for for-in array");
        bb.bind_label(continue_label, self.bytecode.il_mut());
        // idx = idx + 1
        self.bytecode.push_load(idx_slot);
        self.bytecode.push_const(1);
        self.bytecode.push(Byte::new(Instruction::ADD));
        self.bytecode.push_store_pop(idx_slot);

        bb.emit_jump_to(top_label, BbJumpKind::Unconditional, self.bytecode.il_mut());
        bb.bind_label(exit_label, self.bytecode.il_mut());
    }

    /// Homogeneous tuple → temp `[A; N]` via Index, then array for-in.
    fn emit_for_in_tuple(
        &mut self,
        iterable: &Output<'_>,
        body: &Output<'_>,
        binding_name: &str,
        arity: usize,
    ) {
        let tup_slot = self.alloc_temp_slot();
        let mut iter_bc = self.do_compile(iterable);
        self.bytecode.append(&mut iter_bc);
        self.bytecode.push_store_pop(tup_slot);
        for i in 0..arity {
            self.bytecode.push_load(tup_slot);
            self.bytecode.push_const(i as i32);
            self.bytecode.push_index();
        }
        self.bytecode.push_make_array(arity as u32);
        self.emit_for_in_array_loop(body, binding_name, true, None);
    }

    /// Dict → `DictEntries` → array of `(string, V)` pairs → array for-in.
    fn emit_for_in_dict(&mut self, iterable: &Output<'_>, body: &Output<'_>, binding_name: &str) {
        let mut iter_bc = self.do_compile(iterable);
        self.bytecode.append(&mut iter_bc);
        self.bytecode.push(Byte::new(Instruction::DictEntries));
        self.emit_for_in_array_loop(body, binding_name, true, None);
    }

    /// Lazy range for-in (`int`/`byte`/`float`).
    ///
    /// Fast path when the iterable is a `Range` literal: locals for
    /// `cur`/`end` only — no heap. First-class range values
    /// (`let r = 0..n; for x in r`) are dicts `{start,end,inclusive}`
    /// unpacked via `GetField`.
    ///
    /// `float` selects LEF/LEQF/ADDF with step `1.0`; otherwise LE/LEQ/ADD
    /// with step `1` (shared by `int` and `byte`).
    fn emit_for_in_range(
        &mut self,
        iterable: &Output<'_>,
        body: &Output<'_>,
        binding_name: &str,
        inclusive: bool,
        float: bool,
    ) {
        if !float {
            if let Expression::Range { start, end, .. } = iterable.1.as_ref()
                && !crate::const_fold::body_has_loop_control(body)
            {
                if let Some(trips) = crate::const_fold::range_trip_count(start, end, inclusive) {
                    let _ = self.next_emit_id();
                    let binding_slot = self.alloc_binding_slot(binding_name);
                    let _ = self.next_emit_id();
                    if let Some(ConstValue::Int(s)) = crate::const_fold::eval_expr(start, self.const_env())
                    {
                        for k in 0..trips {
                            let val = s + k as i64;
                            let mut trip_bc = CodeBuf::new();
                            self.emit_const_value(&ConstValue::Int(val), &mut trip_bc);
                            trip_bc.push_store_pop(binding_slot);
                            let mut body_bc = self.do_compile(body);
                            trip_bc.append(&mut body_bc);
                            self.bytecode.append(&mut trip_bc);
                        }
                        return;
                    }
                }
            }
        }

        let cur_slot = self.alloc_temp_slot();
        let end_slot = self.alloc_temp_slot();

        match iterable.1.as_ref() {
            Expression::Range { start, end, .. } => {
                // Consume the Range node's ID (pre-walk: Range → start → end).
                let _ = self.next_emit_id();
                let mut start_bc = self.do_compile(start);
                self.bytecode.append(&mut start_bc);
                self.bytecode.push_store_pop(cur_slot);
                let mut end_bc = self.do_compile(end);
                self.bytecode.append(&mut end_bc);
                self.bytecode.push_store_pop(end_slot);
            }
            _ => {
                let range_slot = self.alloc_temp_slot();
                let mut iter_bc = self.do_compile(iterable);
                self.bytecode.append(&mut iter_bc);
                self.bytecode.push_store_pop(range_slot);

                self.bytecode.push_load(range_slot);
                let start_idx = self.intern_string("start");
                self.bytecode.push_string(start_idx);
                self.bytecode.push_get_field();
                self.bytecode.push_store_pop(cur_slot);

                self.bytecode.push_load(range_slot);
                let end_idx = self.intern_string("end");
                self.bytecode.push_string(end_idx);
                self.bytecode.push_get_field();
                self.bytecode.push_store_pop(end_slot);
            }
        }

        // Consume binding Identifier NodeId (iterable → binding → body).
        let _ = self.next_emit_id();
        let binding_slot = self.alloc_binding_slot(binding_name);

        let mut bb = BlockBuilder::new();
        let top_label = bb.fresh_label(self.bytecode.il_mut());
        let continue_label = bb.fresh_label(self.bytecode.il_mut());
        let exit_label = bb.fresh_label(self.bytecode.il_mut());
        bb.bind_label(top_label, self.bytecode.il_mut());

        // cond: cur < end  (half-open) or cur <= end (inclusive)
        self.bytecode.push_load(cur_slot);
        self.bytecode.push_load(end_slot);
        self.bytecode.push(Byte::new(if float {
            if inclusive {
                Instruction::LEQF
            } else {
                Instruction::LEF
            }
        } else if inclusive {
            Instruction::LEQ
        } else {
            Instruction::LE
        }));
        bb.emit_jump_to(exit_label, BbJumpKind::JumpIfFalse, self.bytecode.il_mut());

        // x = cur
        self.bytecode.push_load(cur_slot);
        self.bytecode.push_store_pop(binding_slot);

        self.loop_stack.push((continue_label, exit_label));
        self.loop_bbs.push(bb);
        let mut body_bc = self.do_compile(body);
        self.bytecode.append(&mut body_bc);
        let mut bb = self
            .loop_bbs
            .pop()
            .expect("loop builder stack balanced for for-in range");
        self.loop_stack
            .pop()
            .expect("loop label stack balanced for for-in range");
        bb.bind_label(continue_label, self.bytecode.il_mut());
        // cur = cur + 1  (or + 1.0 for float)
        self.bytecode.push_load(cur_slot);
        if float {
            let bits = Value::from(1.0_f64).raw() as u64;
            let idx = self.intern_constant(bits);
            self.bytecode.push_const_pool(idx);
            self.bytecode.push(Byte::new(Instruction::ADDF));
        } else {
            self.bytecode.push_const(1);
            self.bytecode.push(Byte::new(Instruction::ADD));
        }
        self.bytecode.push_store_pop(cur_slot);

        bb.emit_jump_to(top_label, BbJumpKind::Unconditional, self.bytecode.il_mut());
        bb.bind_label(exit_label, self.bytecode.il_mut());
    }

    /// Coroutine for-in: resume → bind; skip body when `done` (completion
    /// value excluded). Same layout as the Phase CORO for-in path.
    fn emit_for_in_coro(&mut self, iterable: &Output<'_>, body: &Output<'_>, binding_name: &str) {
        let handle_slot = self.alloc_temp_slot();
        let mut iter_bc = self.do_compile(iterable);
        self.bytecode.append(&mut iter_bc);
        self.bytecode.push_store_pop(handle_slot);

        let _ = self.next_emit_id();
        let binding_slot = self.alloc_binding_slot(binding_name);

        let mut bb = BlockBuilder::new();
        let top_label = bb.fresh_label(self.bytecode.il_mut());
        let exit_label = bb.fresh_label(self.bytecode.il_mut());
        bb.bind_label(top_label, self.bytecode.il_mut());

        self.bytecode.push_load(handle_slot);
        self.bytecode
            .push(Byte::new(Instruction::ResumeCoro).with_operand_u32(0));
        self.bytecode.push_store_pop(binding_slot);

        self.bytecode.push_load(handle_slot);
        self.bytecode.push(Byte::new(Instruction::DoneCoro));
        self.bytecode.push(Byte::new(Instruction::LogNot));
        bb.emit_jump_to(exit_label, BbJumpKind::JumpIfFalse, self.bytecode.il_mut());

        self.loop_stack.push((top_label, exit_label));
        self.loop_bbs.push(bb);
        let mut body_bc = self.do_compile(body);
        self.bytecode.append(&mut body_bc);
        let mut bb = self
            .loop_bbs
            .pop()
            .expect("loop builder stack balanced for for-in coro");
        self.loop_stack
            .pop()
            .expect("loop label stack balanced for for-in coro");

        bb.emit_jump_to(top_label, BbJumpKind::Unconditional, self.bytecode.il_mut());
        bb.bind_label(exit_label, self.bytecode.il_mut());
    }

    /// User `IntoIterator` / `Iterator`: `into_iter` then `next` → Option.
    ///
    /// Trait instance methods unbox type-parameter args in their prologue
    /// (`ValueTag::Instance` for classes), so call sites must `BoxValue`
    /// the carrier before the direct CALL.
    fn emit_for_in_custom(
        &mut self,
        iterable: &Output<'_>,
        body: &Output<'_>,
        binding_name: &str,
        into_iter_fqn: &str,
        next_fqn: &str,
        _item_ty: Option<&Ty>,
    ) {
        let none_tag = self
            .checker
            .tag_for(common::BUILTIN_OPTION_ENUM, "None")
            .unwrap_or(0);
        let carrier_tag = ValueTag::Instance as u32;

        let it_slot = self.alloc_temp_slot();
        let mut iter_bc = self.do_compile(iterable);
        self.bytecode.append(&mut iter_bc);
        self.bytecode.push_box_value(carrier_tag);
        if !self.emit_named_entry_on_module(into_iter_fqn, 1, crate::il::EntryKind::Call) {
            self.missing_call_target(into_iter_fqn, iterable.0.into_range());
        }
        self.bytecode.push_store_pop(it_slot);

        let _ = self.next_emit_id();
        let binding_slot = self.alloc_binding_slot(binding_name);

        let mut bb = BlockBuilder::new();
        let top_label = bb.fresh_label(self.bytecode.il_mut());
        let exit_label = bb.fresh_label(self.bytecode.il_mut());
        bb.bind_label(top_label, self.bytecode.il_mut());

        self.bytecode.push_load(it_slot);
        self.bytecode.push_box_value(carrier_tag);
        if !self.emit_named_entry_on_module(next_fqn, 1, crate::il::EntryKind::Call) {
            self.missing_call_target(next_fqn, iterable.0.into_range());
        }

        // `Option::None` → exit (JumpIfMatch pops unit None).
        bb.emit_jump_to(
            exit_label,
            BbJumpKind::JumpIfMatch {
                tag: none_tag,
                arity: 0,
            },
            self.bytecode.il_mut(),
        );
        // Fall-through: Some(v) — unpack payload into binding.
        self.bytecode
            .push(Byte::new(Instruction::Unpack).with_operand_u32(1));
        self.bytecode.push_store_pop(binding_slot);

        self.loop_stack.push((top_label, exit_label));
        self.loop_bbs.push(bb);
        let mut body_bc = self.do_compile(body);
        self.bytecode.append(&mut body_bc);
        let mut bb = self
            .loop_bbs
            .pop()
            .expect("loop builder stack balanced for for-in custom");
        self.loop_stack
            .pop()
            .expect("loop label stack balanced for for-in custom");

        bb.emit_jump_to(top_label, BbJumpKind::Unconditional, self.bytecode.il_mut());
        bb.bind_label(exit_label, self.bytecode.il_mut());
    }

    /// Replace `new Class(args).field` with the selected constructor argument.
    ///
    /// The temporary object has no observable identity because it is consumed
    /// immediately. Arguments still evaluate in declaration order and are
    /// staged into slots so non-selected arguments retain their side effects.
    fn try_emit_direct_class_field_access(
        &mut self,
        bytecode: &mut CodeBuf,
        receiver: &Output<'_>,
        field: &str,
    ) -> bool {
        // Pratt wraps nodes in `Expr`; `(…)` is `Group(Fragment([…]))`. Peel
        // wrappers so `(new C(args)).field` matches bare `new C(args).field`.
        let mut receiver = receiver;
        loop {
            match receiver.1.as_ref() {
                Expression::Group(inner) | Expression::Expr(inner) => receiver = inner,
                Expression::Fragment(items) if items.len() == 1 => receiver = &items[0],
                _ => break,
            }
        }
        let Expression::Instantiate(class, Some(args)) = receiver.1.as_ref() else {
            return false;
        };
        let Expression::Identifier(class_name) = class.1.as_ref() else {
            return false;
        };
        if self.decorated_class_ctors.contains_key(*class_name)
            || self
                .checker
                .resolve_class_key(class_name)
                .is_some_and(|k| self.decorated_class_ctors.contains_key(&k))
        {
            return false;
        }
        if self.checker.class_has_drop(class_name) {
            return false;
        }
        if args.iter().any(|arg| self.arg_emits_on_self_bytecode(arg)) {
            return false;
        }
        let class_key = self.resolve_class_ident(class_name);
        let Some(fields) = self.context.classes.get(&class_key).cloned() else {
            return false;
        };
        if args.len() != fields.len() {
            return false;
        }
        let Some((field_index, _)) = fields
            .iter()
            .enumerate()
            .find(|(_, (name, _))| name == field)
        else {
            return false;
        };

        let mut arg_slots = Vec::with_capacity(args.len());
        for arg in args {
            let mut arg_bc = CodeBuf::new();
            self.append_with_existential_pack(&mut arg_bc, arg);
            bytecode.append(&mut arg_bc);
            let slot = self.alloc_temp_slot();
            bytecode.push_store_pop(slot);
            arg_slots.push(slot);
        }
        bytecode.push_load(arg_slots[field_index]);
        true
    }

    fn emit_field_name(&mut self, bytecode: &mut impl EmitBuf, field: &str) {
        if let Some(&slot) = self.field_key_slots.get(field) {
            bytecode.push_load(slot);
            return;
        }
        self.emit_raw_string_literal(bytecode, field);
    }

    /// Count GetField/SetField string-key uses in `node` (Access / OptionalAccess).
    fn count_field_key_uses(node: &Output<'_>, counts: &mut HashMap<String, u32>) {
        use Expression::*;
        match node.1.as_ref() {
            Access(recv, field) | OptionalAccess(recv, field) => {
                *counts.entry((*field).to_string()).or_insert(0) += 1;
                Self::count_field_key_uses(recv, counts);
            }
            CompoundAssign(target, _, rhs) | Assignment(target, rhs) => {
                Self::count_field_key_uses(target, counts);
                Self::count_field_key_uses(rhs, counts);
            }
            Adjust { target, .. } => Self::count_field_key_uses(target, counts),
            Negate(e)
            | Not(e)
            | LogicalNot(e)
            | Positive(e)
            | Return(e)
            | ImplicitReturn(e)
            | Raise(e)
            | Panic(e)
            | Yield(e)
            | YieldFrom(e)
            | Try(e)
            | Expr(e)
            | Group(e)
            | ExprStatement(e)
            | Statement(e)
            | Readonly(e)
            | Noop(e)
            | Dload(e)
            | Done(e)
            | Spread(e)
            | NamedArg(_, e)
            | Member(e)
            | Method(_, e)
            | Constant(e, _)
            | Variable(_, Some(e)) => Self::count_field_key_uses(e, counts),
            Variable(_, None) => {}
            Resume(e, Some(v)) | Coalesce(e, v) | Cast(e, v) | Index(e, Some(v)) => {
                Self::count_field_key_uses(e, counts);
                Self::count_field_key_uses(v, counts);
            }
            Resume(e, None) | Index(e, None) => Self::count_field_key_uses(e, counts),
            Add(a, b)
            | Sub(a, b)
            | Mul(a, b)
            | Div(a, b)
            | Mod(a, b)
            | Pow(a, b)
            | Shl(a, b)
            | Shr(a, b)
            | Xor(a, b)
            | And(a, b)
            | BitAnd(a, b)
            | Or(a, b)
            | BitOr(a, b)
            | Eq(a, b)
            | Neq(a, b)
            | Leq(a, b)
            | Geq(a, b)
            | Le(a, b)
            | Gt(a, b)
            | TypeFun(a, b) => {
                Self::count_field_key_uses(a, counts);
                Self::count_field_key_uses(b, counts);
            }
            Range { start, end, .. } => {
                Self::count_field_key_uses(start, counts);
                Self::count_field_key_uses(end, counts);
            }
            List(v) | Array(v) | Fragment(v) | Block(v) | Program(v) | Tuple(v) | If(v)
            | Declare(v) | Invoke(v) => {
                for c in v {
                    Self::count_field_key_uses(c, counts);
                }
            }
            Dict(fields) => {
                for f in fields {
                    Self::count_field_key_uses(&f.value, counts);
                }
            }
            Branch(cond, body) => {
                if let Some(c) = cond {
                    Self::count_field_key_uses(c, counts);
                }
                Self::count_field_key_uses(body, counts);
            }
            Call { name, args } => {
                Self::count_field_key_uses(name, counts);
                if let Some(as_) = args {
                    for a in as_ {
                        Self::count_field_key_uses(a, counts);
                    }
                }
            }
            Loop {
                identifier,
                iterable,
                body,
            } => {
                if let Some(id) = identifier {
                    Self::count_field_key_uses(id, counts);
                }
                Self::count_field_key_uses(iterable, counts);
                Self::count_field_key_uses(body, counts);
            }
            LetDestructure { rhs, .. } => Self::count_field_key_uses(rhs, counts),
            Defer { body, .. } | Lambda { body, .. } | TestCase { body, .. } => {
                Self::count_field_key_uses(body, counts);
            }
            Function { body: Some(b), .. } => Self::count_field_key_uses(b, counts),
            Function { body: None, .. } => {}
            Instantiate(recv, args) => {
                Self::count_field_key_uses(recv, counts);
                if let Some(as_) = args {
                    for a in as_ {
                        Self::count_field_key_uses(a, counts);
                    }
                }
            }
            Match { scrutinee, arms } => {
                Self::count_field_key_uses(scrutinee, counts);
                for arm in arms {
                    Self::count_field_key_uses(&arm.body, counts);
                }
            }
            Construct { fields, .. } => match fields {
                parser::ast::EnumConstructPayload::Tuple(parts) => {
                    for p in parts {
                        Self::count_field_key_uses(p, counts);
                    }
                }
                parser::ast::EnumConstructPayload::Record(fs) => {
                    for f in fs {
                        Self::count_field_key_uses(&f.value, counts);
                    }
                }
                parser::ast::EnumConstructPayload::Unit => {}
            },
            StaticDecl { init, .. } => Self::count_field_key_uses(init, counts),
            Field { init: Some(i), .. } => Self::count_field_key_uses(i, counts),
            // Type-only / declaration / leaf nodes — no runtime field keys.
            _ => {}
        }
    }

    /// Materialize field-name strings used ≥2 times into temp slots at fn entry.
    fn emit_field_key_prologue(&mut self, body: &Output<'_>) {
        let mut counts = HashMap::new();
        Self::count_field_key_uses(body, &mut counts);
        let mut keys: Vec<String> = counts
            .into_iter()
            .filter(|(_, n)| *n >= 2)
            .map(|(k, _)| k)
            .collect();
        keys.sort();
        self.field_key_slots.clear();
        for key in keys {
            let slot = self.alloc_temp_slot();
            let idx = self.intern_string(&key);
            self.bytecode.push_string(idx);
            self.bytecode.push_store_pop(slot);
            self.field_key_slots.insert(key, slot);
        }
    }

    fn emit_raw_string_literal(&mut self, bytecode: &mut impl EmitBuf, value: &str) {
        self.push_string_literal(bytecode, value);
    }

    fn variable_slot(&mut self, name: &str) -> Option<u32> {
        self.lookup_slot(name)
    }

    fn is_float_ty(&self, _node: &Output) -> bool {
        if matches!(
            self.codegen_ident_ty(_node),
            Some(crate::typechecking::ty::Ty::Con(ref ty))
                if ty == crate::typechecking::ty::FLOAT
        ) {
            return true;
        }
        let Some(id) = self.checker.id_table().ids().get(self.emit_idx).copied() else {
            return false;
        };
        matches!(
            self.checker.lookup_at(id),
            Some(crate::typechecking::ty::Ty::Con(ref name))
                if name == crate::typechecking::ty::FLOAT
        )
    }

    fn is_string_expr(&self, node: &Output) -> bool {
        matches!(
            self.codegen_expr_ty(node),
            Some(Ty::Con(ref name)) if name == crate::typechecking::ty::STRING
        )
    }

    /// `Length::len` instance FQN for a receiver type, if registered.
    fn len_instance_method_fqn(&self, ty: &Ty) -> Option<String> {
        let pruned = crate::typechecking::subst::apply_ty_prune(self.checker.subst(), ty);
        let head = match &pruned {
            Ty::Con(name) => name.clone(),
            Ty::Constructor { owner, .. } => match owner.as_ref() {
                Ty::Con(name) => name.clone(),
                _ => return None,
            },
            _ => return None,
        };
        self.checker
            .instance_method_fqn("Length", &[Ty::Con(head)], "len")
            .map(str::to_string)
    }

    /// Static length from a value's type (fixed arrays, tuples, records).
    fn static_len_of(&self, node: &Output) -> Option<usize> {
        use crate::typechecking::ty::{ArrayLength, strip_readonly};
        let ty = self.codegen_expr_ty(node)?;
        let pruned = crate::typechecking::subst::apply_ty_prune(self.checker.subst(), &ty);
        match strip_readonly(&pruned) {
            Ty::Array {
                length: ArrayLength::Static(n),
                ..
            } => Some(*n),
            Ty::Tuple(elems) => Some(elems.len()),
            Ty::Record { fields } => Some(fields.len()),
            _ => None,
        }
    }

    /// Resolve an `impl Class<…>` type argument to a [`Ty`] for FQN mangling.
    /// Mirrors the typechecker's `parse_instance_head` so `Option<int>`
    /// becomes `App(Option, [int])`, not `unknown`.
    fn codegen_instance_head_ty(&self, arg: &Output) -> Ty {
        match arg.1.as_ref() {
            Expression::Type(name) | Expression::Identifier(name) => {
                match name.to_ascii_lowercase().as_str() {
                    "option" => Ty::Con(common::BUILTIN_OPTION_ENUM.into()),
                    "result" => Ty::Con(common::BUILTIN_RESULT_ENUM.into()),
                    "int" => Ty::Con("int".into()),
                    "float" => Ty::Con("float".into()),
                    "string" => Ty::Con("string".into()),
                    "bool" => Ty::Con("bool".into()),
                    "void" | "unit" => Ty::Con("unit".into()),
                    _ => Ty::Con(name.to_string()),
                }
            }
            Expression::TypeApp { name, args } => {
                let head = match name.to_ascii_lowercase().as_str() {
                    "option" => Ty::Con(common::BUILTIN_OPTION_ENUM.into()),
                    "result" => Ty::Con(common::BUILTIN_RESULT_ENUM.into()),
                    _ => Ty::Con(name.to_string()),
                };
                let arg_tys: Vec<Ty> = args
                    .iter()
                    .map(|a| self.codegen_instance_head_ty(a))
                    .collect();
                Ty::App(Box::new(head), arg_tys)
            }
            _ => self
                .codegen_expr_ty(arg)
                .unwrap_or_else(|| Ty::Con("unknown".into())),
        }
    }

    fn codegen_expr_ty(&self, node: &Output) -> Option<Ty> {
        if let Expression::NamedArg(_, value) = node.1.as_ref() {
            return self.codegen_expr_ty(value);
        }
        if let Expression::Identifier(_) = node.1.as_ref() {
            return self.codegen_ident_ty(node);
        }
        if let Some(ty) = self.sidecar_ty_of(node) {
            return Some(ty);
        }
        match node.1.as_ref() {
            Expression::Integer(_) => Some(Ty::Con(crate::typechecking::ty::INT.into())),
            Expression::Float(_) => Some(Ty::Con(crate::typechecking::ty::FLOAT.into())),
            Expression::Bool(_) => Some(Ty::Con(crate::typechecking::ty::BOOL.into())),
            Expression::String(_) => Some(Ty::Con(crate::typechecking::ty::STRING.into())),
            Expression::Tuple(items) => {
                let mut tys = Vec::with_capacity(items.len());
                for item in items {
                    tys.push(self.codegen_expr_ty(item)?);
                }
                Some(Ty::Tuple(tys))
            }
            Expression::Dict(fields) => {
                let mut tys = Vec::with_capacity(fields.len());
                for field in fields {
                    tys.push((field.name.to_string(), self.codegen_expr_ty(&field.value)?));
                }
                tys.sort_by(|a, b| a.0.cmp(&b.0));
                Some(Ty::Record { fields: tys })
            }
            Expression::Identifier(_) => self.codegen_ident_ty(node),
            Expression::Instantiate(class, _) => match class.1.as_ref() {
                Expression::Identifier(name) | Expression::Type(name) => {
                    Some(Ty::Con(self.resolve_class_ident(name)))
                }
                _ => None,
            },
            Expression::Add(lhs, rhs) if self.is_string_expr(lhs) && self.is_string_expr(rhs) => {
                Some(Ty::Con(crate::typechecking::ty::STRING.into()))
            }
            Expression::Access(receiver, field) => {
                let receiver_ty = self.receiver_type(receiver)?;
                if let Ty::Record { fields } = &receiver_ty {
                    return fields
                        .iter()
                        .find(|(name, _)| name == field)
                        .map(|(_, ty)| ty.clone());
                }
                if let Some(name) = Checker::class_name_of_ty(&receiver_ty) {
                    if self.checker.is_class(name) {
                        return self.codegen_class_field_ty(name, field, &receiver_ty);
                    }
                }
                extract_enum_name(&receiver_ty)
                    .and_then(|name| self.checker.field_type_for(&name, field))
            }
            Expression::OptionalAccess(receiver, field) => {
                use crate::typechecking::ty::{is_option_ty, option_inner, option_ty};
                let recv_ty = self.codegen_expr_ty(receiver)?;
                let inner = if is_option_ty(&recv_ty) {
                    option_inner(&recv_ty)?
                } else {
                    return None;
                };
                let field_ty = if let Ty::Record { fields } = &inner {
                    fields
                        .iter()
                        .find(|(name, _)| name == field)
                        .map(|(_, ty)| ty.clone())
                } else if let Some(name) = Checker::class_name_of_ty(&inner) {
                    if self.checker.is_class(name) {
                        self.codegen_class_field_ty(name, field, &inner)
                    } else {
                        extract_enum_name(&inner)
                            .and_then(|n| self.checker.field_type_for(&n, field))
                    }
                } else {
                    extract_enum_name(&inner).and_then(|n| self.checker.field_type_for(&n, field))
                }?;
                Some(option_ty(field_ty))
            }
            Expression::Expr(inner)
            | Expression::Group(inner)
            | Expression::Statement(inner)
            | Expression::ExprStatement(inner) => self.codegen_expr_ty(inner),
            _ => None,
        }
    }

    fn binop_for_assign_op(op: parser::ast::AssignOp, is_float: bool) -> Instruction {
        use parser::ast::AssignOp;
        match (op, is_float) {
            (AssignOp::Add, false) => Instruction::ADD,
            (AssignOp::Add, true) => Instruction::ADDF,
            (AssignOp::Sub, false) => Instruction::SUB,
            (AssignOp::Sub, true) => Instruction::SUBF,
            (AssignOp::Mul, false) => Instruction::MUL,
            (AssignOp::Mul, true) => Instruction::MULF,
            (AssignOp::Div, false) => Instruction::DIV,
            (AssignOp::Div, true) => Instruction::DIVF,
            (AssignOp::Mod, false) => Instruction::MOD,
            (AssignOp::Mod, true) => Instruction::MODF,
            (AssignOp::Pow, false) => Instruction::Pow,
            (AssignOp::Pow, true) => Instruction::PowF,
            (AssignOp::Shl, _) => Instruction::SHL,
            (AssignOp::Shr, _) => Instruction::SHR,
            (AssignOp::BitAnd, _) => Instruction::BITAND,
            (AssignOp::BitOr, _) => Instruction::BITOR,
            (AssignOp::BitXor, _) => Instruction::XOR,
        }
    }

    fn emit_read_lvalue(&mut self, bytecode: &mut CodeBuf, target: &Output) -> bool {
        match target.1.as_ref() {
            Expression::Identifier(name) => {
                if let Some(slot) = self.variable_slot(name) {
                    bytecode.push_load(slot);
                    self.is_float_ty(target)
                } else {
                    false
                }
            }
            Expression::Access(receiver, field) => {
                bytecode.append(&mut self.do_compile(receiver));
                self.emit_field_name(bytecode, field);
                bytecode.push_get_field();
                matches!(
                    self.receiver_type(receiver),
                    Some(crate::typechecking::Ty::Con(ref n))
                        if n == crate::typechecking::ty::FLOAT
                )
            }
            Expression::Index(arr, Some(idx)) => {
                if let Expression::Identifier(name) = arr.1.as_ref()
                    && let Some((base, n)) = self.stack_array_info(name)
                    && let Expression::Integer(i) = idx.1.as_ref()
                    && *i >= 0
                    && (*i as usize) < n
                {
                    bytecode.push_load(base + *i as u32);
                    return self.is_float_ty(target);
                }
                let tmp_arr = self.alloc_temp_slot();
                let tmp_idx = self.alloc_temp_slot();
                bytecode.append(&mut self.do_compile(arr));
                bytecode.push_store_pop(tmp_arr);
                bytecode.append(&mut self.do_compile(idx));
                bytecode.push_store_pop(tmp_idx);
                bytecode.push_load(tmp_arr);
                bytecode.push_load(tmp_idx);
                bytecode.push_index();
                false
            }
            Expression::Index(_, None) => false,
            _ => false,
        }
    }

    fn emit_write_lvalue(
        &mut self,
        bytecode: &mut CodeBuf,
        target: &Output,
        leave_value_on_stack: bool,
    ) {
        match target.1.as_ref() {
            Expression::Identifier(name) => {
                if let Some(slot) = self.variable_slot(name) {
                    if leave_value_on_stack {
                        bytecode.push(Byte::new(Instruction::DUPLICATE));
                    }
                    bytecode.push_store_pop(slot);
                }
            }
            Expression::Access(receiver, field) => {
                if leave_value_on_stack {
                    bytecode.push(Byte::new(Instruction::DUPLICATE));
                }
                bytecode.append(&mut self.do_compile(receiver));
                self.emit_field_name(bytecode, field);
                bytecode.push_set_field();
                // SetField leaves the value; caller uses leave_value_on_stack /
                // discard_statement_value to keep or POP.
            }
            Expression::Index(arr, Some(idx)) => {
                // Const store into a multi-slot stack array → direct STORE.
                if let Expression::Identifier(name) = arr.1.as_ref()
                    && let Some((base, n)) = self.stack_array_info(name)
                    && let Expression::Integer(i) = idx.1.as_ref()
                    && *i >= 0
                    && (*i as usize) < n
                {
                    let slot = base + *i as u32;
                    if leave_value_on_stack {
                        bytecode.push(Byte::new(Instruction::DUPLICATE));
                    }
                    bytecode.push_store_pop(slot);
                } else {
                    // Always stash the RHS — `StoreIndex` pops value/index/array.
                    // Dropping with POP when `leave_value_on_stack == false` left
                    // StoreIndex without a value (stack underflow / wrong write).
                    let stack_info = match arr.1.as_ref() {
                        Expression::Identifier(name) => self.stack_array_info(name),
                        _ => None,
                    };
                    let tmp_val = self.alloc_temp_slot();
                    if stack_info.is_none() {
                        // Heap array: RHS is always spilled. Leaving the array on
                        // the operand stack is only safe when `idx` is a pure push
                        // (ident/int): a call/inline that `STORE`s temps seeks
                        // `tell` past the stranded array, so StoreIndex pops a
                        // stale slot instead of the array pointer.
                        bytecode.push_store_pop(tmp_val);
                        if Self::index_keeps_array_on_stack_safe(idx.1.as_ref()) {
                            let depth_on_entry = self.expr_depth;
                            bytecode.append(&mut self.do_compile(arr));
                            self.expr_depth = depth_on_entry + 1;
                            bytecode.append(&mut self.do_compile(idx));
                            self.expr_depth = depth_on_entry;
                            bytecode.push_load(tmp_val);
                        } else {
                            let tmp_arr = self.alloc_temp_slot();
                            let tmp_idx = self.alloc_temp_slot();
                            bytecode.append(&mut self.do_compile(arr));
                            bytecode.push_store_pop(tmp_arr);
                            bytecode.append(&mut self.do_compile(idx));
                            bytecode.push_store_pop(tmp_idx);
                            bytecode.push_load(tmp_arr);
                            bytecode.push_load(tmp_idx);
                            bytecode.push_load(tmp_val);
                        }
                        bytecode.push(Byte::new(Instruction::StoreIndex));
                        if leave_value_on_stack {
                            // StoreIndex leaves the value on the stack; keep it.
                        } else {
                            bytecode.push_pop();
                        }
                        return;
                    }
                    // Stack array: `emit_unbox_stack_array` needs the boxed
                    // address back, so keep it in a temp.
                    let tmp_arr = self.alloc_temp_slot();
                    let tmp_idx = self.alloc_temp_slot();
                    bytecode.push_store_pop(tmp_val);
                    bytecode.append(&mut self.do_compile(arr));
                    bytecode.push_store_pop(tmp_arr);
                    bytecode.append(&mut self.do_compile(idx));
                    bytecode.push_store_pop(tmp_idx);
                    bytecode.push_load(tmp_arr);
                    bytecode.push_load(tmp_idx);
                    bytecode.push_load(tmp_val);
                    bytecode.push(Byte::new(Instruction::StoreIndex));
                    if let Some((base, n)) = stack_info {
                        bytecode.push_pop();
                        self.emit_unbox_stack_array(bytecode, tmp_arr, base, n);
                        if leave_value_on_stack {
                            bytecode.push_load(tmp_val);
                        }
                    } else if leave_value_on_stack {
                        // StoreIndex leaves the value on the stack; keep it.
                    } else {
                        bytecode.push_pop();
                    }
                }
            }
            Expression::Index(_, None) => {}
            _ => {
                bytecode.push_pop();
            }
        }
    }

    fn emit_compound_assign(
        &mut self,
        bytecode: &mut CodeBuf,
        self_id: Option<crate::typechecking::id::NodeId>,
        span_start: usize,
        span_end: usize,
        target: &Output,
        op: parser::ast::AssignOp,
        rhs: &Output,
    ) {
        if matches!(op, parser::ast::AssignOp::Add)
            && self.is_string_expr(target)
            && self.is_string_expr(rhs)
        {
            self.emit_raw_string_literal(bytecode, "%s%s");
            let _ = self.emit_read_lvalue(bytecode, target);
            bytecode.append(&mut self.do_compile(rhs));
            bytecode.push(Byte::new(Instruction::FORMAT).with_operand_u32(2));
            self.emit_write_lvalue(bytecode, target, false);
            return;
        }

        let agg_op = match op {
            parser::ast::AssignOp::Add => Some(crate::typechecking::AggregateOp::Add),
            parser::ast::AssignOp::Sub => Some(crate::typechecking::AggregateOp::Sub),
            parser::ast::AssignOp::Mul => Some(crate::typechecking::AggregateOp::Mul),
            parser::ast::AssignOp::Div => Some(crate::typechecking::AggregateOp::Div),
            parser::ast::AssignOp::Mod => Some(crate::typechecking::AggregateOp::Mod),
            parser::ast::AssignOp::Pow => Some(crate::typechecking::AggregateOp::Pow),
            _ => None,
        };
        if let Some(agg_op) = agg_op {
            let mut tmp = CodeBuf::new();
            if self.try_emit_matrix_op(&mut tmp, self_id, span_start, span_end, target, Some(rhs)) {
                bytecode.append(&mut tmp);
                self.emit_write_lvalue(bytecode, target, false);
                return;
            }
            if self.try_emit_aggregate_arith(
                &mut tmp,
                self_id,
                span_start,
                span_end,
                target,
                Some(rhs),
                agg_op,
            ) {
                bytecode.append(&mut tmp);
                self.emit_write_lvalue(bytecode, target, false);
                return;
            }
        }

        if let Expression::Index(arr, Some(idx)) = target.1.as_ref() {
            // Const index into a multi-slot stack array → read/op/write slots.
            // The heap path below boxes via Identifier escape and StoreIndex
            // would mutate only that temporary.
            if let Expression::Identifier(name) = arr.1.as_ref()
                && let Some((base, n)) = self.stack_array_info(name)
                && let Expression::Integer(i) = idx.1.as_ref()
                && *i >= 0
                && (*i as usize) < n
            {
                let slot = base + *i as u32;
                let is_float = self.is_float_ty(target);
                bytecode.push_load(slot);
                bytecode.append(&mut self.do_compile(rhs));
                bytecode.push(Byte::new(Self::binop_for_assign_op(op, is_float)));
                bytecode.push_store_pop(slot);
                return;
            }
            let stack_info = match arr.1.as_ref() {
                Expression::Identifier(name) => self.stack_array_info(name),
                _ => None,
            };
            let tmp_arr = self.alloc_temp_slot();
            let tmp_idx = self.alloc_temp_slot();
            bytecode.append(&mut self.do_compile(arr));
            bytecode.push_store_pop(tmp_arr);
            bytecode.append(&mut self.do_compile(idx));
            bytecode.push_store_pop(tmp_idx);
            bytecode.push_load(tmp_arr);
            bytecode.push_load(tmp_idx);
            bytecode.push_index();
            bytecode.append(&mut self.do_compile(rhs));
            bytecode.push(Byte::new(Self::binop_for_assign_op(op, false)));
            let tmp_val = self.alloc_temp_slot();
            bytecode.push_store_pop(tmp_val);
            bytecode.push_load(tmp_arr);
            bytecode.push_load(tmp_idx);
            bytecode.push_load(tmp_val);
            bytecode.push(Byte::new(Instruction::StoreIndex));
            if let Some((base, n)) = stack_info {
                bytecode.push_pop();
                self.emit_unbox_stack_array(bytecode, tmp_arr, base, n);
            }
            return;
        }

        let is_float = self.emit_read_lvalue(bytecode, target);
        bytecode.append(&mut self.do_compile(rhs));
        bytecode.push(Byte::new(Self::binop_for_assign_op(op, is_float)));
        self.emit_write_lvalue(bytecode, target, false);
    }

    fn emit_adjust(
        &mut self,
        bytecode: &mut CodeBuf,
        target: &Output,
        op: parser::ast::AdjustOp,
        prefix: bool,
    ) {
        if let Expression::Identifier(name) = target.1.as_ref() {
            if let Some(slot) = self.variable_slot(name) {
                let is_float = self.is_float_ty(target);
                let instr = match op {
                    parser::ast::AdjustOp::Inc => Instruction::INC,
                    parser::ast::AdjustOp::Dec => Instruction::DEC,
                };
                bytecode.push(Byte::new(instr).with_inc_dec(slot, prefix, is_float));
                return;
            }
        }

        let delta: i64 = match op {
            parser::ast::AdjustOp::Inc => 1,
            parser::ast::AdjustOp::Dec => -1,
        };

        if let Expression::Index(arr, Some(idx)) = target.1.as_ref() {
            if let Expression::Identifier(name) = arr.1.as_ref()
                && let Some((base, n)) = self.stack_array_info(name)
                && let Expression::Integer(i) = idx.1.as_ref()
                && *i >= 0
                && (*i as usize) < n
            {
                let slot = base + *i as u32;
                let is_float = self.is_float_ty(target);
                let instr = match op {
                    parser::ast::AdjustOp::Inc => Instruction::INC,
                    parser::ast::AdjustOp::Dec => Instruction::DEC,
                };
                bytecode.push(Byte::new(instr).with_inc_dec(slot, prefix, is_float));
                return;
            }
            let tmp_arr = self.alloc_temp_slot();
            let tmp_idx = self.alloc_temp_slot();
            bytecode.append(&mut self.do_compile(arr));
            bytecode.push_store_pop(tmp_arr);
            bytecode.append(&mut self.do_compile(idx));
            bytecode.push_store_pop(tmp_idx);
            bytecode.push_load(tmp_arr);
            bytecode.push_load(tmp_idx);
            bytecode.push_index();
            let tmp_old = if !prefix {
                let t = self.alloc_temp_slot();
                bytecode.push_store_pop(t);
                bytecode.push_load(tmp_arr);
                bytecode.push_load(tmp_idx);
                bytecode.push_index();
                t
            } else {
                0
            };
            bytecode.push(Byte::new_with_value(
                Instruction::CONST,
                Value::from(delta).raw() as _,
            ));
            bytecode.push(Byte::new(Instruction::ADD));
            let tmp_val = self.alloc_temp_slot();
            bytecode.push_store_pop(tmp_val);
            bytecode.push_load(tmp_arr);
            bytecode.push_load(tmp_idx);
            bytecode.push_load(tmp_val);
            bytecode.push(Byte::new(Instruction::StoreIndex));
            if prefix {
                bytecode.push_load(tmp_val);
            } else {
                bytecode.push_load(tmp_old);
            }
            return;
        }

        let is_float = self.emit_read_lvalue(bytecode, target);
        let tmp_old = if !prefix {
            let tmp = self.alloc_temp_slot();
            bytecode.push_store_pop(tmp);
            tmp
        } else {
            0
        };
        if is_float {
            bytecode.push(Byte::new_with_value(
                Instruction::CONST,
                Value::from(delta as f64).raw() as _,
            ));
            bytecode.push(Byte::new(Instruction::ADDF));
        } else {
            bytecode.push(Byte::new_with_value(
                Instruction::CONST,
                Value::from(delta).raw() as _,
            ));
            bytecode.push(Byte::new(Instruction::ADD));
        }
        self.emit_write_lvalue(bytecode, target, false);
        if prefix {
            self.emit_read_lvalue(bytecode, target);
        } else {
            bytecode.push_load(tmp_old);
        }
    }

    fn qualify_static_fqn(&self, name: &str) -> String {
        if self.namespace.is_empty() {
            name.to_string()
        } else {
            format!("{}::{}", self.namespace, name)
        }
    }

    /// Source ident / `use` alias → class table key (`module::Name`).
    fn resolve_class_ident(&self, name: &str) -> String {
        self.checker
            .resolve_class_key(name)
            .unwrap_or_else(|| name.to_string())
    }

    fn class_member_fqn(&self, owner: &str, member: &str) -> String {
        format!("{}::{}", self.resolve_class_ident(owner), member)
    }

    fn emit_static_initializer(&mut self, fqn: &str, init: &Output) {
        let Some(slot) = self.checker.static_slot_index(fqn) else {
            return;
        };
        if let Some(val) = crate::const_fold::eval_expr(init, self.const_env()) {
            if self.checker.is_static_const_fqn(fqn) {
                self.static_const_values.insert(fqn.to_string(), val);
            }
        }
        let mut init_bc = self.do_compile(init);
        self.static_init.append(&mut init_bc);
        self.static_init
            .push(Byte::new(Instruction::StoreStatic).with_operand_u32(slot));
    }

    /// Resolve enum name for field access via the codegen side-table.
    /// Receiver enum name for field-access codegen (kept for tests / callers).
    #[allow(dead_code)]
    fn enum_name_for_receiver(&mut self, receiver: &Output) -> Option<String> {
        // Cannot use infer cache inside function bodies (ID misalignment) or env (frame popped).
        let ty = self.receiver_type(receiver)?;
        extract_enum_name(&ty)
    }

    /// Receiver type for field access / method calls.
    ///
    /// Handles identifiers, chained access, parentheses/`Group` wrappers, and
    /// falls back to [`Self::codegen_expr_ty`] for forms like `new Class(...)`
    /// so `(self).field` and `(new C(...)).method()` resolve as class
    /// instances (not the LoadField/empty-owner miscompile path).
    fn receiver_type(&self, receiver: &Output) -> Option<Ty> {
        match receiver.1.as_ref() {
            Expression::Expr(inner)
            | Expression::Group(inner)
            | Expression::Statement(inner)
            | Expression::ExprStatement(inner) => self.receiver_type(inner),
            Expression::Identifier(_) => self.codegen_ident_ty(receiver),
            Expression::Access(inner, field) => {
                let inner_ty = self.receiver_type(inner)?;
                if let Some(name) = Checker::class_name_of_ty(&inner_ty) {
                    if self.checker.is_class(name) {
                        return self.codegen_class_field_ty(name, field, &inner_ty);
                    }
                }
                if let Some(name) = extract_enum_name(&inner_ty) {
                    return self.checker.field_type_for(&name, field);
                }
                if let Ty::Record { fields } = &inner_ty {
                    return fields
                        .iter()
                        .find(|(n, _)| n == field)
                        .map(|(_, t)| t.clone());
                }
                None
            }
            // `new Class(...)`, calls, etc. — reuse the general expr-type helper
            // (span cache / Instantiate Con) instead of treating the receiver
            // as unknown and emitting LoadField(0).
            _ => self.codegen_expr_ty(receiver),
        }
    }

    /// Class field type for codegen, substituting type args from `App`.
    fn codegen_class_field_ty(&self, class: &str, field: &str, receiver_ty: &Ty) -> Option<Ty> {
        use crate::typechecking::ty::subst_ty_params;
        let fty = self.checker.class_field_ty(class, field)?.clone();
        let params = self
            .checker
            .generics()
            .generic_type_ctors
            .get(class)
            .cloned()
            .unwrap_or_default();
        if params.is_empty() {
            return Some(fty);
        }
        let args = match receiver_ty {
            Ty::App(_, args) => args.clone(),
            _ => return Some(fty),
        };
        let mut map = std::collections::HashMap::new();
        for (p, a) in params.iter().zip(args.iter()) {
            map.insert(p.clone(), a.clone());
        }
        Some(subst_ty_params(&fty, &map))
    }

    /// True when `expr` is (or produces) the built-in `Option` sum.
    fn expr_is_option(&self, expr: &Output) -> bool {
        use crate::typechecking::ty::is_option_ty;
        match expr.1.as_ref() {
            Expression::Construct { enum_name, .. } => common::is_builtin_option_enum(enum_name),
            Expression::Group(inner) | Expression::Expr(inner) => self.expr_is_option(inner),
            _ => self
                .codegen_expr_ty(expr)
                .map(|t| is_option_ty(&t))
                .unwrap_or(false),
        }
    }

    /// Return the payload type when `Option<T>` can use `0` as `None`.
    ///
    /// Only ground heap values qualify: immediates and stack-shaped
    /// aggregates keep the existing boxed enum representation.
    fn niche_option_inner_ty(&self, _ty: &Ty) -> Option<Ty> {
        None
    }

    fn expr_is_niche_option(&self, _expr: &Output) -> bool {
        false
    }

    fn is_option_construct(expr: &Output) -> bool {
        match expr.1.as_ref() {
            Expression::Construct { enum_name, .. } => {
                common::is_builtin_option_enum(enum_name)
            }
            Expression::Group(inner) | Expression::Expr(inner) => Self::is_option_construct(inner),
            _ => false,
        }
    }

    fn niche_heap_only_ty(ty: &Ty, checker: &Checker) -> bool {
        match ty {
            Ty::Readonly(inner) | Ty::Constructor { owner: inner, .. } => {
                Self::niche_heap_only_ty(inner, checker)
            }
            Ty::Con(name) => name == "string" || checker.is_class(name),
            Ty::App(head, args) => {
                let Ty::Con(name) = head.as_ref() else {
                    return false;
                };
                checker.is_class(name)
                    && args.iter().all(|arg| Self::niche_ground_ty(arg, checker))
            }
            Ty::List(_) | Ty::Sum { .. } | Ty::Tuple(_) | Ty::Record { .. } => {
                Self::niche_ground_ty(ty, checker)
            }
            _ => false,
        }
    }

    fn niche_ground_ty(ty: &Ty, checker: &Checker) -> bool {
        match ty {
            Ty::Var(_) | Ty::Fun(_, _) | Ty::Existential { .. } | Ty::Forall { .. } => false,
            Ty::Con(name) => name == "string" || checker.is_class(name),
            Ty::App(head, args) => {
                let Ty::Con(name) = head.as_ref() else {
                    return false;
                };
                checker.is_class(name)
                    && args.iter().all(|arg| Self::niche_ground_ty(arg, checker))
            }
            Ty::List(inner) | Ty::Readonly(inner) => Self::niche_ground_ty(inner, checker),
            Ty::Sum { variants, .. } => variants.iter().all(|(_, payload)| {
                payload
                    .field_types()
                    .into_iter()
                    .all(|field| Self::niche_ground_ty(field, checker))
            }),
            Ty::Constructor { owner, .. } => Self::niche_ground_ty(owner, checker),
            Ty::Tuple(items) => items.iter().all(|item| Self::niche_ground_ty(item, checker)),
            Ty::Record { fields } => fields
                .iter()
                .all(|(_, field)| Self::niche_ground_ty(field, checker)),
            Ty::Array { .. } | Ty::Never => false,
        }
    }

    /// Wrap the top-of-stack value as `Ok(v)` (Result) or `Some(v)` (Option).
    fn emit_ok_or_some_wrap(bytecode: &mut impl EmitBuf, is_option: bool) {
        let tag = if is_option { 1u16 } else { 0u16 }; // Some=1, Ok=0
        bytecode.push_make_enum(tag, 1);
    }

    /// Wrap the top-of-stack value as `Result::Err(e)`.
    fn emit_result_err(bytecode: &mut impl EmitBuf) {
        bytecode.push_make_enum(1, 1); // Err tag=1 arity=1
    }

    /// Emit `Matrix` ops (`*`, `+`, `-`, unary `-`) when the typechecker
    /// recorded [`LinearAlgebraInfo`] on this node.
    fn try_emit_matrix_op(
        &mut self,
        bytecode: &mut CodeBuf,
        self_id: Option<crate::typechecking::id::NodeId>,
        span_start: usize,
        span_end: usize,
        lhs: &Output,
        rhs: Option<&Output>,
    ) -> bool {
        let Some(info) = self_id
            .and_then(|id| self.checker.linear_algebra_at(id))
            .cloned()
            .or_else(|| self.checker.linear_algebra_span(span_start, span_end).cloned())
        else {
            return false;
        };
        match &info.kind {
            crate::typechecking::LinearAlgebraKind::MatMul { .. }
            | crate::typechecking::LinearAlgebraKind::MatrixZip { .. } => {
                let Some(rhs) = rhs else {
                    return false;
                };
                let args = [lhs.clone(), rhs.clone()];
                self.emit_linear_algebra(bytecode, self_id, span_start, span_end, &args);
                true
            }
            crate::typechecking::LinearAlgebraKind::MatrixNeg { .. } => {
                let args = [lhs.clone()];
                self.emit_linear_algebra(bytecode, self_id, span_start, span_end, &args);
                true
            }
            _ => false,
        }
    }

    /// Emit `dot` / `matmul` / `cross` / Matrix ops from the linear-algebra side table.
    ///
    /// Approach A: Dot / MatMul / MatrixZip / MatrixNeg lower to packed fat
    /// opcodes when dims fit the operand packing; otherwise keep the scalar
    /// unroll. `cross` stays unrolled (fixed N=3).
    fn emit_linear_algebra(
        &mut self,
        bytecode: &mut CodeBuf,
        self_id: Option<crate::typechecking::id::NodeId>,
        span_start: usize,
        span_end: usize,
        args: &[Output],
    ) {
        use crate::typechecking::{AggregateOp, LinearAlgebraKind};

        let Some(info) = self_id
            .and_then(|id| self.checker.linear_algebra_at(id))
            .cloned()
            .or_else(|| self.checker.linear_algebra_span(span_start, span_end).cloned())
        else {
            return;
        };

        let needs_two = !matches!(info.kind, LinearAlgebraKind::MatrixNeg { .. });
        if needs_two && args.len() != 2 {
            return;
        }
        if !needs_two && args.is_empty() {
            return;
        }

        // Prefer packed HostInvoke kernels (Approach A) for Dot / MatMul / Matrix*.
        if self.try_emit_packed_linear_algebra(bytecode, &info.kind, args) {
            return;
        }

        let t0 = self.alloc_temp_slot();
        bytecode.append(&mut self.do_compile(&args[0]));
        bytecode.push_store_pop(t0);
        let t1 = if needs_two {
            let slot = self.alloc_temp_slot();
            bytecode.append(&mut self.do_compile(&args[1]));
            bytecode.push_store_pop(slot);
            Some(slot)
        } else {
            None
        };

        match info.kind {
            LinearAlgebraKind::Dot {
                length,
                elem_is_float,
                ..
            } => {
                let t1 = t1.expect("dot needs two args");
                let mul = if elem_is_float {
                    Instruction::MULF
                } else {
                    Instruction::MUL
                };
                let add = if elem_is_float {
                    Instruction::ADDF
                } else {
                    Instruction::ADD
                };
                for i in 0..length {
                    bytecode.push_load(t0);
                    bytecode.push_const(i as i32);
                    bytecode.push_index();
                    bytecode.push_load(t1);
                    bytecode.push_const(i as i32);
                    bytecode.push_index();
                    bytecode.push(Byte::new(mul));
                    if i > 0 {
                        bytecode.push(Byte::new(add));
                    }
                }
            }
            LinearAlgebraKind::Cross {
                left_is_tuple,
                elem_is_float,
            } => {
                let t1 = t1.expect("cross needs two args");
                let mul = if elem_is_float {
                    Instruction::MULF
                } else {
                    Instruction::MUL
                };
                let sub = if elem_is_float {
                    Instruction::SUBF
                } else {
                    Instruction::SUB
                };
                // Load components into temps for clarity.
                let ax = self.alloc_temp_slot();
                let ay = self.alloc_temp_slot();
                let az = self.alloc_temp_slot();
                let bx = self.alloc_temp_slot();
                let by = self.alloc_temp_slot();
                let bz = self.alloc_temp_slot();
                for (slot, src, i) in [
                    (ax, t0, 0),
                    (ay, t0, 1),
                    (az, t0, 2),
                    (bx, t1, 0),
                    (by, t1, 1),
                    (bz, t1, 2),
                ] {
                    bytecode.push_load(src);
                    bytecode.push_const(i);
                    bytecode.push_index();
                    bytecode.push_store_pop(slot);
                }
                // i = ay*bz - az*by
                bytecode.push_load(ay);
                bytecode.push_load(bz);
                bytecode.push(Byte::new(mul));
                bytecode.push_load(az);
                bytecode.push_load(by);
                bytecode.push(Byte::new(mul));
                bytecode.push(Byte::new(sub));
                // j = az*bx - ax*bz
                bytecode.push_load(az);
                bytecode.push_load(bx);
                bytecode.push(Byte::new(mul));
                bytecode.push_load(ax);
                bytecode.push_load(bz);
                bytecode.push(Byte::new(mul));
                bytecode.push(Byte::new(sub));
                // k = ax*by - ay*bx
                bytecode.push_load(ax);
                bytecode.push_load(by);
                bytecode.push(Byte::new(mul));
                bytecode.push_load(ay);
                bytecode.push_load(bx);
                bytecode.push(Byte::new(mul));
                bytecode.push(Byte::new(sub));
                if left_is_tuple {
                    bytecode.push_make_tuple(3);
                } else {
                    bytecode.push_make_array(3);
                }
            }
            LinearAlgebraKind::MatMul {
                m,
                k,
                n,
                outer_is_tuple,
                row_is_tuple,
                elem_is_float,
            } => {
                let t1 = t1.expect("matmul needs two args");
                let mul = if elem_is_float {
                    Instruction::MULF
                } else {
                    Instruction::MUL
                };
                let add = if elem_is_float {
                    Instruction::ADDF
                } else {
                    Instruction::ADD
                };
                for i in 0..m {
                    for j in 0..n {
                        for t in 0..k {
                            // A[i][t]
                            bytecode.push_load(t0);
                            bytecode.push_const(i as i32);
                            bytecode.push_index();
                            bytecode.push_const(t as i32);
                            bytecode.push_index();
                            // B[t][j]
                            bytecode.push_load(t1);
                            bytecode.push_const(t as i32);
                            bytecode.push_index();
                            bytecode.push_const(j as i32);
                            bytecode.push_index();
                            bytecode.push(Byte::new(mul));
                            if t > 0 {
                                bytecode.push(Byte::new(add));
                            }
                        }
                    }
                    if row_is_tuple {
                        bytecode.push_make_tuple(n as u32);
                    } else {
                        bytecode.push_make_array(n as u32);
                    }
                }
                if outer_is_tuple {
                    bytecode.push_make_tuple(m as u32);
                } else {
                    bytecode.push_make_array(m as u32);
                }
            }
            LinearAlgebraKind::MatrixZip {
                m,
                n,
                op,
                outer_is_tuple,
                row_is_tuple,
                elem_is_float,
            } => {
                let t1 = t1.expect("matrix zip needs two args");
                let cell_op = match (op, elem_is_float) {
                    (AggregateOp::Add, false) => Instruction::ADD,
                    (AggregateOp::Add, true) => Instruction::ADDF,
                    (AggregateOp::Sub, false) => Instruction::SUB,
                    (AggregateOp::Sub, true) => Instruction::SUBF,
                    _ => Instruction::ADD,
                };
                for i in 0..m {
                    for j in 0..n {
                        bytecode.push_load(t0);
                        bytecode.push_const(i as i32);
                        bytecode.push_index();
                        bytecode.push_const(j as i32);
                        bytecode.push_index();
                        bytecode.push_load(t1);
                        bytecode.push_const(i as i32);
                        bytecode.push_index();
                        bytecode.push_const(j as i32);
                        bytecode.push_index();
                        bytecode.push(Byte::new(cell_op));
                    }
                    if row_is_tuple {
                        bytecode.push_make_tuple(n as u32);
                    } else {
                        bytecode.push_make_array(n as u32);
                    }
                }
                if outer_is_tuple {
                    bytecode.push_make_tuple(m as u32);
                } else {
                    bytecode.push_make_array(m as u32);
                }
            }
            LinearAlgebraKind::MatrixNeg {
                m,
                n,
                outer_is_tuple,
                row_is_tuple,
                elem_is_float,
            } => {
                for i in 0..m {
                    for j in 0..n {
                        bytecode.push_load(t0);
                        bytecode.push_const(i as i32);
                        bytecode.push_index();
                        bytecode.push_const(j as i32);
                        bytecode.push_index();
                        self.emit_neg_tos(bytecode, elem_is_float);
                    }
                    if row_is_tuple {
                        bytecode.push_make_tuple(n as u32);
                    } else {
                        bytecode.push_make_array(n as u32);
                    }
                }
                if outer_is_tuple {
                    bytecode.push_make_tuple(m as u32);
                } else {
                    bytecode.push_make_array(m as u32);
                }
            }
        }
    }

    /// Emit Approach A packed LA via `HostInvoke` (no new opcodes) when dims fit.
    /// Returns false to fall back to scalar unroll.
    fn try_emit_packed_linear_algebra(
        &mut self,
        bytecode: &mut CodeBuf,
        kind: &crate::typechecking::LinearAlgebraKind,
        args: &[Output],
    ) -> bool {
        use crate::typechecking::{AggregateOp, LinearAlgebraKind};

        let (native_name, meta, value_args): (&str, u32, &[Output]) = match kind {
            LinearAlgebraKind::Dot {
                length,
                elem_is_float,
                ..
            } => {
                if *length == 0 || *length > u16::MAX as usize || args.len() != 2 {
                    return false;
                }
                let mut ops = (*length as u32) & 0xFFFF;
                if *elem_is_float {
                    ops |= 1 << 16;
                }
                (common::PACKED_DOT, ops, args)
            }
            LinearAlgebraKind::MatMul {
                m,
                k,
                n,
                outer_is_tuple,
                row_is_tuple,
                elem_is_float,
            } => {
                if args.len() != 2
                    || *m == 0
                    || *k == 0
                    || *n == 0
                    || *m > u8::MAX as usize
                    || *k > u8::MAX as usize
                    || *n > u8::MAX as usize
                {
                    return false;
                }
                let mut ops = (*m as u32) | ((*k as u32) << 8) | ((*n as u32) << 16);
                if *elem_is_float {
                    ops |= 1 << 24;
                }
                if *outer_is_tuple {
                    ops |= 1 << 25;
                }
                if *row_is_tuple {
                    ops |= 1 << 26;
                }
                (common::PACKED_MATMUL, ops, args)
            }
            LinearAlgebraKind::MatrixZip {
                m,
                n,
                op,
                outer_is_tuple,
                row_is_tuple,
                elem_is_float,
            } => {
                if args.len() != 2
                    || *m == 0
                    || *n == 0
                    || *m > u8::MAX as usize
                    || *n > u8::MAX as usize
                {
                    return false;
                }
                let zip_kind: u32 = match op {
                    AggregateOp::Add => 0,
                    AggregateOp::Sub => 1,
                    _ => return false,
                };
                let mut ops = (*m as u32) | ((*n as u32) << 8) | (zip_kind << 16);
                if *elem_is_float {
                    ops |= 1 << 24;
                }
                if *outer_is_tuple {
                    ops |= 1 << 25;
                }
                if *row_is_tuple {
                    ops |= 1 << 26;
                }
                (common::PACKED_MATRIX_ZIP, ops, args)
            }
            LinearAlgebraKind::MatrixNeg {
                m,
                n,
                outer_is_tuple,
                row_is_tuple,
                elem_is_float,
            } => {
                if args.is_empty()
                    || *m == 0
                    || *n == 0
                    || *m > u8::MAX as usize
                    || *n > u8::MAX as usize
                {
                    return false;
                }
                let mut ops = (*m as u32) | ((*n as u32) << 8);
                if *elem_is_float {
                    ops |= 1 << 16;
                }
                if *outer_is_tuple {
                    ops |= 1 << 17;
                }
                if *row_is_tuple {
                    ops |= 1 << 18;
                }
                (common::PACKED_MATRIX_NEG, ops, args)
            }
            LinearAlgebraKind::Cross { .. } => return false,
        };

        let Some(native_id) = self.native_id(native_name) else {
            return false;
        };

        // HostInvoke stack: [id, args_tuple]; tuple = [arg0, …, meta].
        // Meta is a full u32 bitfield — must use `with_operand_u32` (not
        // `with_value_u32`, which only keeps the low 16 bits).
        let depth_on_entry = self.expr_depth;
        bytecode.push(Byte::new(Instruction::CONST).with_operand_u32(native_id as u32));
        self.expr_depth = depth_on_entry + 1;
        for arg in value_args {
            bytecode.append(&mut self.do_compile(arg));
            self.expr_depth += 1;
        }
        bytecode.push(Byte::new(Instruction::CONST).with_operand_u32(meta));
        self.expr_depth += 1;
        let arity = value_args.len() + 1; // + meta
        bytecode.push_make_tuple(arity as u32);
        bytecode.push_host_invoke(arity as u32);
        self.expr_depth = depth_on_entry;
        true
    }

    /// Desugar `assert(cond[, msg])` to Ok(()) / Err(msg) via MakeEnum.
    ///
    /// Emits into `self.bytecode` so nested absolute jumps stay valid.
    fn emit_assert(&mut self, args: &[Output]) {
        if args.is_empty() || args.len() > 2 {
            return;
        }

        let mut bb = BlockBuilder::new();
        let fail = bb.fresh_label(self.bytecode.il_mut());
        let end = bb.fresh_label(self.bytecode.il_mut());

        let mut cond_bc = self.do_compile(&args[0]);
        self.bytecode.append(&mut cond_bc);
        bb.emit_jump_to(fail, BbJumpKind::JumpIfFalse, self.bytecode.il_mut());

        // Success: Ok(())
        self.bytecode.push(Byte::new_with_value(
            Instruction::CONST,
            Value::from(0i64).raw() as _,
        ));
        Self::emit_ok_or_some_wrap(&mut self.bytecode, false);
        bb.emit_jump_to(end, BbJumpKind::Unconditional, self.bytecode.il_mut());
        bb.bind_label(fail, self.bytecode.il_mut());
        if let Some(msg) = args.get(1) {
            let mut msg_bc = self.do_compile(msg);
            self.bytecode.append(&mut msg_bc);
        } else {
            self.emit_string_literal("assertion failed");
        }
        Self::emit_result_err(&mut self.bytecode);
        bb.bind_label(end, self.bytecode.il_mut());
    }

    /// Drive `block_on(coro)`: resume until `done`, leave completion value on stack.
    ///
    /// Intermediate yields are discarded. Between resumes, `wait_ready` parks on
    /// any registered IO waiters so cooperative `await_*` inside the coroutine
    /// can batch instead of busy-spinning.
    fn emit_block_on(&mut self, args: &[Output]) {
        let Some(coro_expr) = args.first() else {
            return;
        };
        let handle_slot = self.alloc_temp_slot();
        let value_slot = self.alloc_temp_slot();
        let mut coro_bc = self.do_compile(coro_expr);
        self.bytecode.append(&mut coro_bc);
        self.bytecode.push_store_pop(handle_slot);

        let mut bb = BlockBuilder::new();
        let top = bb.fresh_label(self.bytecode.il_mut());
        let done_exit = bb.fresh_label(self.bytecode.il_mut());
        bb.bind_label(top, self.bytecode.il_mut());

        self.bytecode.push_load(handle_slot);
        self.bytecode
            .push(Byte::new(Instruction::ResumeCoro).with_operand_u32(0));
        self.bytecode.push_store_pop(value_slot);

        self.bytecode.push_load(handle_slot);
        self.bytecode.push(Byte::new(Instruction::DoneCoro));
        // Done → fall through to exit; not done → wait for batched IO then loop.
        bb.emit_jump_to(done_exit, BbJumpKind::JumpIfTrue, self.bytecode.il_mut());
        if self.native_id("wait_ready").is_some() {
            self.emit_host_native_invoke("wait_ready", &[]);
            self.bytecode.push(Byte::new(Instruction::POP));
        }
        bb.emit_jump_to(top, BbJumpKind::Unconditional, self.bytecode.il_mut());
        bb.bind_label(done_exit, self.bytecode.il_mut());
        self.bytecode.push_load(value_slot);
    }

    /// Emit a synthetic `main` that runs every harness test case in one VM
    /// (standalone `cargo run -- tests/foo.hy`). Prints
    /// `> Test "<name>" failed` on soft failures and panics with
    /// `"tests failed"` if any case failed.
    fn emit_virtual_test_main(&mut self) {
        let cases: Vec<(String, u32)> = self.test_cases.clone();
        if cases.is_empty() {
            return;
        }

        self.bind_function_entry("main".to_string());
        let body_start = self.bytecode.len();

        let prev_vars = std::mem::take(&mut self.context.variables);
        self.context.variables = Interner::default();
        // slot 0 = failed count
        let failed_slot = self.context.variables.intern("failed".to_string()) as u32;
        self.bytecode.push(Byte::new_with_value(
            Instruction::CONST,
            Value::from(0i64).raw() as _,
        ));
        self.bytecode.push_store_pop(failed_slot);

        let mut bb = BlockBuilder::new();
        for (desc, offset) in &cases {
            if let Some(label) = self.bytecode.entry_label_for_offset(*offset as usize) {
                self.bytecode.emit_entry(EntryKind::Call, 0, label);
            } else {
                // Fallback for cases without a bound entry label (should be rare
                // after `bind_function_entry`); packed CALL(0, pc) keeps harness green.
                self.bytecode
                    .push(Byte::new(Instruction::CALL).with_call_packed(0, *offset));
            }
            // Jump if Result::Err (tag 1) — on match, payload (message) is pushed.
            let fail = bb.fresh_label(self.bytecode.il_mut());
            let done = bb.fresh_label(self.bytecode.il_mut());
            bb.emit_jump_to(
                fail,
                BbJumpKind::JumpIfMatch { tag: 1, arity: 1 },
                self.bytecode.il_mut(),
            );
            // Ok path: discard whole Result enum.
            self.bytecode.push_pop();
            bb.emit_jump_to(done, BbJumpKind::Unconditional, self.bytecode.il_mut());
            bb.bind_label(fail, self.bytecode.il_mut());
            // Discard Err message payload.
            self.bytecode.push_pop();
            let msg = format!("> Test \"{desc}\" failed\n");
            self.emit_string_literal(&msg);
            self.bytecode.push_print();
            // failed += 1
            self.bytecode.push_load(failed_slot);
            self.bytecode.push(Byte::new_with_value(
                Instruction::CONST,
                Value::from(1i64).raw() as _,
            ));
            self.bytecode.push(Byte::new(Instruction::ADD));
            self.bytecode.push_store_pop(failed_slot);
            bb.bind_label(done, self.bytecode.il_mut());
        }

        // if failed != 0 { panic "tests failed" }
        let panic_lbl = bb.fresh_label(self.bytecode.il_mut());
        let end_lbl = bb.fresh_label(self.bytecode.il_mut());
        self.bytecode.push_load(failed_slot);
        self.bytecode.push(Byte::new_with_value(
            Instruction::CONST,
            Value::from(0i64).raw() as _,
        ));
        self.bytecode.push(Byte::new(Instruction::EQ));
        // failed == 0 → EQ true → fall through JMPF; else JMPF → panic.
        bb.emit_jump_to(panic_lbl, BbJumpKind::JumpIfFalse, self.bytecode.il_mut());
        bb.emit_jump_to(end_lbl, BbJumpKind::Unconditional, self.bytecode.il_mut());
        bb.bind_label(panic_lbl, self.bytecode.il_mut());
        self.emit_string_literal("tests failed");
        self.bytecode.push(Byte::new(Instruction::Panic));
        bb.bind_label(end_lbl, self.bytecode.il_mut());
        self.bytecode.push_const(0);
        self.bytecode.push_return();

        let body_end = self.bytecode.len();
        self.record_fn_span("main".to_string(), body_start, body_end);
        let entry = self.fn_entry_labels.get("main").copied();
        self.bytecode
            .record_func_with_sp("main".to_string(), entry, body_start, body_end, 0);
        self.context.variables = prev_vars;
    }

    fn do_compile<'compiler>(
        &mut self,
        ast: &(SimpleSpan, Box<Expression<'compiler>>),
    ) -> CodeBuf {
        self.codegen_depth += 1;
        if self.codegen_depth > super::CODEGEN_RECURSION_LIMIT {
            self.messages.push(Message::error(
                ErrorCode::ExpressionNestingTooDeep,
                format!(
                    "expression nested too deeply (over {} levels) for codegen",
                    super::CODEGEN_RECURSION_LIMIT
                ),
                ast.0.into_range(),
            ));
            std::panic::panic_any(super::CodegenRecursionLimitExceeded);
        }
        let result = self.do_compile_inner(ast);
        self.codegen_depth -= 1;
        result
    }

    fn do_compile_inner<'compiler>(
        &mut self,
        ast: &(SimpleSpan, Box<Expression<'compiler>>),
    ) -> CodeBuf {
        let mut bytecode = CodeBuf::new();
        let self_id = self.next_emit_id();
        let (span, child) = ast;

        match child.borrow() {
            Expression::Comment(_) => (),
            // --- Modules ---
            Expression::Use {
                path: p,
                name,
                alias,
            } => {
                // Virtual modules are applied during typecheck
                // (`Checker::apply_virtual_use`); no disk FQN alias.
                if self.checker.virtual_modules().resolves_use(p, name) {
                    // Scope already populated by check_program.
                } else if name == "*" {
                    // Disk-module wildcards are rejected in typecheck
                    // (`ErrorCode::WildcardImport`); leave aliases unchanged.
                } else {
                    // B3: free-fn names resolve through DefId / sidecar, not
                    // `Compiler.aliases`. Typecheck already bound `use`.
                    let _ = (p, name, alias);
                }
            }
            Expression::Noop(_) => (),
            // `mod foo;` — pipeline loads the file; no bytecode.
            Expression::Module(_, _body) => {}
            Expression::Group(e) => bytecode.append(&mut self.do_compile(e)),
            // Named call-site arg — compile the value (defensive; Call reorders).
            Expression::NamedArg(_, value) => {
                bytecode.append(&mut self.do_compile(value));
            }
            Expression::Program(children) => {
                self.reserve_program_callable_entries(children);
                if Self::program_needs_phased_emit(children) {
                    // Emit phases (COI-109): helpers before `impl`, but free fns
                    // that call user `impl` methods (and their callers) must
                    // follow their `impl` blocks, in source order within the
                    // deferred set so callees bind before callers.
                    // All free fns after inherent impls (source order). Reserve
                    // entries so `impl` methods can Entry-call later helpers.
                    self.reserve_phased_free_fn_entries(children);
                    let phase = |c: &Output| -> u8 {
                        match c.1.as_ref() {
                            Expression::Function { name, .. } if *name == "main" => 3,
                            Expression::Function { .. } => 25,
                            Expression::Implementation { .. } => 2,
                            Expression::TestCase { .. } => 3,
                            _ => 0,
                        }
                    };
                    for p in [0u8, 1, 2, 25, 3] {
                        for child in children.iter().filter(|c| phase(c) == p) {
                            bytecode.append(&mut self.do_compile(child));
                        }
                    }
                } else {
                    for child in children {
                        bytecode.append(&mut self.do_compile(child));
                    }
                }
                if !self.test_cases.is_empty() && !self.user_main_defined {
                    self.emit_virtual_test_main();
                }
            }
            // --- `let (a, b) = expr` / `let { x, y } = expr` ---
            Expression::LetDestructure { pattern, rhs } => {
                let rhs_is_match = Self::rhs_is_match_expr(rhs);
                if rhs_is_match {
                    self.emit_binding_rhs(rhs);
                } else {
                    self.append_binding_rhs(&mut bytecode, rhs);
                }
                let tmp = self.alloc_temp_slot();
                if rhs_is_match {
                    self.bytecode.push_store_pop(tmp);
                    let mut binds = CodeBuf::new();
                    self.emit_let_pattern_binds(pattern, tmp, &mut binds);
                    self.bytecode.append(&mut binds);
                } else {
                    bytecode.push_store_pop(tmp);
                    self.emit_let_pattern_binds(pattern, tmp, &mut bytecode);
                }
            }

            // --- Let / const bindings ---
            Expression::Fragment(children) => {
                // `let x = expr` / `const x = expr` → compile RHS, then
                // StorePop into x's slot.
                let mut is_binding = false;
                if children.len() == 2 {
                    let binding = match children[0].1.as_ref() {
                        Expression::Variable(name, _ty) => Some((name.to_string(), false)),
                        Expression::Constant(name, _ty) => {
                            Some((self.resolve_variable(name), true))
                        }
                        _ => None,
                    };
                    if let Some((name, is_const)) = binding {
                        let binder_span = (children[0].0.start, children[0].0.end);
                        // Check if the RHS is a bare identifier that names a generic fn.
                        // If so, track this variable as holding an ObjPolyFn.
                        let polyfn_source = match unwrapped_identifier(&children[1]) {
                            Some(rhs_name) => {
                                let resolved = self.resolve_free_fn(rhs_name);
                                (self.checker.is_generic_fn(&resolved)
                                    || self.functions.get(&resolved).is_some()
                                        && self.checker.is_generic_fn(rhs_name))
                                .then_some(resolved)
                            }
                            _ => None,
                        };
                        if let Some(source) = polyfn_source {
                            self.polyfn_vars.insert(name.clone());
                            self.polyfn_sources.insert(name.clone(), source);
                        } else if self
                            .checker
                            .is_polyfn_binding_at(binder_span.0, binder_span.1)
                        {
                            // Returned/captured PolyFn (`let f = capture_show(0)`).
                            self.polyfn_vars.insert(name.clone());
                        }
                        let rhs_is_match = Self::rhs_is_match_expr(&children[1]);
                        if is_const {
                            // Compile the RHS BEFORE interning the binding name.
                            // Match payload slots use `variables.len()` as the first
                            // free slot; interning early (e.g. `let v = match e`)
                            // reserved a hole and made bindings land one slot too
                            // high while JumpIfMatch still pushed at the real
                            // cursor.
                            if rhs_is_match {
                                self.emit_binding_rhs(&children[1]);
                            } else {
                                self.append_binding_rhs(&mut bytecode, &children[1]);
                            }
                            if let Some(val) = crate::const_fold::eval_expr(&children[1], self.const_env())
                            {
                                self.const_env_mut().insert(name.clone(), val);
                            } else {
                                let slot = self.alloc_binding_slot(&name);
                                self.context.constants.insert(slot as usize, true);
                                if rhs_is_match {
                                    self.bytecode.push_store_pop(slot);
                                } else {
                                    bytecode.push_store_pop(slot);
                                }
                            }
                            is_binding = true;
                        } else {
                            // Fixed `[T; N]` locals (`N >= 1`): N consecutive slots.
                            // Element Values may be immediates or heap pointers;
                            // nested array *elements* compile to MakeArray and
                            // occupy one pointer slot each in the outer spine.
                            // Layout decision is structural where possible: array
                            // literal length, or copy from a known multi-slot local.
                            // Flat `codegen_var_type` is last-wins across functions
                            // (sibling tests often reuse `a`/`b`), so it must not be
                            // the sole source of `N`.
                            let rhs_node = unwrap_expr_output(&children[1]);
                            let bind_ty = self
                                .expr_codegen_ty(rhs_node)
                                .or_else(|| self.codegen_expr_ty(&children[1]))
                                .or_else(|| self.sidecar_ty_of(&children[0]));
                            let fixed_n = bind_ty
                                .as_ref()
                                .and_then(|ty| Self::stack_array_bind_len(ty));
                            let stack_n = match rhs_node.1.as_ref() {
                                Expression::Array(items) if items.len() >= 1 => Some(items.len()),
                                Expression::Identifier(src) => {
                                    self.stack_array_info(src).map(|(_, sn)| sn)
                                }
                                _ => None,
                            }
                            .or(fixed_n);
                            if let Some(n) = stack_n {
                                let can_stack = match rhs_node.1.as_ref() {
                                    Expression::Array(items) if items.len() == n => true,
                                    Expression::Identifier(src) => self
                                        .stack_array_info(src)
                                        .is_some_and(|(_, sn)| sn == n),
                                    _ => false,
                                };
                                if can_stack {
                                    // Binder is a leaf (`Variable` / `Constant`); consume
                                    // its NodeId so array elements see their own sidecar.
                                    let _ = self.next_emit_id();
                                    let base = self.alloc_stack_array_slots(&name, n);
                                    let ok = self.try_emit_stack_array_init(
                                        &mut bytecode,
                                        &children[1],
                                        base,
                                        n,
                                    );
                                    debug_assert!(ok, "can_stack implies init emit");
                                    is_binding = true;
                                } else if rhs_is_match {
                                    self.emit_binding_rhs(&children[1]);
                                    let slot = self.alloc_binding_slot(&name);
                                    self.bytecode.push_store_pop(slot);
                                    is_binding = true;
                                } else {
                                    self.append_binding_rhs(&mut bytecode, &children[1]);
                                    let slot = self.alloc_binding_slot(&name);
                                    bytecode.push_store_pop(slot);
                                    is_binding = true;
                                }
                            } else {
                                if rhs_is_match {
                                    self.emit_binding_rhs(&children[1]);
                                } else {
                                    self.append_binding_rhs(&mut bytecode, &children[1]);
                                }
                                let slot = self.alloc_binding_slot(&name);
                                if rhs_is_match {
                                    self.bytecode.push_store_pop(slot);
                                } else {
                                    bytecode.push_store_pop(slot);
                                }
                                is_binding = true;
                            }
                        }
                    }
                }
                if !is_binding {
                    children.iter().for_each(|child| {
                        bytecode.append(&mut self.do_compile(child));
                    });
                }
            }
            Expression::Block(children) => {
                // Isolate PolyFn tracking the same way function bodies do:
                // keep outer entries visible inside the block, then restore
                // so an inner `let f = capture_show(0)` cannot poison an
                // outer same-named ObjFn / mono local after the block.
                let saved_polyfn_vars = self.polyfn_vars.clone();
                let saved_polyfn_sources = self.polyfn_sources.clone();
                self.push_const_env();
                let ctx = self.context.child();
                self.context = ctx;
                // Append each child to self.bytecode (Print/control-flow emit in-place).
                for child in children {
                    let mut bc = self.do_compile(child);
                    self.bytecode.append(&mut bc);
                }

                self.context = *self.context.get_prev().clone().unwrap();
                self.pop_const_env();
                self.polyfn_vars = saved_polyfn_vars;
                self.polyfn_sources = saved_polyfn_sources;
            }
            Expression::Function {
                docs: _,
                attrs: _,
                name,
                is_coro,
                is_static: _,
                type_params,
                args,
                returns: _returns,
                where_constraints: _,
                body,
            } => {
                let Some(body) = body else {
                    return CodeBuf::new();
                };
                let qualified = if self.namespace.is_empty() {
                    name.to_string()
                } else {
                    format!("{}::{}", self.namespace, name)
                };
                if *name == "main" {
                    self.user_main_defined = true;
                }
                self.module_items
                    .entry(self.namespace.clone())
                    .or_default()
                    .push(name.to_string());
                let (fixed_arity, has_rest) = fn_arity_from_args(args);
                let table_key = if self.checker.is_overloaded(name)
                    || self.checker.is_overloaded(&qualified)
                {
                    if let Some((decl_id, fa, rest)) =
                        self.checker.overload_decl_at(span.start, span.end)
                    {
                        overload_fn_key(&qualified, fa, rest, decl_id)
                    } else {
                        overload_fn_key(&qualified, fixed_arity, has_rest, 0)
                    }
                } else {
                    qualified.clone()
                };
                let _ = self.bind_function_entry(table_key.clone());
                self.fn_arities
                    .insert(table_key.clone(), (fixed_arity as u32, has_rest));
                // Overloads share the unmangled FQN; do not mirror arity
                // under `qualified` (last decl would win and poison
                // fallbacks that lack `selected_overload_at`).
                if *is_coro {
                    self.coroutine_fns.insert(qualified.clone());
                }

                // Fresh slot map per function so locals start at 0
                // (or 1 with `self`) for this frame. Sharing one
                // Interner across functions made later `let`s use high
                // slots; `StorePop` then left holes and match bindings
                // at slot 1 read garbage. Extern preload slots live in
                // the entry frame (bytecode before `main`).
                let prev_fn_vars = std::mem::take(&mut self.context.variables);
                let prev_stack_arrays = std::mem::take(&mut self.context.stack_array_locals);
                let prev_fn_polyfn_vars = std::mem::take(&mut self.polyfn_vars);
                let prev_fn_polyfn_sources = std::mem::take(&mut self.polyfn_sources);
                let prev_fn_qualified = self.current_function_qualified.take();
                let prev_fn_table_key = self.current_function_table_key.take();
                self.current_function_qualified = Some(qualified.clone());
                self.current_function_table_key = Some(table_key.clone());
                // Sync checker so `is_ffi_declare_variadic_for_fn_id` can see
                // param call-site `declare` metadata for bare fn-id params.
                let prev_checker_fn = if !self.compiling_method {
                    self.checker
                        .set_current_function(Some(name.to_string()))
                } else {
                    None
                };
                self.push_const_env();
                self.context.variables = Interner::default();
                self.context.stack_array_locals.clear();
                self.expr_depth = 0;
                if self.compiling_method {
                    let slot = self.context.variables.intern("self".to_string()) as u32;
                    self.record_debug_local("self", slot);
                }

                let prev_result_mode = self.compiling_result_mode;
                let prev_result_ok_is_result = self.compiling_result_ok_is_result;
                self.compiling_result_mode = self.checker.fn_is_result_mode(name);
                self.compiling_result_ok_is_result = self.checker.fn_result_ok_is_result(name);
                let prev_pair_mode = self.compiling_pair_mode;
                let prev_pair_is_option = self.compiling_pair_is_option;
                let pair_kind = if *is_coro {
                    self.pin_pair_return_kind(&table_key, None);
                    None
                } else {
                    self.pair_return_kind(&table_key)
                };
                self.compiling_pair_mode = pair_kind.is_some();
                self.compiling_pair_is_option = pair_kind.unwrap_or(false);

                let mut a = self.do_compile(args);

                // ── Dictionary-passing prologue ────────────────────────────────
                // Generic functions with user-defined trait constraints receive
                // extra dict tuple arguments after the value params.  Reserve a
                // stack slot `__dictN` for each expected dict so that the Interner
                // assigns a slot number that can later be LOAD-ed by CallIndirect
                // dispatch paths.  The VM pushes these as the trailing elements of
                // the call frame, one per user constraint, in constraint order.
                // Every trait constraint (including builtin Num/Ord/Eq/Show)
                // gets a trailing `__dictN` slot for dictionary dispatch.
                // Prefer the qualified FQN: bare names are dropped by
                // `fn_dict_arity.retain(|k| k.contains("::"))` across modules,
                // while inherent methods always register `Owner::method`.
                let dict_arity = {
                    let via_fqn = self.checker.dict_arity_for(&qualified);
                    if via_fqn > 0 {
                        via_fqn
                    } else {
                        self.checker.dict_arity_for(name)
                    }
                };
                for dict_idx in 0..dict_arity {
                    self.context.variables.intern(format!("__dict{}", dict_idx));
                }

                // Args + self + dicts occupy the shared stack at body entry.
                let entry_sp = self.context.variables.len() as u32;

                self.bytecode.append(&mut a);

                let body_start = self.bytecode.len();
                // Provisional span so self-recursive peels can see the opening
                // predicate while the body is still streaming into `self.bytecode`.
                self.record_fn_span(table_key.clone(), body_start, body_start);
                let body_op_start = self.bytecode.ops().len();
                let prev_field_keys = std::mem::take(&mut self.field_key_slots);
                self.emit_field_key_prologue(body);
                let prev_active = self.active_fn_name.take();
                let prev_fn_defers = std::mem::take(&mut self.fn_defers);
                self.active_fn_name = Some(name.to_string());
                let mut c = self.do_compile(body);
                self.active_fn_name = prev_active;
                self.bytecode.append(&mut c);

                if !self.region_ends_with_return(body_op_start) {
                    self.emit_fallthrough_return(name, body.0);
                }

                self.fn_defers = prev_fn_defers;
                self.compiling_result_mode = prev_result_mode;
                self.compiling_result_ok_is_result = prev_result_ok_is_result;
                self.compiling_pair_mode = prev_pair_mode;
                self.compiling_pair_is_option = prev_pair_is_option;
                self.pop_const_env();
                if !self.compiling_method {
                    self.checker.set_current_function(prev_checker_fn);
                }
                self.current_function_qualified = prev_fn_qualified;
                self.current_function_table_key = prev_fn_table_key;
                self.field_key_slots = prev_field_keys;
                let body_end = self.bytecode.len();
                self.record_fn_span(table_key.clone(), body_start, body_end);
                let entry = self.fn_entry_labels.get(&table_key).copied();
                self.bytecode
                    .record_func_with_sp(table_key.clone(), entry, body_start, body_end, entry_sp);
                self.context.variables = prev_fn_vars;
                self.context.stack_array_locals = prev_stack_arrays;
                self.polyfn_vars = prev_fn_polyfn_vars;
                self.polyfn_sources = prev_fn_polyfn_sources;

                self.emit_mono_specializations_for_function(
                    &qualified,
                    type_params,
                    args,
                    Some(body),
                    name,
                );
                self.emit_par_specializations_for(name, &table_key);
            }
            Expression::Lambda {
                args,
                captures,
                body,
            } => {
                // Layout in self.bytecode:
                //   JMP after_body
                //   entry: <captures slots 0..n> <params> <body> RETURN
                //   after_body: LOAD captures...; CONST 0; CodePtr entry; MakeFn
                use crate::block_builder::{BlockBuilder, JumpKind};
                let mut bb = BlockBuilder::new();
                let after = bb.fresh_label(self.bytecode.il_mut());
                bb.emit_jump_to(after, JumpKind::Unconditional, self.bytecode.il_mut());
                // Entry label keeps the lambda body alive for later dead_block.
                self.bytecode.bind_fresh_entry();
                let entry = self.bytecode.len() as u32;

                let prev_fn_vars = std::mem::take(&mut self.context.variables);
                for cap in captures {
                    self.context.variables.intern((*cap).to_string());
                }
                let mut a = self.do_compile(args);
                self.bytecode.append(&mut a);
                let (arity, is_rest) = fn_arity_from_args(args);
                let prev_field_keys = std::mem::take(&mut self.field_key_slots);
                self.emit_field_key_prologue(body);
                let mut b = self.do_compile(body);
                self.bytecode.append(&mut b);
                // Expression-bodied lambdas (`=> x + y` / `{ …; last }`) leave
                // the result on the stack — emit a bare RETURN. Pushing
                // `CONST 0; RETURN` (named-fn fall-through) would discard that
                // value; peephole then fuses it to `ConstReturnImm` and every
                // call returns 0.
                if !matches!(
                    self.bytecode.last_byte().map(|b| *b.bytecode()),
                    Some(Instruction::RETURN)
                ) {
                    let body_empty = matches!(
                        body.1.as_ref(),
                        Expression::Block(items) if items.is_empty()
                    );
                    if body_empty {
                        self.bytecode.push_const(0);
                    }
                    self.bytecode.push_return();
                }
                self.field_key_slots = prev_field_keys;
                self.context.variables = prev_fn_vars;

                bb.bind_label(after, self.bytecode.il_mut());

                for cap in captures {
                    if let Some(slot) = self.lookup_slot(cap) {
                        bytecode.push_load(slot);
                    } else {
                        let mut message = Message::error(
                            ErrorCode::UnknownValue,
                            format!("Cannot find capture `{}`", cap),
                            span.into_range(),
                        );
                        message.push(DiagLabel::new(
                            format!("`{}` must be in scope at the lambda", cap),
                            span.into_range(),
                        ));
                        self.messages.push(message);
                    }
                }
                bytecode.push_const(0);
                bytecode.push(Byte::new(Instruction::CodePtr).with_operand_u32(entry));
                bytecode.push(
                    Byte::new(Instruction::MakeFn).with_operand_u32(make_fn_operand(
                        captures.len() as u32,
                        0,
                        arity as u32,
                        is_rest,
                    )),
                );
            }
            Expression::Expr(child) | Expression::Statement(child) => {
                bytecode.append(&mut self.do_compile(child))
            }
            Expression::ExprStatement(child) => {
                bytecode.append(&mut self.do_compile(child));
                // Also skip the POP for a bare `yield expr;` / `yield from expr;`
                // statement. The parser's `expr_statement()` alternative matches
                // `yield` before the dedicated (POP-free) `self.yield_()` statement
                // parser ever gets a chance (see `parser::statement`), so every
                // bare yield lands here. A trailing POP would be DEAD CODE at
                // compile time (nothing is pushed when the yield executes) but
                // becomes the coroutine's `resume_ip` — the NEXT time the
                // coroutine is resumed, the VM starts by executing that POP,
                // which pops whatever happens to be on top of the (shared)
                // stack at the resumer's call site. For a `resume` used inline
                // inside another expression (e.g. formatting `resume h`),
                // that top-of-stack value belongs to the RESUMER (e.g. the
                // format string), not the coroutine — corrupting it.
                Self::discard_statement_value(&mut bytecode);
            }
            // ---- Userland FFI builtins ----
            Expression::Dload(path) => {
                let mut bc = self.do_compile(path);
                self.bytecode.append(&mut bc);
                self.bytecode.push(Byte::new(Instruction::FfiLoad));
            }
            Expression::Done(handle) => {
                let mut bc = self.do_compile(handle);
                self.bytecode.append(&mut bc);
                self.bytecode.push(Byte::new(Instruction::DoneCoro));
            }
            // --- Aggregates ---
            Expression::Tuple(items) => {
                for c in items {
                    let mut bc = self.do_compile(c);
                    bytecode.append(&mut bc);
                }
                let arity = items.len() as u32;
                bytecode.push_make_tuple(arity);
            }
            Expression::Array(items) => {
                for c in items {
                    let mut bc = self.do_compile(c);
                    bytecode.append(&mut bc);
                }
                let arity = items.len() as u32;
                bytecode.push_make_array(arity);
            }
            // --- Dict literals ---
            Expression::Dict(items) => {
                // Eagerly resolve field names to strings before
                // any bytecode emission so the byte offsets
                // remain stable.
                let field_names: Vec<&str> = items.iter().map(|f| f.name).collect();
                for (f, name) in items.iter().zip(field_names.iter()) {
                    // value first (so it's UNDER the field name
                    // when both are pushed). MakeDict pops the
                    // top first (which is the field-name) and
                    // then the value, so they end up correctly
                    // paired in (name, value) order in the
                    // runtime's pair Vec.
                    let mut bc = self.do_compile(&f.value);
                    bytecode.append(&mut bc);
                    self.emit_raw_string_literal(&mut bytecode, name);
                }
                let arity = items.len() as u32;
                bytecode.push(Byte::new(Instruction::MakeDict).with_operand_u32(arity));
            }
            // Lazy range value: dict `{ start, end, inclusive }` so
            // first-class `let r = 0..n; for x in r` works via GetField.
            // Direct `for x in 0..n` uses the no-heap fast path instead.
            Expression::Range {
                start,
                end,
                inclusive,
            } => {
                let mut start_bc = self.do_compile(start);
                bytecode.append(&mut start_bc);
                self.emit_raw_string_literal(&mut bytecode, "start");
                let mut end_bc = self.do_compile(end);
                bytecode.append(&mut end_bc);
                self.emit_raw_string_literal(&mut bytecode, "end");
                bytecode.push_const(if *inclusive { 1 } else { 0 });
                self.emit_raw_string_literal(&mut bytecode, "inclusive");
                bytecode.push(Byte::new(Instruction::MakeDict).with_operand_u32(3));
            }
            // `t[i]` — pop the index (top), pop the target,
            // push the element at `target[index]`. The Index
            // opcode carries no operand (the index is at the top
            // of the operand stack at dispatch time).
            Expression::Index(target, Some(index)) => {
                // Const index into a multi-slot stack array → direct LOAD.
                if let Expression::Identifier(name) = target.1.as_ref()
                    && let Some((base, n)) = self.stack_array_info(name)
                    && let Expression::Integer(idx) = index.1.as_ref()
                    && *idx >= 0
                    && (*idx as usize) < n
                {
                    bytecode.push_load(base + *idx as u32);
                } else {
                    bytecode.append(&mut self.do_compile(target));
                    // When the index stages binary operands (`len(a) - 1`),
                    // STORE seeks the shared stack past the live receiver and
                    // Index pops a temp (−1) instead of the Vec. Stash both.
                    if self.expr_may_clobber_operand_stack(index) {
                        let tgt_slot = self.alloc_temp_slot();
                        bytecode.push_store_pop(tgt_slot);
                        bytecode.append(&mut self.do_compile(index));
                        let idx_slot = self.alloc_temp_slot();
                        bytecode.push_store_pop(idx_slot);
                        bytecode.push_load(tgt_slot);
                        bytecode.push_load(idx_slot);
                    } else {
                        bytecode.append(&mut self.do_compile(index));
                    }
                    bytecode.push_index();
                }
            }
            Expression::Index(_, None) => {}
            Expression::Readonly(inner) => {
                let mut inner_bc = self.do_compile(inner);
                bytecode.append(&mut inner_bc);
            }
            Expression::QualifiedAccess { owner, member } => {
                let fqn = self.class_member_fqn(owner, member);
                if let Some(slot) = self.checker.static_slot_index(&fqn) {
                    bytecode.push(Byte::new(Instruction::LoadStatic).with_operand_u32(slot));
                }
            }
            Expression::StaticDecl { name, init, .. } => {
                let fqn = self.qualify_static_fqn(name);
                self.emit_static_initializer(&fqn, init);
            }
            // --- FFI declare/invoke (legacy AST; prefer Call + use ffi::{…}) ---
            Expression::Declare(args) => self.emit_ffi_declare(*span, args),
            Expression::Invoke(args) => self.emit_ffi_invoke(*span, args),
            Expression::Return(expr) | Expression::ImplicitReturn(expr) => {
                let tail_match = self.return_is_tail_match(expr);
                if !self.compiling_pair_mode
                    && !tail_match
                    && self.try_emit_tail_call_expr(expr, &mut bytecode)
                {
                    if self.compiling_result_mode {
                        if !self.skip_result_ok_wrap_for_return(expr) {
                            Self::emit_ok_or_some_wrap(&mut bytecode, false);
                        }
                    }
                    return bytecode;
                }

                if tail_match {
                    self.match_tail_call = true;
                }
                // Evaluate the return value first, then run defers (LIFO).
                // Each defer thunk returns a sentinel that we POP so the
                // pending return value stays on top for RETURN.
                // Flush the value into `self.bytecode` before labeled defers.
                let pair_expr = self.expr_pairs_with_return(expr);
                let pair_enum_kind = self.expr_is_return_enum(expr);
                let previous_pair_context = self.pair_value_context;
                self.pair_value_context = pair_expr;
                self.append_with_existential_pack(&mut bytecode, expr);
                self.pair_value_context = previous_pair_context;
                self.match_tail_call = false;
                // Result-mode functions: bare `return v` becomes `Ok(v)`.
                if self.compiling_pair_mode {
                    if !pair_expr {
                        if let Some(is_option) = pair_enum_kind {
                            bytecode.push(
                                Byte::new(Instruction::HeapToPair)
                                    .with_operand_u32(u32::from(is_option)),
                            );
                        } else {
                            bytecode.push_const(0);
                        }
                    }
                } else if self.compiling_result_mode {
                    // Explicit flat `return Result::Ok/Err` already builds the
                    // enum — do not Ok-wrap again (COI-113). Nested Result Ok
                    // payloads still wrap.
                    if !self.skip_result_ok_wrap_for_return(expr) {
                        Self::emit_ok_or_some_wrap(&mut bytecode, false);
                    }
                }
                self.bytecode.append(&mut bytecode);
                self.emit_run_defers();
                if !matches!(child.borrow(), Expression::ImplicitReturn(_)) {
                    if self.compiling_pair_mode {
                        self.push_return_pair();
                    } else {
                        self.bytecode.push_return();
                    }
                }
            }
            Expression::Yield(expr) => {
                bytecode.append(&mut self.do_compile(expr));
                bytecode.push(Byte::new(Instruction::YieldCoro));
            }
            Expression::YieldFrom(expr) => {
                bytecode.append(&mut self.do_compile(expr));
                bytecode.push(Byte::new(Instruction::YieldFromCoro));
            }
            Expression::Resume(target, arg) => {
                if let Some(a) = arg {
                    bytecode.append(&mut self.do_compile(a));
                }
                bytecode.append(&mut self.do_compile(target));
                let has_send = if arg.is_some() { 1u32 } else { 0u32 };
                bytecode.push(Byte::new(Instruction::ResumeCoro).with_operand_u32(has_send));
            }
            Expression::Class {
                docs: _,
                name,
                fields: state,
                ..
            } => {
                use parser::ast::FieldModifier;
                let class_key = self.resolve_class_ident(name);
                let mut instance_fields: Vec<(String, usize)> = Vec::new();
                let mut idx = 0usize;
                for v in state {
                    match v.1.borrow() {
                        Expression::Field {
                            docs: _,
                            modifier,
                            name: n,
                            init,
                            ..
                        } => {
                            let fname = self.resolve_variable(n);
                            if matches!(modifier, FieldModifier::Static) {
                                if let Some(init_expr) = init {
                                    let fqn = format!("{}::{}", class_key, fname);
                                    self.emit_static_initializer(&fqn, init_expr);
                                }
                            } else {
                                instance_fields.push((fname, idx));
                                idx += 1;
                            }
                        }
                        _ => {
                            unreachable!("There should be only fields inside of a class definition")
                        }
                    }
                }
                self.context
                    .classes
                    .insert(class_key.clone(), instance_fields);
                self.context.symbols.intern(class_key);
            }
            Expression::Implementation { owner, methods, .. } => {
                let saved_ns = self.namespace.clone();
                let owner_key = self.resolve_class_ident(owner);
                self.namespace = owner_key.clone();

                for method_node in methods {
                    if let Expression::Method(_, body) = method_node.1.borrow()
                        && let Expression::Function { name, .. } = body.1.borrow()
                    {
                        let fqn = format!("{}::{}", owner_key, name);
                        self.context
                            .methods
                            .entry(owner_key.clone())
                            .or_default()
                            .insert(name.to_string(), fqn.clone());
                        self.reserve_function_entry(fqn);
                    }
                }

                for method_node in methods {
                    match method_node.1.borrow() {
                        Expression::Method(_, body) => {
                            if let Expression::Function {
                                docs: _,
                                name, is_static, ..
                            } = body.1.borrow()
                            {
                                let fqn = format!("{}::{}", owner_key, name);
                                // Instance methods reserve slot 0 for `self`;
                                // static methods start params at slot 0.
                                self.compiling_method = !*is_static;
                                self.do_compile(body);
                                self.compiling_method = false;
                                self.context
                                    .methods
                                    .entry(owner_key.clone())
                                    .or_default()
                                    .insert(name.to_string(), fqn);
                            } else {
                                self.compiling_method = true;
                                self.do_compile(body);
                                self.compiling_method = false;
                            }
                        }
                        _ => {
                            self.do_compile(method_node);
                        }
                    }
                }

                self.context
                    .impementations
                    .insert(owner_key.clone(), owner_key);
                self.namespace = saved_ns;
            }
            Expression::Method(_vis, body) => {
                let is_static = matches!(
                    body.1.borrow(),
                    Expression::Function {
                        is_static: true,
                        ..
                    }
                );
                self.compiling_method = !is_static;
                bytecode.append(&mut self.do_compile(body));
                self.compiling_method = false;
            }
            Expression::Instantiate(class, args) => {
                let name = self.resolve_variable_checked(class);
                let name = self.resolve_class_ident(&name);
                let ctor_name = self
                    .decorated_class_ctors
                    .get(&name)
                    .filter(|ctor| self.active_fn_name.as_deref() != Some(ctor.as_str()))
                    .cloned();
                if let Some(ctor) = ctor_name {
                    let arg_slice = args.as_deref().unwrap_or(&[]);
                    if self.functions.contains_key(ctor.as_str())
                        || self.fn_entry_labels.contains_key(ctor.as_str())
                    {
                        let arity =
                            self.emit_call_args_with_rest(&ctor, arg_slice, &mut bytecode, false);
                        if !self.emit_direct_fn_call(&mut bytecode, &ctor, arity) {
                            self.missing_call_target(&ctor, class.0.into_range());
                        }
                    } else {
                        self.messages.push(Message::error(
                            ErrorCode::CodegenError,
                            format!(
                                "Decorated constructor `{ctor}` for class `{name}` was not found"
                            ),
                            class.0.into_range(),
                        ));
                    }
                } else {
                    let fields = self.context.classes.get(&name).cloned().unwrap_or_default();
                    let type_id = self.checker.class_type_id(&name);
                    bytecode
                        .push(Byte::new(Instruction::InitTyped).with_operand_u32(type_id));
                    // SetField stack order is value, target, name (same as
                    // Assignment to Access). Stash the instance, then for
                    // each ctor arg emit that sequence and discard the
                    // value SetField pushes back.
                    //
                    // `StorePop` keeps the instance at `tmp` with the cursor
                    // past that slot — so the stashed value is already TOS
                    // for the expression result. Do **not** emit a final
                    // `LOAD tmp`: that would push a second copy and leave
                    // the stash sitting between any live values below
                    // (e.g. a HostInvoke native-id CONST) and the result,
                    // so `MakeTuple`/`HostInvoke` would pick up the instance
                    // as the native id.
                    let tmp_inst = self.alloc_temp_slot();
                    bytecode.push_store_pop(tmp_inst);
                    if let Some(arg_list) = args {
                        for (arg, (fname, _)) in arg_list.iter().zip(fields.iter()) {
                            bytecode.append(&mut self.do_compile(arg));
                            bytecode.push_load(tmp_inst);
                            self.emit_field_name(&mut bytecode, fname);
                            bytecode.push_set_field();
                            bytecode.push_pop();
                        }
                    }
                    // Ctor args may stage temps above `tmp_inst` (binary / CALL
                    // staging). STORE seeks the shared cursor past those temps,
                    // so the instance is no longer TOS for MakeEnum/assignment.
                    // Seek back so `tmp_inst` is the expression result.
                    bytecode.push_seek(tmp_inst + 1);
                }
            }
            Expression::Adjust { op, prefix, target } => {
                self.emit_adjust(&mut bytecode, target, *op, *prefix);
            }
            Expression::CompoundAssign(target, op, rhs) => {
                self.emit_compound_assign(
                    &mut bytecode,
                    self_id,
                    span.start,
                    span.end,
                    target,
                    *op,
                    rhs,
                );
            }
            // --- Loop codegen ---
            // `while`: [top] cond, JMPF→exit, body, JMP→top, [exit]
            // `for x in`: IntoIterator/Iterator (array/tuple/dict/coro/custom)
            Expression::Loop {
                identifier,
                iterable,
                body,
            } => {
                if let Some(binding) = identifier {
                    let binding_name = match binding.1.as_ref() {
                        Expression::Identifier(n) => (*n).to_string(),
                        _ => "__for_in_x".to_string(),
                    };
                    let info = self.sidecar_for_in(self_id, span.start, span.end);
                    let item_ty = info.as_ref().map(|i| i.item_ty.clone());
                    let kind = info.map(|i| i.kind).unwrap_or(ForInKind::Coroutine);
                    match kind {
                        ForInKind::Array => {
                            self.emit_for_in_array_loop(body, &binding_name, false, Some(iterable));
                        }
                        ForInKind::Tuple { arity } => {
                            self.emit_for_in_tuple(iterable, body, &binding_name, arity);
                        }
                        ForInKind::Dict => {
                            self.emit_for_in_dict(iterable, body, &binding_name);
                        }
                        ForInKind::Coroutine => {
                            self.emit_for_in_coro(iterable, body, &binding_name);
                        }
                        ForInKind::Range { inclusive, float } => {
                            self.emit_for_in_range(iterable, body, &binding_name, inclusive, float);
                        }
                        ForInKind::Custom {
                            into_iter_fqn,
                            next_fqn,
                        } => {
                            self.emit_for_in_custom(
                                iterable,
                                body,
                                &binding_name,
                                &into_iter_fqn,
                                &next_fqn,
                                item_ty.as_ref(),
                            );
                        }
                    }
                } else {
                    if let Some(ConstValue::Bool(false)) =
                        crate::const_fold::eval_expr(iterable, self.const_env())
                    {
                        self.discard_compile(iterable);
                        self.discard_compile(body);
                        return bytecode;
                    }
                    if self.try_emit_par_loop(*span, iterable, body) {
                        return bytecode;
                    }
                    let mut bb = BlockBuilder::new();
                    let top_label = bb.fresh_label(self.bytecode.il_mut());
                    let exit_label = bb.fresh_label(self.bytecode.il_mut());
                    bb.bind_label(top_label, self.bytecode.il_mut());

                    let mut iter_bc = self.do_compile(iterable);
                    self.bytecode.append(&mut iter_bc);

                    bb.emit_jump_to(exit_label, BbJumpKind::JumpIfFalse, self.bytecode.il_mut());

                    self.loop_stack.push((top_label, exit_label));
                    self.loop_bbs.push(bb);
                    let mut body_bc = self.do_compile(body);
                    self.bytecode.append(&mut body_bc);
                    let mut bb = self
                        .loop_bbs
                        .pop()
                        .expect("loop builder stack balanced for while");
                    self.loop_stack
                        .pop()
                        .expect("loop label stack balanced for while");

                    bb.emit_jump_to(top_label, BbJumpKind::Unconditional, self.bytecode.il_mut());
                    bb.bind_label(exit_label, self.bytecode.il_mut());

                }
            }
            Expression::Break => {
                if let Some((_, break_label)) = self.loop_stack.last().copied() {
                    self.emit_loop_jump(Some(break_label), "break", span.into_range());
                } else {
                    self.emit_loop_jump(None, "break", span.into_range());
                }
            }
            Expression::Continue => {
                if let Some((continue_label, _)) = self.loop_stack.last().copied() {
                    self.emit_loop_jump(Some(continue_label), "continue", span.into_range());
                } else {
                    self.emit_loop_jump(None, "continue", span.into_range());
                }
            }
            Expression::Defer { captures, body } => {
                // Layout (emitted into `self.bytecode` so nested Blocks that
                // write in-place stay contiguous with the thunk):
                //   JMP after_thunk
                //   thunk:                ← fn_defers label bound here
                //     <thunk body>
                //     (slots 0..N-1 = use captures, pushed by emit_run_defers)
                //   CONST 0; RETURN
                // after_thunk:
                let mut bb = BlockBuilder::new();
                let after = bb.fresh_label(self.bytecode.il_mut());
                let thunk = bb.fresh_label(self.bytecode.il_mut());
                bb.emit_jump_to(after, BbJumpKind::Unconditional, self.bytecode.il_mut());

                bb.bind_label(thunk, self.bytecode.il_mut());
                let cap_names: Vec<String> = captures.iter().map(|c| (*c).to_string()).collect();
                self.fn_defers.push((thunk, cap_names));

                // Remap locals so capture names occupy slots 0..N-1 inside the
                // thunk (matching the CALL args pushed by emit_run_defers).
                let prev_vars = std::mem::take(&mut self.context.variables);
                for cap in captures {
                    self.context.variables.intern((*cap).to_string());
                }
                let mut body_bc = self.do_compile(body);
                self.bytecode.append(&mut body_bc);
                self.context.variables = prev_vars;

                self.bytecode.push_const(0);
                self.bytecode.push_return();

                bb.bind_label(after, self.bytecode.il_mut());
            }
            Expression::Call { name, args } => bytecode.append(&mut self.compile_call_expr(name, args, ast, self_id, span)),
            Expression::Argument { ty, name: n, .. } => {
                let slot = self.context.variables.intern(n.to_string()) as u32;
                self.record_debug_local(n, slot);
                if ty
                    .as_ref()
                    .is_some_and(|t| matches!(t.1.as_ref(), Expression::Forall { .. }))
                {
                    self.polyfn_vars.insert(n.to_string());
                }
                // bytecode.push(Byte::new(Instruction::LOAD)
            }
            Expression::Type(_)
            | Expression::TypeFun(_, _)
            | Expression::TypeFnSig { .. }
            | Expression::Forall { .. } => {
                // Type names appear as metadata inside enum
                // declarations (e.g. `Some(int)` wraps `int` as
                // an `Expression::Type`). The typechecker has
                // already extracted the type name and registered
                // the enum shape; no runtime bytecode is
                // emitted for the type wrapper. The pre-walk
                // still mints a NodeId (so this arm consumes one
                // to stay in lockstep), but the bytecode here is
                // empty.
            }
            Expression::TypeClass { name, methods, .. } => {
                for method in methods {
                    match method.1.as_ref() {
                        Expression::AssocTypeDecl { .. } => {
                            // Type-level only — do_compile consumes the NodeId.
                            let _ = self.do_compile(method);
                        }
                        Expression::Function {
                            docs: _,
                            name: method_name,
                            body,
                            ..
                        } => {
                            let has_default = body.as_ref().is_some_and(|b| {
                                !matches!(b.1.as_ref(), Expression::Block(items) if items.is_empty())
                            });
                            if has_default {
                                let fqn =
                                    crate::typechecking::generics::Generics::default_method_fqn(
                                        name,
                                        method_name,
                                    );
                                self.compile_function_output_with_name(method, fqn, &[], 1);
                            } else {
                                self.consume_function_signature_output(method);
                            }
                        }
                        _ => {
                            self.consume_function_signature_output(method);
                        }
                    }
                }
            }
            Expression::TypeClassImpl {
                class,
                args,
                methods,
            } => {
                // Resolve instance heads by AST shape (not span cache). Bare
                // `Option`/`Result` must stay `Con(...)` so FQNs match the
                // typechecker (`Container__Option__first`). Preferring
                // `codegen_expr_ty` here can pick up a misaligned span type
                // (e.g. `unit`) and emit `Container__unit__first` instead.
                let arg_tys: Vec<Ty> = args
                    .iter()
                    .map(|arg| self.codegen_instance_head_ty(arg))
                    .collect();
                for arg in args {
                    bytecode.append(&mut self.do_compile(arg));
                }
                let ty_part = arg_tys
                    .iter()
                    .map(|ty| ty.to_string())
                    .collect::<Vec<_>>()
                    .join("_");
                for method in methods {
                    match method.1.as_ref() {
                        Expression::AssocTypeDef { .. } => {
                            // Type-level only — do_compile consumes wrapper + RHS IDs.
                            let _ = self.do_compile(method);
                        }
                        Expression::Function {
                            docs: _,
                            name: method_name, ..
                        } => {
                            let fqn = format!("{}__{}__{}", class, ty_part, method_name);
                            let unbox_tys =
                                self.instance_method_unbox_tys(class, method_name, &arg_tys);
                            self.compile_function_output_with_name(method, fqn, &unbox_tys, 1);
                        }
                        Expression::Method(_, body) => {
                            let _method_wrapper_id = self.checker.id_table().ids()[self.emit_idx];
                            self.emit_idx += 1;
                            if let Expression::Function {
                                docs: _,
                                name: method_name, ..
                            } = body.1.as_ref()
                            {
                                let fqn = format!("{}__{}__{}", class, ty_part, method_name);
                                let unbox_tys =
                                    self.instance_method_unbox_tys(class, method_name, &arg_tys);
                                self.compile_function_output_with_name(body, fqn, &unbox_tys, 1);
                            } else {
                                self.consume_function_signature_output(body);
                            }
                        }
                        _ => {
                            self.consume_function_signature_output(method);
                        }
                    }
                }
            }
            Expression::AssocTypeDecl { .. } | Expression::TypeProjection { .. } => {
                // Type-level only — no bytecode (NodeId already consumed by do_compile).
            }
            Expression::AssocTypeDef { ty, .. } => {
                bytecode.append(&mut self.do_compile(ty));
            }
            Expression::Identifier(n) => {
                let resolved = self.resolve_free_fn(n);
                if let Some(v) = self
                    .const_env()
                    .get(&resolved)
                    .or_else(|| self.const_env().get(*n))
                    .cloned()
                {
                    self.emit_const_value(&v, &mut bytecode);
                } else if let Some(v) = self
                    .static_const_values
                    .get(&resolved)
                    .filter(|_| self.checker.is_static_const_fqn(&resolved))
                    .cloned()
                {
                    self.emit_const_value(&v, &mut bytecode);
                } else if let Some(v) = self
                    .static_const_values
                    .get(&self.qualify_static_fqn(n))
                    .filter(|_| {
                        self.checker
                            .is_static_const_fqn(&self.qualify_static_fqn(n))
                    })
                    .cloned()
                {
                    self.emit_const_value(&v, &mut bytecode);
                } else if let Some(static_slot) = self
                    .checker
                    .static_slot_index(&resolved)
                    .or_else(|| self.checker.static_slot_for_module_name(n))
                {
                    bytecode.push(Byte::new(Instruction::LoadStatic).with_operand_u32(static_slot));
                } else if let Some(slot) = self.lookup_slot(n) {
                    if let Some((base, len)) = self.stack_array_info(n) {
                        // Escape multi-slot local to a heap ObjArray.
                        self.emit_box_stack_array(&mut bytecode, base, len);
                    } else {
                        bytecode.push_load(slot);
                    }
                } else {
                    // Not a local variable — check if it's a generic function
                    // escaping into a non-call position (e.g. `let f = id;`).
                    // In that case, emit MakePolyFn instead of a direct CALL offset,
                    // so the variable holds an ObjPolyFn that CallIndirect can use.
                    let resolved_n = self.resolve_free_fn(n);
                    if self.checker.is_generic_fn(&resolved_n) {
                        if let Some(&entry_offset) = self.functions.get(&resolved_n) {
                            // Phase 4: constrained generics always escape via
                            // MakePolyFnCapture. Fill slots from in-scope
                            // `__dictN` or concrete instance synthesis; leave
                            // null only when evidence is unavailable (e.g.
                            // top-level `let f = show`).
                            let escape_ty = self.codegen_expr_ty(ast);
                            let dict_arity = self.emit_polyfn_escape_dicts(
                                &mut bytecode,
                                &resolved_n,
                                escape_ty.as_ref(),
                            );
                            if dict_arity == 0 {
                                bytecode.push(
                                    Byte::new(Instruction::MakePolyFn)
                                        .with_operand_u32(entry_offset as u32),
                                );
                            } else {
                                bytecode.push(
                                    Byte::new(Instruction::CodePtr)
                                        .with_operand_u32(entry_offset as u32),
                                );
                                bytecode.push(
                                    Byte::new(Instruction::MakePolyFnCapture)
                                        .with_operand_u32(dict_arity as u32),
                                );
                            }
                        } else {
                            // Function not yet compiled (forward reference) — fall
                            // through to the unknown-variable diagnostic.
                            let mut message = Message::error(
                                ErrorCode::UnknownValue,
                                "Unknown generic function".to_string(),
                                span.into_range(),
                            );
                            message.push(DiagLabel::new(
                                format!("Generic function '{}' not found in bytecode", n),
                                span.into_range(),
                            ));
                            self.messages.push(message);
                        }
                    } else {
                        // Monomorphic function in value position → MakeFn.
                        let (fa, is_rest, entry_key) = if let Some((fa, is_rest, id)) =
                            self.sidecar_overload(self_id, span.start, span.end)
                        {
                            let keyed = overload_fn_key(&resolved_n, fa, is_rest, id);
                            (fa, is_rest, keyed)
                        } else if self.checker.is_overloaded(&resolved_n) {
                            // Ambiguous — typechecker should have diagnosed.
                            let mut message = Message::error(
                                ErrorCode::UnknownValue,
                                "Ambiguous overload in value position".to_string(),
                                span.into_range(),
                            );
                            message.push(DiagLabel::new(
                                format!(
                                    "Cannot reify overloaded `{}` without a type annotation",
                                    n
                                ),
                                span.into_range(),
                            ));
                            self.messages.push(message);
                            return bytecode;
                        } else {
                            let rest = self.checker.fn_has_rest(&resolved_n);
                            let fa = self
                                .checker
                                .fn_param_names(&resolved_n)
                                .map(|names| {
                                    if rest {
                                        names.len().saturating_sub(1)
                                    } else {
                                        names.len()
                                    }
                                })
                                .unwrap_or(0);
                            (fa, rest, resolved_n.clone())
                        };
                        if let Some(&entry_offset) = self
                            .functions
                            .get(&entry_key)
                            .or_else(|| self.functions.get(&resolved_n))
                        {
                            // Prefer codegen-recorded arity: multi-file
                            // `check_program` clears `fn_param_names`, so
                            // imported names would otherwise MakeFn with
                            // arity 0 and break `spawn(f, arg)`.
                            let (fa, is_rest) = self
                                .fn_arities
                                .get(&entry_key)
                                .or_else(|| self.fn_arities.get(&resolved_n))
                                .copied()
                                .map(|(a, r)| (a as usize, r))
                                .unwrap_or((fa, is_rest));
                            bytecode.push_const(0);
                            bytecode.push(
                                Byte::new(Instruction::CodePtr)
                                    .with_operand_u32(entry_offset as u32),
                            );
                            bytecode.push(
                                Byte::new(Instruction::MakeFn)
                                    .with_operand_u32(make_fn_operand(0, 0, fa as u32, is_rest)),
                            );
                        } else {
                            let mut message = Message::error(
                                ErrorCode::UnknownValue,
                                "Unknown variable".to_string(),
                                span.into_range(),
                            );
                            message.push(DiagLabel::new(
                                format!("Unknown variable '{}'", n),
                                span.into_range(),
                            ));
                            self.messages.push(message);
                        }
                    }
                }
            }
            // --- If codegen ---
            // Layout: c1, JMPF1, b1, JMP1, c2, JMPF2, b2, JMP2, b3, [end]
            Expression::If(branches) => {
                if self.try_compile_const_if(branches) {
                    return bytecode;
                }
                // `if (!c) { A } else { B }` ≡ `if (c) { B } else { A }` — exposes
                // BinSlot*/Cmp JMPF fusion (avoids LogNotJmpf after fused cond).
                let inverted = Self::try_invert_not_if_else(branches);
                let branches: &[Output<'_>] = inverted.as_deref().unwrap_or(branches);

                let mut bb = BlockBuilder::new();
                let end_label = bb.fresh_label(self.bytecode.il_mut());
                let mut branch_start_labels: Vec<Option<crate::block_builder::Label>> =
                    Vec::with_capacity(branches.len());
                for i in 0..branches.len() {
                    if i + 1 < branches.len() {
                        branch_start_labels.push(Some(bb.fresh_label(self.bytecode.il_mut())));
                    } else {
                        branch_start_labels.push(None);
                    }
                }

                for (i, (_, branch)) in branches.iter().enumerate() {
                    let (cond_opt, body) = match branch.borrow() {
                        Expression::Branch(c, b) => (c.as_ref(), b),
                        _ => unreachable!("If branch must be Expression::Branch"),
                    };

                    // If this is not the first branch, bind the
                    // previous branch's pre-allocated start label to
                    // the CURRENT bytecode position (= the start of
                    // this branch). This patches the JMPF placeholder
                    // emitted by the previous iteration.
                    if i > 0
                        && let Some(prev_label) = branch_start_labels[i - 1]
                    {
                        let _target = self.bytecode.len() as u32;
                        bb.bind_label(prev_label, self.bytecode.il_mut());
                    }

                    // Emit cond then JMPF (including single-branch if).
                    if let Some(cond) = cond_opt {
                        let mut cond_bc = self.do_compile(cond);
                        self.bytecode.append(&mut cond_bc);
                        let jmpf_target = branch_start_labels[i].unwrap_or(end_label);
                        bb.emit_jump_to(
                            jmpf_target,
                            BbJumpKind::JumpIfFalse,
                            self.bytecode.il_mut(),
                        );
                    }

                    // Body after cond+JMPF so Print/nested control-flow offsets stay correct.
                    let mut body_bc = self.do_compile(body);
                    self.bytecode.append(&mut body_bc);

                    // Emit a `JMP → end` placeholder for every
                    // branch except the last. The last branch falls
                    // through to `end_pos`.
                    if i + 1 < branches.len() {
                        bb.emit_jump_to(
                            end_label,
                            BbJumpKind::Unconditional,
                            self.bytecode.il_mut(),
                        );
                    }
                }

                // Bind `end_label` to the current bytecode position
                // (= past the last branch's body / JMP). This patches
                // every JMP → end placeholder AND the last JMPF
                // placeholder (if any).
                bb.bind_label(end_label, self.bytecode.il_mut());

                // Validate: every label that had a pending jump must
                // be bound. (Allocated-but-unused labels are allowed.)
            }
            Expression::Le(lhs, rhs) => {
                let hint = self.bound_operator_hint(self_id, span.start, span.end);
                if let Some(hint) = hint
                    && self.emit_bound_operator_call(
                        &mut bytecode,
                        lhs,
                        rhs,
                        hint.dict_index,
                        hint.method_slot,
                    )
                {
                    // Intentional empty body: the emit/try_emit call in the
                    // condition already wrote bytecode as a side effect.
                } else if self.emit_concrete_operator_call(&mut bytecode, lhs, rhs, "Lt", "lt") {
                    // Intentional empty body: the emit/try_emit call in the
                    // condition already wrote bytecode as a side effect.
                } else {
                    let is_float = self.compile_binary_operands(&mut bytecode, lhs, rhs);
                    bytecode.push(Byte::new(if is_float {
                        Instruction::LEF
                    } else {
                        Instruction::LE
                    }));
                }
            }
            Expression::Gt(lhs, rhs) => {
                let hint = self.bound_operator_hint(self_id, span.start, span.end);
                if let Some(hint) = hint
                    && self.emit_bound_operator_call(
                        &mut bytecode,
                        lhs,
                        rhs,
                        hint.dict_index,
                        hint.method_slot,
                    )
                {
                    // Intentional empty body: the emit/try_emit call in the
                    // condition already wrote bytecode as a side effect.
                } else if self.emit_concrete_operator_call(&mut bytecode, lhs, rhs, "Gt", "gt") {
                    // Intentional empty body: the emit/try_emit call in the
                    // condition already wrote bytecode as a side effect.
                } else {
                    let is_float = self.compile_binary_operands(&mut bytecode, lhs, rhs);
                    bytecode.push(Byte::new(if is_float {
                        Instruction::GTF
                    } else {
                        Instruction::GT
                    }));
                }
            }
            Expression::Leq(lhs, rhs) => {
                let hint = self.bound_operator_hint(self_id, span.start, span.end);
                if let Some(hint) = hint
                    && self.emit_bound_operator_call(
                        &mut bytecode,
                        lhs,
                        rhs,
                        hint.dict_index,
                        hint.method_slot,
                    )
                {
                    // Intentional empty body: the emit/try_emit call in the
                    // condition already wrote bytecode as a side effect.
                } else if self.emit_concrete_operator_call(&mut bytecode, lhs, rhs, "Le", "le") {
                    // Intentional empty body: the emit/try_emit call in the
                    // condition already wrote bytecode as a side effect.
                } else {
                    let is_float = self.compile_binary_operands(&mut bytecode, lhs, rhs);
                    bytecode.push(Byte::new(if is_float {
                        Instruction::LEQF
                    } else {
                        Instruction::LEQ
                    }));
                }
            }
            Expression::Geq(lhs, rhs) => {
                let hint = self.bound_operator_hint(self_id, span.start, span.end);
                if let Some(hint) = hint
                    && self.emit_bound_operator_call(
                        &mut bytecode,
                        lhs,
                        rhs,
                        hint.dict_index,
                        hint.method_slot,
                    )
                {
                    // Intentional empty body: the emit/try_emit call in the
                    // condition already wrote bytecode as a side effect.
                } else if self.emit_concrete_operator_call(&mut bytecode, lhs, rhs, "Ge", "ge") {
                    // Intentional empty body: the emit/try_emit call in the
                    // condition already wrote bytecode as a side effect.
                } else {
                    let is_float = self.compile_binary_operands(&mut bytecode, lhs, rhs);
                    bytecode.push(Byte::new(if is_float {
                        Instruction::GEQF
                    } else {
                        Instruction::GEQ
                    }));
                }
            }
            Expression::Eq(lhs, rhs) => {
                let hint = self.bound_operator_hint(self_id, span.start, span.end);
                if let Some(hint) = hint
                    && self.emit_bound_operator_call(
                        &mut bytecode,
                        lhs,
                        rhs,
                        hint.dict_index,
                        hint.method_slot,
                    )
                {
                    // Intentional empty body: the emit/try_emit call in the
                    // condition already wrote bytecode as a side effect.
                } else if self.emit_concrete_operator_call(&mut bytecode, lhs, rhs, "Eq", "eq") {
                    // Intentional empty body: the emit/try_emit call in the
                    // condition already wrote bytecode as a side effect.
                } else {
                    binary!(bytecode, self, lhs, rhs, Byte::new(Instruction::EQ));
                }
            }
            Expression::Not(lhs) => {
                unary!(bytecode, self, lhs, Byte::new(Instruction::NOT));
            }
            Expression::LogicalNot(lhs) => {
                unary!(bytecode, self, lhs, Byte::new(Instruction::LogNot));
            }
            Expression::Negate(lhs) => {
                if self.try_emit_matrix_op(&mut bytecode, self_id, span.start, span.end, lhs, None)
                {
                    // Intentional empty body: the emit/try_emit call in the
                    // condition already wrote bytecode as a side effect.
                } else if self.try_emit_aggregate_arith(
                    &mut bytecode,
                    self_id,
                    span.start,
                    span.end,
                    lhs,
                    None,
                    crate::typechecking::AggregateOp::Neg,
                ) {
                    // Intentional empty body: the emit/try_emit call in the
                    // condition already wrote bytecode as a side effect.
                } else {
                    unary!(bytecode, self, lhs, Byte::new(Instruction::NEG));
                }
            }
            Expression::Add(lhs, rhs) => {
                // `allow_mul_shl` is irrelevant for Add (strength_mul_to_shl
                // only matches Mul); pass true for the shared helper API.
                if self.try_emit_folded_expr(ast, &mut bytecode, true) {
                    // Intentional empty body: the emit/try_emit call in the
                    // condition already wrote bytecode as a side effect.
                } else if self.try_emit_matrix_op(
                    &mut bytecode,
                    self_id,
                    span.start,
                    span.end,
                    lhs,
                    Some(rhs),
                ) {
                    // Intentional empty body: the emit/try_emit call in the
                    // condition already wrote bytecode as a side effect.
                } else if self.try_emit_aggregate_arith(
                    &mut bytecode,
                    self_id,
                    span.start,
                    span.end,
                    lhs,
                    Some(rhs),
                    crate::typechecking::AggregateOp::Add,
                ) {
                    // Intentional empty body: the emit/try_emit call in the
                    // condition already wrote bytecode as a side effect.
                } else if self.is_string_expr(lhs) && self.is_string_expr(rhs) {
                    self.emit_raw_string_literal(&mut bytecode, "%s%s");
                    if self.string_concat_needs_staging(lhs, rhs) {
                        let mut lhs_slot = 0;
                        let mut rhs_slot = 0;
                        self.stage_call_arg_to_temp(lhs, false, &mut lhs_slot);
                        self.stage_call_arg_to_temp(rhs, false, &mut rhs_slot);
                        bytecode.push_load(lhs_slot);
                        bytecode.push_load(rhs_slot);
                    } else {
                        bytecode.append(&mut self.do_compile(lhs));
                        bytecode.append(&mut self.do_compile(rhs));
                    }
                    bytecode.push(Byte::new(Instruction::FORMAT).with_operand_u32(2));
                } else if let Some(hint) = self.bound_operator_hint(self_id, span.start, span.end)
                    && self.emit_bound_operator_call(
                        &mut bytecode,
                        lhs,
                        rhs,
                        hint.dict_index,
                        hint.method_slot,
                    )
                {
                    // Intentional empty body: the emit/try_emit call in the
                    // condition already wrote bytecode as a side effect.
                } else {
                    let is_float = likely(self.compile_binary_operands(&mut bytecode, lhs, rhs));
                    bytecode.push(Byte::new(if is_float {
                        Instruction::ADDF
                    } else {
                        Instruction::ADD
                    }));
                }
            }
            Expression::Sub(lhs, rhs) => {
                if self.try_emit_matrix_op(
                    &mut bytecode,
                    self_id,
                    span.start,
                    span.end,
                    lhs,
                    Some(rhs),
                ) {
                    // Intentional empty body: the emit/try_emit call in the
                    // condition already wrote bytecode as a side effect.
                } else if self.try_emit_aggregate_arith(
                    &mut bytecode,
                    self_id,
                    span.start,
                    span.end,
                    lhs,
                    Some(rhs),
                    crate::typechecking::AggregateOp::Sub,
                ) {
                    // Intentional empty body: the emit/try_emit call in the
                    // condition already wrote bytecode as a side effect.
                } else if let Some(hint) = self.bound_operator_hint(self_id, span.start, span.end)
                    && self.emit_bound_operator_call(
                        &mut bytecode,
                        lhs,
                        rhs,
                        hint.dict_index,
                        hint.method_slot,
                    )
                {
                    // Intentional empty body: the emit/try_emit call in the
                    // condition already wrote bytecode as a side effect.
                } else {
                    let is_float = likely(self.compile_binary_operands(&mut bytecode, lhs, rhs));
                    bytecode.push(Byte::new(if is_float {
                        Instruction::SUBF
                    } else {
                        Instruction::SUB
                    }));
                }
            }
            Expression::Mul(lhs, rhs) => {
                // Matrix / aggregate Mul take precedence over scalar fold and
                // `x * 2^n` → SHL (matmul and element-wise vector ops).
                if self.try_emit_matrix_op(
                    &mut bytecode,
                    self_id,
                    span.start,
                    span.end,
                    lhs,
                    Some(rhs),
                ) {
                    // Intentional empty body: the emit/try_emit call in the
                    // condition already wrote bytecode as a side effect.
                } else if self.try_emit_aggregate_arith(
                    &mut bytecode,
                    self_id,
                    span.start,
                    span.end,
                    lhs,
                    Some(rhs),
                    crate::typechecking::AggregateOp::Mul,
                ) {
                    // Intentional empty body: the emit/try_emit call in the
                    // condition already wrote bytecode as a side effect.
                } else {
                    // Prefer trait/`Mul` dictionary dispatch over primitive
                    // `x * 2^n` → SHL when the checker recorded a bound operator
                    // (non-primitive `T * 2^n` must not emit int SHL).
                    // `try_emit_folded_expr` also const-folds literal×literal and
                    // identity-reduces `* 1` before bound/primitive fallback.
                    let bound_mul = self.bound_operator_hint(self_id, span.start, span.end);
                    if self.try_emit_folded_expr(ast, &mut bytecode, bound_mul.is_none()) {
                        // Intentional empty body: the emit/try_emit call in the
                        // condition already wrote bytecode as a side effect.
                    } else if let Some(hint) = bound_mul
                        && self.emit_bound_operator_call(
                            &mut bytecode,
                            lhs,
                            rhs,
                            hint.dict_index,
                            hint.method_slot,
                        )
                    {
                        // Intentional empty body: the emit/try_emit call in the
                        // condition already wrote bytecode as a side effect.
                    } else {
                        let is_float =
                            likely(self.compile_binary_operands(&mut bytecode, lhs, rhs));
                        bytecode.push(Byte::new(if is_float {
                            Instruction::MULF
                        } else {
                            Instruction::MUL
                        }));
                    }
                }
            }
            Expression::Mod(lhs, rhs) => {
                if self.try_emit_aggregate_arith(
                    &mut bytecode,
                    self_id,
                    span.start,
                    span.end,
                    lhs,
                    Some(rhs),
                    crate::typechecking::AggregateOp::Mod,
                ) {
                    // Intentional empty body: the emit/try_emit call in the
                    // condition already wrote bytecode as a side effect.
                } else if self.operand_is_open_ty(lhs) || self.operand_is_open_ty(rhs) {
                    let _ = self.compile_binary_operands(&mut bytecode, lhs, rhs);
                    bytecode.push(Byte::new(Instruction::DynMod));
                } else {
                    let is_float = likely(self.compile_binary_operands(&mut bytecode, lhs, rhs));
                    bytecode.push(Byte::new(if is_float {
                        Instruction::MODF
                    } else {
                        Instruction::MOD
                    }));
                }
            }
            Expression::Div(lhs, rhs) => {
                if self.try_emit_aggregate_arith(
                    &mut bytecode,
                    self_id,
                    span.start,
                    span.end,
                    lhs,
                    Some(rhs),
                    crate::typechecking::AggregateOp::Div,
                ) {
                    // Intentional empty body: the emit/try_emit call in the
                    // condition already wrote bytecode as a side effect.
                } else {
                    let bound_div = self.bound_operator_hint(self_id, span.start, span.end);
                    if self.try_emit_folded_expr(ast, &mut bytecode, bound_div.is_none()) {
                        // Const-fold, `/ 1`, or `byte / 2^n` → SHR.
                    } else if let Some(hint) = bound_div
                        && self.emit_bound_operator_call(
                            &mut bytecode,
                            lhs,
                            rhs,
                            hint.dict_index,
                            hint.method_slot,
                        )
                    {
                        // Intentional empty body: the emit/try_emit call in the
                        // condition already wrote bytecode as a side effect.
                    } else {
                        let is_float =
                            likely(self.compile_binary_operands(&mut bytecode, lhs, rhs));
                        bytecode.push(Byte::new(if is_float {
                            Instruction::DIVF
                        } else {
                            Instruction::DIV
                        }));
                    }
                }
            }
            Expression::And(lhs, rhs) => {
                binary!(bytecode, self, lhs, rhs, Byte::new(Instruction::AND));
            }
            Expression::Positive(lhs) => {
                bytecode.append(&mut self.do_compile(lhs));
            }
            Expression::Pow(lhs, rhs) => {
                if self.try_emit_folded_expr(ast, &mut bytecode, true) {
                    // **0 / **1 / **2 strength-reduced or const-folded.
                } else if self.try_emit_aggregate_arith(
                    &mut bytecode,
                    self_id,
                    span.start,
                    span.end,
                    lhs,
                    Some(rhs),
                    crate::typechecking::AggregateOp::Pow,
                ) {
                    // Intentional empty body: the emit/try_emit call in the
                    // condition already wrote bytecode as a side effect.
                } else {
                    let is_float = self.compile_binary_operands(&mut bytecode, lhs, rhs);
                    bytecode.push(Byte::new(if is_float {
                        Instruction::PowF
                    } else {
                        Instruction::Pow
                    }));
                }
            }
            Expression::Shl(lhs, rhs) => {
                if self.try_emit_folded_expr(ast, &mut bytecode, true) {
                } else {
                    binary!(bytecode, self, lhs, rhs, Byte::new(Instruction::SHL));
                }
            }
            Expression::Shr(lhs, rhs) => {
                if self.try_emit_folded_expr(ast, &mut bytecode, true) {
                } else {
                    binary!(bytecode, self, lhs, rhs, Byte::new(Instruction::SHR));
                }
            }
            Expression::Xor(lhs, rhs) => {
                if self.try_emit_folded_expr(ast, &mut bytecode, true) {
                } else {
                    binary!(bytecode, self, lhs, rhs, Byte::new(Instruction::XOR));
                }
            }
            Expression::BitAnd(lhs, rhs) => {
                if self.try_emit_folded_expr(ast, &mut bytecode, true) {
                } else {
                    binary!(bytecode, self, lhs, rhs, Byte::new(Instruction::BITAND));
                }
            }
            Expression::BitOr(lhs, rhs) => {
                if self.try_emit_folded_expr(ast, &mut bytecode, true) {
                } else {
                    binary!(bytecode, self, lhs, rhs, Byte::new(Instruction::BITOR));
                }
            }
            Expression::Or(lhs, rhs) => {
                binary!(bytecode, self, lhs, rhs, Byte::new(Instruction::OR));
            }
            Expression::Neq(lhs, rhs) => {
                let hint = self.bound_operator_hint(self_id, span.start, span.end);
                if let Some(hint) = hint
                    && self.emit_bound_operator_call(
                        &mut bytecode,
                        lhs,
                        rhs,
                        hint.dict_index,
                        hint.method_slot,
                    )
                {
                    // Intentional empty body: the emit/try_emit call in the
                    // condition already wrote bytecode as a side effect.
                } else if self.emit_concrete_operator_call(&mut bytecode, lhs, rhs, "Eq", "ne") {
                    // Intentional empty body: the emit/try_emit call in the
                    // condition already wrote bytecode as a side effect.
                } else {
                    binary!(bytecode, self, lhs, rhs, Byte::new(Instruction::NEQ));
                }
            }
            Expression::Integer(num) => bytecode.push(Byte::new_with_value(
                Instruction::CONST,
                Value::from(*num).raw() as _,
            )),
            Expression::Bool(state) => bytecode.push(Byte::new_with_value(
                Instruction::CONST,
                Value::from(*state).raw() as _,
            )),
            Expression::Float(num) => {
                let bits = Value::from(*num).raw() as u64;
                let idx = self.intern_constant(bits);
                bytecode.push_const_pool(idx);
            }
            Expression::String(str) => {
                let escaped = unescape_coil_string(str);
                // Prefer this node's sidecar (pointer / span) over `emit_idx`.
                // Stack-array init used to skip the array NodeId so sequential
                // lookup returned the parent `[byte; N]` and `or_else` never
                // ran — elements emitted as heap strings (byte_string_lit.hy).
                let escaped_len = escaped.as_bytes().len();
                let is_byte_or_bytes = |ty: &Ty| match ty {
                    Ty::Con(n) if n == "byte" => true,
                    Ty::Array { element, length }
                        if matches!(element.as_ref(), Ty::Con(n) if n == "byte") =>
                    {
                        match length {
                            crate::typechecking::ty::ArrayLength::Static(n) => *n == escaped_len,
                            crate::typechecking::ty::ArrayLength::Dynamic => true,
                        }
                    }
                    _ => false,
                };
                // Prefer this node's sidecar (pointer / span) over `emit_idx`.
                // If `self_id` drifted onto a parent `[byte; N]`, ignore a
                // non-matching type so the string node's span/NodeId can still coerce.
                let span_ty = self
                    .node_id_of(ast)
                    .and_then(|id| self.sidecar_ty(id))
                    .filter(is_byte_or_bytes)
                    .or_else(|| self.sidecar_ty_of(ast).filter(is_byte_or_bytes))
                    .or_else(|| {
                        self_id
                            .and_then(|id| self.sidecar_ty(id))
                            .filter(is_byte_or_bytes)
                    });
                // Single-byte string literals typed as `byte` emit CONST.
                let as_byte = span_ty
                    .as_ref()
                    .is_some_and(|ty| matches!(ty, Ty::Con(n) if n == "byte"));
                // String literals typed as `[byte]` / `[byte; N]` emit CONST*N + MakeArray.
                let as_bytes = span_ty.as_ref().is_some_and(|ty| {
                    matches!(
                        ty,
                        Ty::Array { element, .. }
                            if matches!(element.as_ref(), Ty::Con(n) if n == "byte")
                    )
                });
                if as_byte {
                    match escaped.as_bytes() {
                        [b] => {
                            bytecode.push(Byte::new_with_value(
                                Instruction::CONST,
                                Value::from(*b as i64).raw() as _,
                            ));
                        }
                        _ => {
                            self.emit_raw_string_literal(&mut bytecode, &escaped);
                        }
                    }
                } else if as_bytes {
                    let bytes = escaped.as_bytes();
                    for &b in bytes {
                        bytecode.push(Byte::new_with_value(
                            Instruction::CONST,
                            Value::from(b as i64).raw() as _,
                        ));
                    }
                    bytecode.push_make_array(bytes.len() as u32);
                } else {
                    self.emit_raw_string_literal(&mut bytecode, &escaped);
                }
            }
            Expression::Variable(name, _ty) => {
                if unlikely(self.context.variables.contains(&name.to_string())) {
                    let mut message = Message::error(
                        ErrorCode::VariableRedeclaration,
                        "Variable redeclaration".to_string(),
                        span.into_range(),
                    );
                    message.push(DiagLabel::new(
                        format!("Variable '{}' already declared", name),
                        span.into_range(),
                    ));
                    self.messages.push(message);
                }

                self.context.variables.intern(name.to_string());
            }
            Expression::Constant(name, _ty) => {
                let name = self.resolve_variable(name);
                if self.context.variables.contains(&name) {
                    let mut message = Message::error(
                        ErrorCode::VariableRedeclaration,
                        "Constand redeclaration".to_string(),
                        span.into_range(),
                    );
                    message.push(DiagLabel::new(
                        format!("Constant '{}' already declared", name),
                        span.into_range(),
                    ));
                    self.messages.push(message);
                }

                let symbol = self.context.variables.intern(name.clone());

                self.context.constants.insert(symbol, false);
            }
            Expression::Assignment(lhs, value) => match lhs.1.as_ref() {
                Expression::QualifiedAccess { owner, member } => {
                    let fqn = self.class_member_fqn(owner, member);
                    if let Some(slot) = self.checker.static_slot_index(&fqn) {
                        self.append_binding_rhs(&mut bytecode, value);
                        bytecode.push(Byte::new(Instruction::StoreStatic).with_operand_u32(slot));
                    }
                }
                Expression::Construct {
                    enum_name,
                    variant_name,
                    fields,
                } if matches!(fields, parser::ast::EnumConstructPayload::Unit) => {
                    let fqn = self.class_member_fqn(enum_name, variant_name);
                    if let Some(slot) = self.checker.static_slot_index(&fqn) {
                        self.append_binding_rhs(&mut bytecode, value);
                        bytecode.push(Byte::new(Instruction::StoreStatic).with_operand_u32(slot));
                    }
                }
                Expression::Access(target_expr, field) => {
                    self.append_binding_rhs(&mut bytecode, value);
                    bytecode.append(&mut self.do_compile(target_expr));
                    self.emit_field_name(&mut bytecode, field);
                    bytecode.push_set_field();
                    // Value left on stack for expression result; ExprStatement POPs.
                }
                Expression::Index(arr, None) => {
                    let _ = arr;
                    self.messages.push({
                        let mut m = Message::error(
                            ErrorCode::InvalidAssignment,
                            "append assignment `arr[] = value` is not supported".to_string(),
                            lhs.0.into_range(),
                        );
                        m.push(DiagLabel::new(
                            "use `vec.push(value)` on a `Vec<T>`".to_string(),
                            lhs.0.into_range(),
                        ));
                        m
                    });
                }
                Expression::Index(arr, Some(idx)) => {
                    if let Expression::Identifier(name) = arr.1.as_ref()
                        && let Some((base, n)) = self.stack_array_info(name)
                        && let Expression::Integer(i) = idx.1.as_ref()
                        && *i >= 0
                        && (*i as usize) < n
                    {
                        self.append_binding_rhs(&mut bytecode, value);
                        bytecode.push_store_pop(base + *i as u32);
                        // Leave value on stack like StoreIndex.
                        bytecode.push_load(base + *i as u32);
                    } else {
                        // RHS is evaluated first and spilled. Array may stay on
                        // the operand stack only for push-only index exprs —
                        // see `index_keeps_array_on_stack_safe`.
                        let tmp_val = self.alloc_temp_slot();
                        let depth_on_entry = self.expr_depth;
                        self.append_binding_rhs(&mut bytecode, value);
                        bytecode.push_store_pop(tmp_val);
                        if Self::index_keeps_array_on_stack_safe(idx.1.as_ref()) {
                            bytecode.append(&mut self.do_compile(arr));
                            self.expr_depth = depth_on_entry + 1;
                            bytecode.append(&mut self.do_compile(idx));
                            self.expr_depth = depth_on_entry;
                            bytecode.push_load(tmp_val);
                        } else {
                            let tmp_arr = self.alloc_temp_slot();
                            let tmp_idx = self.alloc_temp_slot();
                            bytecode.append(&mut self.do_compile(arr));
                            bytecode.push_store_pop(tmp_arr);
                            bytecode.append(&mut self.do_compile(idx));
                            bytecode.push_store_pop(tmp_idx);
                            bytecode.push_load(tmp_arr);
                            bytecode.push_load(tmp_idx);
                            bytecode.push_load(tmp_val);
                        }
                        bytecode.push(Byte::new(Instruction::StoreIndex));
                    }
                }
                Expression::Identifier(name) => {
                    let resolved = self.resolve_free_fn(name);
                    if let Some(static_slot) = self
                        .checker
                        .static_slot_index(&resolved)
                        .or_else(|| self.checker.static_slot_for_module_name(name))
                    {
                        self.append_binding_rhs(&mut bytecode, value);
                        bytecode.push(
                            Byte::new(Instruction::StoreStatic).with_operand_u32(static_slot),
                        );
                    } else {
                        self.context.assignments.insert(name.to_string(), true);
                        let symbol_opt = if let Some(map) = &self.context.match_bindings {
                            if let Some(&slot) = map.get(*name) {
                                Some(slot as usize)
                            } else {
                                self.context.variables.key(&name.to_string())
                            }
                        } else {
                            self.context.variables.key(&name.to_string())
                        };

                        if let Some(symbol) = symbol_opt {
                            if unlikely(self.context.constants.contains_key(&symbol)) {
                                let assigned =
                                    likely(*self.context.constants.get(&symbol).unwrap());
                                if !assigned {
                                    self.context.constants.entry(symbol).and_modify(|state| {
                                        *state = true;
                                    });
                                } else {
                                    let mut message = Message::error(
                                        ErrorCode::InvalidAssignment,
                                        "Assignment error".to_string(),
                                        span.into_range(),
                                    );
                                    message.push(DiagLabel::new(
                                        format!(
                                            "Unable to assign to an already assigned constant '{}'",
                                            name
                                        ),
                                        span.into_range(),
                                    ));
                                    self.messages.push(message);
                                }
                            }
                            // Multi-slot stack array: rewrite slots in place.
                            if let Some((base, n)) = self.stack_array_info(name) {
                                // Assignment: `name` Identifier is pre-walked
                                // before `value`; consume it when we skip emit.
                                let _ = self.next_emit_id();
                                let ok = self.try_emit_stack_array_init(
                                    &mut bytecode,
                                    value,
                                    base,
                                    n,
                                );
                                if !ok {
                                    // Heap `[T; N]` (e.g. call return): box into slots.
                                    self.append_binding_rhs(&mut bytecode, value);
                                    let tmp = self.alloc_temp_slot();
                                    bytecode.push_store_pop(tmp);
                                    for i in 0..n {
                                        bytecode.push_load(tmp);
                                        bytecode.push_const(i as i32);
                                        bytecode.push_index();
                                        bytecode.push_store_pop(base + i as u32);
                                    }
                                }
                            } else {
                                self.append_binding_rhs(&mut bytecode, value);
                                bytecode.push_store_pop(symbol as u32);
                            }
                        } else {
                            let mut message = Message::error(
                                ErrorCode::UnknownValue,
                                "Undefined variable".to_string(),
                                span.into_range(),
                            );
                            message.push(DiagLabel::new(
                                format!(
                                    "Unable to assign to a non-existing variable/constant '{}'",
                                    name
                                ),
                                span.into_range(),
                            ));
                            self.messages.push(message);
                        }
                    }
                }
                _ => {
                    bytecode.append(&mut self.do_compile(value));
                    bytecode.push_pop();
                }
            },

            // --- Sum types, extern, construct ---
            Expression::ExternBlock {
                library,
                declarations,
            } => {
                // Emit into `ffi_init` so setup is spliced into the prologue
                // at finalize — works for `extern` in imported modules too.
                std::mem::swap(&mut self.bytecode, &mut self.ffi_init);
                // Extern lib / fn-id handles live in static slots so function
                // locals (which share the prologue frame and restart at slot 0)
                // cannot overwrite them — that produced `invalid library handle`
                // on a second call or after any earlier `let`.
                let lib_slot =
                    if let Some(&existing) = self.extern_runtime_libs.get(library.as_str()) {
                        existing
                    } else {
                        let fqn = format!("__ext_lib_{}", library);
                        let slot = self.checker.alloc_synthetic_static_slot(
                            fqn,
                            crate::typechecking::ty::int(),
                        );
                        self.extern_runtime_libs.insert(library.clone(), slot);
                        slot
                    };
                // dlopen once per library short name for the compile unit.
                if !self.extern_runtime_libs_loaded.contains(library.as_str()) {
                    self.extern_runtime_libs_loaded.insert(library.clone());
                    let span: SimpleSpan = (0..0).into();
                    let path_expr: parser::ast::Output = (
                        span,
                        Box::new(parser::ast::Expression::String(library.as_str())),
                    );
                    let mut bc = self.do_compile(&path_expr);
                    self.bytecode.append(&mut bc);
                    self.bytecode.push(Byte::new(Instruction::FfiLoad));
                    self.emit_result_unwrap_or_panic();
                    self.bytecode
                        .push(Byte::new(Instruction::StoreStatic).with_operand_u32(lib_slot));
                }
                // For each declared function, emit declare(lib, name, …) and
                // store the fn id in a static slot.
                for decl in declarations {
                    let fn_name = decl.name.to_string();
                    let nfixed = if let Expression::Fragment(items) = decl.args.1.as_ref() {
                        items
                            .iter()
                            .filter(|a| matches!(a.1.as_ref(), Expression::Argument { .. }))
                            .count()
                    } else {
                        0
                    };
                    // Key fixed-arity overloads; keep bare name for
                    // single decls and for C-varargs (not overload members).
                    let table_name = if !decl.variadic && self.checker.is_overloaded(decl.name) {
                        overload_fn_key(&fn_name, nfixed, false, 0)
                    } else {
                        fn_name.clone()
                    };
                    // First-wins on the same table key across blocks.
                    if self.extern_runtime_functions.contains_key(&table_name) {
                        continue;
                    }
                    let fn_id_fqn = format!("__ext_fn_{}", table_name);
                    let fn_id_slot = self.checker.alloc_synthetic_static_slot(
                        fn_id_fqn,
                        crate::typechecking::ty::int(),
                    );
                    // Push the library handle.
                    self.bytecode
                        .push(Byte::new(Instruction::LoadStatic).with_operand_u32(lib_slot));
                    // Push the function name (string literal).
                    let span: SimpleSpan = (0..0).into();
                    let sym = decl.symbol.unwrap_or(decl.name);
                    let name_expr: parser::ast::Output =
                        (span, Box::new(parser::ast::Expression::String(sym)));
                    let mut name_bc = self.do_compile(&name_expr);
                    self.bytecode.append(&mut name_bc);
                    // Push each arg type as a CONST tag.
                    let mut arg_type_tags: Vec<u32> = Vec::new();
                    if let Expression::Fragment(items) = decl.args.1.as_ref() {
                        for arg in items {
                            if let Expression::Argument {
                                ty: type_expr,
                                ..
                            } = arg.1.as_ref()
                                && let Some(type_expr) = type_expr
                            {
                                if let Some((tag, aux)) =
                                    ffi_type_tag_from_output(&self.checker, type_expr)
                                {
                                    emit_ffi_type_const(&mut self.bytecode, tag, aux);
                                    arg_type_tags.push(tag);
                                } else {
                                    self.messages.push({
                                        let mut m = Message::error(
                                           ErrorCode::GenericTypeError, "Unknown FFI argument type".to_string(),
                                            arg.0.into_range(),
                                        );
                                        m.push(DiagLabel::new(
                                            "use Int/Ptr after `use ffi::types::{Int, Ptr, …}`, a bare type name, [T], (T, U), or an extern struct".to_string(),
                                            arg.0.into_range(),
                                        ));
                                        m
                                    });
                                    arg_type_tags.push(0);
                                }
                            } else {
                                self.messages.push({
                                    let mut m = Message::error(
                                        ErrorCode::GenericTypeError,
                                        "Extern fn argument must be `name: type` form".to_string(),
                                        arg.0.into_range(),
                                    );
                                    m.push(DiagLabel::new(
                                        "got an unexpected expression".to_string(),
                                        arg.0.into_range(),
                                    ));
                                    m
                                });
                                arg_type_tags.push(0);
                            }
                        }
                    }
                    let arity = arg_type_tags.len() as u32;
                    self.bytecode.push_make_tuple(arity);
                    // Push the ret type tag (top of stack for DeclareFFI).
                    let (ret_tag, ret_aux) = decl
                        .returns
                        .as_ref()
                        .and_then(|r| ffi_type_tag_from_output(&self.checker, r))
                        .unwrap_or((tag::VOID, 0));
                    emit_ffi_type_const(&mut self.bytecode, ret_tag, ret_aux);
                    // Emit DeclareFFI (bit 16 = C varargs).
                    let mut operand = arity & 0xFFFF;
                    if decl.variadic {
                        operand |= 1 << 16;
                    }
                    self.bytecode
                        .push(Byte::new(Instruction::DeclareFFI).with_operand_u32(operand));
                    self.emit_result_unwrap_or_panic();
                    // Store the function id.
                    self.bytecode
                        .push(Byte::new(Instruction::StoreStatic).with_operand_u32(fn_id_slot));
                    self.extern_runtime_functions
                        .insert(table_name, (lib_slot, fn_id_slot));
                }
                std::mem::swap(&mut self.bytecode, &mut self.ffi_init);
            }
            Expression::EnumDecl {
                docs: _,
                name: _, variants, ..
            } => {
                // Recurse into each variant. Each variant's
                // `do_compile` consumes 1 ID (for the variant
                // itself) and then descends into each payload's
                // `Type` expression. We don't emit any bytecode
                // here — the enum declaration is metadata that's
                // already been registered with the typechecker
                // (15B).
                for v in variants {
                    bytecode.append(&mut self.do_compile(v));
                }
            }
            Expression::TypeAlias { ty, .. } => {
                bytecode.append(&mut self.do_compile(ty));
            }
            Expression::TestCase { name, body } => {
                // Consume name NodeIds (discard emitted string bytes).
                let _ = self.do_compile(name);
                let desc = match name.1.as_ref() {
                    Expression::String(s) => (*s).to_string(),
                    Expression::Expr((_, inner)) => match inner.as_ref() {
                        Expression::String(s) => (*s).to_string(),
                        _ => format!("test_{}", self.test_cases.len()),
                    },
                    _ => format!("test_{}", self.test_cases.len()),
                };
                let case_index = self.test_cases.len();
                let fn_name = crate::typechecking::Checker::test_case_fn_name(case_index);
                let (offset, _) = self.bind_function_entry(fn_name.clone());
                let offset = offset as u32;
                self.test_cases.push((desc, offset));

                let prev_fn_vars = std::mem::take(&mut self.context.variables);
                let prev_fn_polyfn_vars = std::mem::take(&mut self.polyfn_vars);
                let prev_fn_polyfn_sources = std::mem::take(&mut self.polyfn_sources);
                self.context.variables = Interner::default();

                let prev_result_mode = self.compiling_result_mode;
                let prev_result_ok_is_result = self.compiling_result_ok_is_result;
                self.compiling_result_mode = self.checker.fn_is_result_mode(&fn_name);
                self.compiling_result_ok_is_result =
                    self.checker.fn_result_ok_is_result(&fn_name);
                let prev_pair_mode = self.compiling_pair_mode;
                let prev_pair_is_option = self.compiling_pair_is_option;
                let pair_kind = self.pair_return_kind(&fn_name);
                self.compiling_pair_mode = pair_kind.is_some();
                self.compiling_pair_is_option = pair_kind.unwrap_or(false);

                let body_op_start = self.bytecode.ops().len();
                let prev_field_keys = std::mem::take(&mut self.field_key_slots);
                self.emit_field_key_prologue(body);
                let mut body_bc = self.do_compile(body);
                self.bytecode.append(&mut body_bc);

                if !self.region_ends_with_return(body_op_start) {
                    // Test cases are typed as unit / Result<(), string> — zero is safe.
                    self.emit_fallthrough_return(&fn_name, body.0);
                }

                let body_end = self.bytecode.len();
                // Flatten remaps per IlFunc; unrecorded tests share the epilogue
                // and collide with ArrayPin labels in earlier bodies.
                self.record_fn_span(fn_name.clone(), offset as usize, body_end);
                let entry = self.fn_entry_labels.get(&fn_name).copied();
                self.bytecode.record_func_with_sp(
                    fn_name.clone(),
                    entry,
                    offset as usize,
                    body_end,
                    0,
                );

                self.compiling_result_mode = prev_result_mode;
                self.compiling_result_ok_is_result = prev_result_ok_is_result;
                self.compiling_pair_mode = prev_pair_mode;
                self.compiling_pair_is_option = prev_pair_is_option;
                self.field_key_slots = prev_field_keys;
                self.context.variables = prev_fn_vars;
                self.polyfn_vars = prev_fn_polyfn_vars;
                self.polyfn_sources = prev_fn_polyfn_sources;
            }
            Expression::ExternStruct(decl) => {
                for (_, ty) in &decl.fields {
                    bytecode.append(&mut self.do_compile(ty));
                }
            }
            Expression::EnumVariant { payload, .. } => {
                // Recurse into each payload's `Type` expression
                // (or `RecordFieldDecl`'s value type). We don't
                // emit bytecode — the variant's payload shape is
                // metadata that's already registered with the
                // typechecker (15B). the payload is
                // `EnumVariantPayload` (Unit / Tuple / Record);
                // only Tuple and Record have children to walk.
                use parser::ast::EnumVariantPayload;
                match payload {
                    EnumVariantPayload::Unit => {}
                    EnumVariantPayload::Tuple(parts) => {
                        for p in parts {
                            bytecode.append(&mut self.do_compile(p));
                        }
                    }
                    EnumVariantPayload::Record(fields) => {
                        for f in fields {
                            bytecode.append(&mut self.do_compile(&f.value));
                        }
                    }
                }
            }
            Expression::Construct {
                enum_name,
                variant_name,
                fields,
            } => {
                use parser::ast::EnumConstructPayload;
                // Look up the variant's tag and arity in the
                // typechecker's tables. Unknown enum/variant is a
                // type error with recovery — still walk children for
                // NodeId alignment, but do not emit MakeEnum (and do
                // not panic: release builds use panic=abort).
                let Some(tag) = self.checker.tag_for(enum_name, variant_name) else {
                    let fqn = self.class_member_fqn(enum_name, variant_name);
                    // Match typechecker order for Unit form: static field
                    // wins over a same-named 0-arg static method.
                    if matches!(fields, EnumConstructPayload::Unit)
                        && let Some(slot) = self.checker.static_slot_index(&fqn)
                    {
                        bytecode.push(Byte::new(Instruction::LoadStatic).with_operand_u32(slot));
                        return bytecode;
                    }
                    // `Class::static_method(...)` — same surface as enum
                    // Construct; lower to a direct CALL or Entry{Call} when
                    // the method is compiled or reserved (COI-108 forward).
                    if self.functions.contains_key(&fqn) || self.fn_entry_labels.contains_key(&fqn) {
                        let arg_slice: &[Output] = match fields {
                            EnumConstructPayload::Unit => &[],
                            EnumConstructPayload::Tuple(args) => args.as_slice(),
                            EnumConstructPayload::Record(parts) => {
                                for part in parts {
                                    bytecode.append(&mut self.do_compile(&part.value));
                                }
                                // Record form is a type error for static
                                // methods; still emit a CALL for recovery.
                                let _ = self.emit_direct_fn_call(
                                    &mut bytecode,
                                    &fqn,
                                    parts.len() as u32,
                                );
                                return bytecode;
                            }
                        };
                        let arity =
                            self.emit_call_args_with_rest(&fqn, arg_slice, &mut bytecode, false);
                        let _ = self.emit_direct_fn_call(&mut bytecode, &fqn, arity);
                        return bytecode;
                    }
                    match fields {
                        EnumConstructPayload::Unit => {}
                        EnumConstructPayload::Tuple(args) => {
                            for arg in args {
                                bytecode.append(&mut self.do_compile(arg));
                            }
                        }
                        EnumConstructPayload::Record(parts) => {
                            for part in parts {
                                bytecode.append(&mut self.do_compile(&part.value));
                            }
                        }
                    }
                    return bytecode;
                };
                let arity = self.checker.arity_for(enum_name, variant_name).unwrap_or(0);

                let pair_enum = self.pair_value_context
                    && (common::is_builtin_option_enum(enum_name)
                        || common::is_builtin_result_enum(enum_name))
                    && arity <= 1;
                if pair_enum {
                    match fields {
                        EnumConstructPayload::Unit if arity == 0 => {
                            // Keep a payload slot for `Option::None` so every
                            // pair has the same `[payload, tag]` shape.
                            bytecode.push_const(0);
                        }
                        EnumConstructPayload::Tuple(args) if args.len() == 1 => {
                            bytecode.append(&mut self.do_compile(&args[0]));
                        }
                        EnumConstructPayload::Record(parts) if parts.len() == 1 => {
                            bytecode.append(&mut self.do_compile(&parts[0].value));
                        }
                        _ => {}
                    }
                    if matches!(fields, EnumConstructPayload::Unit) && arity != 0 {
                        // Invalid constructor shapes are already reported by
                        // typechecking; keep the stack balanced for recovery.
                        bytecode.push_const(0);
                    }
                    bytecode.push_const(tag as i32);
                    return bytecode;
                }

                if common::is_builtin_option_enum(enum_name)
                    && !self.force_heap_option
                    && self
                        .codegen_expr_ty(ast)
                        .is_some_and(|ty| self.niche_option_inner_ty(&ty).is_some())
                    || (common::is_builtin_option_enum(enum_name)
                        && !self.force_heap_option
                        && self.force_niche_option)
                {
                    match (*variant_name, fields) {
                        ("None", EnumConstructPayload::Unit) => {
                            bytecode.push_const(0);
                            return bytecode;
                        }
                        ("Some", EnumConstructPayload::Tuple(args)) if args.len() == 1 => {
                            bytecode.append(&mut self.do_compile(&args[0]));
                            return bytecode;
                        }
                        ("Some", EnumConstructPayload::Record(parts)) if parts.len() == 1 => {
                            bytecode.append(&mut self.do_compile(&parts[0].value));
                            return bytecode;
                        }
                        _ => {}
                    }
                }

                // Emit args in reverse declaration order for MAKE_ENUM stack discipline.
                match fields {
                    EnumConstructPayload::Unit => {}
                    EnumConstructPayload::Tuple(args) => {
                        let in_generic = self
                            .current_function_qualified
                            .as_deref()
                            .is_some_and(|n| self.generic_return_is_boxed(n));
                        for arg in args.iter().rev() {
                            let mut arg_bc = self.do_compile(arg);
                            if in_generic {
                                if let Some(ty) = self.codegen_expr_ty(arg) {
                                    Self::emit_unbox_if_needed(&mut arg_bc, &ty);
                                }
                            }
                            bytecode.append(&mut arg_bc);
                        }
                    }
                    EnumConstructPayload::Record(parts) => {
                        // Build a name → &Output map for the call site.
                        let call_site: std::collections::HashMap<&str, &Output> =
                            parts.iter().map(|p| (p.name, &p.value)).collect();
                        let decl_order = self.checker.payload_tys_for(enum_name, variant_name);
                        // Walk DECLARATION order REVERSED — so when
                        // MAKE_ENUM pops, payload[0] is `decl_fields[0]`.
                        for (decl_name, _) in decl_order.iter().rev() {
                            if let Some(arg) = call_site.get(decl_name.as_str()) {
                                bytecode.append(&mut self.do_compile(arg));
                            }
                            // Missing field: typechecker has already
                            // reported; skip silently to keep bytecode
                            // emission in lockstep with IDs.
                        }
                    }
                }

                // Emit MAKE_ENUM with the tag (upper 16) and
                // arity (lower 16) packed in the operand.
                bytecode.push_make_enum(tag as u16, arity as u16);
            }
            // --- Match codegen (threaded layout) ---
            // Forward: scrutinee, JUMP_IF_MATCH cascade, last-arm UNPACK/POP/STORE.
            // Reverse: arm bindings + bodies; non-first arms JMP to end.
            Expression::Match { scrutinee, arms } => bytecode.append(&mut self.compile_match_expr(scrutinee, arms)),
            // Parser maps `_`/`default` to Pattern::Wildcard; arm consumes NodeId only.
            Expression::Default(_) => (),

            // --- Field access ---
            // receiver bytecode + LoadField(index) or GetField(name) for
            // dicts / class instances.
            Expression::Access(receiver, field) => {
                if self.try_emit_direct_class_field_access(&mut bytecode, receiver, field) {
                    return bytecode;
                }
                bytecode.append(&mut self.do_compile(receiver));

                let receiver_ty = self.receiver_type(receiver);
                let is_record =
                    matches!(&receiver_ty, Some(crate::typechecking::Ty::Record { .. }));
                let is_class = receiver_ty
                    .as_ref()
                    .is_some_and(|ty| self.checker.ty_is_class(ty));
                // LoadField only for confirmed sum record payloads.
                // Prefer GetField for classes and anonymous records.
                // `extract_enum_name` alone is unsafe (Ty::Con class
                // names look like enums) — require field_index_for
                // or an is_class check. Unknown receivers that are
                // not classes fall back to LoadField(0) (legacy
                // defensive path) rather than GetField, which would
                // corrupt ObjEnum stacks.
                let enum_field_index = if !is_record && !is_class {
                    self.receiver_type(receiver).and_then(|ty| {
                        use crate::typechecking::ty::Ty;
                        if self.checker.ty_is_class(&ty) {
                            return None;
                        }
                        match &ty {
                            Ty::Constructor { tag, owner, .. } => {
                                let name = extract_enum_name(owner)?;
                                if self.checker.is_class(&name) {
                                    return None;
                                }
                                self.checker
                                    .field_index_for_tagged(&name, field, Some(*tag))
                                    .map(|(_variant, idx)| idx)
                            }
                            _ => {
                                let name = extract_enum_name(&ty)?;
                                if self.checker.is_class(&name) {
                                    return None;
                                }
                                self.checker
                                    .field_index_for(&name, field)
                                    .map(|(_variant, idx)| idx)
                            }
                        }
                    })
                } else {
                    None
                };
                if let Some(field_index) = enum_field_index {
                    bytecode.push_load_field(field_index as u32);
                } else if is_record || is_class {
                    self.emit_field_name(&mut bytecode, field);
                    bytecode.push_get_field();
                } else {
                    // Unknown receiver — do not emit GetField (enum
                    // match bindings historically lacked side-table
                    // types). LoadField(0) keeps the stack balanced;
                    // VM hardens non-enum receivers.
                    bytecode.push_load_field(0);
                }
            }

            Expression::Field { .. } => {
                // Class field decls are metadata only — consumed for ID alignment.
            }

            // --- Error-handling operators (desugar to MakeEnum / JumpIfMatch) ---
            Expression::Raise(expr) => {
                // `raise e` → push e, wrap Err(e), RETURN.
                let mut expr_bc = self.do_compile(expr);
                self.emit_bytes(*span, &mut expr_bc);
                if self.compiling_pair_mode {
                    self.bytecode.push_const(1);
                } else {
                    Self::emit_result_err(&mut self.bytecode);
                }
                self.pad_debug_locs();
                let loc = self.loc_from_span(*span);
                if self.compiling_pair_mode {
                    self.push_return_pair();
                } else {
                    self.bytecode.push_return_at(loc);
                }
                self.debug_locs.push(loc);
            }
            Expression::Panic(expr) => {
                let mut expr_bc = self.do_compile(expr);
                self.emit_bytes(*span, &mut expr_bc);
                self.emit_byte(*span, Byte::new(Instruction::Panic));
            }
            Expression::TypeOf(inner) => {
                // Advance emit_idx through the operand without evaluating it.
                self.discard_compile(inner);
                match self.codegen_expr_ty(inner).and_then(|ty| {
                    let pruned =
                        crate::typechecking::subst::apply_ty_prune(self.checker.subst(), &ty);
                    crate::typechecking::pretty::format_ty_fqn(
                        &pruned,
                        &self.checker.generics().nominal_type_modules,
                    )
                }) {
                    Some(fqn) => {
                        self.emit_raw_string_literal(&mut bytecode, &fqn);
                    }
                    None => {
                        let mut message = Message::error(
                            ErrorCode::GenericTypeError,
                            "`typeof` requires a ground type".to_string(),
                            span.into_range(),
                        );
                        message.push(DiagLabel::new(
                            "type is not fully known at compile time".to_string(),
                            span.into_range(),
                        ));
                        self.messages.push(message);
                        self.emit_raw_string_literal(&mut bytecode, "<unknown>");
                    }
                }
            }
            Expression::Try(inner) => {
                // `e?` → if Ok/Some, leave payload; else RETURN the failure.
                let is_option = self.expr_is_option(inner);
                let success_tag: u32 = if is_option { 1 } else { 0 }; // Some=1, Ok=0

                let pair_inner = self.expr_pairs_with_return(inner);
                // Mismatched Ok/Some payloads still leave a ReturnPair on the
                // stack (`pair_producer`); keep pair context so the call is not
                // boxed before the tag check below.
                let pair_producer =
                    self.compiling_pair_mode && self.expr_is_pair_producer(inner);
                let previous_pair_context = self.pair_value_context;
                self.pair_value_context = pair_inner || pair_producer;
                let mut inner_bc = self.do_compile(inner);
                self.pair_value_context = previous_pair_context;
                self.bytecode.append(&mut inner_bc);

                let mut bb = BlockBuilder::new();
                let success = bb.fresh_label(self.bytecode.il_mut());
                if pair_inner || pair_producer {
                    let failure = bb.fresh_label(self.bytecode.il_mut());
                    let after_failure = bb.fresh_label(self.bytecode.il_mut());
                    self.bytecode.push(Byte::new(Instruction::DUPLICATE));
                    self.bytecode.push_const(success_tag as i32);
                    self.bytecode.push(Byte::new(Instruction::EQ));
                    bb.emit_jump_to_hinted(
                        failure,
                        BbJumpKind::JumpIfFalse,
                        FuseHint::nofuse_value_under_jmp(),
                        self.bytecode.il_mut(),
                    );
                    self.bytecode.push_pop();
                    bb.emit_jump_to(
                        after_failure,
                        BbJumpKind::Unconditional,
                        self.bytecode.il_mut(),
                    );
                    bb.bind_label(failure, self.bytecode.il_mut());
                    self.push_return_pair();
                    bb.bind_label(after_failure, self.bytecode.il_mut());
                } else if self.expr_is_niche_option(inner) {
                    self.bytecode.push(Byte::new(Instruction::DUPLICATE));
                    self.bytecode.push(Byte::new(Instruction::LogNot));
                    bb.emit_jump_to_hinted(
                        success,
                        BbJumpKind::JumpIfFalse,
                        FuseHint::nofuse_value_under_jmp(),
                        self.bytecode.il_mut(),
                    );
                    if self.compiling_pair_mode {
                        self.bytecode.push_pop();
                        self.bytecode.push_const(0);
                        self.bytecode.push_const(0);
                        self.push_return_pair();
                    } else {
                        self.bytecode.push_return();
                    }
                    bb.bind_label(success, self.bytecode.il_mut());
                } else if self.compiling_pair_mode {
                    bb.emit_jump_to(
                        success,
                        BbJumpKind::JumpIfMatch {
                            tag: success_tag,
                            arity: 1,
                        },
                        self.bytecode.il_mut(),
                    );
                    self.bytecode.push(
                        Byte::new(Instruction::HeapToPair)
                            .with_operand_u32(u32::from(is_option)),
                    );
                    self.push_return_pair();
                    bb.bind_label(success, self.bytecode.il_mut());
                } else {
                    bb.emit_jump_to(
                        success,
                        BbJumpKind::JumpIfMatch {
                            tag: success_tag,
                            arity: 1,
                        },
                        self.bytecode.il_mut(),
                    );
                    // Miss: failure value still on stack — propagate via
                    // the ordinary boxed return.
                    self.bytecode.push_return();
                    bb.bind_label(success, self.bytecode.il_mut());
                }
                // Payload left on stack for the caller (e.g. StorePop).
            }
            Expression::Cast(expr, ty_ann) => {
                use crate::typechecking::ty::{ArrayLength, Ty};

                let dst_ty = self_id.and_then(|id| self.sidecar_ty(id));
                let src_ty = self.codegen_expr_ty(expr);
                let string_to_bytes = matches!(
                    (src_ty.as_ref(), dst_ty.as_ref()),
                    (
                        Some(Ty::Con(s)),
                        Some(Ty::Array {
                            element,
                            length: ArrayLength::Dynamic,
                            ..
                        })
                    ) if s == "string"
                        && matches!(element.as_ref(), Ty::Con(n) if n == "byte")
                );
                if string_to_bytes {
                    self.emit_host_native_invoke("to_bytes", std::slice::from_ref(expr));
                    return bytecode;
                }

                bytecode.append(&mut self.do_compile(expr));
                let src_ty = self.codegen_expr_ty(expr);
                let dst_name = primitive_name_from_type_ann(ty_ann);
                if let (Some(from), Some(to)) =
                    (src_ty.as_ref().and_then(primitive_type_name), dst_name)
                {
                    if from != to {
                        if let Some(op) = primitive_cast_opcode(from, to) {
                            bytecode.push(Byte::new(op));
                        }
                    }
                }
            }
            Expression::Coalesce(lhs, rhs) => {
                // `a ?? b` → Ok/Some payload, else evaluate b.
                let is_option = self.expr_is_option(lhs);
                let success_tag: u32 = if is_option { 1 } else { 0 };
                let niche_lhs = self.expr_is_niche_option(lhs);

                let direct_pair_lhs = matches!(
                    lhs.1.as_ref(),
                    Expression::Call { .. } | Expression::Construct { .. }
                );
                let pair_lhs = !niche_lhs
                    && self.expr_is_pair_producer(lhs)
                    && (self.compiling_pair_mode || direct_pair_lhs);
                let previous_pair_context = self.pair_value_context;
                self.pair_value_context = pair_lhs;
                let previous_niche_context = self.force_niche_option;
                self.force_niche_option = niche_lhs;
                let mut lhs_bc = self.do_compile(lhs);
                self.force_niche_option = previous_niche_context;
                self.pair_value_context = previous_pair_context;
                self.bytecode.append(&mut lhs_bc);

                let mut bb = BlockBuilder::new();
                let success = bb.fresh_label(self.bytecode.il_mut());
                let end = bb.fresh_label(self.bytecode.il_mut());
                if pair_lhs {
                    let failure = bb.fresh_label(self.bytecode.il_mut());
                    self.bytecode.push(Byte::new(Instruction::DUPLICATE));
                    self.bytecode.push_const(success_tag as i32);
                    self.bytecode.push(Byte::new(Instruction::EQ));
                    bb.emit_jump_to_hinted(
                        failure,
                        BbJumpKind::JumpIfFalse,
                        FuseHint::nofuse_value_under_jmp(),
                        self.bytecode.il_mut(),
                    );
                    self.bytecode.push_pop();
                    bb.emit_jump_to(end, BbJumpKind::Unconditional, self.bytecode.il_mut());
                    bb.bind_label(failure, self.bytecode.il_mut());
                    self.bytecode.push_pop();
                    self.bytecode.push_pop();
                    let mut rhs_bc = self.do_compile(rhs);
                    self.bytecode.append(&mut rhs_bc);
                    bb.emit_jump_to(end, BbJumpKind::Unconditional, self.bytecode.il_mut());
                    bb.bind_label(success, self.bytecode.il_mut());
                } else if niche_lhs {
                    self.bytecode.push(Byte::new(Instruction::DUPLICATE));
                    self.bytecode.push(Byte::new(Instruction::LogNot));
                    bb.emit_jump_to_hinted(
                        success,
                        BbJumpKind::JumpIfFalse,
                        FuseHint::nofuse_value_under_jmp(),
                        self.bytecode.il_mut(),
                    );
                    self.bytecode.push_pop();
                    let mut rhs_bc = self.do_compile(rhs);
                    self.bytecode.append(&mut rhs_bc);
                    bb.emit_jump_to(end, BbJumpKind::Unconditional, self.bytecode.il_mut());
                    bb.bind_label(success, self.bytecode.il_mut());
                } else {
                    bb.emit_jump_to(
                        success,
                        BbJumpKind::JumpIfMatch {
                            tag: success_tag,
                            arity: 1,
                        },
                        self.bytecode.il_mut(),
                    );
                    // Miss: discard failure, evaluate rhs, jump to end.
                    self.bytecode.push_pop();
                    let mut rhs_bc = self.do_compile(rhs);
                    self.bytecode.append(&mut rhs_bc);
                    bb.emit_jump_to(end, BbJumpKind::Unconditional, self.bytecode.il_mut());
                    bb.bind_label(success, self.bytecode.il_mut());
                }
                // Success: payload already on stack from JumpIfMatch.
                bb.bind_label(end, self.bytecode.il_mut());
            }
            Expression::OptionalAccess(receiver, field) => {
                // `opt?.field` → None if opt is None, else Some(opt.field).
                if self.expr_is_niche_option(receiver)
                    && self
                        .codegen_expr_ty(ast)
                        .is_some_and(|ty| self.niche_option_inner_ty(&ty).is_some())
                {
                    let mut recv_bc = self.do_compile(receiver);
                    self.bytecode.append(&mut recv_bc);

                    let mut bb = BlockBuilder::new();
                    let end = bb.fresh_label(self.bytecode.il_mut());
                    self.bytecode.push(Byte::new(Instruction::DUPLICATE));
                    self.bytecode.push(Byte::new(Instruction::LogNot));
                    bb.emit_jump_to(
                        end,
                        BbJumpKind::JumpIfTrue,
                        self.bytecode.il_mut(),
                    );
                    // Some(payload) is represented by the payload itself.
                    let inner_ty = self
                        .codegen_expr_ty(receiver)
                        .and_then(|ty| crate::typechecking::ty::option_inner(&ty));
                    let is_record =
                        matches!(&inner_ty, Some(crate::typechecking::Ty::Record { .. }));
                    let is_class = inner_ty
                        .as_ref()
                        .is_some_and(|ty| self.checker.ty_is_class(ty));
                    let enum_field_index = if !is_record && !is_class {
                        inner_ty
                            .as_ref()
                            .and_then(extract_enum_name)
                            .and_then(|name| {
                                if self.checker.is_class(&name) {
                                    return None;
                                }
                                self.checker
                                    .field_index_for(&name, field)
                                    .map(|(_variant, idx)| idx)
                            })
                    } else {
                        None
                    };
                    if let Some(field_index) = enum_field_index {
                        self.bytecode.push_load_field(field_index as u32);
                    } else if is_record || is_class {
                        let mut field_bc = CodeBuf::new();
                        self.emit_field_name(&mut field_bc, field);
                        self.bytecode.append(&mut field_bc);
                        self.bytecode.push_get_field();
                    } else {
                        self.bytecode.push_load_field(0);
                    }
                    bb.bind_label(end, self.bytecode.il_mut());
                    return bytecode;
                }

                let receiver_is_niche = self.expr_is_niche_option(receiver);
                let previous_force = self.force_heap_option;
                if receiver_is_niche {
                    self.force_heap_option = true;
                }
                let mut recv_bc = self.do_compile(receiver);
                self.force_heap_option = previous_force;
                self.bytecode.append(&mut recv_bc);
                if receiver_is_niche && !Self::is_option_construct(receiver) {
                    self.bytecode
                        .push(Byte::new(Instruction::OptionNicheToHeap));
                }

                let mut bb = BlockBuilder::new();
                let success = bb.fresh_label(self.bytecode.il_mut());
                let end = bb.fresh_label(self.bytecode.il_mut());
                bb.emit_jump_to(
                    success,
                    BbJumpKind::JumpIfMatch {
                        tag: 1, // Some
                        arity: 1,
                    },
                    self.bytecode.il_mut(),
                );
                // Miss: None stays on stack; skip field access.
                bb.emit_jump_to(end, BbJumpKind::Unconditional, self.bytecode.il_mut());
                bb.bind_label(success, self.bytecode.il_mut());

                // Payload (inner of Some) on stack — read `.field` then re-wrap Some.
                use crate::typechecking::ty::{is_option_ty, option_inner};
                let inner_ty = self.codegen_expr_ty(receiver).and_then(|t| {
                    if is_option_ty(&t) {
                        option_inner(&t)
                    } else {
                        None
                    }
                });
                let is_record = matches!(&inner_ty, Some(crate::typechecking::Ty::Record { .. }));
                let is_class = inner_ty
                    .as_ref()
                    .is_some_and(|ty| self.checker.ty_is_class(ty));
                let enum_field_index = if !is_record && !is_class {
                    inner_ty
                        .as_ref()
                        .and_then(extract_enum_name)
                        .and_then(|name| {
                            if self.checker.is_class(&name) {
                                return None;
                            }
                            self.checker
                                .field_index_for(&name, field)
                                .map(|(_variant, idx)| idx)
                        })
                } else {
                    None
                };
                if let Some(field_index) = enum_field_index {
                    self.bytecode.push_load_field(field_index as u32);
                } else if is_record || is_class {
                    if let Some(&slot) = self.field_key_slots.get(*field) {
                        self.bytecode.push_load(slot);
                    } else {
                        let idx = self.intern_string(field);
                        self.bytecode.push_string(idx);
                    }
                    self.bytecode.push_get_field();
                } else {
                    self.bytecode.push_load_field(0);
                }
                Self::emit_ok_or_some_wrap(&mut self.bytecode, true);
                bb.bind_label(end, self.bytecode.il_mut());
            }
            Expression::TypeApp { args, .. } => {
                // Type-position only — consume child IDs, emit no bytes.
                for arg in args {
                    let _ = self.do_compile(arg);
                }
            }
            Expression::Spread(_) => {
                // Call sites flatten spread before emission; this arm keeps
                // ID alignment if a spread node is reached defensively.
            }

            _expr => {
                let mut message = Message::error(
                    ErrorCode::UnknownExpression,
                    "Unknown expression".to_string(),
                    span.into_range(),
                );
                message.push(DiagLabel::new(
                    "Unable to compile expression".to_string(),
                    span.into_range(),
                ));
                self.messages.push(message);
            }
        }

        bytecode
    }

    /// [`compile`] calls [`finalize_bytecode`] afterwards for unit tests.
    fn compile_unfused<'compiler>(
        &mut self,
        module: &str,
        ast: &mut (SimpleSpan, Box<Expression<'compiler>>),
        prepared: bool,
    ) {
        let ns = self.namespace.clone();
        self.namespace = module.to_string();

        self.emit_idx = 0;
        self.temp_counter = 0;
        self.expr_depth = 0;
        self.codegen_depth = 0;
        self.const_env_stack.clear();
        self.const_env_stack.push(HashMap::new());
        self.static_const_values.clear();
        self.current_function_qualified = None;
        self.current_function_table_key = None;
        self.force_heap_option = false;
        self.force_niche_option = false;
        self.compiling_pair_mode = false;
        self.pair_value_context = false;
        // Peel/unroll must not see other modules' bodies (label/CFG mix-up).
        self.fn_bytecode_spans.clear();
        if self.bytecode.len() <= PROLOGUE_BYTECODE_LEN {
            self.fn_inline_spans.clear();
            self.fn_defining_module.clear();
            self.fn_debug_locals.clear();
        }
        // `use` aliases are per-module; leftovers from a prior
        // `compile_module` would otherwise redirect bare names.
        self.aliases.clear();
        self.loop_stack.clear();
        self.loop_bbs.clear();
        // Constant pool is shared across multi-file `compile_module`
        // calls. `JumpIfMatch` (and pool-backed `CONST`) store indices
        // into this vec; clearing between modules orphans earlier
        // instructions so the worker VM panics in
        // `Byte::jump_if_match_target` (e.g. index 2, len 1) when a
        // dependency uses `?` / match. Only reset on a fresh compile
        // (still just the CALL/JMP/HALT prologue).
        if self.bytecode.len() <= PROLOGUE_BYTECODE_LEN {
            self.constants.clear();
            self.strings.clear();
            self.string_indices.clear();
        }
        self.mono_offsets.clear();
        self.mono_codegen_var_types.clear();
        self.test_cases.clear();
        self.user_main_defined = false;
        if !prepared {
            if !self.include_tests {
                crate::strip_tests::strip_test_declarations(ast);
            }
            // Expand `derive` / `ffi` then check (see `expand_and_check`).
            self.expand_and_check(module, ast);
        } else {
            self.checker.set_current_module(module);
            // Check already ran via `parse_expand_check` / `typecheck_module`.
            self.typed_sidecar = self.checker.typed_sidecar();
        }
        {
            let mut recount = crate::typechecking::id::IdTable::new();
            crate::typechecking::id::pre_walk(ast, &mut recount);
            let checked = self.checker.id_table().len();
            if checked != 0 && recount.len() != checked {
                self.messages.push(Message::error(
                    ErrorCode::CodegenError,
                    format!(
                        "emit NodeId table length {checked} does not match pre-walk {}",
                        recount.len()
                    ),
                    ast.0.into_range(),
                ));
            }
        }
        // Recursion depth / `#[max_depth]` — independent of auto-par.
        let stack_bound = crate::typechecking::analyze_stack_bounds(ast);
        self.messages.extend(stack_bound.messages);
        self.operand_stack_slots = stack_bound.operand_slots_needed;
        self.recursive_pure = if auto_par_enabled() {
            crate::typechecking::analyze_recursive_pure(ast)
        } else {
            HashSet::new()
        };
        self.pure_fns = crate::typechecking::analyze_pure_fns(ast);
        if auto_par_enabled() {
            // IPA sites on any pure function (self-recursion or helper arms).
            let pure = &self.pure_fns;
            self.par_shapes = crate::typechecking::analyze_par_fork_sites(ast, pure);
            self.par_spec_args =
                crate::typechecking::collect_par_specialization_args(ast, &self.par_shapes);
            self.loop_par_sites = crate::typechecking::analyze_loop_par_sites(ast, &self.pure_fns);
        } else {
            self.par_shapes.clear();
            self.par_spec_args.clear();
            self.loop_par_sites = crate::typechecking::LoopParSites::new();
        }
        self.emit_builtin_dict_thunks();
        self.emit_vec_method_thunks();
        self.emit_stream_method_thunks();
        // Builtin dictionary thunks are emitted immediately after the
        // prologue and before user code. Keep `program_start_offset`
        // pointing at the first user byte so `extern` prologue JMPs
        // don't fall into a Num/Ord/Eq/Show thunk body.
        self.program_start_offset = self.bytecode.len() as u32;
        self.setup_entry_offset = self.program_start_offset;
        // Label the setup / top-level region so `dead_block` keeps it
        // after prologue HALT / prelude RETURN (reachability is
        // label-based until entry-aware DCE).
        self.bytecode.bind_fresh_entry();
        self.mono_plan = crate::monomorphize::run_monomorphize_pass(module, ast, &self.checker);
        for hit in &self.mono_plan.cap_hits {
            let kind = if hit.per_fn {
                "per-function"
            } else {
                "total"
            };
            self.messages.push(Message::warn(
                ErrorCode::MonomorphizeCap,
                format!(
                    "monomorphization {kind} cap hit for `{}`; using shared generic body",
                    hit.fn_name
                ),
                hit.call_span.start..hit.call_span.end,
            ));
        }

        let mut program = self.do_compile(ast);
        self.namespace = ns.to_string();

        self.messages.extend(self.checker.take_messages());

        self.bytecode.append(&mut program);
        self.pad_debug_locs();
    }

    /// Register `type_id → drop PC` via internal `gc_register_finalizer`.
    ///
    /// Emitted on the main buffer (so drop labels stay in-namespace) then
    /// moved to the pre-`main` prologue.
    fn emit_finalizer_registry(&mut self, insert_at: usize) -> Option<usize> {
        let native_id = self.native_id("gc_register_finalizer")?;
        let owners: Vec<String> = self.checker.classes_with_drop().cloned().collect();
        if owners.is_empty() {
            return None;
        }
        let raw_start = self.bytecode.il().raw_len();
        let code_start = self.bytecode.len();
        for owner in owners {
            let fqn = format!("{owner}::drop");
            let Some(label) = self.fn_entry_labels.get(&fqn).copied() else {
                continue;
            };
            let type_id = self.checker.class_type_id(&owner);
            self.bytecode
                .push(Byte::new(Instruction::CONST).with_value_u32(native_id as u32));
            self.bytecode
                .push(Byte::new(Instruction::CONST).with_value_u32(type_id));
            self.bytecode
                .emit_entry(crate::il::EntryKind::CodePtr, 0, label);
            self.bytecode.push_make_tuple(2);
            self.bytecode.push_host_invoke(2);
            self.bytecode.push_pop();
        }
        let n = self.bytecode.len().saturating_sub(code_start);
        if n == 0 {
            return None;
        }
        self.bytecode
            .move_raw_suffix_to_code_pos(raw_start, insert_at);
        Some(n)
    }

    /// Lower stack IL to VM bytecode (fusion select + label resolution).
    ///
    /// Called once after multi-file linking by the pipeline, or at the end
    /// of single-file [`compile`] so unit tests observe fused output.
    pub fn finalize_bytecode(&mut self) {
        let _ = self.finalize_bytecode_inner(false);
    }

    /// Retain post-opt pre-fuse IL on the next [`Self::finalize_bytecode`].
    pub(crate) fn set_retain_cursor_il(&mut self, retain: bool) {
        self.retain_cursor_il = retain;
        if !retain {
            self.cursor_il = None;
        }
    }

    pub(crate) fn take_cursor_il(&mut self) -> Option<crate::il::tell::CursorIlSnap> {
        self.cursor_il.take()
    }

    /// Like [`finalize_bytecode`], but also returns a pre-opt IL snapshot for dissect.
    #[cfg(any(test, feature = "dissect"))]
    pub fn finalize_bytecode_capturing_il(&mut self) -> crate::dissect::IlSnapshot {
        self.finalize_bytecode_inner(true)
            .expect("capture_il requested")
    }

    fn finalize_bytecode_inner(&mut self, capture_il: bool) -> FinalizeIlOut {
        // Splice static initializers + `extern` setup into the IL before lower.
        // Order: user static inits, then FFI dlopen/declare, then JMP → main.
        let setup_pos = self.program_start_offset as usize;
        let mut init_len = 0usize;

        if !self.static_init.is_empty() {
            self.setup_entry_offset = setup_pos as u32;
            let inits = std::mem::take(&mut self.static_init);
            let n = inits.len();
            self.bytecode.splice_buf_at(setup_pos + init_len, inits);
            self.bytecode
                .bump_absolute_entry_targets(setup_pos + init_len, n);
            self.bytecode.bump_func_spans(setup_pos + init_len, n);
            init_len += n;
        }

        if !self.ffi_init.is_empty() {
            self.setup_entry_offset = setup_pos as u32;
            let ffi = std::mem::take(&mut self.ffi_init);
            let n = ffi.len();
            self.bytecode.splice_buf_at(setup_pos + init_len, ffi);
            self.bytecode
                .bump_absolute_entry_targets(setup_pos + init_len, n);
            self.bytecode.bump_func_spans(setup_pos + init_len, n);
            init_len += n;
        }

        if let Some(n) = self.emit_finalizer_registry(setup_pos + init_len) {
            self.setup_entry_offset = setup_pos as u32;
            self.bytecode
                .bump_absolute_entry_targets(setup_pos + init_len, n);
            self.bytecode.bump_func_spans(setup_pos + init_len, n);
            init_len += n;
        }

        let static_init_region = if init_len > 0 {
            self.bytecode.entry_label_at(setup_pos);
            self.program_start_offset += init_len as u32;
            for offset in self.functions.values_mut() {
                if *offset >= setup_pos {
                    *offset += init_len;
                }
            }
            for (_, offset) in self.test_cases.iter_mut() {
                if (*offset as usize) >= setup_pos {
                    *offset += init_len as u32;
                }
            }
            for offset in self.mono_offsets.values_mut() {
                if *offset >= setup_pos {
                    *offset += init_len;
                }
            }
            Some((setup_pos, init_len))
        } else {
            None
        };

        // After setup region, insert JMP → main.
        let main_off = self.functions.get("main").copied();
        if let (Some((pos, init_len)), Some(main_off)) = (static_init_region, main_off) {
            let jmp_pos = pos + init_len;
            let target_label = self.bytecode.entry_label_at(main_off);
            self.bytecode.insert_jump_at(jmp_pos, target_label);
            self.bytecode.bump_absolute_entry_targets(jmp_pos, 1);
            self.bytecode.bump_func_spans(jmp_pos, 1);
            for offset in self.functions.values_mut() {
                if *offset >= jmp_pos {
                    *offset += 1;
                }
            }
            for (_, offset) in self.test_cases.iter_mut() {
                if (*offset as usize) >= jmp_pos {
                    *offset += 1;
                }
            }
            for offset in self.mono_offsets.values_mut() {
                if *offset >= jmp_pos {
                    *offset += 1;
                }
            }
            if (self.program_start_offset as usize) > jmp_pos {
                self.program_start_offset += 1;
            }
        }

        // Drop unused function bodies (eager builtin thunks, unreferenced user
        // fns) before IL opts / lower. Skip when there is no `main` so snippet
        // / unit-test compiles keep their bodies.
        if self.functions.contains_key("main") {
            let roots = vec!["main".to_string()];
            let (_dropped, shrinks) = crate::il::prune_unused_functions(
                &mut self.bytecode,
                crate::il::TreeshakeInput {
                    functions: &mut self.functions,
                    fn_entry_labels: &mut self.fn_entry_labels,
                    fn_debug_locals: &mut self.fn_debug_locals,
                    test_cases: &mut self.test_cases,
                    root_names: &roots,
                    include_tests: self.include_tests,
                    preserve_emit_start: Some(self.setup_entry_offset as usize),
                },
            );
            for (threshold, delta) in shrinks {
                for pc in self.mono_offsets.values_mut() {
                    if *pc >= threshold {
                        *pc -= delta;
                    }
                }
                if (self.program_start_offset as usize) >= threshold {
                    self.program_start_offset -= delta as u32;
                }
                if (self.setup_entry_offset as usize) >= threshold {
                    self.setup_entry_offset -= delta as u32;
                }
            }
            let live_pcs: std::collections::HashSet<usize> =
                self.functions.values().copied().collect();
            self.mono_offsets.retain(|_, pc| live_pcs.contains(pc));
        }

        #[cfg(any(test, feature = "dissect"))]
        let il_snapshot = if capture_il {
            Some(crate::dissect::IlSnapshot::new(
                self.bytecode.ops().to_vec(),
                self.bytecode.funcs().to_vec(),
            ))
        } else {
            None
        };

        let label_callees = self
            .fn_entry_labels
            .iter()
            .map(|(name, label)| (label.0, name.clone()))
            .collect();
        let offset_callees = self
            .functions
            .iter()
            .map(|(name, off)| (*off as u32, name.clone()))
            .collect();
        self.opt_options.pure_call_ctx = Some(crate::il::PureCallCtx {
            pure_fns: self.pure_fns.clone(),
            label_callees,
            offset_callees,
        });
        self.bytecode.set_opt_options(self.opt_options.clone());
        let mut lowered = if self.retain_cursor_il {
            self.bytecode.lower_in_place_capturing(&mut self.constants)
        } else {
            self.bytecode.lower_in_place(&mut self.constants)
        };
        let cursor_ops = lowered.pre_fuse_ops.take();
        let map = |t: usize| -> usize {
            if let Some(&p) = lowered.pre_to_post.get(&t) {
                return p;
            }
            let mut best = lowered.code_len;
            for (&pre, &post) in &lowered.pre_to_post {
                if pre >= t && post < best {
                    best = post;
                }
            }
            best
        };
        // Prefer entry labels: IL opts (dead_block) shift emitting indices
        // before fuse, so raw `functions` / `test_cases` PCs are stale.
        // Per-function chunk remaps avoid collisions in the cumulative map.
        let func_label_maps = &lowered.func_label_maps;
        let funcs = self.bytecode.funcs();
        let flat_label = |func_idx: usize, emit_id: u32| -> u32 {
            func_label_maps
                .get(func_idx)
                .and_then(|m| m.get(&emit_id).copied())
                .unwrap_or(emit_id)
        };
        let func_idx_for_pre = |pre: usize| -> Option<usize> {
            funcs
                .iter()
                .position(|f| pre >= f.code_start && pre < f.code_end)
        };
        let func_idx_for_name = |name: &str| -> Option<usize> {
            funcs.iter().position(|f| f.name == name)
        };
        let resolve_fn_label_pc = |name: &str, emit_id: u32| -> Option<usize> {
            let idx = func_idx_for_name(name)?;
            let flat_id = flat_label(idx, emit_id);
            lowered.label_pcs.get(&flat_id).copied()
        };
        let resolve_entry = |pre: usize| -> usize {
            if let Some(label) = self.bytecode.entry_label_for_offset(pre) {
                if let Some(idx) = func_idx_for_pre(pre) {
                    let flat_id = flat_label(idx, label.0);
                    if let Some(pc) = lowered.label_pcs.get(&flat_id).copied() {
                        return pc;
                    }
                }
                let global_flat = lowered
                    .label_remap
                    .get(&label.0)
                    .copied()
                    .unwrap_or(label.0);
                if let Some(pc) = lowered.label_pcs.get(&global_flat).copied() {
                    return pc;
                }
                if let Some(&pc) = lowered.label_pcs.get(&label.0) {
                    return pc;
                }
            }
            map(pre)
        };
        for (name, offset) in self.functions.iter_mut() {
            if let Some(label) = self.fn_entry_labels.get(name) {
                if let Some(pc) = resolve_fn_label_pc(name, label.0) {
                    *offset = pc;
                    continue;
                }
            }
            *offset = resolve_entry(*offset);
        }
        for (_, offset) in self.test_cases.iter_mut() {
            *offset = resolve_entry(*offset as usize) as u32;
        }
        for offset in self.mono_offsets.values_mut() {
            *offset = resolve_entry(*offset);
        }
        self.program_start_offset = resolve_entry(self.program_start_offset as usize) as u32;
        self.setup_entry_offset = resolve_entry(self.setup_entry_offset as usize) as u32;

        self.debug_locs = lowered.debug_locs;

        self.operand_stack_slots = self
            .operand_stack_slots
            .min(crate::typechecking::MAX_OPERAND_STACK_SLOTS);

        debug_assert_eq!(
            self.debug_locs.len(),
            self.bytecode.len(),
            "debug_locs / bytecode length mismatch after finalize"
        );

        if self.retain_cursor_il {
            self.cursor_il = Some(crate::il::tell::CursorIlSnap {
                ops: cursor_ops.unwrap_or_default(),
                pre_to_post: lowered.pre_to_post.clone(),
            });
        }

        #[cfg(any(test, feature = "dissect"))]
        return il_snapshot;
        #[cfg(not(any(test, feature = "dissect")))]
        {
            debug_assert!(!capture_il);
            ()
        }
    }

    /// Post-lower function symbols sorted by entry PC (for dissect / debug).
    #[cfg(any(test, feature = "dissect"))]
    pub fn function_symbols(&self) -> Vec<crate::dissect::FnSym> {
        let mut syms: Vec<_> = self
            .functions
            .iter()
            .map(|(name, &pc)| {
                let mut locals: Vec<(String, u32)> = self
                    .fn_debug_locals
                    .get(name)
                    .map(|m| m.iter().map(|(n, &s)| (n.clone(), s)).collect())
                    .unwrap_or_default();
                locals.sort_by_key(|(_, s)| *s);
                crate::dissect::FnSym {
                    name: name.clone(),
                    entry_pc: pc as u32,
                    locals,
                }
            })
            .collect();
        syms.sort_by_key(|s| s.entry_pc);
        syms
    }

    pub fn compile<'compiler>(
        &mut self,
        module: &str,
        ast: &mut (SimpleSpan, Box<Expression<'compiler>>),
    ) -> Vec<Byte> {
        self.compile_unfused(module, ast, false);
        self.finalize_bytecode();
        self.bytecode.clone_bytes()
    }

    /// Append this module's IL to the shared buffer (multi-file pipeline).
    ///
    /// Returns an empty vec for API compatibility; the pipeline should call
    /// [`finalize_bytecode`] once on the linked compiler buffer.
    pub fn compile_module<'compiler>(
        &mut self,
        module: &str,
        ast: &mut (SimpleSpan, Box<Expression<'compiler>>),
    ) -> Vec<Byte> {
        self.compile_module_inner(module, ast, false)
    }

    /// Like [`Self::compile_module`], but skip strip / expand / check.
    ///
    /// Used after [`Self::parse_expand_check`] / pipeline `parse_expand_check_file`.
    pub fn compile_prepared_module<'compiler>(
        &mut self,
        module: &str,
        ast: &mut (SimpleSpan, Box<Expression<'compiler>>),
    ) -> Vec<Byte> {
        self.compile_module_inner(module, ast, true)
    }

    fn compile_module_inner<'compiler>(
        &mut self,
        module: &str,
        ast: &mut (SimpleSpan, Box<Expression<'compiler>>),
        prepared: bool,
    ) -> Vec<Byte> {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.compile_unfused(module, ast, prepared);
        }));
        if let Err(payload) = result
            && payload
                .downcast_ref::<super::CodegenRecursionLimitExceeded>()
                .is_none()
        {
            // Only swallow our own recursion-limit signal (message already
            // recorded in `do_compile`) — any other panic is a real bug.
            std::panic::resume_unwind(payload);
        }
        Vec::new()
    }

    /// Final lowered bytecode after [`finalize_bytecode`].
    pub fn bytecode_slice(&self) -> &[Byte] {
        self.bytecode.as_slice()
    }

    pub fn bytecode_vec(&self) -> Vec<Byte> {
        self.bytecode.clone_bytes()
    }
}

#[cfg(test)]
#[path = "lib.tests.rs"]
mod tests;
