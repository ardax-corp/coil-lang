//! The side tables direct MIR lowering reads ([`crate::mir::HirMirInput`]),
//! planned over the HIR: the entries the stack emission of a body calls,
//! the strings it interns, worked out from the plan, the function and
//! label tables and the string interner, never from the ops emitted.
//!
//! The walk visits what emission evaluates (an operand emission skips,
//! such as `typeof`'s, is skipped here too) and repeats emission's side
//! effects through the same helpers (interning, method clones), so it
//! gives the same answer whether or not the body was emitted first.

use super::*;

/// What [`Compiler::hir_mir_tables`] plans for one body.
#[derive(Default)]
pub(super) struct HirMirTables {
    /// String table index of each `string` literal (and `const` string,
    /// `typeof` name, string index panic message), by expression.
    pub(super) strings: HashMap<u32, u32>,
    /// String table index of the format string of each `format` call
    /// whose arguments all print as words (`%v` as `%s`), and of string
    /// `+` (`"%s%s"`), by expression.
    pub(super) formats: HashMap<u32, u32>,
    /// Per argument of such a `format` call (after the format string): the
    /// raw `show` entry a `%v` argument goes through, `None` for a word.
    pub(super) format_shows: HashMap<u32, Vec<Option<crate::il::Label>>>,
    /// `matrix(data)` calls, whose value is the data itself.
    pub(super) matrix_wraps: HashSet<u32>,
    /// Entry of each plain one-word-ABI `CALL` (no dictionaries), by call
    /// site: calls, and user operators on a raw instance method.
    pub(super) calls: HashMap<u32, crate::mir::HirCallSite>,
    /// User operators that call their instance method with boxed operands.
    pub(super) op_calls: HashMap<u32, crate::mir::HirOpCall>,
    /// User operators emitted as a raw opcode on the operands.
    pub(super) op_prims: HashSet<u32>,
    /// Calls through a function value emitted as a plain `CallIndirect`.
    pub(super) value_calls: HashSet<u32>,
    /// Auto-par call sites sent to a fork-join worker, by call: the direct
    /// MIR lowering's calls.
    pub(super) par_forks: HashMap<u32, crate::mir::HirParFork>,
    /// Some value emission evaluates packs as a bare-class existential.
    pub(super) existential_packs: bool,
    /// Function values built with `MakeFn` / `MakePolyFn` (lambdas and
    /// named functions read as values), by expression.
    pub(super) fn_values: HashMap<u32, crate::mir::HirFnValue>,
    /// Entry of the user `into_iter` a counted `for` loop calls, by loop.
    pub(super) into_iters: HashMap<u32, crate::il::Label>,
    /// The `into_iter` and `next` entries a `for` loop over a user
    /// iterator calls, by loop.
    pub(super) iter_loops: HashMap<u32, crate::mir::HirIterLoop>,
}

/// Which children of an expression emission evaluates.
enum Kids {
    All,
    None,
    /// All but the first (a `format` call's literal format string).
    Rest,
}

impl Compiler {
    /// The side tables of `hir`, planned as [`Self::hir_value`] emits it.
    pub(super) fn hir_mir_tables(&mut self, hir: &HirBody, emit: &HirEmit) -> HirMirTables {
        let mut tables = HirMirTables::default();
        if let Some(root) = hir.root {
            self.hir_mir_walk(hir, emit, root, &mut tables);
        }
        tables
    }

    fn hir_mir_walk(&mut self, hir: &HirBody, emit: &HirEmit, id: HirId, t: &mut HirMirTables) {
        // A parallel loop outlined into its fork-join function: its code is
        // not this body's.
        if emit.par_calls.contains_key(&id.0) {
            return;
        }
        let kids = self.hir_mir_note(hir, emit, id, t);
        let all = lower::children(hir, id);
        let kids = match kids {
            Kids::All => &all[..],
            Kids::None => &[],
            Kids::Rest => all.get(1..).unwrap_or_default(),
        };
        for &k in kids {
            self.hir_mir_walk(hir, emit, k, t);
        }
    }

    /// Plan `id`'s own entries, as [`Self::hir_value_unpacked`]'s arm for
    /// it records them.
    fn hir_mir_note(&mut self, hir: &HirBody, emit: &HirEmit, id: HirId, t: &mut HirMirTables) -> Kids {
        // As `hir_value` packs a value after emitting it.
        t.existential_packs |= self.hir_existential_pack(hir, id).is_some();
        let e = hir.expr(id);
        match &e.kind {
            HirKind::Lit(Lit::Str(raw))
                if Self::hir_ty(hir, id).and_then(lower::primitive) != Some(crate::typechecking::ty::BYTE)
                    && !Self::hir_ty(hir, id).is_some_and(lower::is_byte_array) =>
            {
                let idx = self.intern_string(unescape_coil_string(raw));
                t.strings.insert(id.0, idx);
            }
            HirKind::Global { .. } if emit.plan.statics.contains_key(&id.0) => {}
            HirKind::Lambda { .. } => {
                // Its body lowers in its own frame, at the entry label
                // reserved when it was planned.
                if let Some(lam) = emit.plan.lambdas.get(&id.0) {
                    let target = lam.label;
                    let operand = make_fn_operand(lam.body.captures.len() as u32, 0, lam.body.params.len() as u32, false);
                    let captures = lam.body.captures.iter().map(|&(outer, _)| outer).collect();
                    t.fn_values.insert(id.0, crate::mir::HirFnValue { kind: crate::mir::MirAllocKind::Fn { target, operand }, captures });
                }
            }
            // A generic function's `MakePolyFn`, when it takes no
            // dictionaries.
            HirKind::Global { .. } if let Some(name) = emit.plan.polyfns.get(&id.0) => {
                if self.checker.dict_arity_for(name) == 0
                    && let Some(target) = self.functions.get(name).and_then(|&entry| self.bytecode.entry_label_for_offset(entry))
                {
                    t.fn_values.insert(id.0, crate::mir::HirFnValue { kind: crate::mir::MirAllocKind::PolyFn { target }, captures: Vec::new() });
                }
            }
            HirKind::Global { .. } if let Some(&(entry, arity, rest)) = emit.plan.fn_refs.get(&id.0) => {
                if let Some(target) = self.bytecode.entry_label_for_offset(entry) {
                    let operand = make_fn_operand(0, 0, arity, rest);
                    t.fn_values.insert(id.0, crate::mir::HirFnValue { kind: crate::mir::MirAllocKind::Fn { target, operand }, captures: Vec::new() });
                }
            }
            HirKind::Global { .. } => {
                if let Some(crate::const_fold::ConstValue::Str(text)) = emit.plan.consts.get(&id.0) {
                    let idx = self.intern_string(text);
                    t.strings.insert(id.0, idx);
                }
            }
            HirKind::Bin { op: BinOp::StrConcat, .. } => {
                let idx = self.intern_string("%s%s");
                t.formats.insert(id.0, idx);
            }
            // The operand's type name; the operand is not evaluated.
            HirKind::Builtin { op: Builtin::TypeOf, args } => {
                if let Some(name) = self.hir_typeof(hir, args[0]) {
                    let idx = self.intern_string(&name);
                    t.strings.insert(id.0, idx);
                }
                return Kids::None;
            }
            // `while false { .. }` is dropped.
            HirKind::Loop { body }
                if lower::while_shape(hir, *body).is_some_and(|(cond, _)| matches!(hir.expr(cond).kind, HirKind::Lit(Lit::Bool(false)))) =>
            {
                return Kids::None;
            }
            HirKind::Bin {
                op: BinOp::Overloaded(_),
                lhs,
                rhs,
            } => match emit.plan.ops.get(&id.0) {
                Some(HirOp::Prim(_)) => {
                    t.op_prims.insert(id.0);
                }
                Some(HirOp::Call { lookup, fqn, class, method }) => {
                    let (lookup, fqn) = (lookup.clone(), fqn.clone());
                    self.hir_mir_op_call(hir, id, &[*lhs, *rhs], (class, method, fqn), lookup, t);
                }
                _ => {}
            },
            // A negated literal is its constant.
            HirKind::Un { op: UnOp::Neg, operand } if matches!(hir.expr(*operand).kind, HirKind::Lit(Lit::Int(_) | Lit::Float(_))) => {}
            HirKind::Un { operand, .. }
                if let Some(HirOp::Call { lookup, fqn, class, method }) = emit.plan.ops.get(&id.0) =>
            {
                let (lookup, fqn) = (lookup.clone(), fqn.clone());
                self.hir_mir_op_call(hir, id, &[*operand], (class, method, fqn), lookup, t);
            }
            HirKind::Call { callee: Callee::Value(f), .. } => {
                if !lower::polyfn_callee(hir, &self.checker, *f) {
                    t.value_calls.insert(id.0);
                }
            }
            // A fixed `len` of a local, literal or literal array evaluates
            // nothing.
            HirKind::Call { args, .. } if let Some(Some(_)) = emit.plan.lens.get(&id.0) => {
                if matches!(hir.expr(args[0]).kind, HirKind::Local(_) | HirKind::Lit(_) | HirKind::Make { .. }) {
                    return Kids::None;
                }
            }
            HirKind::Call { .. } if emit.plan.calls.get(&id.0).is_some_and(|c| matches!(c.builtin, Some(HirBuiltin::Matrix))) => {
                t.matrix_wraps.insert(id.0);
            }
            HirKind::Call { args, .. }
                if !emit.plan.lens.contains_key(&id.0)
                    && emit.plan.calls.get(&id.0).is_some_and(|c| matches!(c.builtin, Some(HirBuiltin::Format))) =>
            {
                self.hir_mir_format(hir, id, args, t);
                return Kids::Rest;
            }
            HirKind::Call { args, .. } if !emit.plan.lens.contains_key(&id.0) && emit.plan.calls.contains_key(&id.0) => {
                self.hir_mir_call(hir, emit, id, args, t);
            }
            HirKind::ForIn {
                pat,
                iterable,
                kind: Some(kind),
                ..
            } => self.hir_mir_for_in(hir, id, pat, *iterable, kind, t),
            HirKind::Index {
                base,
                index,
                kind: IndexKind::String,
            } if Self::hir_product_index(hir, emit, *base, *index).is_none() => {
                let idx = self.intern_string("index out of bounds");
                t.strings.insert(id.0, idx);
            }
            _ => {}
        }
        Kids::All
    }

    /// The user `into_iter` / `next` entries `for` loop `id` calls, as
    /// [`Self::hir_for_in`] emits them.
    fn hir_mir_for_in(&mut self, hir: &HirBody, id: HirId, pat: &HirPat, iterable: HirId, kind: &ForInKind, t: &mut HirMirTables) {
        let ForInKind::Custom { into_iter_fqn, next_fqn, counted } = kind else {
            return;
        };
        // A counted result runs that result's loop; a range bound read
        // from the iterable needs no call.
        if let Some(counted) = counted {
            use crate::typechecking::infer::ForInCounted as C;
            if matches!(counted, C::Range { .. }) && lower::range_bounds(hir, iterable).is_some() {
                return;
            }
            self.raw_instance_call = false;
            if let Some(target) = self.named_entry_target(into_iter_fqn, true) {
                t.into_iters.insert(id.0, target);
            }
            return;
        }
        let (Some(next_fqn), &HirPat::Bind(local)) = (next_fqn, pat) else {
            return;
        };
        let item = hir.local(local).ty.clone();
        let niche = item
            .as_ref()
            .is_some_and(|ty| crate::typechecking::value_layout::niche_heap_only(&self.checker, ty));
        // Pinned as emission pins it, so `next`'s `CALL` agrees.
        if let Some(item) = &item {
            let opt = crate::typechecking::ty::option_ty(item.clone());
            let pair = crate::typechecking::return_layout::two_word_return_enum(&self.checker, &opt);
            self.pin_two_word_return_kind(next_fqn, pair);
        }
        let two_slot = self.two_word_return_kind(next_fqn).is_some();
        self.raw_instance_call = false;
        let into_iter = self.named_entry_target(into_iter_fqn, true);
        self.raw_instance_call = false;
        let next = self.named_entry_target(next_fqn, true);
        if let (Some(into_iter), Some(next), Some(iter_ty)) = (into_iter, next, self.fn_return_ty(into_iter_fqn)) {
            let shape = if niche {
                crate::mir::HirIterNext::Niche
            } else if two_slot {
                crate::mir::HirIterNext::Pair
            } else {
                let some = self.checker.tag_for(common::BUILTIN_OPTION_ENUM, "Some").unwrap_or(1);
                crate::mir::HirIterNext::Boxed { some }
            };
            t.iter_loops.insert(id.0, crate::mir::HirIterLoop { into_iter, next, iter_ty, shape });
        }
    }

    /// User operator `id` on `operands` calling its instance method, as
    /// `hir_value_unpacked`'s `HirOp::Call` arms: a raw method takes the
    /// operands' own words (a plain call), any other a binary operator's
    /// boxed operands (an operator call).
    fn hir_mir_op_call(
        &mut self,
        hir: &HirBody,
        id: HirId,
        operands: &[HirId],
        (class, method, fqn): (&str, &str, String),
        lookup: Ty,
        t: &mut HirMirTables,
    ) {
        let fqn = self.instance_clone(&fqn, std::slice::from_ref(&lookup)).unwrap_or(fqn);
        let raw = self.raw_instance_methods.contains(&fqn);
        let span = hir.expr(id).span;
        let dict = self.hir_mir_quiet(|this| this.emit_call_instance_dict(&mut CodeBuf::new(), (class, method, &fqn), std::slice::from_ref(&lookup), span.0..span.1));
        // A two-word result comes back as two words (or boxed after the call).
        let one_word = self.two_word_return_kind(&fqn).is_none();
        self.raw_instance_call = raw;
        let Some(target) = self.named_entry_target(&fqn, false).filter(|_| one_word && !dict) else {
            return;
        };
        let arity = operands.len();
        let site = crate::mir::HirCallSite {
            target,
            pair_args: vec![false; arity],
            pair_ret: false,
            coro: false,
            boxed_ret: false,
        };
        if raw {
            // Each operand's MIR word is the boxed-layout word the stack passes.
            if operands.iter().all(|&o| Self::hir_ty(hir, o).is_some_and(|t| self.value_layout(t) == ValueLayout::Boxed)) {
                t.calls.insert(id.0, site);
            }
        } else if arity == 2
            && let Some(tag) = Self::ty_to_value_tag(&lookup)
        {
            let shift = matches!(class, "Shl" | "Shr");
            t.op_calls.insert(id.0, crate::mir::HirOpCall { site, tag: tag as u32, shift });
        }
    }

    /// Call `id` of a function or method, as `hir_value_unpacked`'s
    /// `HirKind::Call` arms past the builtins: its entry when it is one
    /// plain `CALL` (or `TailCall` / `MakeCoro`) of the arguments' own
    /// words with no dictionary.
    fn hir_mir_call(&mut self, hir: &HirBody, emit: &HirEmit, id: HirId, args: &[HirId], t: &mut HirMirTables) {
        let call = &emit.plan.calls[&id.0];
        if call.builtin.is_some() || call.key == format!("{}::push", common::BUILTIN_VEC_TYPE) {
            return;
        }
        let natural = Self::hir_call_rep(call);
        let span = hir.expr(id).span;
        let natural_args = |this: &Self, params: &[ValueLayout]| {
            args.iter().zip(params).all(|(&a, &p)| this.hir_natural(hir, emit, a) == Some(Rep::Word(p)))
        };
        if let Some(inst) = call.instance.as_ref().filter(|i| i.ground.is_some()) {
            let ground = inst.ground.as_ref().expect("ground call");
            let method = &ground.method;
            let key = self.instance_clone(&call.key, &inst.args).unwrap_or_else(|| call.key.clone());
            let dict = self.hir_mir_quiet(|this| this.emit_call_instance_dict(&mut CodeBuf::new(), (&inst.class, method, &key), &inst.args, span.0..span.1));
            let raw = self.raw_instance_methods.contains(&key);
            self.raw_instance_call = raw;
            let target = self.named_entry_target(&key, true);
            // Raw words (or nothing boxed: `T::default()`) and no
            // dictionary: direct MIR calls the same entry.
            if (raw || ground.boxed.iter().all(Option::is_none))
                && !dict
                && natural_args(self, &call.params)
                && let Some(target) = target
                && let Some(boxed_ret) = self.hir_ret_shape(hir, id, &natural)
            {
                t.calls.insert(
                    id.0,
                    crate::mir::HirCallSite {
                        target,
                        pair_args: vec![false; args.len()],
                        pair_ret: natural.words() == 2,
                        coro: false,
                        boxed_ret,
                    },
                );
            }
            return;
        }
        if call.method {
            let instance = call.instance.as_deref();
            let key = match instance.and_then(|i| i.recv_box.clone()) {
                Some(recv) if call.generic.is_none() => self.instance_clone(&call.key, &[recv]).unwrap_or_else(|| call.key.clone()),
                _ => call.key.clone(),
            };
            let raw = self.raw_instance_methods.contains(&key);
            let instance = instance.filter(|_| !raw);
            let boxed = (0..args.len()).any(|i| {
                call.generic.as_ref().is_some_and(|g| g.boxed[i].is_some()) || (i == 0 && instance.is_some_and(|inst| inst.recv_box.is_some()))
            });
            // A shared generic body's call is plain with no dictionary.
            let generic_plain = call.generic.as_deref().is_none_or(|g| self.hir_generic_plain(hir, emit, g, args, &call.params));
            let dict = call.generic.as_deref().is_some_and(|g| self.hir_mir_dicts(g) > 0)
                || instance.is_some_and(|inst| self.hir_mir_quiet(|this| this.emit_instance_dict(&mut CodeBuf::new(), &inst.class, &inst.args)));
            let coro = self.coroutine_fns.contains(&key);
            self.hir_request_method_clone(&key);
            self.raw_instance_call = raw;
            let target = self.named_entry_target(&key, true);
            if !dict
                && generic_plain
                && let Some(boxed_ret) = self.hir_ret_shape(hir, id, &natural)
                && !boxed
                && args.iter().zip(&call.params).all(|(&a, &p)| self.hir_arg_word(hir, emit, a, p))
                && let Some(target) = target
            {
                t.calls.insert(
                    id.0,
                    crate::mir::HirCallSite {
                        target,
                        pair_args: vec![false; args.len()],
                        pair_ret: natural.words() == 2,
                        coro,
                        boxed_ret,
                    },
                );
            }
            return;
        }
        if let Some(plan) = self.hir_par_call_plan(hir, &call.key, args) {
            // The worker (and a dynamic site's sequential function) as the
            // `CALL`s `hir_par_call` emits resolve them.
            let (worker, seq) = match plan {
                HirParCall::Spec { worker, .. } => (worker, None),
                HirParCall::Dynamic { cutoff, orig, worker } => (worker, Some((orig, cutoff))),
            };
            let label = |offset: usize| self.bytecode.entry_label_for_offset(offset);
            let seq = match seq {
                None => Some(None),
                Some((orig, cutoff)) => label(orig).map(|l| Some((l, cutoff))),
            };
            if let (Some(worker), Some(seq)) = (label(worker), seq) {
                self.hir_note_par_fork(hir, emit, id, worker, seq, &mut t.par_forks);
            }
            return;
        }
        let tail = emit.plan.tail_calls.contains(&id.0);
        let coro = !tail && self.coroutine_fns.contains(&call.key);
        self.raw_instance_call = false;
        let target = self.named_entry_target(&call.key, true);
        if call.generic.as_deref().is_none_or(|g| self.hir_generic_plain(hir, emit, g, args, &call.params) && self.hir_mir_dicts(g) == 0)
            && let Some(boxed_ret) = self.hir_ret_shape(hir, id, &natural)
            && let Some(target) = target
        {
            let pair_args = (0..args.len()).map(|i| Self::hir_arg_rep(call, i).words() == 2).collect();
            // A `TailCall`'s callee returns for this function, so in this
            // function's words, whatever the call's own rep.
            let pair_ret = if tail { matches!(emit.plan.ret, Rep::Pair(_)) } else { natural.words() == 2 };
            t.calls.insert(id.0, crate::mir::HirCallSite { target, pair_args, pair_ret, coro, boxed_ret });
        }
    }

    /// How many dictionaries [`Self::hir_push_dicts`] pushes for `generic`.
    /// Whether direct MIR passes argument `id` in `param` as HIR emission
    /// does: its own word in that layout, or a numeric range's two words
    /// boxed to its typed object (MIR's `box_pair_val`, as
    /// `emit_box_range_slots`).
    fn hir_arg_word(&self, hir: &HirBody, emit: &HirEmit, id: HirId, param: ValueLayout) -> bool {
        self.hir_natural(hir, emit, id) == Some(Rep::Word(param))
            || (param == ValueLayout::Boxed
                && Self::hir_ty(hir, id).is_some_and(|ty| {
                    let ty = apply_ty_prune(self.checker.subst(), ty);
                    crate::typechecking::return_layout::two_word_range_kind(&ty).is_some()
                }))
    }

    fn hir_mir_dicts(&mut self, generic: &HirGeneric) -> u32 {
        let forwarded = generic.forwarded.iter().filter(|&&i| self.lookup_slot(&format!("__dict{i}")).is_some()).count();
        let site = self.hir_mir_quiet(|this| this.emit_call_site_dicts(&mut CodeBuf::new(), &generic.lookup, &generic.arg_tys, Some(&generic.ret_ty)));
        (forwarded + site) as u32
    }

    /// `f(self)` with the diagnostics it reports dropped: the walk asks
    /// what emission decided, which reported them already.
    fn hir_mir_quiet<R>(&mut self, f: impl FnOnce(&mut Self) -> R) -> R {
        let messages = self.messages.len();
        let r = f(self);
        self.messages.truncate(messages);
        r
    }

    /// A `format` call whose `%v` arguments each go through one plain
    /// `show` entry: its format string (`%v` as `%s`) and those entries.
    fn hir_mir_format(&mut self, hir: &HirBody, id: HirId, args: &[HirId], t: &mut HirMirTables) {
        let HirKind::Lit(Lit::Str(fmt)) = &hir.expr(args[0]).kind else {
            return;
        };
        let specs = Self::format_consuming_specs(fmt);
        let fmt = Self::rewrite_format_v_to_s(fmt);
        let mut shows = Vec::with_capacity(args.len().saturating_sub(1));
        let mut shows_ok = true;
        for (i, &arg) in args.iter().enumerate().skip(1) {
            if specs.get(i - 1) == Some(&'v') {
                let show = self.hir_show_entry(hir, arg);
                shows_ok &= show.is_some();
                shows.push(show);
            } else {
                shows.push(None);
            }
        }
        if shows_ok {
            let idx = self.intern_string(unescape_coil_string(&fmt));
            t.formats.insert(id.0, idx);
            if specs.contains(&'v') {
                t.format_shows.insert(id.0, shows);
            }
        }
    }

    /// The raw `show` entry [`Self::hir_show`] calls for `arg` when that is
    /// one plain one-word `CALL` of the boxed-layout value and nothing else
    /// (no boxing, conversion or dictionary), as `emit_show_for_stack_value`
    /// picks it.
    pub(super) fn hir_show_entry(&mut self, hir: &HirBody, arg: HirId) -> Option<crate::il::Label> {
        if self.hir_bound_show(hir, arg).is_some() {
            return None;
        }
        let ty = apply_ty_prune(self.checker.subst(), Self::hir_ty(hir, arg)?);
        if matches!(ty, Ty::Tuple(_) | Ty::Record { .. }) || self.value_layout(&ty) != ValueLayout::Boxed {
            return None;
        }
        let lookup_ty = Self::show_lookup_ty_for_instance(&ty);
        let fqn = self.find_show_instance(&lookup_ty)?.method_fqns.get("show").cloned()?;
        if !self.functions.contains_key(&fqn) && !self.fn_entry_labels.contains_key(&fqn) {
            return None;
        }
        let fqn = self.instance_clone(&fqn, std::slice::from_ref(&lookup_ty)).unwrap_or(fqn);
        let raw = self.raw_instance_methods.contains(&fqn);
        if !raw && Self::ty_to_value_tag(&lookup_ty).is_some() {
            return None;
        }
        // A dictionary argument: not a plain call. Its code goes nowhere.
        let dict = self.hir_mir_quiet(|this| this.emit_call_instance_dict(&mut CodeBuf::new(), ("Show", "show", &fqn), std::slice::from_ref(&lookup_ty), 0..0));
        if dict || self.two_word_return_kind(&fqn).is_some() {
            return None;
        }
        self.raw_instance_call = raw;
        self.named_entry_target(&fqn, true)
    }
}
