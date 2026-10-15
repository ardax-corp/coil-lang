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

/// How many value operands `dload` / `declare` / `invoke` read before
/// the signature or argument tuple.
fn kind_arity(kind: crate::typechecking::FfiBuiltin) -> usize {
    match kind {
        crate::typechecking::FfiBuiltin::Dload => 1,
        _ => 2,
    }
}

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
    /// Per argument of such a `format` call (after the format string): how
    /// a `%v` argument is shown, `None` for a word.
    pub(super) format_shows: HashMap<u32, Vec<Option<crate::mir::HirShow>>>,
    /// `matrix(data)` calls, whose value is the data itself.
    pub(super) matrix_wraps: HashSet<u32>,
    /// Matrix / vector operators and `dot` / `matmul` calls on a packed
    /// kernel: the native and the
    /// meta word packing the shape, by expression.
    pub(super) linalgs: HashMap<u32, (u16, u32)>,
    /// `cross(a, b)` calls emission unrolls: whether the result is a
    /// tuple and whether its elements are floats, by call.
    pub(super) crosses: HashMap<u32, (bool, bool)>,
    /// `extern` calls and `dload` / `declare` / `invoke`
    /// (`HirBuiltin::Ffi` / `FfiDyn`): their opcode and operands, by call.
    pub(super) ffis: HashMap<u32, crate::mir::HirFfi>,
    /// `dot` / `matmul` calls and matrix operators emission unrolls (a
    /// shape past the packed kernel's limits), by expression.
    pub(super) unrolled_las: HashMap<u32, crate::typechecking::LinearAlgebraKind>,
    /// Fixed array locals copied as they are bound or assigned to another
    /// local ([`Compiler::hir_array_copy`]): the `vec_from_array` native,
    /// by value.
    pub(super) array_copies: HashMap<u32, u16>,
    /// Entry of each plain one-word-ABI `CALL` (no dictionaries), by call
    /// site: calls, and user operators on a raw instance method.
    pub(super) calls: HashMap<u32, crate::mir::HirCallSite>,
    /// User operators that call their instance method with boxed operands.
    pub(super) op_calls: HashMap<u32, crate::mir::HirOpCall>,
    /// User operators emitted as a raw opcode on the operands.
    pub(super) op_prims: HashSet<u32>,
    /// Calls through a function value emitted as a plain `CallIndirect`.
    pub(super) value_calls: HashSet<u32>,
    /// Calls through a `PolyFn` local with no dictionaries.
    pub(super) poly_calls: HashMap<u32, crate::mir::HirPolyCall>,
    /// Trait methods and operators on a bound type parameter through one
    /// of the body's dictionary parameters (`HirBuiltin::Bound`,
    /// `HirOp::Bound`), by call or operator.
    pub(super) bound_calls: HashMap<u32, crate::mir::HirBoundCall>,
    /// Trait methods on a bare-class existential pack
    /// (`HirBuiltin::Existential`): the method's dictionary slot, by call.
    pub(super) existential_calls: HashMap<u32, u32>,
    /// Calls and dictionary operators typed a bare type parameter in a
    /// shared generic body, whose result MIR unboxes
    /// (`UNBOX_ANY_TAG`).
    pub(super) unbox_any: HashSet<u32>,
    /// A shared generic body's parameters typed a bare type parameter,
    /// which its prologue unboxes (`UNBOX_ANY_TAG`).
    pub(super) open_params: Vec<LocalId>,
    /// Parallel loops the plan moved into their own fork-join functions,
    /// by loop: the direct MIR lowering's calls.
    pub(super) par_loops: HashMap<u32, crate::mir::HirParLoop>,
    /// Auto-par call sites sent to a fork-join worker, by call: the direct
    /// MIR lowering's calls.
    pub(super) par_forks: HashMap<u32, crate::mir::HirParFork>,
    /// Some value emission packs as a bare-class existential has no
    /// [`crate::mir::HirDictOp`] program in `packs`.
    pub(super) existential_packs: bool,
    /// Values emission packs as a bare-class existential, by expression:
    /// `emit_existential_pack_recipe` as a dictionary program run on the
    /// value's word.
    pub(super) packs: HashMap<u32, Vec<crate::mir::HirDictOp>>,
    /// Function values built with `MakeFn` / `MakePolyFn` (lambdas, named
    /// functions read as values and partial applications), by expression.
    pub(super) fn_values: HashMap<u32, crate::mir::HirFnValue>,
    /// Entry of the user `into_iter` a counted `for` loop calls, by loop.
    pub(super) into_iters: HashMap<u32, crate::il::Label>,
    /// The `into_iter` and `next` entries a `for` loop over a user
    /// iterator calls, by loop.
    pub(super) iter_loops: HashMap<u32, crate::mir::HirIterLoop>,
    /// Direct MIR's frame: the local id of each local's first word.
    pub(super) frame: HirMirFrame,
    /// Some code emission writes for this body reads one of the frame's
    /// dictionaries (`__dictN`) ([`Compiler::hir_mir_dict_reads`]).
    pub(super) reads_dicts: bool,
    /// The niche word of each `Result` variant made at a niche layout
    /// other than its own type's ([`Compiler::hir_mir_make_niches`]).
    pub(super) make_niches: HashMap<u32, crate::mir::MirTy>,
    /// The key of each field a record `let` pattern binds (`None`: a
    /// wildcard), by `let`: MIR pushes each as a string.
    pub(super) record_pats: HashMap<u32, Vec<Option<u32>>>,
    /// Shared generic call arguments of a niche layout passed in a boxed
    /// parameter ([`Compiler::hir_niche_arg`]): boxed to their enum
    /// object, by argument.
    pub(super) niche_args: HashMap<u32, ValueLayout>,
    /// `block_on(h)` calls: the `wait_ready`
    /// native run between resumes, if the program has one, by call.
    pub(super) block_ons: HashMap<u32, Option<u16>>,
    /// `new C(args).f` reads emission elides the object of
    /// ([`Compiler::hir_field_of_new`]): each argument runs in order and
    /// `f`'s is the value, by field read.
    pub(super) field_news: HashSet<u32>,
}

/// The frame direct MIR keeps a body's locals in, planned over the HIR:
/// each parameter where the entry put it, then each local the body binds,
/// in local order, one id per word (a stack array's elements, a frame-slot
/// class local's fields, a pair local's two words).
#[derive(Default)]
pub(super) struct HirMirFrame {
    /// First id of each local that has words (`None`: never bound here).
    pub(super) slots: Vec<Option<u32>>,
    /// Second id of each two-word (pair) local.
    pub(super) tags: HashMap<u32, u32>,
    /// Words of each bound local.
    pub(super) widths: HashMap<u32, u32>,
}

/// Which children of an expression emission evaluates.
enum Kids {
    All,
    None,
    /// All but the first (a `format` call's literal format string).
    Rest,
}

impl Compiler {
    /// The side tables direct MIR lowers `hir` with.
    pub(super) fn hir_mir_tables(&mut self, hir: &HirBody, emit: &HirEmit) -> HirMirTables {
        let mut tables = HirMirTables::default();
        self.hir_mir_intern_keys(hir);
        if let Some(root) = hir.root {
            self.hir_mir_walk(hir, emit, root, &mut tables);
            self.hir_mir_make_niches(hir, emit, root, None, &mut tables.make_niches);
        }
        tables.frame = self.hir_mir_frame(hir, emit);
        // As `lower_hir_body`'s prologue unboxes them.
        if self.hir_shared_generic() {
            tables.open_params = hir.params.iter().copied().filter(|&p| self.hir_open_ty(hir.local(p).ty.as_ref())).collect();
        }
        tables
    }

    /// Intern the strings direct MIR reads by name that `hir`'s code
    /// pushes: record keys (a record literal's, a field read or written by
    /// name) and the backing string of each string-backed scalar enum
    /// variant it makes or matches.
    fn hir_mir_intern_keys(&mut self, hir: &HirBody) {
        let mut variants: Vec<(String, String)> = Vec::new();
        fn pat_variants(pat: &HirPat, out: &mut Vec<(String, String)>) {
            match pat {
                HirPat::Variant { enum_name, variant, fields, .. } => {
                    out.push((enum_name.clone(), variant.clone()));
                    match fields {
                        HirPatFields::Unit => {}
                        HirPatFields::Tuple(ps) => ps.iter().for_each(|p| pat_variants(p, out)),
                        HirPatFields::Record(ps) => ps.iter().for_each(|(_, p)| pat_variants(p, out)),
                    }
                }
                HirPat::Tuple(ps) => ps.iter().for_each(|p| pat_variants(p, out)),
                HirPat::Record(ps) => ps.iter().for_each(|(_, p)| pat_variants(p, out)),
                HirPat::Wild | HirPat::Bind(_) | HirPat::Int(_) => {}
            }
        }
        for e in &hir.exprs {
            match &e.kind {
                HirKind::Make { kind: MakeKind::Record(names), .. } => {
                    for name in names {
                        self.intern_string(name);
                    }
                }
                HirKind::Field { base, name } if matches!(self.hir_field(hir, *base, name), Some((FieldAt::Name, _))) => {
                    self.intern_string(name);
                }
                HirKind::Make { kind: MakeKind::Variant { enum_name, variant, .. }, .. } => {
                    variants.push((enum_name.clone(), variant.clone()));
                }
                HirKind::Match { arms, .. } => arms.iter().for_each(|a| pat_variants(&a.pat, &mut variants)),
                HirKind::LetPat { pat, .. } => pat_variants(pat, &mut variants),
                _ => {}
            }
        }
        for (enum_name, variant) in variants {
            if let Some(crate::typechecking::ty::ScalarBacking::String(s)) = self.checker.scalar_for(&enum_name, &variant).cloned() {
                self.intern_string(unescape_coil_string(&s));
            }
        }
    }

    /// Record the `Result` variants under `id` that emission makes at a
    /// niche layout other than their own type's. `hir_check_make` builds
    /// a make at the layout its context wants, and a `Result::Ok(())` is
    /// typed `Result<(), E>` (a `NicheResult`, `Ok` the `()` tuple) even
    /// where a `Result<unit, E>` (a `NicheUnitResult`, `Ok` the zero word)
    /// is wanted: returned from a `-> Result<unit, E>` body, or as the
    /// value of an inlined call. The wanted layout reaches a value as
    /// `hir_check_value` passes it: a `return`'s value is wanted as the
    /// body returns it, a block's tail, an `if`'s arms and a `match`'s
    /// arms as their parent is; any other value at its own type's layout.
    fn hir_mir_make_niches(&self, hir: &HirBody, emit: &HirEmit, id: HirId, want: Option<ValueLayout>, out: &mut HashMap<u32, crate::mir::MirTy>) {
        use ValueLayout as L;
        if emit.plan.par_loops.contains_key(&id.0) {
            return;
        }
        let own = Self::hir_ty(hir, id).map(|t| self.value_layout(t));
        let want = want.or(own);
        let kind = &hir.expr(id).kind;
        if let HirKind::Make {
            kind: MakeKind::Variant { enum_name, .. },
            ..
        } = kind
            && common::is_builtin_result_enum(enum_name)
        {
            match (own, want) {
                (Some(L::NicheResult), Some(L::NicheUnitResult)) => {
                    out.insert(id.0, crate::mir::MirTy::NicheOpt);
                }
                (Some(L::NicheUnitResult), Some(L::NicheResult)) => {
                    out.insert(id.0, crate::mir::MirTy::NicheRes);
                }
                _ => {}
            }
        }
        let forwards: Vec<HirId> = match kind {
            HirKind::Block { tail: Some(tail), .. } => vec![*tail],
            HirKind::If { then, els: Some(els), .. } => vec![*then, *els],
            HirKind::Match { arms, .. } => arms.iter().map(|a| a.body).collect(),
            _ => Vec::new(),
        };
        // A stack array's literal init: each item into its own slot, in
        // the element's layout.
        let stack_init = match kind {
            HirKind::Let { local, init: Some(init) } => Some((*local, *init)),
            HirKind::Assign { place, value } => match hir.expr(*place).kind {
                HirKind::Local(local) => Some((local, *value)),
                _ => None,
            },
            _ => None,
        }
        .filter(|(local, _)| emit.plan.stacks.contains_key(&local.0));
        for k in lower::children(hir, id) {
            if let Some((local, init)) = stack_init
                && k == init
                && let HirKind::Make { args, .. } = &hir.expr(init).kind
            {
                let Rep::Word(elem) = self.hir_stack_rep(hir, local) else {
                    unreachable!("stack array elements are one word")
                };
                for &item in args {
                    self.hir_mir_make_niches(hir, emit, item, Some(elem), out);
                }
                continue;
            }
            let want = match kind {
                HirKind::Return(_) => match emit.plan.ret {
                    Rep::Word(l) => Some(l),
                    Rep::Pair(_) => None,
                },
                _ if forwards.contains(&k) => want,
                // An argument as `hir_check_value` wants it: in its
                // parameter's layout (a `Vec` element argument at the
                // element's niche).
                HirKind::Call { callee, args }
                    if !matches!(callee, Callee::Value(_))
                        && !emit.plan.lens.contains_key(&id.0)
                        && let Some(call) = emit.plan.calls.get(&id.0)
                        && call.builtin.is_none()
                        && let Some(i) = args.iter().position(|&a| a == k)
                        && i < call.params.len() =>
                {
                    match Self::hir_arg_rep(call, i) {
                        Rep::Word(l) => Some(l),
                        Rep::Pair(_) => None,
                    }
                }
                // A value stored by index, at the element slot's niche.
                HirKind::Assign { place, value } if *value == k && let HirKind::Index { base, .. } = hir.expr(*place).kind => {
                    match self.hir_index_store_rep(hir, emit, *place, base) {
                        Some(Rep::Word(l)) => Some(l),
                        _ => None,
                    }
                }
                _ => None,
            };
            self.hir_mir_make_niches(hir, emit, k, want, out);
        }
    }

    /// The frame of `hir` ([`HirMirFrame`]): the locals emission binds in
    /// this body's frame, from the HIR and the plan alone.
    fn hir_mir_frame(&self, hir: &HirBody, emit: &HirEmit) -> HirMirFrame {
        let mut bound = HashSet::new();
        // A lambda's body is emitted for its value ([`Self::hir_lambda_fn`]).
        if let Some(root) = hir.root {
            let value = hir.kind == crate::hir::BodyKind::Lambda;
            self.hir_mir_bound(hir, emit, root, value, false, &mut bound);
        }
        let mut frame = HirMirFrame {
            slots: vec![None; hir.locals.len()],
            ..HirMirFrame::default()
        };
        for (&local, &(slot, tag)) in &emit.plan.params {
            frame.slots[local as usize] = Some(slot);
            frame.widths.insert(local, 1);
            if let Some(tag) = tag {
                frame.tags.insert(local, tag);
            }
        }
        let inits: HashMap<u32, HirId> = hir
            .exprs
            .iter()
            .filter_map(|e| match e.kind {
                HirKind::Let { local, init: Some(init) } => Some((local.0, init)),
                _ => None,
            })
            .collect();
        let mut next = emit.plan.frame_base;
        for local in 0..hir.locals.len() as u32 {
            if emit.plan.params.contains_key(&local) || !bound.contains(&local) {
                continue;
            }
            let width = if let Some(&n) = emit.plan.stacks.get(&local) {
                n as u32
            } else if emit.plan.sroa.contains_key(&local) {
                inits
                    .get(&local)
                    .and_then(|&init| self.hir_new_layout(hir, init))
                    .map_or(1, |(_, tys)| tys.len() as u32)
            } else if emit.plan.pair_locals.contains_key(&local) {
                frame.tags.insert(local, next + 1);
                2
            } else {
                1
            };
            frame.slots[local as usize] = Some(next);
            frame.widths.insert(local, width);
            next += width;
        }
        frame
    }

    /// Collect the locals `id` binds in this body's frame, as emission
    /// binds them: not a `defer` body's (its thunk's frame), an outlined
    /// parallel loop's (its worker's), a `()` `let`'s, code emission drops,
    /// or a `match` arm's that emission reads off the operand stack.
    /// `value`: whether emission wants `id`'s value, not
    /// only its effect.
    fn hir_mir_bound(&self, hir: &HirBody, emit: &HirEmit, id: HirId, value: bool, defer: bool, bound: &mut HashSet<u32>) {
        if emit.plan.par_loops.contains_key(&id.0) {
            return;
        }
        let walk = |k: HirId, value: bool, defer: bool, bound: &mut HashSet<u32>| self.hir_mir_bound(hir, emit, k, value, defer, bound);
        match &hir.expr(id).kind {
            HirKind::Let { local, init } => {
                if !defer && init.is_some() && !lower::is_unit_local(hir, &self.checker, *local) {
                    bound.insert(local.0);
                }
            }
            HirKind::LetPat { pat, .. } if !defer => {
                Self::hir_pat_locals(pat, &mut |l| {
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
                if lower::while_shape(hir, *body).is_some_and(|(cond, _)| matches!(hir.expr(cond).kind, HirKind::Lit(Lit::Bool(false)))) =>
            {
                return;
            }
            HirKind::Loop { body } => {
                walk(*body, false, defer, bound);
                return;
            }
            HirKind::ForIn { pat, iterable, body, .. } => {
                if !defer {
                    Self::hir_pat_locals(pat, &mut |l| {
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
        for k in lower::children(hir, id) {
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
            pat => out.extend(lower::arm_fields(hir, pat).unwrap_or_default().into_iter().take(n).flatten()),
        };
        if lower::has_nested_test(arms) || lower::is_scalar_match(&self.checker, hir, scrutinee, arms) {
            for arm in arms {
                Self::hir_pat_locals(&arm.pat, &mut |l| out.push(l));
            }
            return out;
        }
        let Some(ty) = Self::hir_ty(hir, scrutinee) else {
            return out;
        };
        let identity = |arm: &HirArm| value && lower::is_identity_arm(hir, arm);
        let payloads = |variant: &str| self.hir_payload_tys(ty, variant).unwrap_or_default();
        match self.hir_dispatch_rep(hir, emit, scrutinee, arms) {
            Rep::Word(ValueLayout::Boxed) => {
                let in_place = lower::match_needs_slots(hir, arms);
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
                            let named = lower::arm_fields(hir, pat).is_ok_and(|f| f.iter().any(Option::is_some));
                            let reads = !payloads(side).is_empty() && (lower::is_identity_arm(hir, arm) || named);
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

    /// Each local `pat` binds.
    fn hir_pat_locals(pat: &HirPat, f: &mut impl FnMut(LocalId)) {
        match pat {
            HirPat::Bind(l) => f(*l),
            HirPat::Tuple(items) => items.iter().for_each(|p| Self::hir_pat_locals(p, f)),
            HirPat::Record(fields) => fields.iter().for_each(|(_, p)| Self::hir_pat_locals(p, f)),
            HirPat::Variant { fields, .. } => match fields {
                HirPatFields::Unit => {}
                HirPatFields::Tuple(items) => items.iter().for_each(|p| Self::hir_pat_locals(p, f)),
                HirPatFields::Record(fields) => fields.iter().for_each(|(_, p)| Self::hir_pat_locals(p, f)),
            },
            _ => {}
        }
    }

    fn hir_mir_walk(&mut self, hir: &HirBody, emit: &HirEmit, id: HirId, t: &mut HirMirTables) {
        // A parallel loop outlined into its fork-join function: its code is
        // not this body's.
        if let Some(par) = emit.plan.par_loops.get(&id.0) {
            t.par_loops.insert(id.0, par.call.clone());
            return;
        }
        if !t.reads_dicts {
            t.reads_dicts = self.hir_mir_dict_reads(hir, emit, id);
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

    /// Plan `id`'s own entries.
    fn hir_mir_note(&mut self, hir: &HirBody, emit: &HirEmit, id: HirId, t: &mut HirMirTables) -> Kids {
        // A value packed after it is built.
        if let Some(pack) = self.hir_existential_pack(hir, id) {
            let code = self.hir_mir_once(|this| {
                let mut code = CodeBuf::new();
                this.emit_existential_pack_recipe(&mut code, &pack);
                code
            });
            match self.hir_mir_dict_program(&code) {
                Some(ops) => {
                    t.packs.insert(id.0, ops);
                }
                None => t.existential_packs = true,
            }
        }
        let e = hir.expr(id);
        // A call's or dictionary operator's result unboxed.
        let produced = match e.kind {
            HirKind::Call { .. } => true,
            HirKind::Bin { .. } | HirKind::Un { .. } => matches!(emit.plan.ops.get(&id.0), Some(HirOp::Bound { .. })),
            _ => false,
        };
        if produced && self.hir_open_ty(e.ty.as_ref()) && self.hir_shared_generic() {
            t.unbox_any.insert(id.0);
        }
        // A bound type parameter's operator or method through one of this
        // body's dictionary parameters.
        let bound = match &e.kind {
            HirKind::Bin { .. } | HirKind::Un { .. } => match emit.plan.ops.get(&id.0) {
                Some(&HirOp::Bound { dict, method }) => Some((dict, method)),
                _ => None,
            },
            HirKind::Call { .. } if !emit.plan.lens.contains_key(&id.0) => match emit.plan.calls.get(&id.0).and_then(|c| c.builtin) {
                Some(HirBuiltin::Bound { dict, method }) => Some((dict, method)),
                _ => None,
            },
            _ => None,
        };
        if let Some((dict, method)) = bound
            && self.hir_mir_dict_param(dict)
        {
            t.bound_calls.insert(id.0, crate::mir::HirBoundCall { dict, method });
        }
        if let HirKind::Call { .. } = e.kind
            && !emit.plan.lens.contains_key(&id.0)
            && let Some(HirBuiltin::Existential { slot }) = emit.plan.calls.get(&id.0).and_then(|c| c.builtin)
        {
            t.existential_calls.insert(id.0, slot);
        }
        // A host call that needs a capability: the check finds its
        // `HostInvoke` in this body's MIR by the call's loc.
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
        if let HirKind::Field { base, name } = &e.kind
            && self.hir_field_of_new(hir, *base, name).is_some()
        {
            t.field_news.insert(id.0);
        }
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
                    t.fn_values.insert(id.0, crate::mir::HirFnValue { kind: crate::mir::MirAllocKind::Fn { target, operand, mask: 0 }, captures, dicts: Vec::new() });
                }
            }
            // A generic function's `MakePolyFn`, or with bounds its
            // `MakePolyFnCapture` over the dictionaries
            // `emit_polyfn_escape_dicts` pushes.
            HirKind::Global { .. } if let Some(name) = emit.plan.polyfns.get(&id.0) => {
                if let Some(target) = self.functions.get(name).and_then(|&entry| self.bytecode.entry_label_for_offset(entry)) {
                    let value = if self.checker.dict_arity_for(name) == 0 {
                        Some(crate::mir::HirFnValue { kind: crate::mir::MirAllocKind::PolyFn { target }, captures: Vec::new(), dicts: Vec::new() })
                    } else {
                        let (name, ty) = (name.clone(), Self::hir_ty(hir, id).map(|t| apply_ty_prune(self.checker.subst(), t)));
                        let code = self.hir_mir_once(|this| {
                            let mut code = CodeBuf::new();
                            this.emit_polyfn_escape_dicts(&mut code, &name, ty.as_ref());
                            code
                        });
                        self.hir_mir_dict_program(&code)
                            .map(|dicts| crate::mir::HirFnValue { kind: crate::mir::MirAllocKind::PolyFnCapture { target }, captures: Vec::new(), dicts })
                    };
                    if let Some(value) = value {
                        t.fn_values.insert(id.0, value);
                    }
                }
            }
            HirKind::Global { .. } if let Some(&(entry, arity, rest)) = emit.plan.fn_refs.get(&id.0) => {
                if let Some(target) = self.bytecode.entry_label_for_offset(entry) {
                    let operand = make_fn_operand(0, 0, arity, rest);
                    t.fn_values.insert(id.0, crate::mir::HirFnValue { kind: crate::mir::MirAllocKind::Fn { target, operand, mask: 0 }, captures: Vec::new(), dicts: Vec::new() });
                }
            }
            HirKind::Global { .. } => {
                if let Some(crate::const_fold::ConstValue::Str(text)) = emit.plan.consts.get(&id.0) {
                    let idx = self.intern_string(text);
                    t.strings.insert(id.0, idx);
                }
            }
            // A fixed array local bound or assigned to another is copied.
            HirKind::Let { init: Some(value), .. } | HirKind::Assign { value, .. }
                if let Some(native) = self.hir_array_copy(hir, emit, *value).and_then(|n| u16::try_from(n).ok()) =>
            {
                t.array_copies.insert(value.0, native);
            }
            // `let { a, b } = r`: each bound field read by its key.
            HirKind::LetPat { pat: HirPat::Record(parts), .. } => {
                let keys = parts
                    .iter()
                    .map(|(name, part)| match part {
                        HirPat::Wild => Some(None),
                        HirPat::Bind(_) => Some(Some(self.intern_string(name))),
                        _ => None,
                    })
                    .collect::<Option<Vec<_>>>();
                if let Some(keys) = keys {
                    t.record_pats.insert(id.0, keys);
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
            // An element-wise operator on the packed kernel: its operands
            // boxed, then the meta word.
            HirKind::Bin { .. } | HirKind::Un { .. }
                if let Some(HirOp::Aggregate(info)) = emit.plan.ops.get(&id.0)
                    && let Some(meta) = Self::packed_aggregate_meta(info)
                    && let Some(native) = self.native_id(common::PACKED_VEC_ARITH) =>
            {
                t.linalgs.insert(id.0, (native as u16, meta));
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
                    t.linalgs.insert(id.0, (native as u16, meta));
                } else if words {
                    // Past the kernel's limits: unrolled.
                    t.unrolled_las.insert(id.0, info.kind.clone());
                }
            }
            HirKind::Bin {
                op: BinOp::Overloaded(_),
                lhs,
                rhs,
            } => match emit.plan.ops.get(&id.0) {
                Some(HirOp::Prim) => {
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
            HirKind::Call { callee: Callee::Value(f), args } => {
                if lower::polyfn_callee(hir, &self.checker, *f) {
                    self.hir_mir_polyfn_call(hir, emit, id, *f, args, t);
                } else {
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
                        t.linalgs.insert(id.0, (native as u16, meta));
                    } else if let crate::typechecking::LinearAlgebraKind::Cross { left_is_tuple, elem_is_float } = info.kind
                        && args.len() == 2
                    {
                        // Unrolled.
                        t.crosses.insert(id.0, (left_is_tuple, elem_is_float));
                    } else if matches!(info.kind, crate::typechecking::LinearAlgebraKind::Dot { .. } | crate::typechecking::LinearAlgebraKind::MatMul { .. })
                        && args.len() == 2
                    {
                        // So are these, past the kernel's limits.
                        t.unrolled_las.insert(id.0, info.kind.clone());
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
                    t.ffis.insert(id.0, site);
                }
                // What emission evaluates: an `extern`'s arguments, or the
                // value operands (a signature's tags are constants, a
                // callback its code pointer).
                if let HirBuiltin::FfiDyn(_) = builtin {
                    for a in Self::hir_ffi_operands(hir, builtin, args) {
                        if self.hir_ffi_callback(hir, builtin, a).is_none() {
                            self.hir_mir_walk(hir, emit, a, t);
                        }
                    }
                    return Kids::None;
                }
            }
            HirKind::Call { .. } if emit.plan.calls.get(&id.0).is_some_and(|c| matches!(c.builtin, Some(HirBuiltin::BlockOn))) => {
                let wait = self.native_id("wait_ready").and_then(|n| u16::try_from(n).ok());
                t.block_ons.insert(id.0, wait);
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
            // A partial application: `MakeFn` over the filled prefix, each
            // argument its own word.
            HirKind::Call { args, .. }
                if let Some(HirCall {
                    builtin: Some(HirBuiltin::Partial { entry, mask, operand }),
                    params,
                    ..
                }) = emit.plan.calls.get(&id.0) =>
            {
                if args.iter().zip(params).all(|(&a, &p)| self.hir_natural(hir, emit, a) == Some(Rep::Word(p)))
                    && let Some(target) = self.bytecode.entry_label_for_offset(*entry as usize)
                {
                    let kind = crate::mir::MirAllocKind::Fn { target, operand: *operand, mask: *mask };
                    t.fn_values.insert(id.0, crate::mir::HirFnValue { kind, captures: Vec::new(), dicts: Vec::new() });
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
                        let items = lower::tuple_items(hir, args[2])?;
                        let sig = items.iter().map(|&t| lower::ffi_tag(hir, &self.checker, t).map(tag)).collect::<Option<Vec<_>>>()?;
                        operands.push(O::Tuple(sig));
                        operands.push(tag(lower::ffi_tag(hir, &self.checker, args[3])?));
                        let variadic = args.get(4).is_some_and(|&v| matches!(hir.expr(v).kind, HirKind::Lit(Lit::Bool(true))));
                        (MirFfiOp::Declare, (items.len() as u32 & 0xFFFF) | (u32::from(variadic) << 16))
                    }
                    FfiBuiltin::Invoke => {
                        let n = values.len() as u32 - 2;
                        let mut items = Vec::with_capacity(n as usize);
                        for (&a, &p) in values.iter().zip(params).skip(2) {
                            items.push(match self.hir_ffi_callback(hir, builtin, a) {
                                Some(offset) => O::CodePtr(self.bytecode.entry_label_for_offset(offset? as usize)?),
                                None => value(self, a, p)?,
                            });
                        }
                        operands.push(O::Tuple(items));
                        let mut operand = n & 0xFFFF;
                        if lower::ffi_fn_variadic(hir, &self.checker, args[1]) {
                            let items = lower::tuple_items(hir, args[2])?;
                            let tags = lower::ffi_variadic_tags(hir, &self.checker, id, items)?;
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
        // The VM pushes a boxed `Result`; a niche-shaped one converts.
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

    /// User operator `id` on `operands` calling its instance method
    /// (`HirOp::Call`): a raw method takes the
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
        let dict = self.hir_mir_once(|this| this.emit_call_instance_dict(&mut CodeBuf::new(), (class, method, &fqn), std::slice::from_ref(&lookup), span.0..span.1));
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
            dicts: Vec::new(),
            boxes: Vec::new(),
            adapt: Vec::new(),
            unbox: None,
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

    /// Call `id` of a function or method, past the builtins: its entry when it is one
    /// plain `CALL` (or `TailCall` / `MakeCoro`) of the arguments' own
    /// words with no dictionary.
    fn hir_mir_call(&mut self, hir: &HirBody, emit: &HirEmit, id: HirId, args: &[HirId], t: &mut HirMirTables) {
        let call = &emit.plan.calls[&id.0];
        if call.builtin.is_some() || call.key == format!("{}::push", common::BUILTIN_VEC_TYPE) {
            return;
        }
        let natural = Self::hir_call_rep(call);
        let span = hir.expr(id).span;
        let natural_args = |this: &Self, params: &[ValueLayout]| args.iter().zip(params).all(|(&a, &p)| this.hir_arg_passes(hir, emit, a, p));
        if let Some(inst) = call.instance.as_ref().filter(|i| i.ground.is_some()) {
            let ground = inst.ground.as_ref().expect("ground call");
            let method = &ground.method;
            let key = self.instance_clone(&call.key, &inst.args).unwrap_or_else(|| call.key.clone());
            let code = self.hir_mir_once(|this| {
                let mut code = CodeBuf::new();
                this.emit_call_instance_dict(&mut code, (&inst.class, method, &key), &inst.args, span.0..span.1);
                code
            });
            let dicts = self.hir_mir_dict_program(&code);
            let raw = self.raw_instance_methods.contains(&key);
            self.raw_instance_call = raw;
            let target = self.named_entry_target(&key, true);
            // Raw words, or each argument boxed where the instance takes it
            // boxed (a word, not a pair), and the instance's dictionary:
            // direct MIR calls the same entry.
            let boxes: Vec<Option<u32>> = if raw {
                Vec::new()
            } else {
                ground.boxed.iter().map(|ty| ty.as_ref().and_then(Self::ty_to_value_tag).map(|t| t as u32)).collect()
            };
            let unpaired = args
                .iter()
                .zip(&boxes)
                .all(|(&a, b)| b.is_none() || !matches!(self.hir_natural(hir, emit, a), Some(Rep::Pair(_))));
            if let Some(dicts) = dicts
                && unpaired
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
                        dicts,
                        boxes,
                        adapt: Vec::new(),
                        unbox: None,
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
            // Bare type parameter arguments (and a generic-head instance's
            // receiver) boxed, the result unboxed, as emission tags them.
            let tag = |ty: Option<&Ty>| ty.and_then(Self::ty_to_value_tag).map(|t| t as u32);
            let boxes: Vec<Option<u32>> = (0..args.len())
                .map(|i| {
                    tag(call
                        .generic
                        .as_ref()
                        .and_then(|g| g.boxed[i].as_ref())
                        .or_else(|| instance.filter(|_| i == 0).and_then(|inst| inst.recv_box.as_ref())))
                })
                .collect();
            let unbox = tag(call.generic.as_ref().and_then(|g| g.unbox.as_ref()));
            // The generic's dictionaries, then the instance's, as
            // `hir_push_dicts` and `emit_instance_dict` push them.
            let mut dicts = match call.generic.as_deref() {
                None => Some(Vec::new()),
                Some(g) if self.hir_generic_boxing(hir, emit, args, &call.params) => self.hir_mir_dict_ops(g),
                Some(_) => None,
            };
            if let Some(inst) = instance
                && dicts.is_some()
            {
                let code = self.hir_mir_once(|this| {
                    let mut code = CodeBuf::new();
                    this.emit_instance_dict(&mut code, &inst.class, &inst.args);
                    code
                });
                let prev = dicts.take();
                dicts = self.hir_mir_dict_program(&code).and_then(|ops| {
                    prev.map(|mut d| {
                        d.extend(ops);
                        d
                    })
                });
            }
            let coro = self.coroutine_fns.contains(&key);
            self.hir_request_method_clone(&key);
            self.raw_instance_call = raw;
            let target = self.named_entry_target(&key, true);
            if let Some(dicts) = dicts
                && let Some(boxed_ret) = self.hir_ret_shape(hir, id, &natural)
                && args.iter().zip(&call.params).zip(&boxes).all(|((&a, &p), b)| {
                    // A boxed argument goes from its own word.
                    if b.is_some() { self.hir_arg_word(hir, emit, a, p) } else { self.hir_arg_passes(hir, emit, a, p) }
                })
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
                        dicts,
                        boxes,
                        adapt: self.hir_mir_adapters(hir, call.generic.as_deref(), args),
                        unbox,
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
        // A shared generic's call is plain, its dictionaries built as
        // `hir_push_dicts` builds them (none forwarded through a slot).
        let dicts = match call.generic.as_deref() {
            None => Some(Vec::new()),
            Some(g) if self.hir_generic_boxing(hir, emit, args, &call.params) => self.hir_mir_dict_ops(g),
            Some(_) => None,
        };
        // Bare type parameter arguments boxed and result unboxed, as
        // `emit_box_if_needed` / `emit_unbox_if_needed` tag them.
        let tag = |ty: &Option<Ty>| ty.as_ref().and_then(Self::ty_to_value_tag).map(|t| t as u32);
        let (boxes, unbox) = call.generic.as_deref().map_or((Vec::new(), None), |g| (g.boxed.iter().map(tag).collect(), tag(&g.unbox)));
        if let Some(dicts) = dicts.filter(|d| (d.is_empty() && unbox.is_none()) || !tail)
            && let Some(boxed_ret) = self.hir_ret_shape(hir, id, &natural)
            && let Some(target) = target
        {
            if call.generic.is_some() {
                for (&arg, &param) in args.iter().zip(&call.params) {
                    if !self.hir_arg_passes(hir, emit, arg, param)
                        && let Some(layout) = self.hir_niche_arg(hir, emit, arg, param)
                    {
                        t.niche_args.insert(arg.0, layout);
                    }
                }
            }
            let adapt = self.hir_mir_adapters(hir, call.generic.as_deref(), args);
            let pair_args = (0..args.len()).map(|i| Self::hir_arg_rep(call, i).words() == 2).collect();
            // A `TailCall`'s callee returns for this function, so in this
            // function's words, whatever the call's own rep.
            let pair_ret = if tail { matches!(emit.plan.ret, Rep::Pair(_)) } else { natural.words() == 2 };
            t.calls.insert(
                id.0,
                crate::mir::HirCallSite {
                    target,
                    pair_args,
                    pair_ret,
                    coro,
                    boxed_ret,
                    dicts,
                    boxes,
                    unbox,
                    adapt,
                },
            );
        }
    }

    /// Call `id` through `PolyFn` local `f`: each argument's `BoxValue` tag, the call site's
    /// dictionaries and the result's `UnboxValue` tag.
    fn hir_mir_polyfn_call(&mut self, hir: &HirBody, emit: &HirEmit, id: HirId, f: HirId, args: &[HirId], t: &mut HirMirTables) {
        let source = self.hir_polyfn_source(hir, emit, f);
        // Only a bare type-parameter parameter is boxed; the shared body
        // takes a concrete one as its own word (#1089).
        let Some(bare) = self.hir_polyfn_bare_params(hir, source.as_deref(), f, args.len()) else {
            return;
        };
        let mut boxes = Vec::with_capacity(args.len());
        let mut arg_tys = Vec::with_capacity(args.len());
        for (i, &arg) in args.iter().enumerate() {
            // Emission passes the boxed-layout word: the MIR word only when
            // that is the argument's own.
            if self.hir_natural(hir, emit, arg) != Some(Rep::Word(ValueLayout::Boxed)) {
                return;
            }
            let Some(ty) = Self::hir_ty(hir, arg) else {
                return;
            };
            let ty = apply_ty_prune(self.checker.subst(), ty);
            boxes.push(if bare[i] { Self::ty_to_value_tag(&ty).map(|tag| tag as u32) } else { None });
            arg_tys.push(ty);
        }
        let call_ty = Self::hir_ty(hir, id).map(|t| apply_ty_prune(self.checker.subst(), t));
        // The call site's dictionaries, as emission pushes them.
        let dicts = match &source {
            Some(source) => {
                let code = self.hir_mir_once(|this| {
                    let mut code = CodeBuf::new();
                    this.emit_call_site_dicts(&mut code, source, &arg_tys, call_ty.as_ref());
                    code
                });
                let Some(dicts) = self.hir_mir_dict_program(&code) else {
                    return;
                };
                dicts
            }
            None => Vec::new(),
        };
        let unbox = match &source {
            Some(source) => self.generic_return_depends_on_type_param(source),
            None => Self::hir_polyfn_returns_var(hir, &self.checker, f),
        };
        let unbox = if unbox { call_ty.as_ref().and_then(Self::ty_to_value_tag).map(|tag| tag as u32) } else { None };
        t.poly_calls.insert(id.0, crate::mir::HirPolyCall { boxes, dicts, unbox });
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

    /// Each argument's closure adapter: a function value the generic
    /// callee calls with boxed arguments. A lambda literal unboxes them itself.
    fn hir_mir_adapters(&mut self, hir: &HirBody, generic: Option<&HirGeneric>, args: &[HirId]) -> Vec<Option<(crate::il::Label, u32)>> {
        let Some(g) = generic else {
            return Vec::new();
        };
        args.iter()
            .zip(&g.adapt)
            .map(|(&arg, unbox)| match unbox {
                Some(unbox) if !matches!(hir.expr(arg).kind, HirKind::Lambda { .. }) => Some(self.hir_fn_adapter(unbox)),
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
        let layout = self.value_layout(&apply_ty_prune(self.checker.subst(), Self::hir_ty(hir, id)?));
        let natural = self.hir_natural(hir, emit, id);
        (layout != ValueLayout::Boxed && natural.is_none_or(|n| n == Rep::Word(layout))).then_some(layout)
    }

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

    /// The dictionaries [`Self::hir_push_dicts`] pushes for `generic`, as
    /// a program of the steps emission takes: the ones it forwards from
    /// this body's dictionary parameters, then the call site's. `None`
    /// when one is forwarded from another frame slot, or a step is not
    /// one of [`crate::mir::HirDictOp`].
    fn hir_mir_dict_ops(&mut self, generic: &HirGeneric) -> Option<Vec<crate::mir::HirDictOp>> {
        let mut ops = Vec::new();
        for &i in &generic.forwarded {
            if let Some(slot) = self.lookup_slot(&format!("__dict{i}")) {
                if !self.hir_mir_dict_param(slot) {
                    return None;
                }
                ops.push(crate::mir::HirDictOp::Load(slot));
            }
        }
        let code = self.hir_mir_once(|this| {
            let mut code = CodeBuf::new();
            this.emit_call_site_dicts(&mut code, &generic.lookup, &generic.arg_tys, Some(&generic.ret_ty));
            code
        });
        ops.extend(self.hir_mir_dict_program(&code)?);
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
        (slot < sp && (0..).map_while(|i| self.lookup_slot(&format!("__dict{i}"))).any(|s| s == slot))
            || self.hir_mir_ctx.iter().any(|&(s, own, _)| s == slot && own < sp)
    }

    /// Dictionary code emission put in `code` as a [`crate::mir::HirDictOp`]
    /// program, when every step is one.
    fn hir_mir_dict_program(&self, code: &CodeBuf) -> Option<Vec<crate::mir::HirDictOp>> {
        use crate::mir::HirDictOp;
        code.ops()
            .iter()
            .map(|op| match op {
                IlOp::Byte { byte, .. } if *byte.bytecode() == Instruction::CodePtr => {
                    self.bytecode.entry_label_for_offset(byte.operand_u32() as usize).map(HirDictOp::Code)
                }
                // One of this body's dictionary parameters.
                IlOp::Load { slot, .. } if self.hir_mir_dict_param(*slot) => Some(HirDictOp::Load(*slot)),
                _ if let Some(b) = op.as_encode_byte()
                    && *b.bytecode() == Instruction::LOAD
                    && b.load_store_count() == 1
                    && self.hir_mir_dict_param(b.load_store_slot_at(0)) =>
                {
                    Some(HirDictOp::Load(b.load_store_slot_at(0)))
                }
                IlOp::Entry { kind: EntryKind::CodePtr, target, .. } => Some(HirDictOp::Code(*target)),
                IlOp::MakeTuple { arity, kinds: 0, .. } => Some(HirDictOp::Tuple(*arity)),
                IlOp::Const { imm: 0, .. } => Some(HirDictOp::Null),
                IlOp::BoxValue { tag, .. } => Some(HirDictOp::Box(*tag)),
                _ => None,
            })
            .collect()
    }

    /// Whether argument `id` of a method or ground instance call, emitted
    /// as `Rep::Word(param)`, is what direct MIR's plain call passes: its
    /// own word ([`Self::hir_arg_word`]), or a two-word value (or a
    /// constructor) of a type whose word is `param`, which both box the
    /// same way (MIR's `box_pair_val`, emission's pair-to-word convert).
    fn hir_arg_passes(&self, hir: &HirBody, emit: &HirEmit, id: HirId, param: ValueLayout) -> bool {
        self.hir_arg_word(hir, emit, id, param)
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
            && self.hir_mir_probe_unplanned(|this, code| this.emit_existential_pack_recipe(code, &pack))
        {
            return true;
        }
        let e = hir.expr(id);
        let span = e.span.0..e.span.1;
        match &e.kind {
            HirKind::Bin { .. } | HirKind::Un { .. } => match emit.plan.ops.get(&id.0) {
                Some(&HirOp::Bound { dict, .. }) => !self.hir_mir_dict_param(dict),
                Some(HirOp::Call { lookup, fqn, class, method }) => {
                    let lookup = lookup.clone();
                    let fqn = self.instance_clone(fqn, std::slice::from_ref(&lookup)).unwrap_or_else(|| fqn.clone());
                    self.hir_mir_probe_dicts(|this, code| {
                        this.emit_call_instance_dict(code, (class, method, &fqn), std::slice::from_ref(&lookup), span);
                    })
                }
                _ => false,
            },
            // A `PolyFn` value's escape dictionaries.
            HirKind::Global { .. } if let Some(name) = emit.plan.polyfns.get(&id.0) => {
                let ty = Self::hir_ty(hir, id).map(|t| apply_ty_prune(self.checker.subst(), t));
                self.hir_mir_probe_unplanned(|this, code| {
                    this.emit_polyfn_escape_dicts(code, name, ty.as_ref());
                })
            }
            // A call through a `PolyFn` local: the call site's dictionaries
            // for the generic function it was bound from.
            HirKind::Call { callee: Callee::Value(f), args } if lower::polyfn_callee(hir, &self.checker, *f) => {
                let Some(source) = self.hir_polyfn_source(hir, emit, *f) else {
                    return false;
                };
                let arg_tys: Vec<Ty> = args.iter().filter_map(|&a| Self::hir_ty(hir, a)).map(|t| apply_ty_prune(self.checker.subst(), t)).collect();
                let call_ty = Self::hir_ty(hir, id).map(|t| apply_ty_prune(self.checker.subst(), t));
                self.hir_mir_probe_unplanned(|this, code| {
                    this.emit_call_site_dicts(code, &source, &arg_tys, call_ty.as_ref());
                })
            }
            HirKind::Call { args, .. } if !emit.plan.lens.contains_key(&id.0) && let Some(call) = emit.plan.calls.get(&id.0) => {
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
                    let key = self.instance_clone(&call.key, &inst.args).unwrap_or_else(|| call.key.clone());
                    return self.hir_mir_probe_unplanned(|this, code| {
                        this.emit_call_instance_dict(code, (&inst.class, method, &key), &inst.args, span);
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
                    Some(recv) if call.generic.is_none() => self.instance_clone(&call.key, &[recv]).unwrap_or_else(|| call.key.clone()),
                    _ => call.key.clone(),
                };
                !self.raw_instance_methods.contains(&key)
                    && self.hir_mir_probe_unplanned(|this, code| {
                        this.emit_instance_dict(code, &inst.class, &inst.args);
                    })
            }
            _ => false,
        }
    }

    /// Whether [`Self::hir_push_dicts`] reads a frame dictionary for
    /// `generic`: one it forwards, or one of the call site's.
    fn hir_mir_generic_dict_reads(&mut self, generic: &HirGeneric) -> bool {
        generic.forwarded.iter().any(|&i| self.lookup_slot(&format!("__dict{i}")).is_some_and(|slot| !self.hir_mir_dict_param(slot)))
            || self.hir_mir_probe_unplanned(|this, code| {
                this.emit_call_site_dicts(code, &generic.lookup, &generic.arg_tys, Some(&generic.ret_ty));
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
                self.hir_mir_probe_dicts(|this, code| {
                    this.emit_call_instance_dict(code, ("Show", "show", &fqn), std::slice::from_ref(&lookup_ty), 0..0);
                })
            }
        }
    }

    /// Run a dictionary helper of emission into a scratch buffer
    /// ([`Self::hir_mir_once`]): whether its code reads a frame
    /// dictionary. The code goes nowhere.
    fn hir_mir_probe_dicts(&mut self, f: impl FnOnce(&mut Self, &mut CodeBuf)) -> bool {
        let mut code = CodeBuf::new();
        self.hir_mir_once(|this| f(this, &mut code));
        self.hir_mir_code_reads_dicts(&code)
    }

    /// As [`Self::hir_mir_probe_dicts`], for a site whose plan builds its
    /// dictionaries from a [`crate::mir::HirDictOp`] program: whether its
    /// code reads a frame dictionary no program step stands for.
    fn hir_mir_probe_unplanned(&mut self, f: impl FnOnce(&mut Self, &mut CodeBuf)) -> bool {
        let mut code = CodeBuf::new();
        self.hir_mir_once(|this| f(this, &mut code));
        self.hir_mir_code_reads_dicts(&code) && self.hir_mir_dict_program(&code).is_none()
    }

    /// Whether `code` loads one of the frame's dictionaries.
    fn hir_mir_code_reads_dicts(&self, code: &CodeBuf) -> bool {
        let dicts: HashSet<u32> = (0..).map_while(|i| self.lookup_slot(&format!("__dict{i}"))).collect();
        code.ops().iter().filter_map(IlOp::as_encode_byte).any(|b| {
            *b.bytecode() == Instruction::LOAD && (0..b.load_store_count()).any(|i| dicts.contains(&b.load_store_slot_at(i)))
        })
    }

    /// `f(self)`, an emission helper the walk asks for its decision, with
    /// each diagnostic it reports kept once: one already reported (the
    /// same message at the same place, as emission reports it when it runs
    /// the helper first) is not repeated. The walk's diagnostics are its
    /// own, so they stand whether or not emission ran.
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
                let show = self.hir_show_plan(hir, arg);
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
    /// one-word `CALL` of the raw `show` entry on the boxed-layout value
    /// (no boxing, conversion or dictionary). It interns the strings
    /// emission does.
    fn hir_show_ty(&mut self, ty: &Ty) -> Option<crate::mir::HirShow> {
        match ty {
            Ty::Tuple(items) => {
                let items = items.iter().map(|t| Some((t.clone(), self.hir_show_ty(t)?))).collect::<Option<Vec<_>>>()?;
                let fmt = self.intern_string(unescape_coil_string(&Self::tuple_show_format(items.len())));
                return Some(crate::mir::HirShow::Tuple { fmt, items });
            }
            Ty::Record { fields } => {
                let mut shown = Vec::with_capacity(fields.len());
                for (name, t) in fields {
                    let key = self.intern_string(name);
                    shown.push((key, t.clone(), self.hir_show_ty(t)?));
                }
                let fmt = self.intern_string(unescape_coil_string(&Self::record_show_format(fields)));
                return Some(crate::mir::HirShow::Record { fmt, fields: shown });
            }
            _ => {}
        }
        if self.value_layout(ty) != ValueLayout::Boxed {
            return None;
        }
        let lookup_ty = Self::show_lookup_ty_for_instance(ty);
        let Some(fqn) = self
            .find_show_instance(&lookup_ty)
            .and_then(|inst| inst.method_fqns.get("show").cloned())
            .filter(|fqn| self.functions.contains_key(fqn) || self.fn_entry_labels.contains_key(fqn))
        else {
            return Some(crate::mir::HirShow::Stringify);
        };
        let fqn = self.instance_clone(&fqn, std::slice::from_ref(&lookup_ty)).unwrap_or(fqn);
        let raw = self.raw_instance_methods.contains(&fqn);
        // A non-raw `show` takes the value boxed (`BoxValue`).
        let boxed = if raw { None } else { Self::ty_to_value_tag(&lookup_ty).map(|tag| tag as u32) };
        // A dictionary argument: not a plain call. Its code goes nowhere.
        let dict = self.hir_mir_once(|this| this.emit_call_instance_dict(&mut CodeBuf::new(), ("Show", "show", &fqn), std::slice::from_ref(&lookup_ty), 0..0));
        if dict || self.two_word_return_kind(&fqn).is_some() {
            return None;
        }
        self.raw_instance_call = raw;
        self.named_entry_target(&fqn, true).map(|target| crate::mir::HirShow::Call { target, boxed })
    }

    /// Decide, before `root` is emitted, which parallel loops under it move
    /// into their own fork-join functions ([`HirPlan::par_loops`]) and
    /// whether one runs its fork-join inline ([`HirPlan::par_inline`]), as
    /// direct MIR lowers each site. A site's enclosing locals
    /// are the ones bound when it runs: `emit`'s bound slots (parameters
    /// and captures, or a chunk worker's site locals), then each local
    /// bound before the site in emission order.
    pub(super) fn hir_plan_par_loops(&mut self, hir: &HirBody, emit: &mut HirEmit, root: HirId) {
        let mut bound: HashSet<u32> = (0..emit.slots.len() as u32).filter(|&l| emit.slots[l as usize].is_some()).collect();
        self.hir_plan_par_walk(hir, emit, root, &mut bound);
    }

    fn hir_plan_par_walk(&mut self, hir: &HirBody, emit: &mut HirEmit, id: HirId, bound: &mut HashSet<u32>) {
        let bind_pat = |pat: &HirPat, bound: &mut HashSet<u32>| {
            Self::hir_pat_locals(pat, &mut |l| {
                bound.insert(l.0);
            })
        };
        match &hir.expr(id).kind {
            // `while false { .. }` is dropped.
            HirKind::Loop { body }
                if lower::while_shape(hir, *body).is_some_and(|(cond, _)| matches!(hir.expr(cond).kind, HirKind::Lit(Lit::Bool(false)))) => {}
            HirKind::Loop { body } if let Some((cond, then)) = lower::while_shape(hir, *body) => {
                if !self.hir_plan_par_site(hir, emit, id, cond, None, then, bound) {
                    self.hir_plan_par_walk(hir, emit, cond, bound);
                    self.hir_plan_par_walk(hir, emit, then, bound);
                }
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
                    if !lower::is_unit_local(hir, &self.checker, *local) {
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
                for k in lower::children(hir, id) {
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
        let Some(locals) = self.hir_par_site(hir, &emit.plan, id, head, bind, body, &|l| bound.contains(&l.0)) else {
            return false;
        };
        if let Some(planned) = self.hir_par_outline(hir, id, &locals, bind, body) {
            emit.plan.par_loops.insert(id.0, planned);
            return true;
        }
        // Inline: the body runs in the chunk worker's frame; what it binds
        // there is unbound after it.
        emit.plan.par_inline = true;
        if let Some(local) = bind {
            bound.insert(local.0);
        }
        let mut inner = bound.clone();
        self.hir_plan_par_walk(hir, emit, body, &mut inner);
        true
    }
}
