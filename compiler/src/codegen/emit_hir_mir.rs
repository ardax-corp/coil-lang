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
        let e = hir.expr(id);
        match &e.kind {
            HirKind::Lit(Lit::Str(raw))
                if Self::hir_ty(hir, id).and_then(lower::primitive) != Some(crate::typechecking::ty::BYTE)
                    && !Self::hir_ty(hir, id).is_some_and(lower::is_byte_array) =>
            {
                let idx = self.intern_string(unescape_coil_string(raw));
                t.strings.insert(id.0, idx);
            }
            HirKind::Global { .. }
                if !emit.plan.statics.contains_key(&id.0)
                    && !emit.plan.polyfns.contains_key(&id.0)
                    && !emit.plan.fn_refs.contains_key(&id.0) =>
            {
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
            HirKind::Call { callee: Callee::Value(_), .. } => {}
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
        // A dictionary argument: not a plain call. Its code goes nowhere;
        // emission reported any diagnostic.
        let messages = self.messages.len();
        let dict = self.emit_call_instance_dict(&mut CodeBuf::new(), ("Show", "show", &fqn), std::slice::from_ref(&lookup_ty), 0..0);
        self.messages.truncate(messages);
        if dict || self.two_word_return_kind(&fqn).is_some() {
            return None;
        }
        self.raw_instance_call = raw;
        self.named_entry_target(&fqn, true)
    }
}
