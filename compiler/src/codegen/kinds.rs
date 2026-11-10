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
use crate::typechecking::ty::{strip_readonly, Scheme, TyVarId};

/// The bits a `kinds` word carries past its mask.
pub(crate) const SENTINEL: u64 = 0xC0DE_5A3C_A5C3_3C5A;

/// The most type parameters a shared generic entry may have: one bit each.
pub(crate) const MAX_PARAMS: usize = 64;

/// [`SENTINEL`]'s bits past a mask of `n` parameters.
pub(crate) fn sentinel(n: u32) -> u64 {
    if n >= 64 { 0 } else { SENTINEL & (u64::MAX << n) }
}

/// Whether a shared generic entry checks its `kinds` word's sentinel bits:
/// debug builds, and any build under `COIL_MIR_VERIFY`.
pub(crate) fn check_on() -> bool {
    cfg!(debug_assertions) || crate::mir::verify_asked()
}

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

    /// The `kinds` frame of shared body `hir`, entry `key`, whose word is in
    /// `slot`: its scheme's variables' bits (and those of the body's own
    /// variables its parameters bind them to), `self`'s scheme type, and
    /// the sentinel check's message.
    pub(super) fn kinds_frame(&mut self, key: &str, hir: &crate::hir::HirBody, slot: u32) -> frame::FrameKinds {
        let scheme = self.kinds_scheme(key).expect("a kinds entry's scheme");
        let mut bits: Vec<(TyVarId, u32)> = scheme.bounds.iter().enumerate().map(|(i, &v)| (v, i as u32)).collect();
        let (params, _) = Self::fun_param_and_ret_tys(&scheme.ty);
        // The scheme may or may not list `self`.
        let skip = hir.params.len().saturating_sub(params.len());
        let mut map = HashMap::new();
        for (p, &local) in params.iter().zip(&hir.params[skip..]) {
            if let Some(ty) = hir.local(local).ty.as_ref() {
                Self::bind_scheme_vars(p, &apply_ty_prune(self.checker.subst(), ty), &mut map);
            }
        }
        for (i, v) in scheme.bounds.iter().enumerate() {
            if let Some(Ty::Var(u)) = map.get(v).map(strip_readonly)
                && !bits.iter().any(|&(w, _)| w == *u)
            {
                bits.push((*u, i as u32));
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
        let self_ty = (self.compiling_method && skip == 0)
            .then(|| params.first().cloned())
            .flatten()
            .filter(|t| matches!(strip_readonly(t), Ty::App(head, _) if matches!(head.as_ref(), Ty::Con(_))));
        let check = check_on().then(|| self.intern_string(format!("internal error: `{key}` entered without its kinds word (compiler bug)")));
        frame::FrameKinds { slot, bits, n: scheme.bounds.len() as u32, self_ty, check }
    }

    /// The bit a call passes for a type parameter at `ty`.
    fn kinds_bit(&self, ty: &Ty, frame: Option<&frame::FrameKinds>) -> Bit {
        let ty = apply_ty_prune(self.checker.subst(), ty);
        let var = match strip_readonly(&ty) {
            Ty::Var(v) => Some(*v),
            Ty::App(head, _) => match strip_readonly(head) {
                Ty::Var(v) => Some(*v),
                _ => None,
            },
            _ => None,
        };
        match var {
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
        let frame = self.hir_body_frame.as_ref().and_then(|f| f.kinds.as_ref());
        let (params, ret) = Self::fun_param_and_ret_tys(&scheme.ty);
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
        let n = scheme.bounds.len() as u32;
        let mut word = sentinel(n);
        let mut from = Vec::new();
        for (i, v) in scheme.bounds.iter().enumerate().take(MAX_PARAMS) {
            match vars.get(v).map(|t| self.kinds_bit(t, frame)) {
                Some(Bit::Const(true)) => word |= 1 << i,
                Some(Bit::From(j)) => from.push(HirDictOp::KindsBit { from: j as u8, to: i as u8 }),
                Some(Bit::Const(false)) | None => {}
            }
        }
        Some(std::iter::once(HirDictOp::Kinds(word)).chain(from).collect())
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
}
