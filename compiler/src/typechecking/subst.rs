//! Substitutions: apply, compose, and free-variable helpers.
//!
//! `apply_ty` does a single lookup per variable (no chain chasing) so
//! [`compose`] stays correct. Use [`apply_ty_prune`] for fully resolved types.

use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasherDefault, Hasher};
use std::sync::Arc;

use super::ty::{Scheme, Ty, TyVarId, ftv_ty};

/// Hasher for dense `TyVarId` keys: one multiply, no SipHash rounds.
#[derive(Default, Clone, Copy)]
struct VarHasher(u64);

impl Hasher for VarHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 = (self.0.rotate_left(5) ^ u64::from(b)).wrapping_mul(0x517c_c1b7_2722_0a95);
        }
    }

    fn write_u32(&mut self, n: u32) {
        self.0 = (self.0 ^ u64::from(n)).wrapping_mul(0x517c_c1b7_2722_0a95);
    }
}

type VarIndex = HashMap<TyVarId, usize, BuildHasherDefault<VarHasher>>;
type PosSet = HashSet<usize, BuildHasherDefault<VarHasher>>;

/// A partial map from `TyVarId` to `Ty`.
///
/// `mappings` is iterated in insertion order, which makes the result of
/// `compose` deterministic (important for snapshot tests). `index` maps a
/// variable to its position, so lookups and inserts are O(1), and the data
/// is shared copy-on-write, so a clone is O(1): unification returns the
/// whole substitution from every leaf, and a whole program's checker
/// composes after every unification (#598).
///
/// A write to shared data goes to `local`, a small overlay, rather than
/// copying it: unification extends the checker's substitution by a few
/// bindings, and copying a whole program's worth for each made checking
/// quadratic in its size (#1857). [`Subst::absorb`] folds such an
/// extension back in place.
#[derive(Debug, Clone, Default)]
pub struct Subst {
    data: Arc<SubstData>,
    /// Bindings over `data`, newest last; one for a variable `data` binds
    /// shadows it.
    local: Vec<(TyVarId, Ty)>,
}

#[derive(Debug, Clone, Default)]
struct SubstData {
    mappings: Vec<(TyVarId, Ty)>,
    index: VarIndex,
    /// Variable to the positions whose type mentioned it (a superset:
    /// a rewritten position stays listed).
    users: HashMap<TyVarId, Vec<usize>, BuildHasherDefault<VarHasher>>,
    /// Positions whose type may mention a bound variable (a superset), the
    /// only ones besides the users of new bindings that `compose` changes.
    stale: PosSet,
}

impl SubstData {
    fn mentions_bound(&self, ty: &Ty) -> bool {
        ftv_ty(ty).iter().any(|v| self.index.contains_key(v))
    }

    /// Note what `mappings[pos]`, just written, mentions.
    fn track(&mut self, pos: usize) {
        let free = ftv_ty(&self.mappings[pos].1);
        if free.iter().any(|v| self.index.contains_key(v)) {
            self.stale.insert(pos);
        }
        for v in free {
            self.users.entry(v).or_default().push(pos);
        }
    }

    fn insert(&mut self, v: TyVarId, ty: Ty) {
        if let Some(&pos) = self.index.get(&v) {
            self.mappings[pos].1 = ty;
            self.track(pos);
            return;
        }
        let pos = self.mappings.len();
        self.index.insert(v, pos);
        self.mappings.push((v, ty));
        // Whatever mentions `v` now mentions a bound variable.
        if let Some(users) = self.users.get(&v) {
            self.stale.extend(users.iter().copied());
        }
        self.track(pos);
    }

    fn reindex(&mut self) {
        self.index.clear();
        self.users.clear();
        self.stale.clear();
        for (pos, (v, _)) in self.mappings.iter().enumerate() {
            self.index.insert(*v, pos);
        }
        for pos in 0..self.mappings.len() {
            self.track(pos);
        }
    }
}

impl PartialEq for Subst {
    fn eq(&self, other: &Self) -> bool {
        (Arc::ptr_eq(&self.data, &other.data) && self.local == other.local) || self.iter().eq(other.iter())
    }
}

impl Eq for Subst {}

impl Subst {
    /// Build an empty substitution.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Build an empty substitution with room for `n` bindings.
    fn with_capacity(n: usize) -> Self {
        Self {
            data: Arc::new(SubstData {
                mappings: Vec::with_capacity(n),
                index: VarIndex::with_capacity_and_hasher(n, Default::default()),
                ..SubstData::default()
            }),
            local: Vec::new(),
        }
    }

    /// Build a substitution with a single binding.
    pub fn singleton(v: TyVarId, ty: Ty) -> Self {
        let mut s = Self::with_capacity(1);
        s.insert(v, ty);
        s
    }

    /// Insert (or overwrite) a binding. Last write wins, matching the
    /// canonical "most recent substitution wins" semantics used by
    /// `compose`.
    pub fn insert(&mut self, v: TyVarId, ty: Ty) {
        if let Some(slot) = self.local.iter_mut().find(|(k, _)| *k == v) {
            slot.1 = ty;
        } else if let Some(data) = Arc::get_mut(&mut self.data) {
            data.insert(v, ty);
        } else {
            self.local.push((v, ty));
        }
    }

    /// Fold `local` into `data`, copying `data` if it is shared.
    fn own(&mut self) -> &mut SubstData {
        let local = std::mem::take(&mut self.local);
        let data = Arc::make_mut(&mut self.data);
        for (v, ty) in local {
            data.insert(v, ty);
        }
        data
    }

    /// Remove any binding for `v`. Returns the removed value if there was
    /// one.
    pub fn remove(&mut self, v: TyVarId) -> Option<Ty> {
        if !self.contains(v) {
            return None;
        }
        let data = self.own();
        let pos = data.index.remove(&v)?;
        let (_, ty) = data.mappings.remove(pos);
        data.reindex();
        Some(ty)
    }

    /// True if `v` is in the domain of this substitution.
    pub fn contains(&self, v: TyVarId) -> bool {
        self.get(v).is_some()
    }

    /// Look up the binding for `v`, if any.
    pub fn get(&self, v: TyVarId) -> Option<&Ty> {
        match self.local.iter().rev().find(|(k, _)| *k == v) {
            Some((_, t)) => Some(t),
            None => self.data.index.get(&v).map(|&pos| &self.data.mappings[pos].1),
        }
    }

    /// True when `other` is this very substitution (a clone that was never
    /// written to): `unify_with` hands back its input when it binds nothing.
    pub fn is_same(&self, other: &Subst) -> bool {
        Arc::ptr_eq(&self.data, &other.data) && self.local.is_empty() && other.local.is_empty()
    }

    /// Number of bindings.
    pub fn len(&self) -> usize {
        self.data.mappings.len() + self.local.iter().filter(|(k, _)| !self.data.index.contains_key(k)).count()
    }

    /// True if the substitution has no bindings.
    pub fn is_empty(&self) -> bool {
        self.data.mappings.is_empty() && self.local.is_empty()
    }

    /// Iterate over `(variable, type)` pairs in insertion order.
    pub fn iter(&self) -> impl Iterator<Item = (TyVarId, &Ty)> {
        let shadowed = |k: TyVarId| self.local.iter().rev().find(|(l, _)| *l == k).map(|(_, t)| t);
        self.data
            .mappings
            .iter()
            .map(move |(k, v)| (*k, shadowed(*k).unwrap_or(v)))
            .chain(self.local.iter().filter(|(k, _)| !self.data.index.contains_key(k)).map(|(k, v)| (*k, v)))
    }

    /// Free variables of the codomain (i.e. of every mapped-to type).
    /// Useful when checking whether a substitution is closed.
    pub fn ftv(&self) -> HashSet<TyVarId> {
        let mut acc = HashSet::new();
        for (_, t) in self.iter() {
            acc.extend(ftv_ty(t));
        }
        acc
    }

    /// `*self = compose(&s, self)`, in place when `s` is `self` extended
    /// with new bindings (what [`unify_with`](super::unify::unify_with)
    /// returns): only the new bindings' users and the positions that may
    /// mention a bound variable change, so the cost follows the extension,
    /// not the whole substitution (#1857).
    ///
    /// When `s` bound nothing it is `self` itself, and composing a
    /// substitution with itself only re-resolves chains one step, which
    /// every reader prunes anyway: that is skipped (#607).
    pub fn absorb(&mut self, s: Subst) {
        if s.is_same(self) {
            return;
        }
        let extends = Arc::ptr_eq(&self.data, &s.data)
            && self.local.is_empty()
            && s.local.iter().all(|(k, _)| !s.data.index.contains_key(k));
        if !extends {
            *self = compose(&s, self);
            return;
        }
        let mut work: PosSet = s.data.stale.clone();
        for (k, _) in &s.local {
            if let Some(users) = s.data.users.get(k) {
                work.extend(users.iter().copied());
            }
        }
        let mut work: Vec<usize> = work.into_iter().collect();
        work.sort_unstable();
        // Every new value reads `s` as it stands, as `compose` does.
        let updates: Vec<(usize, Ty)> = work
            .into_iter()
            .filter_map(|pos| {
                let old = &s.data.mappings[pos].1;
                let new = apply_ty(&s, old);
                (new != *old).then_some((pos, new))
            })
            .collect();
        let Subst { data, local } = s;
        drop(data);
        let data = Arc::make_mut(&mut self.data);
        for (pos, ty) in updates {
            data.mappings[pos].1 = ty;
            data.track(pos);
        }
        for (v, ty) in local {
            data.insert(v, ty);
        }
        let stale: Vec<usize> = data.stale.iter().copied().collect();
        for pos in stale {
            if !data.mentions_bound(&data.mappings[pos].1) {
                data.stale.remove(&pos);
            }
        }
    }
}

impl From<Vec<(TyVarId, Ty)>> for Subst {
    fn from(v: Vec<(TyVarId, Ty)>) -> Self {
        let mut s = Subst::empty();
        for (k, val) in v {
            s.insert(k, val);
        }
        s
    }
}

/// Apply a substitution to a `Ty`. For `Var(v)`, returns the bound type
/// directly (single lookup — does not chase chains).
pub fn apply_ty(subst: &Subst, ty: &Ty) -> Ty {
    match ty {
        Ty::Var(v) => match subst.get(*v) {
            Some(t) => t.clone(),
            None => Ty::Var(*v),
        },
        Ty::Con(_) | Ty::Existential { .. } | Ty::Never => ty.clone(),
        Ty::Fun(a, b) => Ty::Fun(Box::new(apply_ty(subst, a)), Box::new(apply_ty(subst, b))),
        Ty::App(c, args) => Ty::App(
            Box::new(apply_ty(subst, c)),
            args.iter().map(|t| apply_ty(subst, t)).collect(),
        ),
        Ty::Sum { name, variants } => Ty::Sum {
            name: name.clone(),
            variants: variants
                .iter()
                .map(|(n, payload)| {
                    let new_payload = match payload {
                        crate::typechecking::ty::EnumVariantPayloadTy::Unit => {
                            crate::typechecking::ty::EnumVariantPayloadTy::Unit
                        }
                        crate::typechecking::ty::EnumVariantPayloadTy::Tuple(tys) => {
                            crate::typechecking::ty::EnumVariantPayloadTy::Tuple(
                                tys.iter().map(|t| apply_ty(subst, t)).collect(),
                            )
                        }
                        crate::typechecking::ty::EnumVariantPayloadTy::Record(fields) => {
                            crate::typechecking::ty::EnumVariantPayloadTy::Record(
                                fields
                                    .iter()
                                    .map(|(n, t)| (n.clone(), apply_ty(subst, t)))
                                    .collect(),
                            )
                        }
                    };
                    (n.clone(), new_payload)
                })
                .collect(),
        },
        Ty::Constructor { owner, tag, arity } => Ty::Constructor {
            owner: Box::new(apply_ty(subst, owner)),
            tag: *tag,
            arity: *arity,
        },
        // length and field names are inert
        Ty::Tuple(tys) => Ty::Tuple(tys.iter().map(|t| apply_ty(subst, t)).collect()),
        Ty::Array { element, length } => Ty::Array {
            element: Box::new(apply_ty(subst, element)),
            length: *length,
        },
        Ty::Record { fields } => Ty::Record {
            fields: fields
                .iter()
                .map(|(n, t)| (n.clone(), apply_ty(subst, t)))
                .collect(),
        },
        Ty::Forall {
            bounds,
            constraints,
            body,
        } => {
            let mut inner = subst.clone();
            for bound in bounds {
                let _ = inner.remove(*bound);
            }
            Ty::Forall {
                bounds: bounds.clone(),
                constraints: constraints
                    .iter()
                    .map(|c| super::ty::Constraint {
                        class: c.class.clone(),
                        args: c.args.iter().map(|a| apply_ty(&inner, a)).collect(),
                    })
                    .collect(),
                body: Box::new(apply_ty(&inner, body)),
            }
        }
        Ty::Readonly(inner) => Ty::Readonly(Box::new(apply_ty(subst, inner))),
    }
}

/// Apply repeatedly until fixed point (diagnostics / pretty-printing).
pub fn apply_ty_prune(subst: &Subst, ty: &Ty) -> Ty {
    let mut current = apply_ty(subst, ty);
    loop {
        let next = apply_ty(subst, &current);
        if next == current {
            return current;
        }
        current = next;
    }
}

/// Apply a substitution to a `Scheme`. Quantified variables are preserved.
#[allow(dead_code)]
pub fn apply_scheme(subst: &Subst, s: &Scheme) -> Scheme {
    let mut inner = subst.clone();
    for bound in &s.bounds {
        let _ = inner.remove(*bound);
    }
    Scheme {
        bounds: s.bounds.clone(),
        kinds: s.kinds.clone(),
        constraints: s
            .constraints
            .iter()
            .map(|c| super::ty::Constraint {
                class: c.class.clone(),
                args: c.args.iter().map(|a| apply_ty(&inner, a)).collect(),
            })
            .collect(),
        assoc_projections: s
            .assoc_projections
            .iter()
            .map(|p| super::ty::AssocProjection {
                var: p.var,
                name: p.name.clone(),
                args: p.args.iter().map(|a| apply_ty(&inner, a)).collect(),
            })
            .collect(),
        ty: apply_ty(&inner, &s.ty),
    }
}

/// Compose: `apply(compose(s1, s2), t) == apply(s1, apply(s2, t))`.
pub fn compose(s1: &Subst, s2: &Subst) -> Subst {
    let mut result = Subst::with_capacity(s1.len() + s2.len());
    for (v, t) in s2.iter() {
        result.insert(v, apply_ty(s1, t));
    }
    for (v, t) in s1.iter() {
        if !result.contains(v) {
            result.insert(v, t.clone());
        }
    }
    result
}

/// Union of two substitutions. For keys in both, `s2`'s value wins
/// (consistent with `compose`).
#[allow(dead_code)] // exposed for diagnostic helpers
pub fn union(s1: &Subst, s2: &Subst) -> Subst {
    let mut result = s1.clone();
    for (v, t) in s2.iter() {
        result.insert(v, t.clone());
    }
    result
}

/// Re-export of `ftv_ty` at the module level for ergonomics.
#[allow(dead_code)]
pub fn ftv(ty: &Ty) -> HashSet<TyVarId> {
    ftv_ty(ty)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::typechecking::ty::{array, float, int, string};

    fn v(i: u32) -> Ty {
        Ty::Var(TyVarId(i))
    }

    #[test]
    fn apply_passes_through_constructor() {
        let s = Subst::empty();
        assert_eq!(apply_ty(&s, &int()), int());
        assert_eq!(apply_ty(&s, &float()), float());
        assert_eq!(apply_ty(&s, &string()), string());
    }

    #[test]
    fn apply_replaces_var_with_bound_type() {
        let s = Subst::singleton(TyVarId(0), int());
        assert_eq!(apply_ty(&s, &v(0)), int());
    }

    #[test]
    fn apply_unbound_var_is_identity() {
        let s = Subst::empty();
        assert_eq!(apply_ty(&s, &v(7)), v(7));
    }

    #[test]
    fn apply_recurses_through_fun() {
        let s = Subst::singleton(TyVarId(0), int());
        let ty = Ty::Fun(Box::new(v(0)), Box::new(string()));
        assert_eq!(
            apply_ty(&s, &ty),
            Ty::Fun(Box::new(int()), Box::new(string()))
        );
    }

    #[test]
    fn apply_recurses_through_app() {
        let s = Subst::singleton(TyVarId(0), int());
        let ty = Ty::App(Box::new(Ty::Con("Foo".into())), vec![v(0), v(1)]);
        assert_eq!(
            apply_ty(&s, &ty),
            Ty::App(Box::new(Ty::Con("Foo".into())), vec![int(), v(1)])
        );
    }

    #[test]
    fn apply_recurses_through_array() {
        let s = Subst::singleton(TyVarId(0), int());
        assert_eq!(apply_ty(&s, &array(v(0))), array(int()));
    }

    #[test]
    fn apply_does_not_chains_through_bound_var() {
        // With non-recursive apply, α → β, β → int resolves α to β (not
        // int). The chain is not chased.
        let mut s = Subst::empty();
        s.insert(TyVarId(0), v(1));
        s.insert(TyVarId(1), int());
        assert_eq!(apply_ty(&s, &v(0)), v(1));
    }

    #[test]
    fn apply_does_not_chains_in_fun() {
        let mut s = Subst::empty();
        s.insert(TyVarId(0), v(1));
        s.insert(TyVarId(1), int());
        let ty = Ty::Fun(Box::new(v(0)), Box::new(v(0)));
        assert_eq!(apply_ty(&s, &ty), Ty::Fun(Box::new(v(1)), Box::new(v(1))));
    }

    #[test]
    fn apply_prune_fully_resolves_chains() {
        // apply_ty_prune re-applies until no change.
        let mut s = Subst::empty();
        s.insert(TyVarId(0), v(1));
        s.insert(TyVarId(1), int());
        assert_eq!(apply_ty_prune(&s, &v(0)), int());
        assert_eq!(
            apply_ty_prune(&s, &Ty::Fun(Box::new(v(0)), Box::new(v(0)))),
            Ty::Fun(Box::new(int()), Box::new(int())),
        );
    }

    #[test]
    fn apply_prune_is_idempotent_on_already_resolved() {
        // Applying to a fully-resolved type is a no-op.
        let mut s = Subst::empty();
        s.insert(TyVarId(0), int());
        assert_eq!(apply_ty_prune(&s, &int()), int());
        assert_eq!(apply_ty_prune(&s, &v(0)), int());
    }

    #[test]
    fn apply_scheme_preserves_bounds() {
        let s = Subst::singleton(TyVarId(0), int());
        let scheme = Scheme {
            bounds: vec![TyVarId(0)],
            kinds: vec![],
            constraints: vec![],
            assoc_projections: vec![],
            ty: v(0),
        };
        let result = apply_scheme(&s, &scheme);
        assert_eq!(result.bounds, vec![TyVarId(0)]);
        assert_eq!(result.ty, v(0));
    }

    #[test]
    fn apply_scheme_applies_to_free_vars() {
        let s = Subst::singleton(TyVarId(1), int());
        let scheme = Scheme {
            bounds: vec![TyVarId(0)],
            kinds: vec![],
            constraints: vec![],
            assoc_projections: vec![],
            ty: Ty::Fun(Box::new(v(0)), Box::new(v(1))),
        };
        let result = apply_scheme(&s, &scheme);
        assert_eq!(result.ty, Ty::Fun(Box::new(v(0)), Box::new(int())));
    }

    #[test]
    fn insert_overwrites_in_place_and_remove_reindexes() {
        let mut s = Subst::empty();
        for i in 0..5 {
            s.insert(TyVarId(i), v(i + 10));
        }
        s.insert(TyVarId(2), int());
        assert_eq!(s.len(), 5);
        assert_eq!(s.get(TyVarId(2)), Some(&int()));
        assert_eq!(s.remove(TyVarId(1)), Some(v(11)));
        assert_eq!(s.remove(TyVarId(1)), None);
        let order: Vec<u32> = s.iter().map(|(k, _)| k.0).collect();
        assert_eq!(order, vec![0, 2, 3, 4]);
        for i in [0, 2, 3, 4] {
            assert!(s.contains(TyVarId(i)));
        }
        assert_eq!(s.get(TyVarId(4)), Some(&v(14)));
    }

    #[test]
    fn clones_are_independent() {
        let mut a = Subst::singleton(TyVarId(0), int());
        let b = a.clone();
        a.insert(TyVarId(1), string());
        a.insert(TyVarId(0), float());
        assert_eq!(b.len(), 1);
        assert_eq!(b.get(TyVarId(0)), Some(&int()));
        assert_eq!(a.get(TyVarId(0)), Some(&float()));
        assert_ne!(a, b);
    }

    #[test]
    fn compose_of_two_empties_is_empty() {
        assert_eq!(compose(&Subst::empty(), &Subst::empty()), Subst::empty());
    }

    #[test]
    fn absorb_in_place_matches_compose() {
        // A base with a chain, extended through a shared clone (as
        // `unify_with` does) a few times: absorbing each extension in
        // place gives exactly what composing does.
        let fun = |a: Ty, b: Ty| Ty::Fun(Box::new(a), Box::new(b));
        let mut base = Subst::empty();
        base.insert(TyVarId(0), fun(v(1), v(2)));
        base.insert(TyVarId(3), v(4));
        base.insert(TyVarId(4), v(5));
        let mut in_place = base.clone();
        let mut composed = base;
        let steps = [
            vec![(1, int()), (2, v(6))],
            vec![(6, v(7)), (7, string())],
            vec![(5, fun(v(8), v(0)))],
            vec![(8, float())],
        ];
        for step in steps {
            let mut ext = in_place.clone();
            for (k, t) in step {
                ext.insert(TyVarId(k), t);
            }
            composed = compose(&ext, &composed);
            in_place.absorb(ext);
            let got: Vec<(TyVarId, Ty)> = in_place.iter().map(|(k, t)| (k, t.clone())).collect();
            let want: Vec<(TyVarId, Ty)> = composed.iter().map(|(k, t)| (k, t.clone())).collect();
            assert_eq!(got, want);
        }
    }

    #[test]
    fn compose_resolves_chains() {
        // s1 = {α → β}, s2 = {β → int}; compose should give α → β, β → int.
        // With non-recursive apply, α resolves to β (not int).
        let s1 = Subst::singleton(TyVarId(0), v(1));
        let s2 = Subst::singleton(TyVarId(1), int());
        let composed = compose(&s1, &s2);
        assert_eq!(apply_ty(&composed, &v(0)), v(1));
        assert_eq!(apply_ty(&composed, &v(1)), int());
    }

    #[test]
    fn compose_matches_double_apply() {
        // The defining equation: apply(compose(s1, s2), t) == apply(s1, apply(s2, t))
        let s1 = Subst::singleton(TyVarId(0), v(1));
        let s2 = Subst::singleton(TyVarId(1), string());

        let t = Ty::Fun(Box::new(v(0)), Box::new(v(0)));

        let lhs = apply_ty(&compose(&s1, &s2), &t);
        let rhs = apply_ty(&s1, &apply_ty(&s2, &t));
        assert_eq!(lhs, rhs);
    }

    #[test]
    fn compose_is_associative() {
        // (s3 ∘ s2) ∘ s1 == s3 ∘ (s2 ∘ s1)
        let s1 = Subst::singleton(TyVarId(0), v(2));
        let s2 = Subst::singleton(TyVarId(1), v(0));
        let s3 = Subst::singleton(TyVarId(3), v(1));

        let lhs = compose(&compose(&s3, &s2), &s1);
        let rhs = compose(&s3, &compose(&s2, &s1));

        let t = Ty::Fun(Box::new(v(3)), Box::new(v(3)));
        assert_eq!(apply_ty(&lhs, &t), apply_ty(&rhs, &t));
    }

    #[test]
    fn compose_left_keeps_disjoint_keys() {
        let s1 = Subst::singleton(TyVarId(0), int());
        let s2 = Subst::singleton(TyVarId(1), string());
        let composed = compose(&s1, &s2);
        assert_eq!(apply_ty(&composed, &v(0)), int());
        assert_eq!(apply_ty(&composed, &v(1)), string());
    }

    #[test]
    fn compose_for_shared_keys_applies_s1_to_s2_value() {
        // Mathematical compose: s1 ∘ s2(α) = s1(s2(α)).
        // For s1 = {α → int}, s2 = {α → string}, this gives s1(string)
        // = string (since string is not in s1's domain). s2's value wins
        // (after applying s1).
        let s1 = Subst::singleton(TyVarId(0), int());
        let s2 = Subst::singleton(TyVarId(0), string());
        let composed = compose(&s1, &s2);
        assert_eq!(apply_ty(&composed, &v(0)), string());
    }

    #[test]
    fn union_combines_domains() {
        let s1 = Subst::singleton(TyVarId(0), int());
        let s2 = Subst::singleton(TyVarId(1), string());
        let u = union(&s1, &s2);
        assert_eq!(u.len(), 2);
        assert_eq!(apply_ty(&u, &v(0)), int());
        assert_eq!(apply_ty(&u, &v(1)), string());
    }

    #[test]
    fn union_lets_right_side_win_for_shared_keys() {
        let s1 = Subst::singleton(TyVarId(0), int());
        let s2 = Subst::singleton(TyVarId(0), string());
        let u = union(&s1, &s2);
        assert_eq!(apply_ty(&u, &v(0)), string());
    }

    #[test]
    fn subst_singleton_len_and_get() {
        let s = Subst::singleton(TyVarId(0), int());
        assert_eq!(s.len(), 1);
        assert!(!s.is_empty());
        assert!(s.contains(TyVarId(0)));
        assert!(!s.contains(TyVarId(1)));
        assert_eq!(s.get(TyVarId(0)), Some(&int()));
    }

    #[test]
    fn subst_insert_overwrites_existing() {
        let mut s = Subst::singleton(TyVarId(0), int());
        s.insert(TyVarId(0), string());
        assert_eq!(s.len(), 1);
        assert_eq!(s.get(TyVarId(0)), Some(&string()));
    }

    #[test]
    fn subst_remove_drops_binding() {
        let mut s = Subst::singleton(TyVarId(0), int());
        let removed = s.remove(TyVarId(0));
        assert_eq!(removed, Some(int()));
        assert!(s.is_empty());
    }

    #[test]
    fn subst_ftv_is_union_of_codomain_ftvs() {
        let mut s = Subst::empty();
        s.insert(TyVarId(0), Ty::Fun(Box::new(v(1)), Box::new(int())));
        s.insert(TyVarId(2), v(1));
        assert_eq!(s.ftv(), HashSet::from([TyVarId(1)]));
    }
}
