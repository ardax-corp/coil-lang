//! What direct MIR lowering asks about a planned HIR body's nodes
//! ([`crate::mir::HirSite`]): the entry each one's code calls, the strings
//! it interns and the function values it makes, from the plan, the function
//! and label tables and the string interner.
//!
//! "Emission" below is the code MIR lowering writes for the plan. Lowering
//! asks for a node's site where it emits the node's code, and asking runs
//! the plan's side effects through the same helpers (interning, method
//! clones). A node whose code is never written is never asked. The nodes a
//! site exists for are the ones [`Compiler::hir_mir_reach`] visits.

use super::*;
use super::compiler::DictStep;

/// How many value operands `dload` / `declare` / `invoke` read before
/// the signature or argument tuple.
fn kind_arity(kind: crate::typechecking::FfiBuiltin) -> usize {
    match kind {
        crate::typechecking::FfiBuiltin::Dload => 1,
        _ => 2,
    }
}

/// Which children of an expression emission evaluates.
enum Kids {
    All,
    None,
    /// All but the first (a `format` call's literal format string).
    Rest,
    /// A `declare` / `invoke`'s value operands.
    These(Vec<HirId>),
}

impl Compiler {
    /// The nodes of `hir` whose code emission writes, in pre-order: not an
    /// outlined parallel loop's parts, nor an operand emission never
    /// evaluates (`typeof`'s, a fixed `len`'s, a `format` call's literal
    /// format string, an FFI signature or callback).
    pub(super) fn hir_mir_reach(&self, hir: &HirBody, emit: &HirEmit) -> Vec<HirId> {
        let mut reach = Vec::new();
        let mut todo: Vec<HirId> = hir.root.into_iter().collect();
        while let Some(id) = todo.pop() {
            reach.push(id);
            // A parallel loop outlined into its fork-join function: its
            // code is not this body's.
            if emit.plan.par_loops.contains_key(&id.0) {
                continue;
            }
            let kids: Vec<HirId> = match self.hir_mir_kids(hir, emit, id) {
                Kids::All => hir.children(id).collect(),
                Kids::None => Vec::new(),
                Kids::Rest => hir.children(id).skip(1).collect(),
                Kids::These(kids) => kids,
            };
            todo.extend(kids.into_iter().rev());
        }
        reach
    }

    /// Which children of `id` emission evaluates.
    fn hir_mir_kids(&self, hir: &HirBody, emit: &HirEmit, id: HirId) -> Kids {
        let call = || emit.plan.calls.get(&id.0);
        match &hir.expr(id).kind {
            // The operand's type name; the operand is not evaluated.
            HirKind::Builtin { op: Builtin::TypeOf, .. } => Kids::None,
            // `while false { .. }` is dropped.
            HirKind::Loop { body }
                if query::while_shape(hir, *body).is_some_and(|(cond, _)| matches!(hir.expr(cond).kind, HirKind::Lit(Lit::Bool(false)))) =>
            {
                Kids::None
            }
            HirKind::Call { callee: Callee::Value(_), .. } => Kids::All,
            // A fixed `len` of a local, literal or literal array evaluates
            // nothing.
            HirKind::Call { args, .. } if let Some(Some(_)) = emit.plan.lens.get(&id.0) => {
                if matches!(hir.expr(args[0]).kind, HirKind::Local(_) | HirKind::Lit(_) | HirKind::Make { .. }) {
                    Kids::None
                } else {
                    Kids::All
                }
            }
            HirKind::Call { .. } if matches!(call(), Some(HirCall { builtin: Some(HirBuiltin::LinAlg), .. })) => Kids::All,
            // What emission evaluates: an `extern`'s arguments, or the
            // value operands (a signature's tags are constants, a
            // callback its code pointer).
            HirKind::Call { args, .. } if let Some(HirCall { builtin: Some(builtin @ HirBuiltin::FfiDyn(_)), .. }) = call() => Kids::These(
                Self::hir_ffi_operands(hir, *builtin, args)
                    .into_iter()
                    .filter(|&a| self.hir_ffi_callback(hir, *builtin, a).is_none())
                    .collect(),
            ),
            HirKind::Call { .. } if matches!(call(), Some(HirCall { builtin: Some(HirBuiltin::Ffi { .. } | HirBuiltin::Matrix), .. })) => Kids::All,
            HirKind::Call { .. } if !emit.plan.lens.contains_key(&id.0) && matches!(call(), Some(HirCall { builtin: Some(HirBuiltin::Format), .. })) => Kids::Rest,
            _ => Kids::All,
        }
    }

    /// Whether the code emission writes for `reach` reads one of the
    /// frame's dictionaries any way but through a dictionary program
    /// ([`Self::hir_mir_dict_reads`]).
    pub(super) fn hir_mir_reads_dicts(&mut self, hir: &HirBody, emit: &HirEmit, reach: &[HirId]) -> bool {
        reach.iter().any(|&id| !emit.plan.par_loops.contains_key(&id.0) && self.hir_mir_dict_reads(hir, emit, id))
    }

    /// The values of `reach` emission packs as a bare-class existential
    /// after they are built, each with `plan_existential_pack` as a
    /// dictionary program run on the value's word. Worked out for the
    /// whole body before it lowers: a pack with no program, or one lowering
    /// passes by, leaves the body's MIR unused. The boxed-residue lint of
    /// the body's packs and `PolyFn` calls is reported here too, so a
    /// warning does not depend on what lowering reaches.
    pub(super) fn hir_mir_packs(&mut self, hir: &HirBody, emit: &HirEmit, reach: &[HirId]) -> crate::mir::HirPacks {
        let mut packs = crate::mir::HirPacks::default();
        let lint = self.residue_lint.get();
        for &id in reach {
            if emit.plan.par_loops.contains_key(&id.0) {
                continue;
            }
            // The boxed-residue lint of a `PolyFn` call's arguments is the
            // body's, whether or not lowering gets to the call.
            if lint
                && let HirKind::Call { callee: Callee::Value(f), args } = &hir.expr(id).kind
                && query::polyfn_callee(hir, &self.checker, *f)
            {
                let source = self.hir_polyfn_source(hir, emit, *f);
                let _ = self.hir_polyfn_args(hir, emit, source.as_deref(), *f, args);
            }
            let Some(pack) = self.hir_existential_pack(hir, id) else {
                continue;
            };
            if let Some(ty) = Self::hir_ty(hir, id) {
                let span = hir.expr(id).span;
                self.note_box_residue(
                    format!("pack {}", span.0),
                    ty,
                    span,
                    "a value of a bare trait type is one word",
                    "take it as a generic `T: Trait` parameter, whose clone keeps the value's two words",
                );
            }
            let steps = self.hir_mir_once(|this| {
                let mut steps = Vec::new();
                this.plan_existential_pack(&mut steps, &pack);
                steps
            });
            match self.hir_mir_dict_program(&steps) {
                Some(ops) => {
                    packs.programs.insert(id.0, ops);
                }
                None => packs.unplanned = true,
            }
        }
        packs
    }

    /// [`crate::mir::HirPacks::unit_fns`] of `hir`: each reached value a
    /// generic field or payload stores that takes the unit adapter
    /// ([`Self::hir_unit_fn_slot`]), with the adapter's `MakeFn` entry.
    pub(super) fn hir_mir_unit_fns(&mut self, hir: &HirBody, reach: &[HirId]) -> HashMap<u32, (crate::bodies::Label, u32)> {
        let mut out = HashMap::new();
        for &id in reach {
            for (arg, decl, _) in self.hir_generic_slots(hir, id) {
                if let Some(adapt) = self.hir_unit_fn_slot(hir, arg, &decl) {
                    out.insert(arg.0, self.hir_fn_adapter(&adapt));
                }
            }
        }
        out
    }

    /// The locals emission binds in `hir`'s frame, from the HIR and the
    /// plan alone: direct MIR numbers them after the entry words.
    pub(super) fn hir_bound_locals(&self, hir: &HirBody, emit: &HirEmit) -> HashSet<u32> {
        let mut bound = HashSet::new();
        // A lambda's body is emitted for its value ([`Self::hir_lambda_fn`]).
        if let Some(root) = hir.root {
            let value = hir.kind == crate::hir::BodyKind::Lambda;
            self.hir_mir_bound(hir, emit, root, value, false, &mut bound);
        }
        bound
    }

    /// Collect the locals `id` binds in this body's frame, as emission
    /// binds them: not a `defer` body's (its thunk's frame), an outlined
    /// parallel loop's (its worker's), a `()` `let`'s, code emission drops,
    /// or a `match` arm's whose payload word stays the value
    /// ([`Self::hir_mir_arm_binds`]).
    /// `value`: whether emission wants `id`'s value, not
    /// only its effect.
    fn hir_mir_bound(&self, hir: &HirBody, emit: &HirEmit, id: HirId, value: bool, defer: bool, bound: &mut HashSet<u32>) {
        // Walks recurse through here once per level of the tree.
        common::deep_stack(|| self.hir_mir_bound_level(hir, emit, id, value, defer, bound))
    }

    fn hir_mir_bound_level(&self, hir: &HirBody, emit: &HirEmit, id: HirId, value: bool, defer: bool, bound: &mut HashSet<u32>) {
        if emit.plan.par_loops.contains_key(&id.0) {
            return;
        }
        let walk = |k: HirId, value: bool, defer: bool, bound: &mut HashSet<u32>| self.hir_mir_bound(hir, emit, k, value, defer, bound);
        match &hir.expr(id).kind {
            HirKind::Let { local, .. } => {
                if !defer && !classify::is_unit_local(hir, &self.checker, *local) {
                    bound.insert(local.0);
                }
            }
            HirKind::LetPat { pat, .. } if !defer => {
                pat.for_each_bind(&mut |l| {
                    bound.insert(l.0);
                });
            }
            HirKind::Match { scrutinee, arms } => {
                let reach = arms
                    .iter()
                    .position(|a| matches!(a.pat, HirPat::Wild | HirPat::Bind(_)))
                    .map_or(arms.len(), |i| i + 1);
                let arms = &arms[..reach];
                if !defer {
                    for local in self.hir_mir_arm_binds(hir, emit, *scrutinee, arms, value) {
                        bound.insert(local.0);
                    }
                }
                walk(*scrutinee, true, defer, bound);
                for arm in arms {
                    walk(arm.body, value, defer, bound);
                }
                return;
            }
            HirKind::Block { stmts, tail } => {
                for &s in stmts {
                    walk(s, false, defer, bound);
                }
                if let Some(tail) = tail {
                    walk(*tail, value, defer, bound);
                }
                return;
            }
            HirKind::If { cond, then, els } => {
                walk(*cond, true, defer, bound);
                walk(*then, value, defer, bound);
                if let Some(els) = els {
                    walk(*els, value, defer, bound);
                }
                return;
            }
            // `while false { .. }` is dropped.
            HirKind::Loop { body }
                if query::while_shape(hir, *body).is_some_and(|(cond, _)| matches!(hir.expr(cond).kind, HirKind::Lit(Lit::Bool(false)))) =>
            {
                return;
            }
            HirKind::Loop { body } => {
                walk(*body, false, defer, bound);
                return;
            }
            HirKind::ForIn { pat, iterable, body, .. } => {
                if !defer {
                    pat.for_each_bind(&mut |l| {
                        bound.insert(l.0);
                    });
                }
                walk(*iterable, true, defer, bound);
                walk(*body, false, defer, bound);
                return;
            }
            HirKind::Defer { body, .. } => {
                walk(*body, false, true, bound);
                return;
            }
            // The operand's type name; the operand is not evaluated.
            HirKind::Builtin { op: Builtin::TypeOf, .. } => return,
            _ => {}
        }
        for k in hir.children(id) {
            walk(k, true, defer, bound);
        }
    }

    /// The locals the reached `arms` of a `match` on `scrutinee` bind in
    /// frame slots, as the match dispatches bind them. An
    /// arm emission skips (a niche match's third arm, a rewrapping
    /// `Err(e) => return Err(e)`), or whose payload word stays the value
    /// (an identity arm, outside in-place payload slots), binds none.
    fn hir_mir_arm_binds(&self, hir: &HirBody, emit: &HirEmit, scrutinee: HirId, arms: &[HirArm], value: bool) -> Vec<LocalId> {
        let mut out = Vec::new();
        // The arm's binding, or its first `n` payload fields'.
        let fields = |arm: &HirArm, n: usize, out: &mut Vec<LocalId>| match &arm.pat {
            HirPat::Bind(l) => out.push(*l),
            pat => out.extend(query::arm_fields(hir, pat).unwrap_or_default().into_iter().take(n).flatten()),
        };
        if query::has_nested_test(arms) || query::is_scalar_match(&self.checker, hir, scrutinee, arms) {
            for arm in arms {
                arm.pat.for_each_bind(&mut |l| out.push(l));
            }
            return out;
        }
        let Some(ty) = Self::hir_ty(hir, scrutinee) else {
            return out;
        };
        let identity = |arm: &HirArm| value && query::is_identity_arm(hir, arm);
        let payloads = |variant: &str| self.hir_payload_tys(ty, variant).unwrap_or_default();
        match self.hir_dispatch_rep(hir, emit, scrutinee, arms) {
            Rep::Word(ValueLayout::Boxed) => {
                let in_place = query::match_needs_slots(hir, arms);
                for arm in arms {
                    match &arm.pat {
                        HirPat::Variant { variant, .. } if in_place || !identity(arm) => fields(arm, payloads(variant).len(), &mut out),
                        HirPat::Bind(l) => out.push(*l),
                        _ => {}
                    }
                }
            }
            Rep::Pair(kind) => {
                // `Some(x) => x, None => 0` keeps the pair's payload word.
                let unit = |arm: &HirArm| match &arm.pat {
                    HirPat::Variant { variant, .. } => payloads(variant).is_empty() && matches!(hir.expr(arm.body).kind, HirKind::Lit(Lit::Int(0))),
                    _ => false,
                };
                let carries = |arm: &HirArm| matches!(&arm.pat, HirPat::Variant { variant, .. } if !payloads(variant).is_empty());
                if let [a, b] = arms
                    && (unit(a) || unit(b))
                    && [a, b].into_iter().any(|arm| !unit(arm) && identity(arm) && carries(arm))
                {
                    return out;
                }
                for arm in arms {
                    let skip = match &arm.pat {
                        HirPat::Wild => true,
                        HirPat::Variant { .. } => {
                            self.hir_rewrap_return_tag(hir, emit, ty, &kind, arm).is_some() || (identity(arm) && carries(arm))
                        }
                        _ => false,
                    };
                    if !skip {
                        fields(arm, 1, &mut out);
                    }
                }
            }
            Rep::Word(layout) => {
                let (payload_side, zero_side) = match layout {
                    ValueLayout::NicheOption => ("Some", "None"),
                    ValueLayout::NicheUnitResult => ("Err", "Ok"),
                    ValueLayout::NicheResult => ("Ok", "Err"),
                    ValueLayout::Boxed => return out,
                };
                let pick = |side: &str| {
                    arms.iter().position(|a| match &a.pat {
                        HirPat::Variant { variant, .. } => variant == side,
                        _ => true,
                    })
                };
                let (Some(first), Some(second)) = (pick(payload_side), pick(zero_side)) else {
                    return out;
                };
                let mut sides = vec![(first, payload_side)];
                if first != second {
                    sides.push((second, zero_side));
                }
                let rewraps = matches!(emit.plan.ret, Rep::Pair(_)) || emit.plan.ret == BOXED || emit.plan.ret == Rep::Word(layout);
                for (i, side) in sides {
                    let arm = &arms[i];
                    match &arm.pat {
                        HirPat::Wild => {}
                        HirPat::Bind(l) => out.push(*l),
                        _ if rewraps && self.hir_rewrap_tag(hir, ty, arm).is_some() => {}
                        pat => {
                            let named = query::arm_fields(hir, pat).is_ok_and(|f| f.iter().any(Option::is_some));
                            let reads = !payloads(side).is_empty() && (query::is_identity_arm(hir, arm) || named);
                            if reads && !identity(arm) {
                                fields(arm, 1, &mut out);
                            }
                        }
                    }
                }
            }
        }
        out
    }

    /// Note host call `id` for the capability check when its native needs
    /// one: the check finds the call's `HostInvoke` in this body's MIR by
    /// the call's loc. Direct MIR asks for a host call's native through
    /// [`crate::mir::LowerCx::host`], which notes it first, so no
    /// `HostInvoke` of a gated native is written unnoted.
    pub(super) fn hir_note_host_call(&mut self, hir: &HirBody, emit: &HirEmit, id: HirId) {
        let e = hir.expr(id);
        if let HirKind::Call { args, .. } = &e.kind
            && let Some(HirBuiltin::Host(native)) = emit.plan.calls.get(&id.0).and_then(|c| c.builtin)
            && let Some(row) = common::HOST_NATIVES.get(native)
            && self.native_id(row.name) == Some(native)
        {
            let mode = match args.get(1).map(|&a| &hir.expr(a).kind) {
                Some(HirKind::Lit(Lit::Str(m))) => Some(m.as_str()),
                _ => None,
            };
            self.note_gated_host_call(row.name, e.span, mode);
        }
    }

    /// What emission planned for node `id`, as direct MIR lowers it
    /// ([`crate::mir::HirSite`]). It runs the plan's side effects for the
    /// node: lowering asks once, where it writes the node's code.
    pub(super) fn hir_mir_site(&mut self, hir: &HirBody, emit: &HirEmit, id: HirId) -> crate::mir::HirSite {
        let mut site = crate::mir::HirSite::default();
        let t = &mut site;
        // A parallel loop outlined into its fork-join function: its code is
        // not this body's.
        if let Some(par) = emit.plan.par_loops.get(&id.0) {
            t.par_loop = Some(par.call.clone());
            return site;
        }
        let e = hir.expr(id);
        // A bound type parameter's operator or method through one of this
        // body's dictionary parameters; a call's ground niche `Option` /
        // `Result` result the adapter returns boxed converts to its own
        // layout after the call.
        let bound = match &e.kind {
            HirKind::Bin { .. } | HirKind::Un { .. } => match emit.plan.ops.get(&id.0) {
                Some(&HirOp::Bound { dict, method }) => Some((dict, method, false)),
                // An element-wise operator whose element is a bound type
                // parameter: the element's operator, once per element.
                Some(HirOp::Aggregate(_)) => self
                    .bound_operator_hint(e.node, e.span.0, e.span.1)
                    .and_then(|hint| Some((self.frame_dict(hint.dict_index)?, hint.method_slot as u32, false))),
                _ => None,
            },
            HirKind::Call { .. } if !emit.plan.lens.contains_key(&id.0) => match emit.plan.calls.get(&id.0) {
                Some(call @ HirCall { builtin: Some(HirBuiltin::Bound { dict, method }), .. }) => {
                    Some((*dict, *method, self.hir_ret_shape(hir, id, &Self::hir_call_rep(call)) == Some(true)))
                }
                _ => None,
            },
            _ => None,
        };
        // A trait method's own bounds' dictionaries follow the bound's.
        let own = match &e.kind {
            HirKind::Call { .. } => emit.plan.calls.get(&id.0).map(|c| c.own_dicts.clone()).unwrap_or_default(),
            _ => Vec::new(),
        };
        if let Some((dict, method, boxed_ret)) = bound
            && self.hir_mir_dict_param(dict)
            && let Some(dicts) = self.hir_mir_own_dicts(&own)
        {
            t.bound_call = Some(crate::mir::HirBoundCall { dict, method, boxed_ret, dicts, forward: None });
        }
        // A dictionary forwarder's one call (#2476).
        if let HirKind::Call { .. } = e.kind
            && self.hir_synth_calls.is_some()
            && let Some(call) = emit.plan.calls.get(&id.0)
            && let Some((index, method, own)) = self.hir_synth_callees.get(&call.key).and_then(|c| c.forward)
        {
            t.bound_call = Some(crate::mir::HirBoundCall { dict: 0, method, boxed_ret: false, dicts: Vec::new(), forward: Some((index, own)) });
            return site;
        }
        if let HirKind::Call { .. } = e.kind
            && !emit.plan.lens.contains_key(&id.0)
            && let Some(HirBuiltin::Existential { slot }) = emit.plan.calls.get(&id.0).and_then(|c| c.builtin)
        {
            t.existential_call = Some(slot);
        }
        if let HirKind::Field { base, name } = &e.kind
            && self.hir_field_of_new(hir, *base, name).is_some()
        {
            t.field_new = true;
        }
        match &e.kind {
            HirKind::Lit(Lit::Str(raw))
                if Self::hir_ty(hir, id).and_then(classify::primitive) != Some(crate::typechecking::ty::BYTE)
                    && !Self::hir_ty(hir, id).is_some_and(classify::is_byte_array) =>
            {
                let idx = self.intern_string(unescape_coil_string(raw));
                t.string = Some(idx);
            }
            HirKind::Global { .. } if emit.plan.statics.contains_key(&id.0) => {}
            HirKind::Lambda { .. } => {
                // Its body lowers in its own frame, at the entry label
                // reserved when it was planned.
                if let Some(lam) = emit.plan.lambdas.get(&id.0) {
                    let target = lam.label;
                    // Its captured dictionaries follow its captures (#1526).
                    // A two-word capture is two of its words.
                    let pairs: Vec<bool> = lam.pair_captures.iter().map(Option::is_some).collect();
                    let words = (lam.body.captures.len() + pairs.iter().filter(|&&p| p).count() + lam.dicts.len() + usize::from(lam.kinds.is_some())) as u32;
                    let operand = make_fn_operand(words, 0, lam.body.params.len() as u32, false);
                    let captures = lam.body.captures.iter().map(|&(outer, _)| outer).collect();
                    // Then the body's `kinds` word (#2506).
                    let dicts = lam.dicts.iter().map(|&(_, slot)| crate::mir::HirDictOp::Load(slot)).chain(lam.kinds.as_ref().map(|_| crate::mir::HirDictOp::KindsOwn)).collect();
                    t.fn_value = Some(crate::mir::HirFnValue { kind: crate::mir::MirAllocKind::Fn { target, operand, mask: 0 }, captures, pairs, dicts },
                    );
                }
            }
            // A generic function's `MakePolyFn`, or with bounds its
            // `MakePolyFnCapture` over the dictionaries
            // `plan_polyfn_escape_dicts` plans.
            HirKind::Global { .. } if let Some(name) = emit.plan.polyfns.get(&id.0) => {
                if let Some(target) = self.functions.get(name).and_then(|&entry| self.bodies.entry_label_for_offset(entry)) {
                    // Its declared parameters, so a call through the value
                    // given more hands the rest to its result (#1589): a
                    // curried one's value is `MakePolyFnCapture` of no
                    // dictionaries.
                    let params = self.polyfn_params(name);
                    // Its shared entry's `kinds` word, captured after the
                    // dictionaries (#2506), with the bits a call through
                    // the `forall` parameter it is passed to gives (#2621).
                    let escape = Self::hir_ty(hir, id).map(|t| apply_ty_prune(self.checker.subst(), t));
                    let expected = self.checker.rank_n_arg_at(e.span.0, e.span.1);
                    let kinds = self.polyfn_kinds_slot(name, escape.as_ref(), expected.as_ref());
                    let value = if let Some(kinds) = kinds.clone().filter(|_| self.checker.dict_arity_for(name) == 0) {
                        let kind = crate::mir::MirAllocKind::PolyFnCapture { target, params: params.unwrap_or(0) };
                        Some(crate::mir::HirFnValue { kind, captures: Vec::new(), pairs: Vec::new(), dicts: kinds })
                    } else if self.checker.dict_arity_for(name) == 0 {
                        let kind = match params {
                            Some(params) => crate::mir::MirAllocKind::PolyFnCapture { target, params },
                            None => crate::mir::MirAllocKind::PolyFn { target },
                        };
                        Some(crate::mir::HirFnValue { kind, captures: Vec::new(), pairs: Vec::new(), dicts: Vec::new() })
                    } else {
                        let (name, ty) = (name.clone(), Self::hir_ty(hir, id).map(|t| apply_ty_prune(self.checker.subst(), t)));
                        let steps = self.hir_mir_once(|this| {
                            let mut steps = Vec::new();
                            this.plan_polyfn_escape_dicts(&mut steps, &name, ty.as_ref(), expected.as_ref());
                            steps
                        });
                        self.hir_mir_dict_program(&steps).map(|mut dicts| {
                            dicts.extend(kinds.into_iter().flatten());
                            crate::mir::HirFnValue { kind: crate::mir::MirAllocKind::PolyFnCapture { target, params: params.unwrap_or(0) }, captures: Vec::new(), pairs: Vec::new(), dicts }
                        })
                    };
                    if let Some(value) = value {
                        t.fn_value = Some(value);
                    }
                }
            }
            HirKind::Global { .. } if let Some((entry, arity, rest, name)) = emit.plan.fn_refs.get(&id.0).cloned() => {
                if let Some(target) = self.bodies.entry_label_for_offset(entry) {
                    let target = self.fn_value_target(&name, target);
                    let operand = make_fn_operand(0, 0, arity, rest);
                    t.fn_value = Some(crate::mir::HirFnValue { kind: crate::mir::MirAllocKind::Fn { target, operand, mask: 0 }, captures: Vec::new(), pairs: Vec::new(), dicts: Vec::new() });
                }
            }
            HirKind::Global { .. } => {
                if let Some(crate::const_fold::ConstValue::Str(text)) = emit.plan.consts.get(&id.0) {
                    let idx = self.intern_string(text);
                    t.string = Some(idx);
                }
            }
            HirKind::Bin { op: BinOp::StrConcat, .. } => {
                let idx = self.intern_string("%s".repeat(hir.concat_leaves(id).len()));
                t.format = Some(idx);
            }
            // The operand's type name; the operand is not evaluated.
            HirKind::Builtin { op: Builtin::TypeOf, args } => {
                if let Some(name) = self.hir_typeof(hir, args[0]) {
                    let idx = self.intern_string(&name);
                    t.string = Some(idx);
                }
            }
            // `while false { .. }` is dropped.
            HirKind::Loop { body }
                if query::while_shape(hir, *body).is_some_and(|(cond, _)| matches!(hir.expr(cond).kind, HirKind::Lit(Lit::Bool(false)))) => {}
            // An element-wise operator a clone runs at a user type: the
            // type's instance method, called once per element.
            HirKind::Bin { .. } | HirKind::Un { .. }
                if let Some(HirOp::Aggregate(info)) = emit.plan.ops.get(&id.0)
                    && let Some(elem) = query::aggregate_elem(&self.checker, hir, id).filter(|elem| !classify::is_scalar(elem)) =>
            {
                use crate::typechecking::AggregateOp as A;
                let ((class, method), arity) = match info.op {
                    A::Add => (("Add", "add"), 2),
                    A::Sub => (("Sub", "sub"), 2),
                    A::Mul => (("Mul", "mul"), 2),
                    A::Div => (("Div", "div"), 2),
                    A::Mod => (("Rem", "rem"), 2),
                    A::Pow => (("Pow", "pow"), 2),
                    A::Neg => (("Neg", "neg"), 1),
                };
                // Whatever the instance returns (a two-word class, an
                // enum): an element is one word.
                // A nested aggregate's element is a tuple or an array: its
                // structural instance's thunk, which runs the operator one
                // level down (#2544).
                let target = self.concrete_operator_target_ty(&elem, class, method).or_else(|| self.structural_arith_target(&elem, class, method));
                if let Some((lookup, fqn)) = target {
                    let mut call = crate::mir::HirSite::default();
                    self.hir_mir_op_call_tys(hir, id, &vec![Some(elem); arity], (class, method, fqn), lookup, true, &mut call);
                    t.elem_call = call.call.map(|site| (site, true)).or(call.op_call.map(|op| (op.site, false)));
                }
            }
            // An element-wise operator on the packed kernel: its operands
            // boxed, then the meta word. The kernel takes primitive words
            // only: a type parameter's elements (a shared generic body) or
            // a user type's never reach it.
            HirKind::Bin { .. } | HirKind::Un { .. }
                if let Some(HirOp::Aggregate(info)) = emit.plan.ops.get(&id.0)
                    && query::aggregate_elem(&self.checker, hir, id).is_some_and(|elem| classify::is_scalar(&elem))
                    && let Some(meta) = Self::packed_aggregate_meta(info)
                    && let Some(native) = self.native_id(common::PACKED_VEC_ARITH) =>
            {
                t.linalg = Some((native as u16, meta));
            }
            // A matrix operator on a packed kernel, its operands each one
            // word as emission passes them.
            HirKind::Bin { .. } | HirKind::Un { .. } if let Some(HirOp::LinAlg(info)) = emit.plan.ops.get(&id.0) => {
                let operands: Vec<HirId> = match e.kind {
                    HirKind::Bin { lhs, rhs, .. } => vec![lhs, rhs],
                    HirKind::Un { operand, .. } => vec![operand],
                    _ => Vec::new(),
                };
                let words = operands.iter().all(|&x| {
                    let layout = Self::hir_ty(hir, x).map_or(ValueLayout::Boxed, |t| self.value_layout(t));
                    self.hir_arg_passes(hir, emit, x, layout)
                });
                if words && let Some((native, meta)) = self.packed_linear_algebra_op(&info.kind, operands.len()) {
                    t.linalg = Some((native as u16, meta));
                } else if words {
                    // Past the kernel's limits: unrolled.
                    t.unrolled_la = Some(info.kind.clone());
                }
            }
            HirKind::Bin {
                op: BinOp::Overloaded(_),
                lhs,
                rhs,
            } => match emit.plan.ops.get(&id.0) {
                Some(HirOp::Prim) => {
                    t.op_prim = true;
                }
                Some(HirOp::Call { lookup, fqn, class, method }) => {
                    let (lookup, fqn) = (lookup.clone(), fqn.clone());
                    self.hir_mir_op_call(hir, id, &[*lhs, *rhs], (class, method, fqn), lookup, t);
                }
                Some(HirOp::Glue(glue)) => self.hir_mir_glue_call(glue, t),
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
            HirKind::Call { callee: Callee::Value(f), args } => {
                if query::polyfn_callee(hir, &self.checker, *f) {
                    self.hir_mir_polyfn_call(hir, emit, id, *f, args, t);
                } else {
                    t.value_call = true;
                    if let Some(unpair) = hir.expr(id).ty.clone().and_then(|ty| self.hir_unpair(&ty)) {
                        t.value_pair_call = Some(unpair);
                    }
                }
            }
            // A fixed `len` evaluates at most its operand.
            HirKind::Call { .. } if let Some(Some(_)) = emit.plan.lens.get(&id.0) => {}
            // `dot` / `matmul` on a packed kernel.
            HirKind::Call { args, .. }
                if let Some(HirCall {
                    builtin: Some(HirBuiltin::LinAlg),
                    params,
                    ..
                }) = emit.plan.calls.get(&id.0) =>
            {
                if args.iter().zip(params).all(|(&a, &p)| self.hir_arg_passes(hir, emit, a, p))
                    && let Some(info) = self.hir_linear_algebra(hir, id)
                {
                    if let Some((native, meta)) = self.packed_linear_algebra_op(&info.kind, args.len()) {
                        t.linalg = Some((native as u16, meta));
                    } else if let crate::typechecking::LinearAlgebraKind::Cross { left_is_tuple, elem_is_float, elem_is_byte } = info.kind
                        && args.len() == 2
                    {
                        // Unrolled.
                        t.cross = Some((left_is_tuple, elem_is_float, elem_is_byte));
                    } else if matches!(info.kind, crate::typechecking::LinearAlgebraKind::Dot { .. } | crate::typechecking::LinearAlgebraKind::MatMul { .. })
                        && args.len() == 2
                    {
                        // So are these, past the kernel's limits.
                        t.unrolled_la = Some(info.kind.clone());
                    }
                }
            }
            HirKind::Call { args, .. }
                if let Some(HirCall {
                    builtin: Some(builtin @ (HirBuiltin::Ffi { .. } | HirBuiltin::FfiDyn(_))),
                    params,
                    ..
                }) = emit.plan.calls.get(&id.0) =>
            {
                let (builtin, params) = (*builtin, params.clone());
                if let Some(site) = self.hir_mir_ffi(hir, emit, id, builtin, args, &params) {
                    t.ffi = Some(site);
                }
            }
            HirKind::Call { .. } if emit.plan.calls.get(&id.0).is_some_and(|c| matches!(c.builtin, Some(HirBuiltin::Matrix))) => {
                t.matrix_wrap = true;
            }
            HirKind::Call { args, .. }
                if !emit.plan.lens.contains_key(&id.0)
                    && emit.plan.calls.get(&id.0).is_some_and(|c| matches!(c.builtin, Some(HirBuiltin::Format))) =>
            {
                self.hir_mir_format(hir, args, t);
            }
            // A partial application: `MakeFn` over the filled prefix, each
            // argument its own word.
            HirKind::Call { args, .. }
                if let Some(HirCall {
                    builtin: Some(HirBuiltin::Partial { entry, mask, operand, value }),
                    params,
                    ..
                }) = emit.plan.calls.get(&id.0) =>
            {
                let (entry, value) = (*entry as usize, *value);
                if args.iter().zip(params).all(|(&a, &p)| self.hir_natural(hir, emit, a) == Some(Rep::Word(p)))
                    && let Some(label) = self.bodies.entry_label_for_offset(entry)
                    && let Some(target) = self.partial_value_target(entry, label, value)
                {
                    let kind = crate::mir::MirAllocKind::Fn { target, operand: *operand, mask: *mask };
                    // A shared generic entry's `kinds` word, its last filled
                    // parameter (#2506).
                    let dicts = match self.kinds_key_at(label).map(str::to_string) {
                        Some(key) => self.hir_kinds_ops_partial(hir, id, &key, *mask, args),
                        None => Some(Vec::new()),
                    };
                    if let Some(dicts) = dicts {
                        t.fn_value = Some(crate::mir::HirFnValue { kind, captures: Vec::new(), pairs: Vec::new(), dicts });
                    }
                }
            }
            HirKind::Call { args, .. } if !emit.plan.lens.contains_key(&id.0) && emit.plan.calls.contains_key(&id.0) => {
                self.hir_mir_call(hir, emit, id, args, t);
            }
            HirKind::ForIn {
                pat,
                iterable,
                kind: Some(kind),
                ..
            } => self.hir_mir_for_in(hir, pat, *iterable, kind, t),
            HirKind::Index {
                base,
                index,
                kind: IndexKind::String,
            } if Self::hir_product_index(hir, emit, *base, *index).is_none() => {
                let idx = self.intern_string("index out of bounds");
                t.string = Some(idx);
            }
            _ => {}
        }
        site
    }

    /// An FFI call's opcode and operands for `HirBuiltin::Ffi` /
    /// `FfiDyn`; `None` when an operand is not the
    /// word emission pushes.
    fn hir_mir_ffi(&mut self, hir: &HirBody, emit: &HirEmit, id: HirId, builtin: HirBuiltin, args: &[HirId], params: &[ValueLayout]) -> Option<crate::mir::HirFfi> {
        use crate::mir::{HirFfiOperand as O, MirFfiOp};
        use crate::typechecking::FfiBuiltin;
        let value = |this: &Self, a: HirId, p: ValueLayout| this.hir_arg_passes(hir, emit, a, p).then_some(O::Value(a.0));
        let tag = |(t, aux): (u32, u32)| O::Const(i64::from(encode_tag_operand(t, aux)));
        let mut operands = Vec::new();
        let (op, operand) = match builtin {
            HirBuiltin::Ffi { lib, func, variadic } => {
                if params.len() != args.len() {
                    return None;
                }
                let items = args.iter().zip(params).map(|(&a, &p)| value(self, a, p)).collect::<Option<Vec<_>>>()?;
                operands.extend([O::Static(lib), O::Static(func), O::Tuple(items)]);
                let mut operand = args.len() as u32 & 0xFFFF;
                if let Some(def) = variadic {
                    let tags = self.hir_variadic_tags(def, hir.expr(id).span, args.len())?;
                    operands.push(O::Tuple(tags.into_iter().map(tag).collect()));
                    operand |= 1 << 16;
                }
                (MirFfiOp::Invoke, operand)
            }
            HirBuiltin::FfiDyn(kind) => {
                let values = Self::hir_ffi_operands(hir, builtin, args);
                if params.len() < values.len() || values.len() < kind_arity(kind) {
                    return None;
                }
                for (&a, &p) in values.iter().zip(params).take(2) {
                    operands.push(value(self, a, p)?);
                }
                match kind {
                    FfiBuiltin::Dload => (MirFfiOp::Load, 0),
                    FfiBuiltin::Declare => {
                        let items = classify::tuple_items(hir, args[2])?;
                        let sig = items.iter().map(|&t| query::ffi_tag(hir, &self.checker, t).map(tag)).collect::<Option<Vec<_>>>()?;
                        operands.push(O::Tuple(sig));
                        operands.push(tag(query::ffi_tag(hir, &self.checker, args[3])?));
                        let variadic = args.get(4).is_some_and(|&v| matches!(hir.expr(v).kind, HirKind::Lit(Lit::Bool(true))));
                        (MirFfiOp::Declare, (items.len() as u32 & 0xFFFF) | (u32::from(variadic) << 16))
                    }
                    FfiBuiltin::Invoke => {
                        let n = values.len() as u32 - 2;
                        let mut items = Vec::with_capacity(n as usize);
                        for (&a, &p) in values.iter().zip(params).skip(2) {
                            items.push(match self.hir_ffi_callback(hir, builtin, a) {
                                Some(offset) => O::CodePtr(self.bodies.entry_label_for_offset(offset? as usize)?),
                                None => value(self, a, p)?,
                            });
                        }
                        operands.push(O::Tuple(items));
                        let mut operand = n & 0xFFFF;
                        if query::ffi_fn_variadic(hir, &self.checker, args[1]) {
                            let items = classify::tuple_items(hir, args[2])?;
                            let tags = query::ffi_variadic_tags(hir, &self.checker, id, items)?;
                            operands.push(O::Tuple(tags.into_iter().map(tag).collect()));
                            operand |= 1 << 16;
                        }
                        (MirFfiOp::Invoke, operand)
                    }
                }
            }
            _ => return None,
        };
        let unwrap = matches!(builtin, HirBuiltin::Ffi { .. });
        // The runtime pushes a boxed `Result`; a niche-shaped one converts.
        let niche = match Self::hir_call_rep(&emit.plan.calls[&id.0]) {
            _ if unwrap => None,
            Rep::Word(ValueLayout::NicheUnitResult) => Some(crate::mir::MirTy::NicheOpt),
            Rep::Word(ValueLayout::NicheResult) => Some(crate::mir::MirTy::NicheRes),
            Rep::Word(ValueLayout::Boxed) => None,
            _ => return None,
        };
        Some(crate::mir::HirFfi { op, operand, operands, unwrap, niche })
    }

    /// The user `into_iter` / `next` entries `for` loop `id` calls, as
    /// MIR calls them.
    /// The dictionary a `for` loop passes to `into_iter` / `next` entry
    /// `fqn` on a receiver of type `ty`, as a program: none unless the
    /// entry takes one (#2506). `None` when it takes one this cannot build.
    fn hir_iter_dicts(&mut self, fqn: &str, ty: &Ty) -> Option<Vec<crate::mir::HirDictOp>> {
        let (class, method) = self.checker.generics().instances.iter().find_map(|inst| {
            let (method, _) = inst.method_fqns.iter().find(|(_, f)| *f == fqn)?;
            Some((inst.class.clone(), method.clone()))
        })?;
        if !self.instance_call_takes_dict(&class, &method, fqn) || self.raw_instance_methods.contains(fqn) {
            return Some(Vec::new());
        }
        let mut steps = Vec::new();
        let lookup = [ty.clone()];
        if !self.hir_mir_once(|this| this.plan_call_instance_dict(&mut steps, (&class, &method, fqn), &lookup, 0..0)) {
            return None;
        }
        self.hir_mir_dict_program(&steps)
    }

    /// The iterator `into_iter` entry `fqn` returns on an iterable of type
    /// `iterable`: its declared result `declared`, its instance head's
    /// variables bound from `iterable`.
    fn hir_iter_ty_at(&self, fqn: &str, iterable: &Ty, declared: &Ty) -> Option<Ty> {
        let head = self.checker.generics().instances.iter().find(|inst| inst.method_fqns.values().any(|f| f == fqn))?.args.clone();
        let mut vars = HashMap::new();
        for h in &head {
            Self::bind_scheme_vars(h, iterable, &mut vars);
        }
        Some(apply_ty_prune(self.checker.subst(), &Self::apply_ty_var_map(declared, &vars)))
    }

    fn hir_mir_for_in(&mut self, hir: &HirBody, pat: &HirPat, iterable: HirId, kind: &ForInKind, t: &mut crate::mir::HirSite) {
        let ForInKind::Custom { into_iter_fqn, next_fqn, counted } = kind else {
            return;
        };
        // A counted result runs that result's loop; a range bound read
        // from the iterable needs no call.
        if let Some(counted) = counted {
            use crate::typechecking::infer::ForInCounted as C;
            if matches!(counted, C::Range { .. }) && query::range_bounds(hir, iterable).is_some() {
                return;
            }
            self.raw_instance_call = false;
            if let Some(target) = into_iter_fqn.as_deref().and_then(|f| self.named_entry_target(f, true)) {
                t.into_iter = Some(target);
            }
            return;
        }
        let (Some(next_fqn), &HirPat::Bind(local)) = (next_fqn, pat) else {
            return;
        };
        // `next` may be a generic instance's shared body (`Iterator for
        // ListIter<T>`): its declared `Option<Item>`, open in the head's
        // variables, is what it returns, not the loop's ground item (#1912).
        let declared = self
            .fn_return_ty(next_fqn)
            .and_then(|r| crate::typechecking::ty::option_inner(&r))
            .filter(|t| !crate::layout::ty_is_closed(t));
        let item = declared.or_else(|| hir.local(local).ty.clone());
        let niche = item
            .as_ref()
            .is_some_and(|ty| crate::layout::niche_heap_only(&self.checker, ty));
        let two_slot = self.two_word_return_kind(next_fqn).is_some();
        // No `into_iter`: the iterable is the iterator.
        let (into_iter, iter_ty) = match into_iter_fqn {
            Some(f) => {
                self.raw_instance_call = false;
                match (self.named_entry_target(f, true), self.fn_return_ty(f)) {
                    (Some(target), Some(ty)) => (Some(target), ty),
                    _ => return,
                }
            }
            None => match hir.expr(iterable).ty.clone() {
                Some(ty) => (None, ty),
                None => return,
            },
        };
        self.raw_instance_call = false;
        let next = self.named_entry_target(next_fqn, true);
        if let Some(next) = next {
            let shape = if niche {
                crate::mir::HirIterNext::Niche
            } else if two_slot {
                crate::mir::HirIterNext::Pair
            } else {
                let some = self.checker.tag_for(common::BUILTIN_OPTION_ENUM, "Some").unwrap_or(1);
                crate::mir::HirIterNext::Boxed { some }
            };
            // A generic-head instance's `into_iter` / `next` takes its
            // dictionary (#2506): planned at the iterable's and the
            // iterator's types.
            let iterable_ty = hir.expr(iterable).ty.clone().map(|t| apply_ty_prune(self.checker.subst(), &t));
            let iterator_ty = match (into_iter_fqn, &iterable_ty) {
                (Some(f), Some(it)) => self.hir_iter_ty_at(f, it, &iter_ty),
                (None, it) => it.clone(),
                _ => None,
            };
            let (Some(into_iter_dicts), Some(next_dicts)) = (
                match (into_iter_fqn, &iterable_ty) {
                    (Some(f), Some(ty)) => self.hir_iter_dicts(f, ty),
                    _ => Some(Vec::new()),
                },
                iterator_ty.as_ref().map_or(Some(Vec::new()), |ty| self.hir_iter_dicts(next_fqn, ty)),
            ) else {
                return;
            };
            t.iter_loop = Some(crate::mir::HirIterLoop { into_iter, next, iter_ty, shape, into_iter_dicts, next_dicts });
        }
    }

    /// User operator `id` on `operands` calling its instance method
    /// (`HirOp::Call`): a raw method takes the
    /// operands' own words (a plain call), any other a binary operator's
    /// boxed operands (an operator call). An instance taking a dictionary
    /// (`Box<T>`'s derived `Eq` in a shared body, built from `T`'s, #1661)
    /// gets it after the operands, as a dictionary program.
    fn hir_mir_op_call(
        &mut self,
        hir: &HirBody,
        id: HirId,
        operands: &[HirId],
        target: (&str, &str, String),
        lookup: Ty,
        t: &mut crate::mir::HirSite,
    ) {
        let operands: Vec<Option<Ty>> = operands.iter().map(|&o| Self::hir_ty(hir, o).cloned()).collect();
        self.hir_mir_op_call_tys(hir, id, &operands, target, lookup, false, t);
    }

    /// [`Self::hir_mir_op_call`] by the operands' types: an element-wise
    /// operator (`elem`) calls the instance on elements, which are no HIR
    /// node. An element is its boxed word, and so is the result: where the
    /// instance's entry does not take and return them so, its dictionary
    /// adapter does (as a shared body's dictionary call reaches it).
    #[allow(clippy::too_many_arguments)]
    fn hir_mir_op_call_tys(
        &mut self,
        hir: &HirBody,
        id: HirId,
        operands: &[Option<Ty>],
        (class, method, fqn): (&str, &str, String),
        lookup: Ty,
        elem: bool,
        t: &mut crate::mir::HirSite,
    ) {
        let fqn = self.instance_clone(&fqn, std::slice::from_ref(&lookup)).unwrap_or(fqn);
        if Self::is_eq_glue(&fqn) {
            self.reserve_eq_glue(&lookup);
        }
        let raw = self.raw_instance_methods.contains(&fqn);
        let span = hir.expr(id).span;
        let mut steps = Vec::new();
        let dict = self.hir_mir_once(|this| this.plan_call_instance_dict(&mut steps, (class, method, &fqn), std::slice::from_ref(&lookup), span.0..span.1));
        let dicts = if dict {
            match self.hir_mir_dict_program(&steps) {
                Some(dicts) => dicts,
                None => return,
            }
        } else {
            Vec::new()
        };
        // A two-word result comes back as two words (or boxed after the call).
        let one_word = self.two_word_return_kind(&fqn).is_none();
        let arity = operands.len();
        let declared = self.structural_thunk_params.get(&fqn);
        let fits = |i: usize, layout: ValueLayout| {
            layout == ValueLayout::Boxed
                || (Self::is_eq_glue(&fqn) && declared.and_then(|p| p.get(i)).is_some_and(|t| self.value_layout(t) == layout))
        };
        let raw_fits = operands.iter().enumerate().all(|(i, o)| o.as_ref().is_some_and(|t| fits(i, self.value_layout(t))));
        // An element through the adapter: the boxed word in, the boxed
        // word out.
        let adapter = elem && raw && !(one_word && raw_fits);
        self.raw_instance_call = raw && !adapter;
        // A raw method's own entry returns a two-word result as two words,
        // as the method spelling's call takes it (#2433).
        let plain = raw && !elem;
        let Some(target) = self.named_entry_target(&fqn, false).filter(|_| one_word || adapter || plain) else {
            return;
        };
        // A trait default (`Eq::ne`) is one shared body over the trait's
        // type parameter: each operand goes as its own word, a
        // scalar-backed enum's backing word included (#2462).
        let open = !elem && Self::is_default_method_fqn(class, method, &fqn);
        let site = crate::mir::HirCallSite {
            target,
            pair_args: vec![false; arity],
            pair_ret: plain && !one_word,
            coro: false,
            boxed_ret: false,
            dicts,
            words: if open { vec![true; arity] } else { Vec::new() },
            adapt: Vec::new(),
            word_ret: false,
            ret_adapt: None,
            ret_items: None,
        };
        if adapter {
            t.op_call = Some(crate::mir::HirOpCall { site, shift: false });
        } else if raw {
            // Each operand's MIR word is the boxed-layout word the stack passes.
            // A glue function (#1629) takes its own type's words, a niche
            // word as one.
            if raw_fits {
                t.call = Some(site);
            }
        } else if (arity == 2 || elem) && Self::ty_to_value_tag(&lookup).is_some() {
            let shift = matches!(class, "Shl" | "Shr");
            t.op_call = Some(crate::mir::HirOpCall { site, shift });
        }
    }

    /// `==` / `!=` `id` through open glue `glue` (`HirOp::Glue`, #1642):
    /// a plain call of the two operands' words, then each type variable's
    /// `Eq` dictionary, loaded from this body's dictionary parameters.
    fn hir_mir_glue_call(&mut self, glue: &super::eq_glue::DictGlue, t: &mut crate::mir::HirSite) {
        self.reserve_eq_dict_glue(glue);
        if !glue.slots().all(|slot| self.hir_mir_dict_param(slot)) {
            return;
        }
        let mut steps = Vec::new();
        for &dict in &glue.dicts {
            if !self.plan_var_eq_dict(&mut steps, dict) {
                return;
            }
        }
        let Some(dicts) = self.hir_mir_dict_program(&steps) else {
            return;
        };
        let Some(target) = self.named_entry_target(&glue.fqn, false) else {
            return;
        };
        let site = crate::mir::HirCallSite {
            target,
            pair_args: vec![false; 2],
            pair_ret: false,
            coro: false,
            boxed_ret: false,
            dicts,
            words: Vec::new(),
            adapt: Vec::new(),
            word_ret: false,
            ret_adapt: None,
            ret_items: None,
        };
        t.call = Some(site);
    }

    /// The clone a ground instance call goes to: at the instance's types,
    /// then, for a default body with bounds of its own (`fn split<P:
    /// Pattern>`), at the types its own dictionaries are ground at.
    fn ground_instance_clone(&mut self, call: &HirCall, inst_args: &[Ty]) -> Option<String> {
        let mut lookup = inst_args.to_vec();
        for own in &call.own_dicts {
            match own {
                super::HirOwnDict::Ground { lookup: l, .. } if l.len() == 1 => lookup.push(l[0].clone()),
                super::HirOwnDict::Kinds(_) => {}
                _ => return self.instance_clone(&call.key, inst_args),
            }
        }
        self.instance_clone(&call.key, &lookup)
    }

    /// Call `id` of a function or method, past the builtins: its entry when it is one
    /// plain `CALL` (or `TailCall` / `MakeCoro`) of the arguments' own
    /// words with no dictionary. A shared generic entry's `kinds` word
    /// follows its dictionaries (#2506), unless a compiler-made body
    /// planned it.
    fn hir_mir_call(&mut self, hir: &HirBody, emit: &HirEmit, id: HirId, args: &[HirId], t: &mut crate::mir::HirSite) {
        self.hir_mir_call_site(hir, emit, id, args, t);
        if let Some(site) = t.call.as_mut()
            && !matches!(site.dicts.last(), Some(crate::mir::HirDictOp::Kinds(_) | crate::mir::HirDictOp::KindsBit { .. }))
            && let Some(key) = self.kinds_key_at(site.target).map(str::to_string)
        {
            match self.hir_kinds_ops(hir, id, &key, args) {
                Some(ops) => site.dicts.extend(ops),
                None => t.call = None,
            }
        }
    }

    /// [`Self::hir_mir_call`]'s site, before its `kinds` word.
    fn hir_mir_call_site(&mut self, hir: &HirBody, emit: &HirEmit, id: HirId, args: &[HirId], t: &mut crate::mir::HirSite) {
        let call = &emit.plan.calls[&id.0];
        if call.builtin.is_some() || call.key == format!("{}::push", common::BUILTIN_VEC_TYPE) {
            return;
        }
        let natural = Self::hir_call_rep(call);
        let span = hir.expr(id).span;
        if let Some(inst) = call.instance.as_ref().filter(|i| i.ground.is_some()) {
            let ground = inst.ground.as_ref().expect("ground call");
            let method = &ground.method;
            // A niche result the shared entry returns as its enum object
            // (`fn id(T x) -> T` at `Option<T>`, #1595): direct MIR calls
            // that entry, as emission does, and moves it to the niche word.
            let key = match self.hir_ret_shape(hir, id, &natural) {
                Some(true) => call.key.clone(),
                _ => self.ground_instance_clone(call, &inst.args).unwrap_or_else(|| call.key.clone()),
            };
            let steps = self.hir_mir_once(|this| {
                let mut steps = Vec::new();
                this.plan_call_instance_dict(&mut steps, (&inst.class, method, &key), &inst.args, span.0..span.1);
                steps
            });
            // A clone at the own bounds' types takes none of their
            // dictionaries, only the own `kinds` word (#2506).
            let own_kept: Vec<super::HirOwnDict> = call.own_dicts.iter().filter(|d| matches!(d, super::HirOwnDict::Kinds(_))).cloned().collect();
            let own_dicts: &[super::HirOwnDict] = if key != call.key || !self.trait_entry_shared(&key) { &own_kept } else { &call.own_dicts };
            let dicts = self.hir_mir_dict_program(&steps).and_then(|mut d| {
                d.extend(self.hir_mir_own_dicts(own_dicts)?);
                Some(d)
            });
            let raw = self.raw_instance_methods.contains(&key);
            self.raw_instance_call = raw;
            let target = self.named_entry_target(&key, true);
            // Raw words, or each argument as its word where the instance
            // takes a bare type parameter (a word, not a pair), and the
            // instance's dictionary: direct MIR calls the same entry.
            let words: Vec<bool> = if raw {
                Vec::new()
            } else {
                ground.boxed.iter().map(|ty| ty.as_ref().and_then(Self::ty_to_value_tag).is_some()).collect()
            };
            let unpaired = args
                .iter()
                .zip(&words)
                .all(|(&a, &w)| !w || !matches!(self.hir_natural(hir, emit, a), Some(Rep::Pair(_))));
            // A niche word a generic instance's shared entry takes as its
            // enum object (#1579); its clone at the call's types takes the
            // niche word itself.
            let clone = key != call.key;
            let niches: Vec<Option<ValueLayout>> = args
                .iter()
                .zip(&call.params)
                .map(|(&a, &p)| (!self.hir_arg_passes(hir, emit, a, p)).then(|| self.hir_niche_arg(hir, emit, a, p)).flatten())
                .collect();
            if let Some(dicts) = dicts
                && unpaired
                && args.iter().zip(&call.params).zip(&niches).all(|((&a, &p), n)| n.is_some() || self.hir_arg_passes(hir, emit, a, p))
                && let Some(target) = target
                && let Some(boxed_ret) = self.hir_ret_shape(hir, id, &natural)
            {
                for (&arg, &layout) in args.iter().zip(&niches) {
                    if let Some(layout) = layout
                        && !clone
                    {
                        t.niche_args.push((arg.0, layout));
                    }
                }
                t.call = Some(crate::mir::HirCallSite {
                        target,
                        pair_args: vec![false; args.len()],
                        pair_ret: natural.words() == 2,
                        coro: false,
                        boxed_ret,
                        dicts,
                        words,
                        adapt: Vec::new(),
                        word_ret: false,
                        ret_adapt: None,
                        ret_items: None,
                    },
                );
            }
            return;
        }
        if call.method {
            let instance = call.instance.as_deref();
            let key = match instance.and_then(|i| i.recv_box.clone()) {
                Some(recv) if call.generic.is_none() => self.ground_instance_clone(call, &[recv]).unwrap_or_else(|| call.key.clone()),
                _ => call.key.clone(),
            };
            let raw = self.raw_instance_methods.contains(&key);
            let instance = instance.filter(|_| !raw);
            // Bare type parameter arguments (and a generic-head instance's
            // receiver) as their words, the result read back from its word.
            let tag = |ty: Option<&Ty>| ty.and_then(Self::ty_to_value_tag).is_some();
            let words: Vec<bool> = (0..args.len())
                .map(|i| {
                    tag(call
                        .generic
                        .as_ref()
                        .and_then(|g| g.boxed[i].as_ref())
                        .or_else(|| instance.filter(|_| i == 0).and_then(|inst| inst.recv_box.as_ref())))
                })
                .collect();
            let word_ret = tag(call.generic.as_ref().and_then(|g| g.unbox.as_ref()));
            // The generic's dictionaries, then the instance's, as
            // `plan_call_site_dicts` and `plan_instance_dict` plan them.
            let mut dicts = match call.generic.as_deref() {
                None => Some(Vec::new()),
                Some(g) if self.hir_generic_boxing(hir, emit, args, &call.params) => self.hir_mir_dict_ops(g),
                Some(_) => None,
            };
            if let Some(inst) = instance
                && dicts.is_some()
            {
                let steps = self.hir_mir_once(|this| {
                    let mut steps = Vec::new();
                    this.plan_entry_dict(&mut steps, &inst.class, &inst.args);
                    steps
                });
                let prev = dicts.take();
                dicts = self.hir_mir_dict_program(&steps).and_then(|ops| {
                    prev.map(|mut d| {
                        d.extend(ops);
                        d
                    })
                });
            }
            // A trait method's own bounds' dictionaries follow the
            // instance's (#1471); its clone at their types takes none.
            let shared_key = key == call.key && (self.trait_entry_shared(&key) || !call.own_dicts.iter().any(|d| matches!(d, super::HirOwnDict::Kinds(_))));
            let own_kept: Vec<super::HirOwnDict> = call.own_dicts.iter().filter(|d| shared_key || matches!(d, super::HirOwnDict::Kinds(_))).cloned().collect();
            if let Some(d) = dicts.as_mut()
                && !own_kept.is_empty()
            {
                match self.hir_mir_own_dicts(&own_kept) {
                    Some(own) => d.extend(own),
                    None => dicts = None,
                }
            }
            let coro = self.coroutine_fns.contains(&key);
            self.hir_request_method_clone(&key);
            self.raw_instance_call = raw;
            let target = self.named_entry_target(&key, true);
            // A generic-head instance's shared entry takes its receiver as
            // the boxed enum object: a niche word goes as its enum object,
            // a two-word `Option` / `Result` boxed from its pair (#1609).
            // A bare type parameter's argument goes as its own word.
            let shared_entry = key == call.key;
            // The call was planned in its own types' layouts, which the
            // instance's clone takes. The shared entry declares an
            // `Option` / `Result` over the instance's parameters open
            // (`fn pick(Option<T> x, Option<T> y) -> Option<T>`): each is
            // its enum object there, so a niche word converts at the call,
            // on the way in and on the way out (#2475).
            let open = |declared: Option<Ty>| {
                shared_entry && instance.is_some() && call.generic.is_none() && declared.is_some_and(|ty| self.generic_enum_layout(&ty).is_some())
            };
            let declared = if shared_entry && instance.is_some() { self.checker.fn_param_tys(&key) } else { None };
            // A default body declares none of that: its signature is the
            // trait's, over the trait's parameter. Where the receiver goes
            // as its enum object (#1609), so does every other value of
            // that parameter, an argument or the result: the body passes
            // them to the same dictionary's methods (#2490).
            let default_sig = (shared_entry && instance.is_some() && call.generic.is_none())
                .then(|| key.split_once("__default__"))
                .flatten()
                .and_then(|(class, method)| self.checker.typeclass_method_scheme(class, method))
                .map(|scheme| Self::fun_param_and_ret_tys(&scheme.ty))
                .filter(|_| words.first() == Some(&true) && args.first().is_some_and(|&a| self.hir_niche_arg(hir, emit, a, ValueLayout::Boxed).is_some()));
            let as_receiver = |ty: Option<&Ty>| {
                default_sig.as_ref().is_some_and(|(params, _)| matches!(params.first(), Some(recv @ Ty::Var(_)) if ty == Some(recv)))
            };
            let params: Vec<ValueLayout> = call
                .params
                .iter()
                .enumerate()
                .map(|(i, &p)| {
                    let object = open(declared.as_ref().and_then(|d| d.get(i).cloned()))
                        || (i > 0 && as_receiver(default_sig.as_ref().and_then(|(params, _)| params.get(i))));
                    if object { ValueLayout::Boxed } else { p }
                })
                .collect();
            let natural = match natural {
                Rep::Word(_) if open(self.checker.fn_return_ty(&key)) || as_receiver(default_sig.as_ref().map(|(_, ret)| ret)) => {
                    Rep::Word(ValueLayout::Boxed)
                }
                natural => natural,
            };
            let niches: Vec<Option<ValueLayout>> = args
                .iter()
                .zip(&words)
                .enumerate()
                .map(|(i, (&a, &w))| (shared_entry && i == 0 && w).then(|| self.hir_niche_arg(hir, emit, a, ValueLayout::Boxed)).flatten())
                .collect();
            if let Some(dicts) = dicts
                && let Some(boxed_ret) = self.hir_ret_shape(hir, id, &natural)
                && args.iter().zip(&params).zip(&words).zip(&niches).all(|(((&a, &p), &w), n)| {
                    // A boxed argument goes from its own word, its enum
                    // object (a niche word) or its pair boxed (a pair
                    // constructor such as `Some(1)` or `None` too, #1861);
                    // a niche `Option` / `Result` of a shared body as its
                    // enum object.
                    if w {
                        self.hir_arg_word(hir, emit, a, p)
                            || n.is_some()
                            || (shared_entry
                                && matches!(self.hir_natural(hir, emit, a), None | Some(Rep::Pair(_)))
                                && self.hir_arg_passes(hir, emit, a, p))
                    } else {
                        self.hir_arg_passes(hir, emit, a, p) || self.hir_niche_arg(hir, emit, a, p).is_some()
                    }
                })
                && let Some(target) = target
            {
                for ((&arg, &param), (&w, &layout)) in args.iter().zip(&params).zip(words.iter().zip(&niches)) {
                    let layout = layout.or_else(|| {
                        (!w && !self.hir_arg_passes(hir, emit, arg, param))
                            .then(|| self.hir_niche_arg(hir, emit, arg, param))
                            .flatten()
                    });
                    if let Some(layout) = layout {
                        t.niche_args.push((arg.0, layout));
                    }
                }
                t.call = Some(crate::mir::HirCallSite {
                        target,
                        // An inherent method's two-word parameters.
                        pair_args: (0..args.len()).map(|i| Self::hir_arg_rep(call, i).words() == 2).collect(),
                        pair_ret: natural.words() == 2,
                        coro,
                        boxed_ret,
                        dicts,
                        words,
                        adapt: self.hir_mir_adapters(hir, call, args),
                        word_ret,
                        ret_adapt: self.hir_mir_ret_adapter(call),
                        ret_items: Self::hir_mir_ret_items(call),
                    },
                );
            }
            return;
        }
        if let Some(plan) = self.hir_par_call_plan(hir, &call.key, args) {
            // The worker (and a dynamic site's sequential function) as the
            // forking `CALL`s resolve them.
            let (worker, seq) = match plan {
                HirParCall::Spec { worker, .. } => (worker, None),
                HirParCall::Dynamic { cutoff, orig, worker } => (worker, Some((orig, cutoff))),
            };
            let label = |offset: usize| self.bodies.entry_label_for_offset(offset);
            let seq = match seq {
                None => Some(None),
                Some((orig, cutoff)) => label(orig).map(|l| Some((l, cutoff))),
            };
            if let (Some(worker), Some(seq)) = (label(worker), seq) {
                t.par_fork = self.hir_par_fork(hir, emit, id, worker, seq);
            }
            return;
        }
        let tail = emit.plan.tail_calls.contains(&id.0);
        let coro = !tail && self.coroutine_fns.contains(&call.key);
        // A compiler-made body's call as it planned it: the dictionaries
        // it passes, and a raw method's own entry.
        let synth = self.hir_synth_calls.is_some().then(|| self.hir_synth_callees.get(&call.key).cloned()).flatten();
        // A ground call of a raw instance method that returns a pair (a
        // `?` converting through `Into`) passes its own words and takes
        // the pair: the method's entry, not its `$dict` adapter.
        self.raw_instance_call = synth.as_ref().is_some_and(|s| s.raw) || (call.pair.is_some() && self.raw_instance_methods.contains(&call.key));
        let target = self.named_entry_target(&call.key, true);
        // A shared generic's call is plain, its dictionaries the ones
        // `plan_call_site_dicts` plans (none forwarded through a slot).
        let dicts = match (call.generic.as_deref(), synth) {
            (None, Some(synth)) => self.hir_mir_dict_program(&synth.dicts),
            (None, None) => Some(Vec::new()),
            (Some(g), _) if self.hir_generic_boxing(hir, emit, args, &call.params) => self.hir_mir_dict_ops(g),
            (Some(_), _) => None,
        };
        // Bare type parameter arguments as their words and the result read
        // back from its word.
        let tag = |ty: &Option<Ty>| ty.as_ref().and_then(Self::ty_to_value_tag).is_some();
        let (words, word_ret) = call.generic.as_deref().map_or((Vec::new(), false), |g| (g.boxed.iter().map(tag).collect(), tag(&g.unbox)));
        if let Some(dicts) = dicts.filter(|d| (d.is_empty() && !word_ret && Self::hir_mir_ret_items(call).is_none()) || !tail)
            && let Some(boxed_ret) = self.hir_ret_shape(hir, id, &natural)
            && let Some(target) = target
        {
            if call.generic.is_some() {
                for (&arg, &param) in args.iter().zip(&call.params) {
                    if !self.hir_arg_passes(hir, emit, arg, param)
                        && let Some(layout) = self.hir_niche_arg(hir, emit, arg, param)
                    {
                        t.niche_args.push((arg.0, layout));
                    }
                }
            }
            let adapt = self.hir_mir_adapters(hir, call, args);
            let pair_args = (0..args.len()).map(|i| Self::hir_arg_rep(call, i).words() == 2).collect();
            // A `TailCall`'s callee returns for this function, so in this
            // function's words, whatever the call's own rep.
            let pair_ret = if tail { matches!(emit.plan.ret, Rep::Pair(_)) } else { natural.words() == 2 };
            t.call = Some(crate::mir::HirCallSite {
                    target,
                    pair_args,
                    pair_ret,
                    coro,
                    boxed_ret,
                    dicts,
                    words,
                    word_ret,
                    adapt,
                    ret_adapt: self.hir_mir_ret_adapter(call),
                    ret_items: Self::hir_mir_ret_items(call),
                },
            );
        }
    }

    /// The arguments of a call through `PolyFn` local `f` (bound from
    /// generic function `source`): which go as their words, and their
    /// types. It warns of each two-word argument a bare type parameter
    /// takes boxed (the boxed-residue lint).
    fn hir_polyfn_args(&self, hir: &HirBody, emit: &HirEmit, source: Option<&str>, f: HirId, args: &[HirId]) -> Option<(Vec<bool>, Vec<Ty>)> {
        // Only a bare type-parameter parameter is retyped to a word; the
        // shared body takes a concrete one as its own word (#1089).
        let bare = self.hir_polyfn_bare_params(hir, source, f, args.len())?;
        let mut words = Vec::with_capacity(args.len());
        let mut arg_tys = Vec::with_capacity(args.len());
        for (i, &arg) in args.iter().enumerate() {
            // Emission passes the boxed-layout word: the MIR word only when
            // that is the argument's own, or, where a bare type parameter
            // takes it boxed, a two-word value's or niche enum's object
            // (#1479).
            match self.hir_natural(hir, emit, arg) {
                Some(Rep::Word(ValueLayout::Boxed)) => {}
                // (An inlined call's block has no natural rep: its value's
                // words are taken as MIR lowers them.)
                Some(Rep::Pair(_) | Rep::Word(_)) | None if bare[i] => {}
                _ => return None,
            }
            let ty = apply_ty_prune(self.checker.subst(), Self::hir_ty(hir, arg)?);
            if bare[i] {
                let span = hir.expr(arg).span;
                self.note_box_residue(
                    format!("polyfn {}", span.0),
                    &ty,
                    span,
                    "a `forall` function value's shared body takes it as one word",
                    "call a named generic function instead: its clone at this type takes both words",
                );
            }
            words.push(bare[i] && Self::ty_to_value_tag(&ty).is_some());
            arg_tys.push(ty);
        }
        Some((words, arg_tys))
    }

    /// Call `id` through `PolyFn` local `f`: which arguments go as their
    /// words, the call site's dictionaries and whether the result comes
    /// back as its word.
    fn hir_mir_polyfn_call(&mut self, hir: &HirBody, emit: &HirEmit, id: HirId, f: HirId, args: &[HirId], t: &mut crate::mir::HirSite) {
        let source = self.hir_polyfn_source(hir, emit, f);
        let Some((words, arg_tys)) = self.hir_polyfn_args(hir, emit, source.as_deref(), f, args) else {
            return;
        };
        let call_ty = Self::hir_ty(hir, id).map(|t| apply_ty_prune(self.checker.subst(), t));
        // The call site's dictionaries, as emission pushes them, in the
        // order of the value's type (#2636).
        let dicts = match &source {
            Some(source) => {
                let Some(scheme) = self.checker.env().lookup(source).cloned() else {
                    return;
                };
                let steps = self.hir_mir_once(|this| {
                    let mut steps = Vec::new();
                    this.plan_value_call_dicts(&mut steps, (&scheme.bounds, &scheme.ty, &scheme.constraints), &arg_tys, call_ty.as_ref());
                    steps
                });
                let Some(dicts) = self.hir_mir_dict_program(&steps) else {
                    return;
                };
                dicts
            }
            // A `forall T: Show. …` parameter: its own constraints at the
            // call's types (#1188).
            None => match Self::hir_forall_param_ty(hir, &self.checker, f) {
                Some(Ty::Forall { bounds, constraints, body }) if !constraints.is_empty() => {
                    let steps = self.hir_mir_once(|this| {
                        let mut steps = Vec::new();
                        this.plan_value_call_dicts(&mut steps, (&bounds, &body, &constraints), &arg_tys, call_ty.as_ref());
                        steps
                    });
                    let Some(dicts) = self.hir_mir_dict_program(&steps) else {
                        return;
                    };
                    dicts
                }
                _ => Vec::new(),
            },
        };
        let unbox = match &source {
            Some(source) => self.generic_return_depends_on_type_param(source),
            None => Self::hir_polyfn_returns_var(hir, &self.checker, f),
        };
        // A closure result, adapted from the shared body's boxed ABI to the
        // call's ground type (#1589).
        let (unbox, ret_adapt) = match self.hir_polyfn_ret_adapt(hir, emit, id, f, args.len()) {
            None => (unbox, None),
            Some(Ok(adapt)) => (false, adapt.map(|adapt| self.hir_closure_adapter(&adapt))),
            Some(Err(_)) => return,
        };
        let word_ret = unbox && call_ty.as_ref().and_then(Self::ty_to_value_tag).is_some();
        // The bits of the value's type variables after the dictionaries
        // (#2506, #2621): the function's own type for a local bound from
        // it, the `forall`'s for a parameter. The value maps them to its
        // entry's `kinds` word.
        let mut dicts = dicts;
        match &source {
            Some(source) => {
                if let Some(scheme) = self.kinds_scheme(source) {
                    dicts.extend(self.value_kinds_ops(&scheme.bounds, &scheme.ty, &arg_tys, call_ty.as_ref()));
                }
            }
            None => {
                if let Some(Ty::Forall { bounds, body, .. }) = Self::hir_forall_param_ty(hir, &self.checker, f) {
                    dicts.extend(self.value_kinds_ops(&bounds, &body, &arg_tys, call_ty.as_ref()));
                }
            }
        }
        t.poly_call = Some(crate::mir::HirPolyCall { words, dicts, word_ret, ret_adapt });
    }

    /// Whether each of a `PolyFn` call's `n` parameters is a bare type
    /// variable: from the source function's scheme, else the local's own
    /// un-instantiated type.
    fn hir_polyfn_bare_params(&self, hir: &HirBody, source: Option<&str>, f: HirId, n: usize) -> Option<Vec<bool>> {
        let params = match source {
            Some(source) => self.checker.fn_param_tys(source)?,
            None => {
                let HirKind::Local(local) = hir.expr(f).kind else {
                    return None;
                };
                let mut ty = apply_ty_prune(self.checker.subst(), hir.local(local).ty.as_ref()?);
                while let Ty::Forall { body, .. } = ty {
                    ty = *body;
                }
                let mut params = Vec::new();
                while let Ty::Fun(p, ret) = ty {
                    params.push(*p);
                    ty = *ret;
                }
                params
            }
        };
        (params.len() >= n).then(|| params[..n].iter().map(|p| matches!(p, Ty::Var(_))).collect())
    }

    /// The adapter a shared generic body's returned closure is wrapped in
    /// at the call's ground type (#1532): `MakeFn` of its entry (the
    /// label, then the operand) over the closure.
    fn hir_mir_ret_adapter(&mut self, call: &HirCall) -> Option<(crate::bodies::Label, u32)> {
        let adapt = call.generic.as_ref().filter(|_| !call.mono)?.ret_adapt.as_ref().ok()?.as_ref()?;
        Some(self.hir_closure_adapter(adapt))
    }

    /// The items of a shared generic body's result object it holds boxed,
    /// unboxed at the call (#1549).
    fn hir_mir_ret_items(call: &HirCall) -> Option<crate::mir::HirRetItems> {
        call.generic.as_ref().filter(|_| !call.mono)?.ret_items.clone().ok()?
    }

    /// Each argument's closure adapter: a function value the generic
    /// callee calls with boxed arguments. A lambda literal unboxes them itself.
    fn hir_mir_adapters(&mut self, hir: &HirBody, call: &HirCall, args: &[HirId]) -> Vec<Option<(crate::bodies::Label, u32)>> {
        // A shared body (not a clone at ground types) reads an open enum
        // result of a function argument as its object (#2163).
        let open = if call.mono || call.builtin.is_some() { vec![None; args.len()] } else { self.hir_open_ret_adapts(hir, &call.key, args) };
        // A compiler-made body's call as it planned it (a method clone's
        // thunk calling the shared body, #2182).
        let synth = || self.hir_synth_calls.as_ref().and(self.hir_synth_callees.get(&call.key)).map(|c| c.adapt.clone());
        let generic = call.generic.as_deref().map(|g| g.adapt.clone()).or_else(synth).unwrap_or_default();
        if generic.is_empty() && open.iter().all(Option::is_none) {
            return Vec::new();
        }
        let adapts: Vec<Option<super::emit_hir::HirFnAdapt>> = (0..args.len())
            .map(|i| match (generic.get(i).cloned().flatten(), open.get(i).cloned().flatten()) {
                (Some(g), _) if g.ret.is_some() => Some(g),
                (Some(g), Some(o)) if g.unbox.len() == o.unbox.len() => Some(super::emit_hir::HirFnAdapt { ret: o.ret, ..g }),
                (g, o) => g.or(o),
            })
            .collect();
        args.iter()
            .zip(&adapts)
            .map(|(&arg, adapt)| match adapt {
                Some(adapt) if !matches!(hir.expr(arg).kind, HirKind::Lambda { .. }) => Some(self.hir_fn_adapter(adapt)),
                // A lambda literal unboxes its own parameters; its niche
                // result is still boxed by an adapter.
                Some(adapt) if adapt.ret.is_some() => {
                    let only_ret = super::emit_hir::HirFnAdapt { unbox: vec![None; adapt.unbox.len()], ret: adapt.ret };
                    Some(self.hir_fn_adapter(&only_ret))
                }
                _ => None,
            })
            .collect()
    }

    /// Whether a call to the shared body of a generic is the arguments'
    /// own words (each boxed where the body takes a bare type parameter; a
    /// function value an adapter wraps, or a lambda literal unboxing its
    /// own parameters, its own word too). Direct MIR calls the same entry
    /// then, its result unboxed as emission unboxes it;
    /// [`Self::hir_mir_dict_ops`] says whether it can pass the
    /// dictionaries.
    fn hir_generic_boxing(&self, hir: &HirBody, emit: &HirEmit, args: &[HirId], params: &[ValueLayout]) -> bool {
        args.len() == params.len()
            && args.iter().zip(params).all(|(&arg, &param)| {
                // Each argument goes from its own word, or boxed from a
                // pair (a tagged one then boxed to its tag), as a plain
                // call passes them.
                self.hir_arg_passes(hir, emit, arg, param) || self.hir_niche_arg(hir, emit, arg, param).is_some()
            })
    }

    /// The niche layout of argument `id` that emission passes in boxed
    /// `param` as its enum object (at the boxed layout).
    fn hir_niche_arg(&self, hir: &HirBody, emit: &HirEmit, id: HirId, param: ValueLayout) -> Option<ValueLayout> {
        if param != ValueLayout::Boxed {
            return None;
        }
        if let Some(layout) = self.hir_boxed_niche_call(hir, emit, id) {
            return Some(layout);
        }
        let layout = self.value_layout(&apply_ty_prune(self.checker.subst(), Self::hir_ty(hir, id)?));
        let natural = self.hir_natural(hir, emit, id);
        (layout != ValueLayout::Boxed && natural.is_none_or(|n| n == Rep::Word(layout))).then_some(layout)
    }

    /// The niche layout of call `id` whose `Option` / `Result` result
    /// comes back as its boxed enum object (a generic's shared entry,
    /// #2154). Direct MIR reads that result as its niche word, so in a
    /// boxed parameter it goes as the object the call returned, and in a
    /// parameter of its niche layout as that word.
    fn hir_boxed_niche_call(&self, hir: &HirBody, emit: &HirEmit, id: HirId) -> Option<ValueLayout> {
        if !matches!(hir.expr(id).kind, HirKind::Call { .. }) || self.hir_natural(hir, emit, id) != Some(Rep::Word(ValueLayout::Boxed)) {
            return None;
        }
        let layout = self.value_layout(&apply_ty_prune(self.checker.subst(), Self::hir_ty(hir, id)?));
        matches!(layout, ValueLayout::NicheOption | ValueLayout::NicheResult | ValueLayout::NicheUnitResult).then_some(layout)
    }

    /// Whether direct MIR passes argument `id` in `param` as HIR emission
    /// does: its own word in that layout, or a numeric range's two words
    /// boxed to its typed object (MIR's `box_pair_val`).
    fn hir_arg_word(&self, hir: &HirBody, emit: &HirEmit, id: HirId, param: ValueLayout) -> bool {
        (self.hir_natural(hir, emit, id) == Some(Rep::Word(param)) && self.hir_boxed_niche_call(hir, emit, id).is_none())
            || (param == ValueLayout::Boxed
                && Self::hir_ty(hir, id).is_some_and(|ty| {
                    let ty = apply_ty_prune(self.checker.subst(), ty);
                    crate::layout::two_word_range_kind(&ty).is_some()
                }))
    }

    /// The dictionaries a call of `generic` passes, as a program of the
    /// steps: the ones it forwards from this body's dictionary
    /// parameters, then the call site's. `None` when one is forwarded
    /// from another frame slot, or a step is not one of
    /// [`crate::mir::HirDictOp`].
    fn hir_mir_dict_ops(&mut self, generic: &HirGeneric) -> Option<Vec<crate::mir::HirDictOp>> {
        let mut ops = Vec::new();
        for &i in &generic.forwarded {
            if let Some(slot) = self.frame_dict(i) {
                if !self.hir_mir_dict_param(slot) {
                    return None;
                }
                ops.push(crate::mir::HirDictOp::Load(slot));
            }
        }
        let steps = self.hir_mir_once(|this| {
            let mut steps = Vec::new();
            this.plan_call_site_dicts(&mut steps, &generic.lookup, &generic.arg_tys, Some(&generic.ret_ty));
            steps
        });
        ops.extend(self.hir_mir_dict_program(&steps)?);
        Some(ops)
    }

    /// The dictionaries a call passes for a trait method's own bounds, as
    /// a program ([`super::HirOwnDict`]): `None` when
    /// one is a frame dictionary that is no parameter, or a ground one has
    /// no instance dictionary.
    fn hir_mir_own_dicts(&mut self, own: &[super::HirOwnDict]) -> Option<Vec<crate::mir::HirDictOp>> {
        if own.is_empty() {
            return Some(Vec::new());
        }
        let mut steps = Vec::new();
        let mut kinds = Vec::new();
        for dict in own {
            match dict {
                super::HirOwnDict::Kinds(ops) => kinds.extend(ops.iter().cloned()),
                super::HirOwnDict::Frame(slot) => {
                    if !self.hir_mir_dict_param(*slot) {
                        return None;
                    }
                    steps.push(DictStep::Load(*slot));
                }
                super::HirOwnDict::Ground { class, lookup } => {
                    let (class, lookup) = (class.clone(), lookup.clone());
                    if !self.hir_mir_once(|this| this.plan_instance_dict(&mut steps, &class, &lookup)) {
                        return None;
                    }
                }
            }
        }
        let mut ops = self.hir_mir_dict_program(&steps)?;
        ops.extend(kinds);
        Some(ops)
    }

    /// Whether frame slot `slot` is one of this body's dictionary
    /// parameters (a `__dictN` below its entry words), which direct MIR
    /// reads as that parameter. An instance's context dictionaries the
    /// prologue loads out of `__dict0` are not.
    fn hir_mir_dict_param(&self, slot: u32) -> bool {
        let Some(sp) = self.hir_mir_dict_sp else {
            return false;
        };
        (slot < sp && (0..).map_while(|i| self.frame_dict(i)).any(|s| s == slot))
            || self.hir_body_frame.as_ref().is_some_and(|f| f.context.iter().any(|&(s, own, _)| s == slot && own < sp))
    }

    /// [`Self::hir_dict_ops`], with every ground dictionary built once
    /// ([`Self::intern_dict_ops`]).
    fn hir_mir_dict_program(&mut self, steps: &[DictStep]) -> Option<Vec<crate::mir::HirDictOp>> {
        let ops = self.hir_dict_ops(steps)?;
        Some(self.intern_dict_ops(ops))
    }

    /// Dictionary plan `steps` as a [`crate::mir::HirDictOp`] program,
    /// when direct MIR has every step: a code pointer to an entry with a
    /// label, and only this body's own dictionary parameters.
    fn hir_dict_ops(&self, steps: &[DictStep]) -> Option<Vec<crate::mir::HirDictOp>> {
        use crate::mir::HirDictOp;
        steps
            .iter()
            .map(|&step| match step {
                DictStep::CodeAt(offset) => self.bodies.entry_label_for_offset(offset as usize).map(HirDictOp::Code),
                DictStep::CodeLabel(label) => Some(HirDictOp::Code(label)),
                DictStep::Load(slot) => self.hir_mir_dict_param(slot).then_some(HirDictOp::Load(slot)),
                DictStep::Null => Some(HirDictOp::Null),
                DictStep::Tuple(n) => Some(HirDictOp::Tuple(n)),
                DictStep::Word => Some(HirDictOp::Word),
                DictStep::Kinds(bits) => Some(HirDictOp::Kinds(bits)),
                DictStep::KindsBit { from, to } => Some(HirDictOp::KindsBit { from, to }),
                DictStep::KindsOwn => Some(HirDictOp::KindsOwn),
            })
            .collect()
    }

    /// A dictionary made only of method pointers and such dictionaries is
    /// the same value at every run: build it once into a static slot
    /// instead of a tuple per call. Each maximal one becomes
    /// [`crate::mir::HirDictOp::Interned`].
    fn intern_dict_ops(&mut self, ops: Vec<crate::mir::HirDictOp>) -> Vec<crate::mir::HirDictOp> {
        use crate::mir::HirDictOp;
        // Per stack word: the index in `out` where its ops start, and
        // whether it is ground.
        let mut stack: Vec<(usize, bool)> = Vec::new();
        let mut out: Vec<HirDictOp> = Vec::with_capacity(ops.len());
        for op in ops {
            match op {
                HirDictOp::Code(_) => {
                    stack.push((out.len(), true));
                    out.push(op);
                }
                HirDictOp::Tuple(n) => {
                    let Some(at) = stack.len().checked_sub(n as usize) else {
                        out.push(op);
                        continue;
                    };
                    let elems = stack.split_off(at);
                    let start = elems.first().map_or(out.len(), |e| e.0);
                    if elems.iter().all(|e| e.1) {
                        let mut flat = Vec::new();
                        for o in out.drain(start..) {
                            match o {
                                HirDictOp::Interned { ops, .. } => flat.extend(ops),
                                other => flat.push(other),
                            }
                        }
                        flat.push(HirDictOp::Tuple(n));
                        let slot = self.interned_dict_slot(flat.clone());
                        out.push(HirDictOp::Interned { slot, ops: flat });
                        stack.push((start, true));
                    } else {
                        out.push(op);
                        stack.push((start, false));
                    }
                }
                HirDictOp::Word | HirDictOp::KindsBit { .. } => {
                    let start = stack.pop().map_or(out.len(), |e| e.0);
                    out.push(op);
                    stack.push((start, false));
                }
                // A constant `kinds` word: a ground dictionary still is.
                HirDictOp::Kinds(_) => {
                    stack.push((out.len(), true));
                    out.push(op);
                }
                HirDictOp::Null | HirDictOp::Load(_) | HirDictOp::Interned { .. } | HirDictOp::KindsOwn => {
                    stack.push((out.len(), false));
                    out.push(op);
                }
            }
        }
        out
    }

    /// The static slot that holds ground dictionary program `flat`.
    fn interned_dict_slot(&mut self, flat: Vec<crate::mir::HirDictOp>) -> u32 {
        if let Some(&slot) = self.interned_dicts.get(&flat) {
            return slot;
        }
        let next = self.interned_dicts.len();
        let slot = self
            .checker
            .alloc_synthetic_static_slot(format!("__dict${next}"), crate::typechecking::ty::int());
        self.interned_dicts.insert(flat, slot);
        slot
    }


    /// Whether argument `id` of a method or ground instance call, emitted
    /// as `Rep::Word(param)`, is what direct MIR's plain call passes: its
    /// own word ([`Self::hir_arg_word`]), or a two-word value (or a
    /// constructor) of a type whose word is `param`, which both box the
    /// same way (MIR's `box_pair_val`, emission's pair-to-word convert).
    fn hir_arg_passes(&self, hir: &HirBody, emit: &HirEmit, id: HirId, param: ValueLayout) -> bool {
        self.hir_arg_word(hir, emit, id, param)
            || self.hir_boxed_niche_call(hir, emit, id) == Some(param)
            || (matches!(self.hir_natural(hir, emit, id), None | Some(Rep::Pair(_)))
                && Self::hir_ty(hir, id).is_some_and(|ty| self.value_layout(&apply_ty_prune(self.checker.subst(), ty)) == param))
    }

    /// Whether the code emission writes for `id` itself (not its operands)
    /// reads one of the frame's dictionaries (`__dictN`): a bound operator,
    /// bound call or bound `%v`, a generic call's forwarded dictionaries,
    /// or a dictionary the call site's helpers load from the frame
    /// (instance, call-site, `PolyFn` escape and existential pack
    /// dictionaries, run into a scratch buffer as emission runs them).
    fn hir_mir_dict_reads(&mut self, hir: &HirBody, emit: &HirEmit, id: HirId) -> bool {
        // A value packed after it is built.
        if let Some(pack) = self.hir_existential_pack(hir, id)
            && self.hir_mir_probe_unplanned(|this, steps| this.plan_existential_pack(steps, &pack))
        {
            return true;
        }
        let e = hir.expr(id);
        let span = e.span.0..e.span.1;
        match &e.kind {
            HirKind::Bin { .. } | HirKind::Un { .. } => match emit.plan.ops.get(&id.0) {
                Some(&HirOp::Bound { dict, .. }) => !self.hir_mir_dict_param(dict),
                Some(HirOp::Glue(glue)) => glue.slots().any(|slot| !self.hir_mir_dict_param(slot)),
                Some(HirOp::Call { lookup, fqn, class, method }) => {
                    let lookup = lookup.clone();
                    let fqn = self.instance_clone(fqn, std::slice::from_ref(&lookup)).unwrap_or_else(|| fqn.clone());
                    // Its dictionary runs as a program (#1661).
                    self.hir_mir_probe_unplanned(|this, steps| {
                        this.plan_call_instance_dict(steps, (class, method, &fqn), std::slice::from_ref(&lookup), span);
                    })
                }
                _ => false,
            },
            // A closure's captured dictionaries (#1526).
            HirKind::Lambda { .. } if let Some(lam) = emit.plan.lambdas.get(&id.0) => {
                lam.dicts.iter().any(|&(_, slot)| !self.hir_mir_dict_param(slot))
            }
            // A `PolyFn` value's escape dictionaries.
            HirKind::Global { .. } if let Some(name) = emit.plan.polyfns.get(&id.0) => {
                let ty = Self::hir_ty(hir, id).map(|t| apply_ty_prune(self.checker.subst(), t));
                let expected = self.checker.rank_n_arg_at(e.span.0, e.span.1);
                self.hir_mir_probe_unplanned(|this, steps| {
                    this.plan_polyfn_escape_dicts(steps, name, ty.as_ref(), expected.as_ref());
                })
            }
            // A call through a `PolyFn` local: the call site's dictionaries
            // for the generic function it was bound from.
            HirKind::Call { callee: Callee::Value(f), args } if query::polyfn_callee(hir, &self.checker, *f) => {
                let Some(source) = self.hir_polyfn_source(hir, emit, *f) else {
                    return false;
                };
                let arg_tys: Vec<Ty> = args.iter().filter_map(|&a| Self::hir_ty(hir, a)).map(|t| apply_ty_prune(self.checker.subst(), t)).collect();
                let call_ty = Self::hir_ty(hir, id).map(|t| apply_ty_prune(self.checker.subst(), t));
                let Some(scheme) = self.checker.env().lookup(&source).cloned() else {
                    return false;
                };
                self.hir_mir_probe_unplanned(|this, steps| {
                    this.plan_value_call_dicts(steps, (&scheme.bounds, &scheme.ty, &scheme.constraints), &arg_tys, call_ty.as_ref());
                })
            }
            HirKind::Call { args, .. } if !emit.plan.lens.contains_key(&id.0) && let Some(call) = emit.plan.calls.get(&id.0) => {
                if call.own_dicts.iter().any(|d| matches!(d, super::HirOwnDict::Frame(slot) if !self.hir_mir_dict_param(*slot))) {
                    return true;
                }
                match &call.builtin {
                    Some(HirBuiltin::Bound { dict, .. }) => return !self.hir_mir_dict_param(*dict),
                    Some(HirBuiltin::Format) => return self.hir_mir_format_dict_reads(hir, args),
                    Some(_) => return false,
                    None => {}
                }
                if call.key == format!("{}::push", common::BUILTIN_VEC_TYPE) {
                    return false;
                }
                if let Some(inst) = call.instance.as_ref().filter(|i| i.ground.is_some()) {
                    let method = &inst.ground.as_ref().expect("ground call").method;
                    // The entry [`Self::hir_mir_call`] calls (#1595).
                    let key = match self.hir_ret_shape(hir, id, &Self::hir_call_rep(call)) {
                        Some(true) => call.key.clone(),
                        _ => self.ground_instance_clone(call, &inst.args).unwrap_or_else(|| call.key.clone()),
                    };
                    return self.hir_mir_probe_unplanned(|this, steps| {
                        this.plan_call_instance_dict(steps, (&inst.class, method, &key), &inst.args, span);
                    });
                }
                if !call.method && self.hir_par_call_plan(hir, &call.key, args).is_some() {
                    return false;
                }
                if let Some(generic) = call.generic.as_deref()
                    && self.hir_mir_generic_dict_reads(generic)
                {
                    return true;
                }
                if !call.method {
                    return false;
                }
                // A non-raw instance method's instance dictionary.
                let Some(inst) = call.instance.as_deref() else {
                    return false;
                };
                let key = match inst.recv_box.clone() {
                    Some(recv) if call.generic.is_none() => self.ground_instance_clone(call, &[recv]).unwrap_or_else(|| call.key.clone()),
                    _ => call.key.clone(),
                };
                !self.raw_instance_methods.contains(&key)
                    && self.hir_mir_probe_unplanned(|this, steps| {
                        this.plan_entry_dict(steps, &inst.class, &inst.args);
                    })
            }
            _ => false,
        }
    }

    /// Whether a call of `generic` reads a frame dictionary: one it forwards, or one of the call site's.
    fn hir_mir_generic_dict_reads(&mut self, generic: &HirGeneric) -> bool {
        generic.forwarded.iter().any(|&i| self.frame_dict(i).is_some_and(|slot| !self.hir_mir_dict_param(slot)))
            || self.hir_mir_probe_unplanned(|this, steps| {
                this.plan_call_site_dicts(steps, &generic.lookup, &generic.arg_tys, Some(&generic.ret_ty));
            })
    }

    /// Whether a `format` call's `%v` arguments read a frame dictionary
    /// when they are shown.
    fn hir_mir_format_dict_reads(&mut self, hir: &HirBody, args: &[HirId]) -> bool {
        let HirKind::Lit(Lit::Str(fmt)) = &hir.expr(args[0]).kind else {
            return false;
        };
        let specs = Self::format_consuming_specs(fmt);
        args.iter().enumerate().skip(1).filter(|&(i, _)| specs.get(i - 1) == Some(&'v')).any(|(_, &arg)| {
            match self.hir_bound_show(hir, arg) {
                // A bound `show` reads the dictionary, which direct MIR
                // has when it is one of the body's parameters.
                Some((dict, _)) => !self.hir_mir_dict_param(dict),
                None => Self::hir_ty(hir, arg).is_some_and(|ty| self.hir_mir_show_dict_reads(&ty.clone())),
            }
        })
    }

    /// Whether showing a `%v` value of type `ty` reads a frame dictionary: a tuple's or record's parts, or the instance
    /// dictionary of the `show` it calls.
    fn hir_mir_show_dict_reads(&mut self, ty: &Ty) -> bool {
        match apply_ty_prune(self.checker.subst(), ty) {
            Ty::Tuple(items) => items.iter().any(|item| self.hir_mir_show_dict_reads(item)),
            Ty::Record { fields } => fields.iter().any(|(_, field)| self.hir_mir_show_dict_reads(field)),
            other => {
                let lookup_ty = Self::show_lookup_ty_for_instance(&other);
                let Some(fqn) = self.find_show_instance(&lookup_ty).and_then(|instance| instance.method_fqns.get("show").cloned()) else {
                    return false;
                };
                if !self.functions.contains_key(&fqn) && !self.fn_entry_labels.contains_key(&fqn) {
                    return false;
                }
                let fqn = self.instance_clone(&fqn, std::slice::from_ref(&lookup_ty)).unwrap_or(fqn);
                // Its dictionary runs as a program (#2479).
                self.hir_mir_probe_unplanned(|this, steps| {
                    this.plan_call_instance_dict(steps, ("Show", "show", &fqn), std::slice::from_ref(&lookup_ty), 0..0);
                })
            }
        }
    }

    /// Plan a site's dictionaries ([`Self::hir_mir_once`]), which direct
    /// MIR runs as a [`crate::mir::HirDictOp`] program: whether the plan
    /// reads a frame dictionary no program step stands for. The plan goes
    /// nowhere.
    fn hir_mir_probe_unplanned(&mut self, f: impl FnOnce(&mut Self, &mut Vec<DictStep>)) -> bool {
        let mut steps = Vec::new();
        self.hir_mir_once(|this| f(this, &mut steps));
        self.hir_mir_steps_read_dicts(&steps) && self.hir_dict_ops(&steps).is_none()
    }

    /// Whether `steps` load one of the frame's dictionaries.
    fn hir_mir_steps_read_dicts(&self, steps: &[DictStep]) -> bool {
        let dicts: HashSet<u32> = (0..).map_while(|i| self.frame_dict(i)).collect();
        steps.iter().any(|step| matches!(step, DictStep::Load(slot) if dicts.contains(slot)))
    }

    /// `f(self)`, an emission helper a site asks for its decision, with
    /// each diagnostic it reports kept once: one already reported (the
    /// same message at the same place, as the plan reports it when it runs
    /// the helper first) is not repeated.
    fn hir_mir_once<R>(&mut self, f: impl FnOnce(&mut Self) -> R) -> R {
        let before = self.messages.len();
        let r = f(self);
        for message in self.messages.split_off(before) {
            if !self.messages.contains(&message) {
                self.messages.push(message);
            }
        }
        r
    }

    /// A `format` call whose `%v` arguments each show as
    /// [`Self::hir_show_plan`] plans: its format string (`%v` as `%s`) and
    /// those plans.
    fn hir_mir_format(&mut self, hir: &HirBody, args: &[HirId], t: &mut crate::mir::HirSite) {
        let HirKind::Lit(Lit::Str(fmt)) = &hir.expr(args[0]).kind else {
            return;
        };
        let specs = Self::format_consuming_specs(fmt);
        let fmt = Self::rewrite_format_v_to_s(fmt);
        let mut shows = Vec::with_capacity(args.len().saturating_sub(1));
        let mut shows_ok = true;
        for (i, &arg) in args.iter().enumerate().skip(1) {
            if specs.get(i - 1) == Some(&'v') {
                let show = self.hir_show_plan(hir, arg);
                shows_ok &= show.is_some();
                shows.push(show);
            } else {
                shows.push(None);
            }
        }
        if shows_ok {
            let idx = self.intern_string(unescape_coil_string(&fmt));
            t.format = Some(idx);
            if specs.contains(&'v') {
                t.format_shows = Some(shows);
            }
        }
    }

    /// How a `format` shows `arg`: a bound dictionary's method
    /// when that dictionary is one of the body's parameters, else as
    /// [`Self::hir_show_ty`].
    fn hir_show_plan(&mut self, hir: &HirBody, arg: HirId) -> Option<crate::mir::HirShow> {
        if let Some((dict, method)) = self.hir_bound_show(hir, arg) {
            return self.hir_mir_dict_param(dict).then_some(crate::mir::HirShow::Bound { dict, method });
        }
        let ty = apply_ty_prune(self.checker.subst(), Self::hir_ty(hir, arg)?);
        self.hir_show_ty(&ty)
    }

    /// How a `%v` value of type `ty` is shown, when
    /// each step is one direct MIR has: a tuple's or record's items shown
    /// and formatted, `STRINGIFY` with no `Show` instance, or one plain
    /// one-word `CALL` of the raw `show` entry on the boxed-layout or
    /// niche value (no boxing, conversion or dictionary). It interns the strings
    /// emission does.
    fn hir_show_ty(&mut self, ty: &Ty) -> Option<crate::mir::HirShow> {
        // A tuple or record with an item open in the body's type
        // parameters shows through the shape's instance with open items, as
        // its `.show()` does (#2614, #2631); a thunk's items have their own
        // dictionaries.
        let open = crate::typechecking::ty::ftv_ty(ty).iter().any(|v| !self.synth_show_dicts.contains_key(v));
        match ty {
            Ty::Tuple(items) if !open => {
                let parts = items.iter().map(|t| Some((t.clone(), self.hir_show_ty(t)?))).collect::<Option<Vec<_>>>()?;
                let fmt = self.intern_string(unescape_coil_string(&Self::tuple_show_format(parts.len())));
                return Some(crate::mir::HirShow::Product { fmt, parts, tuple: true });
            }
            Ty::Record { fields } if !open => {
                // In slot order: the fields sorted by name.
                let mut fields = fields.clone();
                fields.sort_by(|a, b| a.0.cmp(&b.0));
                let parts = fields.iter().map(|(_, t)| Some((t.clone(), self.hir_show_ty(t)?))).collect::<Option<Vec<_>>>()?;
                let fmt = self.intern_string(unescape_coil_string(&Self::record_show_format(&fields)));
                return Some(crate::mir::HirShow::Product { fmt, parts, tuple: false });
            }
            // An arity-wide tuple thunk's item: its own dictionary (#1479).
            Ty::Var(v) if let Some(&n) = self.synth_show_dicts.get(v) => {
                let dict = self.frame_dict(n)?;
                return Some(crate::mir::HirShow::Bound { dict, method: 0 });
            }
            _ => {}
        }
        // A niche `Option` / `Result` (`Some({ x: 1 })`, #1591) is its
        // niche word, which only a raw entry takes as it is.
        let niche = self.value_layout(ty) != ValueLayout::Boxed;
        let lookup_ty = Self::show_lookup_ty_for_instance(ty);
        let Some(fqn) = self
            .find_show_instance(&lookup_ty)
            .and_then(|inst| inst.method_fqns.get("show").cloned())
            .filter(|fqn| self.functions.contains_key(fqn) || self.fn_entry_labels.contains_key(fqn))
        else {
            return (!niche).then_some(crate::mir::HirShow::Stringify);
        };
        // A shape's instance with open items met at ground types (a
        // per-type clone's `%v` of a `Vec<T>`): the shape's ground instance,
        // registered now if no goal asked for it, which the dictionary
        // planned at these types is (#2613), as a clone's `.show()` takes.
        let fqn = match self.checker.generics().is_structural_open_fqn(&fqn) && !lookup_ty.has_free_var() {
            true => {
                if self.checker.ensure_ground_structural("Show", std::slice::from_ref(&lookup_ty)) {
                    self.reserve_structural_thunks();
                }
                self.ground_structural_fqn("Show", std::slice::from_ref(&lookup_ty), "show").unwrap_or(fqn)
            }
            false => fqn,
        };
        let fqn = self.instance_clone(&fqn, std::slice::from_ref(&lookup_ty)).unwrap_or(fqn);
        let raw = self.raw_instance_methods.contains(&fqn);
        if niche && !raw {
            return None;
        }
        // A non-raw `show` takes the value as its word.
        let word = !raw && Self::ty_to_value_tag(&lookup_ty).is_some();
        if self.two_word_return_kind(&fqn).is_some() {
            return None;
        }
        // The dictionary an entry that takes one is passed (a bounded
        // generic instance's shared entry, #2479), as a program.
        let mut steps = Vec::new();
        self.hir_mir_once(|this| this.plan_call_instance_dict(&mut steps, ("Show", "show", &fqn), std::slice::from_ref(&lookup_ty), 0..0));
        if steps.is_empty() && self.instance_call_takes_dict("Show", "show", &fqn) {
            return None;
        }
        let dicts = self.hir_mir_dict_program(&steps)?;
        self.raw_instance_call = raw;
        self.named_entry_target(&fqn, true).map(|target| crate::mir::HirShow::Call { target, word, dicts })
    }

    /// Decide, before `root` is emitted, which parallel loops under it move
    /// into their own fork-join functions ([`HirPlan::par_loops`]), as
    /// direct MIR lowers each site; every other loop runs sequentially.
    /// A site's enclosing locals
    /// are the ones bound when it runs: `emit`'s bound slots (parameters
    /// and captures, or a chunk worker's site locals), then each local
    /// bound before the site in emission order.
    pub(super) fn hir_plan_par_loops(&mut self, hir: &HirBody, emit: &mut HirEmit, root: HirId) {
        let mut bound: HashSet<u32> = (0..emit.slots.len() as u32).filter(|&l| emit.slots[l as usize].is_some()).collect();
        self.hir_plan_par_walk(hir, emit, root, &mut bound);
    }

    fn hir_plan_par_walk(&mut self, hir: &HirBody, emit: &mut HirEmit, id: HirId, bound: &mut HashSet<u32>) {
        // Walks recurse through here once per level of the tree.
        common::deep_stack(|| self.hir_plan_par_walk_level(hir, emit, id, bound))
    }

    fn hir_plan_par_walk_level(&mut self, hir: &HirBody, emit: &mut HirEmit, id: HirId, bound: &mut HashSet<u32>) {
        let bind_pat = |pat: &HirPat, bound: &mut HashSet<u32>| {
            pat.for_each_bind(&mut |l| {
                bound.insert(l.0);
            })
        };
        match &hir.expr(id).kind {
            // `while false { .. }` is dropped.
            HirKind::Loop { body }
                if query::while_shape(hir, *body).is_some_and(|(cond, _)| matches!(hir.expr(cond).kind, HirKind::Lit(Lit::Bool(false)))) => {}
            HirKind::Loop { body } if let Some((cond, then)) = query::while_shape(hir, *body) => {
                if !self.hir_plan_par_site(hir, emit, id, cond, None, then, bound) {
                    self.hir_plan_par_walk(hir, emit, cond, bound);
                    self.hir_plan_par_walk(hir, emit, then, bound);
                }
            }
            // A loop of any other shape (a `while` under contract checks,
            // a `while let`) is no site.
            HirKind::Loop { body } => {
                if self.loop_par_sites.contains_key(&hir.expr(id).span) {
                    self.hir_par_disagreement(hir, id, "it is not a plain `while`");
                }
                self.hir_plan_par_walk(hir, emit, *body, bound);
            }
            HirKind::ForIn {
                pat: HirPat::Bind(local),
                iterable,
                body,
                ..
            } if self.hir_plan_par_site(hir, emit, id, *iterable, Some(*local), *body, bound) => {}
            HirKind::ForIn { pat, iterable, body, .. } => {
                self.hir_plan_par_walk(hir, emit, *iterable, bound);
                bind_pat(pat, bound);
                self.hir_plan_par_walk(hir, emit, *body, bound);
            }
            HirKind::Let { local, init } => {
                if let Some(init) = init {
                    self.hir_plan_par_walk(hir, emit, *init, bound);
                    if !classify::is_unit_local(hir, &self.checker, *local) {
                        bound.insert(local.0);
                    }
                }
            }
            HirKind::LetPat { pat, init } => {
                self.hir_plan_par_walk(hir, emit, *init, bound);
                bind_pat(pat, bound);
            }
            HirKind::Match { scrutinee, arms } => {
                self.hir_plan_par_walk(hir, emit, *scrutinee, bound);
                for arm in arms {
                    bind_pat(&arm.pat, bound);
                    self.hir_plan_par_walk(hir, emit, arm.body, bound);
                }
            }
            // The thunk's frame holds only the captures.
            HirKind::Defer { captures, body } => {
                let mut inner = captures.iter().flatten().map(|l| l.0).collect();
                self.hir_plan_par_walk(hir, emit, *body, &mut inner);
            }
            // The operand's type name; the operand is not evaluated.
            HirKind::Builtin { op: Builtin::TypeOf, .. } => {}
            _ => {
                for k in hir.children(id) {
                    self.hir_plan_par_walk(hir, emit, k, bound);
                }
            }
        }
    }

    /// Plan parallel-loop site `id` as direct MIR lowers it.
    /// `false` when it runs sequentially (its parts are then walked as
    /// any loop's).
    #[allow(clippy::too_many_arguments)]
    fn hir_plan_par_site(
        &mut self,
        hir: &HirBody,
        emit: &mut HirEmit,
        id: HirId,
        head: HirId,
        bind: Option<LocalId>,
        body: HirId,
        bound: &mut HashSet<u32>,
    ) -> bool {
        let locals = match self.hir_par_site(hir, &emit.plan, id, head, bind, body, &|l| bound.contains(&l.0)) {
            Ok(locals) => locals,
            Err(why) => {
                if let Some(why) = why {
                    self.hir_par_disagreement(hir, id, why);
                }
                return false;
            }
        };
        let Some(planned) = self.hir_par_outline(hir, id, &locals, bind, body) else {
            self.hir_par_disagreement(hir, id, "a word of it is not an `int` local");
            return false;
        };
        emit.plan.par_loops.insert(id.0, planned);
        true
    }
}
