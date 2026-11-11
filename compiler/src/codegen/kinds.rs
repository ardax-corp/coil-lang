//! The `kinds` word of a shared generic entry (#2506).
//!
//! A shared generic body holds a value of a type parameter as the ground
//! type's word, which may be a scalar or an object, so it cannot tell on
//! its own whether a copy-on-write op applies. Its entry takes one more
//! word after the dictionaries: bit `i` is set when type parameter `i` of
//! its scheme (`Scheme::bounds`, in order) is a copy-on-write object at the
//! call, as `hir::cow::cow_ty` decides for a per-type clone. The bits past
//! the scheme's parameters hold [`SENTINEL`]'s, so a word that is no
//! `kinds` word is caught where the entry checks it ([`check_on`]).
//!
//! A ground call passes a constant. A call from a shared body computes each
//! bit from its own: a type parameter forwards its bit, a type whose head
//! is known (`Vec<T>`, `Option<T>`) is a constant. A lambda of a shared
//! body captures the word.

use super::*;
use crate::mir::HirDictOp;
use crate::typechecking::subst::apply_ty_prune;
use crate::typechecking::ty::{strip_readonly, Constraint, Scheme, TyVarId};

/// The bits a `kinds` word carries past its mask.
pub(crate) const SENTINEL: u64 = 0xC0DE_5A3C_A5C3_3C5A;

/// The most type parameters a shared generic entry may have: one bit each,
/// below the sentinel's top bit, so every `kinds` word has bit 63 set.
pub(crate) const MAX_PARAMS: usize = 63;

/// The bits of a dictionary's `kinds` word: its trait's parameters, then
/// its instance head's type variables. The sentinel is above them for every
/// dictionary of every trait, so a default body checks it the same way
/// whatever instance's dictionary it gets.
pub(crate) const DICT_BITS: u32 = 32;

/// [`SENTINEL`]'s bits past a mask of `n` parameters.
pub(crate) fn sentinel(n: u32) -> u64 {
    if n >= 64 { 0 } else { SENTINEL & (u64::MAX << n) }
}

/// Whether a shared generic entry checks its `kinds` word's sentinel bits:
/// debug builds, and any build under `COIL_MIR_VERIFY`.
pub(crate) fn check_on() -> bool {
    cfg!(debug_assertions) || crate::mir::verify_asked()
}

/// A trait method's dictionary `kinds` word: its index in the
/// dictionary, how many bits it has, and each type variable's bit.
pub(super) type DictKinds = (u32, u32, Vec<(TyVarId, u32)>);

/// Where one bit of a call's `kinds` word comes from.
enum Bit {
    Const(bool),
    /// The caller's own bit.
    From(u32),
}

impl Compiler {
    /// `lookup`'s scheme, when its shared entry would take a `kinds` word:
    /// it has type parameters and is no overload.
    fn kinds_scheme_of(&self, lookup: &str) -> Option<&Scheme> {
        if self.checker.is_overloaded(lookup) {
            return None;
        }
        self.checker.env().lookup(lookup).filter(|s| !s.bounds.is_empty())
    }

    /// Record free function or inherent method `key` (scheme `lookup`) as
    /// taking a `kinds` word when it is generic, before any call of it is
    /// planned. A class's `drop` takes no `kinds` word: the runtime calls it on the
    /// object alone.
    pub(super) fn note_kinds_entry(&mut self, key: String, lookup: &str) {
        if lookup.rsplit_once("::").is_some_and(|(_, m)| m == "drop") {
            return;
        }
        if self.kinds_scheme_of(lookup).is_some() {
            self.reserve_function_entry(key.clone());
            if let Some(&label) = self.fn_entry_labels.get(&key) {
                self.kinds_labels.insert(label, key.clone());
            }
            self.kinds_keys.insert(key);
        }
    }

    /// The key of the shared entry at `label`, when it takes a `kinds` word.
    pub(super) fn kinds_key_at(&self, label: crate::bodies::Label) -> Option<&str> {
        self.kinds_labels.get(&label).map(String::as_str)
    }

    /// The scheme of the shared entry `key`, when it takes a `kinds` word.
    pub(crate) fn kinds_scheme(&self, key: &str) -> Option<Scheme> {
        if !self.kinds_keys.contains(key) {
            return None;
        }
        self.kinds_scheme_of(strip_overload_key(key)).or_else(|| self.kinds_scheme_of(key)).cloned()
    }

    /// The type parameters of `key`'s entry word, in bit order, and its
    /// scheme's parameter types: a shared free function's or inherent
    /// method's scheme.
    fn entry_kinds_vars(&self, key: &str) -> Option<(Vec<TyVarId>, Vec<Ty>)> {
        // A trait method's own type parameters (#2506).
        if let Some((class, method)) = self.hir_own_kinds.clone() {
            let vars = self.own_kinds_vars(&class, &method)?;
            let scheme = self.checker.typeclass_method_scheme(&class, &method)?;
            let (params, _) = Self::fun_param_and_ret_tys(&scheme.ty);
            return Some((vars, params));
        }
        let scheme = self.kinds_scheme(key)?;
        let (params, _) = Self::fun_param_and_ret_tys(&scheme.ty);
        Some((scheme.bounds.clone(), params))
    }

    /// Trait method `class::method`'s own type parameters (`fn visit<W>`),
    /// after the trait's in its scheme, when it has any: their `kinds`
    /// word follows every entry's own bounds' dictionaries (#2506).
    pub(super) fn own_kinds_vars(&self, class: &str, method: &str) -> Option<Vec<TyVarId>> {
        let n_trait = self.checker.generics().typeclass(class)?.type_params.len();
        let scheme = self.checker.typeclass_method_scheme(class, method)?;
        // An associated type's projection (`Item`) is no type parameter.
        let own: Vec<TyVarId> = scheme
            .bounds
            .iter()
            .skip(n_trait)
            .filter(|v| !scheme.assoc_projections.iter().any(|p| p.var == **v))
            .copied()
            .collect();
        (!own.is_empty() && own.len() <= MAX_PARAMS).then_some(own)
    }

    /// The own `kinds` word of a trait method call whose own type
    /// parameters `own` the call binds as `vars`, as ops.
    pub(super) fn own_kinds_ops(&self, own: &[TyVarId], vars: &HashMap<TyVarId, Ty>) -> Vec<HirDictOp> {
        let frame = self.hir_body_frame.as_ref().and_then(|f| f.kinds.as_ref());
        let mut word = sentinel(own.len() as u32);
        let mut from = Vec::new();
        for (i, v) in own.iter().enumerate() {
            match vars.get(v).map(|t| self.kinds_bit(t, frame)) {
                Some(Bit::Const(true)) => word |= 1 << i,
                Some(Bit::From(j)) => from.push(HirDictOp::KindsBit { from: j as u8, to: i as u8 }),
                Some(Bit::Const(false)) | None => {}
            }
        }
        std::iter::once(HirDictOp::Kinds(word)).chain(from).collect()
    }

    /// The `kinds` frame of body `hir`, entry `key`, from `base`'s word
    /// sources ([`frame::FrameKinds::slot`] and `dict`): each type
    /// variable's bit (and those of the body's own variables its parameters
    /// bind them to, or the checker solved to them), `self`'s scheme type,
    /// and the sentinel check's message.
    pub(super) fn kinds_frame(&mut self, key: &str, hir: &crate::hir::HirBody, base: frame::FrameKinds) -> frame::FrameKinds {
        let mut bits: Vec<(TyVarId, u32)> = Vec::new();
        let mut n = 0;
        if base.dict.is_some()
            && let Some((_, dict_n, dict_bits)) = self.dict_kinds_layout(key)
        {
            bits = dict_bits;
            n = dict_n;
        }
        let mut params = Vec::new();
        if base.slot.is_some() {
            let (vars, tys) = self.entry_kinds_vars(key).expect("a kinds entry's scheme");
            for (i, &v) in vars.iter().enumerate() {
                bits.push((v, n + i as u32));
            }
            n += vars.len() as u32;
            params = tys;
        }
        // The scheme may or may not list `self`.
        let skip = hir.params.len().saturating_sub(params.len());
        let mut map = HashMap::new();
        for (p, &local) in params.iter().zip(&hir.params[skip..]) {
            if let Some(ty) = hir.local(local).ty.as_ref() {
                Self::bind_scheme_vars(p, &apply_ty_prune(self.checker.subst(), ty), &mut map);
            }
        }
        for (v, ty) in &map {
            if let Ty::Var(u) = strip_readonly(ty)
                && !bits.iter().any(|&(w, _)| w == *u)
                && let Some(&(_, i)) = bits.iter().find(|&&(w, _)| w == *v)
            {
                bits.push((*u, i));
            }
        }
        // A variable the body's types name that the checker solved to one
        // of them has its bit too.
        let mut named = Vec::new();
        for ty in hir.locals.iter().filter_map(|l| l.ty.as_ref()).chain(hir.exprs.iter().filter_map(|e| e.ty.as_ref())) {
            ty.collect_vars(&mut named);
        }
        named.sort_unstable_by_key(|v| v.0);
        named.dedup();
        for u in named {
            if bits.iter().any(|&(w, _)| w == u) {
                continue;
            }
            if let Ty::Var(w) = apply_ty_prune(self.checker.subst(), &Ty::Var(u))
                && let Some(&(_, i)) = bits.iter().find(|&&(x, _)| x == w)
            {
                bits.push((u, i));
            }
        }
        let self_ty = (self.compiling_method && skip == 0 && base.slot.is_some())
            .then(|| params.first().cloned())
            .flatten()
            .filter(|t| matches!(strip_readonly(t), Ty::App(head, _) if matches!(head.as_ref(), Ty::Con(_))));
        let check = check_on().then(|| self.intern_string(format!("internal error: `{key}` entered without its kinds word (compiler bug)")));
        frame::FrameKinds { bits, n, self_ty, check, ..base }
    }

    /// The bit a call passes for a type parameter at `ty`.
    fn kinds_bit(&self, ty: &Ty, frame: Option<&frame::FrameKinds>) -> Bit {
        let ty = apply_ty_prune(self.checker.subst(), ty);
        match head_var(&ty) {
            // A variable the caller has no bit for: no op, as before.
            Some(v) => frame.and_then(|f| f.bit(v)).map_or(Bit::Const(false), Bit::From),
            None => Bit::Const(crate::hir::effects::cow::cow_ty(&self.checker, Some(&ty))),
        }
    }

    /// The `kinds` word a call of `key` on arguments of `arg_tys` (the
    /// receiver first, for a method) returning `ret_ty` passes after its
    /// dictionaries, as ops; `None` when the entry takes none.
    pub(super) fn kinds_ops(&self, key: &str, arg_tys: &[Ty], ret_ty: Option<&Ty>) -> Option<Vec<HirDictOp>> {
        let scheme = self.kinds_scheme(key)?;
        Some(self.kinds_ops_for(&scheme.bounds, &scheme.ty, arg_tys, ret_ty))
    }

    /// The `kinds` word of a call of a function typed `fun_ty`, its type
    /// parameters `bounds` in bit order, on arguments of `arg_tys` (the
    /// receiver first, for a method) returning `ret_ty`, as ops.
    pub(super) fn kinds_ops_for(&self, bounds: &[TyVarId], fun_ty: &Ty, arg_tys: &[Ty], ret_ty: Option<&Ty>) -> Vec<HirDictOp> {
        let frame = self.hir_body_frame.as_ref().and_then(|f| f.kinds.as_ref());
        let (params, ret) = Self::fun_param_and_ret_tys(fun_ty);
        let mut vars = HashMap::new();
        // A method's receiver may be typed by its class alone (`self` in
        // the class's own body): the caller's scheme type of it binds the
        // class's parameters.
        let own_self = if self.compiling_mono_clone { self.hir_clone_self_ty() } else { frame.and_then(|f| f.self_ty.clone()) };
        let skip = arg_tys.len().saturating_sub(params.len());
        for (i, (p, a)) in params.iter().zip(&arg_tys[skip..]).enumerate() {
            let a = apply_ty_prune(self.checker.subst(), a);
            let a = match (i, strip_readonly(p), strip_readonly(&a), &own_self) {
                (0, Ty::App(head, _), Ty::Con(name), Some(own)) if matches!(head.as_ref(), Ty::Con(n) if n == name) && matches!(strip_readonly(own), Ty::App(h, _) if h == head) => own.clone(),
                _ => a,
            };
            Self::bind_scheme_vars(p, &a, &mut vars);
        }
        if let Some(r) = ret_ty {
            Self::bind_scheme_vars(&ret, &apply_ty_prune(self.checker.subst(), r), &mut vars);
        }
        self.kinds_ops_bound(bounds, &vars)
    }

    /// The `kinds` word of type parameters `bounds` (in bit order) bound as
    /// `vars`, as ops.
    pub(super) fn kinds_ops_bound(&self, bounds: &[TyVarId], vars: &HashMap<TyVarId, Ty>) -> Vec<HirDictOp> {
        let frame = self.hir_body_frame.as_ref().and_then(|f| f.kinds.as_ref());
        let mut word = sentinel(bounds.len() as u32);
        let mut from = Vec::new();
        for (i, v) in bounds.iter().enumerate().take(MAX_PARAMS) {
            match vars.get(v).map(|t| self.kinds_bit(t, frame)) {
                Some(Bit::Const(true)) => word |= 1 << i,
                Some(Bit::From(j)) => from.push(HirDictOp::KindsBit { from: j as u8, to: i as u8 }),
                Some(Bit::Const(false)) | None => {}
            }
        }
        std::iter::once(HirDictOp::Kinds(word)).chain(from).collect()
    }

    /// The word a call through a function value of type `body`, its type
    /// variables `binders`, on arguments of `arg_tys` returning `ret_ty`
    /// passes last (#2621): a bit for each of [`value_kinds_order`]'s, then
    /// their count ([`common::KINDS_CALL_COUNT_SHIFT`]), as ops.
    pub(super) fn value_kinds_ops(&self, binders: &[TyVarId], body: &Ty, arg_tys: &[Ty], ret_ty: Option<&Ty>) -> Vec<HirDictOp> {
        let order = value_kinds_order(binders, body);
        let mut ops = self.kinds_ops_for(&order, body, arg_tys, ret_ty);
        if let Some(HirDictOp::Kinds(word)) = ops.first_mut() {
            *word = (*word & !sentinel(order.len() as u32)) | (order.len() as u64) << common::KINDS_CALL_COUNT_SHIFT;
        }
        ops
    }

    /// The `kinds` slot a `PolyFn` value of generic function `name` read at
    /// type `ty` captures, after its dictionaries (#2506): the word at
    /// `ty`, then, for each type parameter `ty` leaves open (the body has no
    /// bit for it), a [`common::kinds_from_call`] word that sets its bit
    /// from the call's. The call passes its word in the order of the type
    /// it sees ([`Self::value_kinds_ops`]): the `forall` parameter
    /// `expected` the value is passed to, else the function's own (a local
    /// bound from it).
    pub(super) fn polyfn_kinds_slot(&self, name: &str, ty: Option<&Ty>, expected: Option<&Ty>) -> Option<Vec<HirDictOp>> {
        let scheme = self.kinds_scheme(name)?;
        let frame = self.hir_body_frame.as_ref().and_then(|f| f.kinds.as_ref());
        let ty = ty.map(|t| apply_ty_prune(self.checker.subst(), t));
        let (binders, body) = match expected.map(strip_readonly) {
            Some(Ty::Forall { bounds, body, .. }) => (bounds.clone(), body.as_ref().clone()),
            _ => (scheme.bounds.clone(), scheme.ty.clone()),
        };
        let order = value_kinds_order(&binders, &body);
        let (mut vars, mut seen) = (HashMap::new(), HashMap::new());
        if let Some(t) = &ty {
            Self::bind_scheme_vars(&scheme.ty, t, &mut vars);
            Self::bind_scheme_vars(t, &body, &mut seen);
        }
        let mut from_call = Vec::new();
        for (i, v) in scheme.bounds.iter().enumerate().take(MAX_PARAMS) {
            let Some(Ty::Var(u)) = vars.get(v).map(strip_readonly) else { continue };
            if frame.is_some_and(|f| f.bit(*u).is_some()) {
                continue;
            }
            // Open here: its type at the call.
            let at = seen.get(u).cloned().unwrap_or(Ty::Var(*u));
            match head_var(&at).and_then(|b| order.iter().position(|&o| o == b)) {
                Some(j) => from_call.push(HirDictOp::Kinds(common::kinds_from_call(order.len() as u32, i as u32, j as u32))),
                None => {
                    vars.insert(*v, at);
                }
            }
        }
        let mut ops = self.kinds_ops_bound(&scheme.bounds, &vars);
        ops.extend(from_call);
        Some(ops)
    }

    /// For each constraint of generic function `name`, its value read at
    /// type `ty`: the index of the dictionary a call through the value
    /// passes for it, in [`value_constraint_order`] of the type the call
    /// sees (the `forall` parameter `expected`, else the function's own),
    /// when the constraint's types are open there (#2636). `None` for one
    /// the value captures: ground, or over the making body's own variables.
    pub(super) fn polyfn_call_dicts(&self, name: &str, ty: Option<&Ty>, expected: Option<&Ty>) -> Vec<Option<u32>> {
        let Some(scheme) = self.checker.env().lookup(name) else {
            return Vec::new();
        };
        let frame = self.hir_body_frame.as_ref().and_then(|f| f.kinds.as_ref());
        let ty = ty.map(|t| apply_ty_prune(self.checker.subst(), t));
        let (binders, body, constraints) = match expected.map(strip_readonly) {
            Some(Ty::Forall { bounds, constraints, body }) => (bounds, body.as_ref(), constraints),
            _ => (&scheme.bounds, &scheme.ty, &scheme.constraints),
        };
        let order = value_constraint_order(binders, body, constraints);
        let (mut vars, mut seen) = (HashMap::new(), HashMap::new());
        if let Some(t) = &ty {
            Self::bind_scheme_vars(&scheme.ty, t, &mut vars);
            Self::bind_scheme_vars(t, body, &mut seen);
        }
        scheme
            .constraints
            .iter()
            .map(|c| {
                let args: Vec<Ty> = c.args.iter().map(|a| a.subst_vars(&|v| vars.get(&v).cloned())).collect();
                let mut free = Vec::new();
                for a in &args {
                    a.collect_vars(&mut free);
                }
                if free.is_empty() || free.iter().any(|u| frame.is_some_and(|f| f.bit(*u).is_some())) {
                    return None;
                }
                let at: Vec<Ty> = args.iter().map(|a| a.subst_vars(&|v| seen.get(&v).cloned())).collect();
                order.iter().position(|o| o.class == c.class && o.args == at).map(|j| j as u32)
            })
            .collect()
    }

    /// [`Self::kinds_ops`] of call `id` in `hir` of `key`, its argument and
    /// result types read from the call.
    pub(super) fn hir_kinds_ops(&self, hir: &crate::hir::HirBody, id: crate::hir::HirId, key: &str, args: &[crate::hir::HirId]) -> Option<Vec<HirDictOp>> {
        self.kinds_scheme(key)?;
        let arg_tys: Vec<Ty> = args.iter().map(|&a| hir.expr(a).ty.clone().unwrap_or(Ty::Con(crate::typechecking::ty::UNIT.into()))).collect();
        self.kinds_ops(key, &arg_tys, hir.expr(id).ty.as_ref())
    }

    /// [`Self::kinds_ops`] of partial application `id` of `key` filling
    /// the parameters of `mask` (its `kinds` bit included) with `args`:
    /// the other parameters and the result are the function value's.
    pub(super) fn hir_kinds_ops_partial(&self, hir: &crate::hir::HirBody, id: crate::hir::HirId, key: &str, mask: u32, args: &[crate::hir::HirId]) -> Option<Vec<HirDictOp>> {
        let mut rest = apply_ty_prune(self.checker.subst(), hir.expr(id).ty.as_ref()?);
        let total = 31 - (mask.leading_zeros() as usize).min(31);
        let mut filled = args.iter();
        let mut arg_tys = Vec::with_capacity(total);
        for i in 0..total {
            if mask & (1 << i) != 0 {
                arg_tys.push(hir.expr(*filled.next()?).ty.clone()?);
            } else {
                let Ty::Fun(p, r) = rest else { return None };
                arg_tys.push(*p);
                rest = *r;
            }
        }
        self.kinds_ops(key, &arg_tys, Some(&rest))
    }

    /// The `kinds` word of instance `head`'s dictionary at goal `lookup`
    /// (#2506), as steps: bit `i` for the trait's parameter `i`, then one
    /// for each of the head's type variables in order of appearance,
    /// [`DICT_BITS`] wide. `None` when they do not fit.
    pub(super) fn dict_kinds_steps(&self, head: &[Ty], lookup: &[Ty]) -> Option<Vec<DictStep>> {
        let frame = self.hir_body_frame.as_ref().and_then(|f| f.kinds.as_ref());
        let mut vars = HashMap::new();
        for (have, want) in head.iter().zip(lookup) {
            Self::bind_scheme_vars(have, want, &mut vars);
        }
        let mut order = Vec::new();
        for h in head {
            h.collect_vars(&mut order);
        }
        let bound: Vec<Option<&Ty>> = lookup.iter().map(Some).chain(order.iter().map(|v| vars.get(v))).collect();
        if bound.len() > DICT_BITS as usize {
            return None;
        }
        let mut word = sentinel(DICT_BITS);
        let mut from = Vec::new();
        for (i, t) in bound.iter().enumerate() {
            match t.map(|t| self.kinds_bit(t, frame)) {
                Some(Bit::Const(true)) => word |= 1 << i,
                Some(Bit::From(j)) => from.push(DictStep::KindsBit { from: j as u8, to: i as u8 }),
                Some(Bit::Const(false)) | None => {}
            }
        }
        Some(std::iter::once(DictStep::Kinds(word)).chain(from).collect())
    }

    /// Where trait method `fqn` reads its `kinds` word from
    /// its own dictionary (#2506): a default body, over the trait's
    /// parameters, or a generic-head instance's method, over its head's
    /// variables. Each type variable's bit in the word, how many bits it
    /// has, and the word's index in the dictionary (after the methods).
    pub(super) fn dict_kinds_layout(&self, fqn: &str) -> Option<DictKinds> {
        let generics = self.checker.generics();
        let class = match fqn.split_once("__default__") {
            Some((class, _)) => class.to_string(),
            None => generics.instances().iter().find(|i| i.method_fqns.values().any(|f| f == fqn))?.class.clone(),
        };
        let class = class.as_str();
        let class_def = generics.typeclass(class)?;
        let index = class_def.flattened_methods(generics).len() as u32;
        let n_trait = class_def.type_params.len();
        if let Some((_, method)) = fqn.split_once("__default__") {
            let scheme = self.checker.typeclass_method_scheme(class, method)?;
            let bits = scheme.bounds.iter().take(n_trait).enumerate().map(|(i, &v)| (v, i as u32)).collect();
            return Some((index, n_trait as u32, bits));
        }
        let inst = generics.instances().iter().find(|i| i.class == class && i.method_fqns.values().any(|f| f == fqn))?;
        let mut order = Vec::new();
        for h in &inst.args {
            h.collect_vars(&mut order);
        }
        if order.is_empty() || inst.args.len() + order.len() > DICT_BITS as usize {
            return None;
        }
        let n_trait = inst.args.len();
        let bits = order.iter().enumerate().map(|(k, &v)| (v, (n_trait + k) as u32)).collect();
        Some((index, (n_trait + order.len()) as u32, bits))
    }

    /// Whether `fqn` is a trait method's shared entry (an instance's method
    /// or a default body), not one of their per-type clones: a clone takes
    /// no dictionary, only the own `kinds` word (#2506).
    pub(super) fn trait_entry_shared(&self, fqn: &str) -> bool {
        fqn.split_once("__default__").is_some_and(|(_, m)| !m.contains('$'))
            || self.checker.generics().instances().iter().any(|i| i.method_fqns.values().any(|f| f == fqn))
    }

    /// Whether instance method `fqn` reads its dictionary's `kinds` word:
    /// it is a generic-head instance's (#2506).
    pub(super) fn instance_reads_kinds(&self, fqn: &str) -> bool {
        let generics = self.checker.generics();
        generics
            .instances()
            .iter()
            .find(|i| i.method_fqns.values().any(|f| f == fqn))
            .is_some_and(|i| i.args.iter().any(Ty::has_var))
            && !self.raw_instance_methods.contains(fqn)
    }
}

/// The type variables of a function value's type `body` among `binders`
/// that a call through the value passes a bit for, in that order: their
/// order of first appearance, so a type and one it renames into another
/// declared order agree (#2621). At most
/// [`common::KINDS_CALL_COUNT_SHIFT`].
pub(super) fn value_kinds_order(binders: &[TyVarId], body: &Ty) -> Vec<TyVarId> {
    let mut order = Vec::new();
    body.collect_vars(&mut order);
    order.retain(|v| binders.contains(v));
    order.truncate(common::KINDS_CALL_COUNT_SHIFT as usize);
    order
}

/// The type variable `ty` is, or the head of its application is.
fn head_var(ty: &Ty) -> Option<TyVarId> {
    match strip_readonly(ty) {
        Ty::Var(v) => Some(*v),
        Ty::App(head, _) => match strip_readonly(head) {
            Ty::Var(v) => Some(*v),
            _ => None,
        },
        _ => None,
    }
}

/// `constraints` of a function value's type `body` over `binders`, in the
/// order a call through the value passes their dictionaries (#2636): by
/// the places of their type variables in [`value_kinds_order`], then by
/// class, so a type and one that renames it, whatever order either lists
/// its constraints in, agree.
pub(super) fn value_constraint_order(binders: &[TyVarId], body: &Ty, constraints: &[Constraint]) -> Vec<Constraint> {
    let order = value_kinds_order(binders, body);
    let mut sorted = constraints.to_vec();
    sorted.sort_by_cached_key(|c| {
        let mut vars = Vec::new();
        for a in &c.args {
            a.collect_vars(&mut vars);
        }
        let places: Vec<usize> = vars.iter().map(|v| order.iter().position(|o| o == v).unwrap_or(usize::MAX)).collect();
        (places, c.class.clone())
    });
    sorted
}
