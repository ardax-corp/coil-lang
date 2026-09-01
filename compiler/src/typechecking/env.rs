//! Scoped environments and scheme instantiation ([`instantiate_with_kinds`]).
//!
//! Production does not let-generalize (not Algorithm W). Bindings are
//! monomorphic unless syntax declares type parameters. `generalize` is
//! test-only.

use std::collections::{HashMap, HashSet};

use super::ty::{Scheme, Ty, TyVarId, ftv_scheme};
#[cfg(test)]
use super::ty::ftv_ty;

/// A counter that mints fresh `TyVarId`s. Each call to [`TyVarCounter::fresh`]
/// returns a distinct id.
///
/// Used by [`instantiate_with_kinds`] and inference to mint fresh type variables.
#[derive(Debug, Default, Clone)]
pub struct TyVarCounter {
    next: u32,
}

impl TyVarCounter {
    pub fn new() -> Self {
        Self { next: 0 }
    }

    /// Mint a new, previously-unused type-variable id.
    pub fn fresh(&mut self) -> TyVarId {
        let id = TyVarId(self.next);
        self.next += 1;
        id
    }

    /// The number of ids minted so far (i.e. `next`).
    #[cfg(test)]
    pub fn count(&self) -> u32 {
        self.next
    }
}

/// A single scope's bindings. Bindings are stored in insertion order so
/// that lookup can prefer the most recently inserted (shadowing) name in
/// linear time.
#[derive(Debug, Default, Clone)]
pub struct Frame {
    bindings: Vec<(String, Scheme)>,
}

impl Frame {
    pub fn new() -> Self {
        Self::default()
    }

    /// Bind `name` to `scheme` in this frame. Later bindings with the same
    /// name shadow earlier ones within the same frame.
    pub fn insert(&mut self, name: impl Into<String>, scheme: Scheme) {
        self.bindings.push((name.into(), scheme));
    }

    /// Number of bindings in this frame.
    pub fn len(&self) -> usize {
        self.bindings.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bindings.is_empty()
    }

    /// Look up `name` in this frame only (no parent walk).
    pub fn lookup(&self, name: &str) -> Option<&Scheme> {
        self.bindings
            .iter()
            .rev()
            .find(|(n, _)| n == name)
            .map(|(_, s)| s)
    }

    /// Iterate bindings in insertion order for tooling such as completions.
    pub fn bindings(&self) -> impl Iterator<Item = (&str, &Scheme)> {
        self.bindings
            .iter()
            .map(|(name, scheme)| (name.as_str(), scheme))
    }
}

/// A scoped environment: a stack of frames. The most recently pushed
/// frame is innermost; lookup walks innermost to outermost so that inner
/// bindings shadow outer ones.
#[derive(Debug, Default, Clone)]
pub struct Env {
    frames: Vec<Frame>,
}

impl Env {
    /// Empty environment with no frames.
    pub fn new() -> Self {
        Self::default()
    }

    /// Push a new (empty) frame on the stack. The frame becomes the
    /// innermost scope.
    pub fn push(&mut self) {
        self.frames.push(Frame::new());
    }

    /// Pop the innermost frame and return it. Returns `None` if the env
    /// is empty.
    pub fn pop(&mut self) -> Option<Frame> {
        self.frames.pop()
    }

    /// Replace all frames with a single empty frame; return the previous stack.
    /// Used by lambdas so only explicit `use` captures + params are visible.
    pub fn take_and_isolate(&mut self) -> Vec<Frame> {
        std::mem::replace(&mut self.frames, vec![Frame::new()])
    }

    /// Restore a frame stack previously returned by [`take_and_isolate`].
    pub fn restore_frames(&mut self, frames: Vec<Frame>) {
        self.frames = frames;
    }

    /// Collect every binding name visible in this env (all frames).
    pub fn all_names(&self) -> HashSet<String> {
        let mut names = HashSet::new();
        for frame in &self.frames {
            for (n, _) in &frame.bindings {
                names.insert(n.clone());
            }
        }
        names
    }

    /// Return visible names with inner frames shadowing outer frames.
    pub fn visible_names(&self) -> Vec<String> {
        let mut names = Vec::new();
        for frame in self.frames.iter().rev() {
            for (name, _) in frame.bindings.iter().rev() {
                if !names.iter().any(|seen| seen == name) {
                    names.push(name.clone());
                }
            }
        }
        names
    }

    /// Number of frames currently on the stack.
    pub fn depth(&self) -> usize {
        self.frames.len()
    }

    /// Insert a binding into the innermost frame.
    ///
    /// # Panics
    /// Panics if the env has no frames. Call [`push`](Self::push) first.
    pub fn insert_top(&mut self, name: impl Into<String>, scheme: Scheme) {
        let frame = self
            .frames
            .last_mut()
            .expect("Env::insert_top called with no frames on the stack");
        frame.insert(name, scheme);
    }

    /// Look up a name, walking from the innermost frame to the outermost.
    /// Returns the scheme of the most-recently-bound matching name.
    pub fn lookup(&self, name: &str) -> Option<&Scheme> {
        for frame in self.frames.iter().rev() {
            if let Some(s) = frame.lookup(name) {
                return Some(s);
            }
        }
        None
    }

    /// Free type variables of every scheme in the environment,
    /// excluding each scheme's quantified variables.
    pub fn ftv(&self) -> HashSet<TyVarId> {
        let mut acc = HashSet::new();
        for frame in &self.frames {
            for (_, scheme) in &frame.bindings {
                acc.extend(ftv_scheme(scheme));
            }
        }
        acc
    }

    /// Borrow the innermost frame mutably.
    pub fn top_mut(&mut self) -> Option<&mut Frame> {
        self.frames.last_mut()
    }

    /// Borrow the innermost frame.
    pub fn top(&self) -> Option<&Frame> {
        self.frames.last()
    }
}

/// Quantify type variables free in `ty` but not in `env`.
///
/// Not called on the typecheck path. Explicit `fn f<T>` / `class C<T>`
/// build schemes from syntax; `let` bindings are [`Scheme::mono`].
#[cfg(test)]
pub fn generalize(env: &Env, ty: &Ty) -> Scheme {
    let env_ftv = env.ftv();
    let ty_ftv = ftv_ty(ty);
    let bounds: Vec<TyVarId> = ty_ftv.difference(&env_ftv).copied().collect();
    Scheme {
        bounds,
        kinds: Vec::new(),
        constraints: Vec::new(),
        assoc_projections: Vec::new(),
        ty: ty.clone(),
    }
}

/// Replace the quantified variables of `scheme` with fresh ones drawn
/// from `counter`, returning the resulting monotype.
///
/// The fresh variables are minted in the order they appear in
/// `scheme.bounds`, so two instantiations of the same scheme produce
/// different but consistently-ordered substitutions.
///
/// Returns the instantiated type (constraints discarded — use
/// [`instantiate_with_kinds`] when they matter).
#[cfg(test)]
pub fn instantiate(scheme: &Scheme, counter: &mut TyVarCounter) -> Ty {
    instantiate_with_kinds(scheme, counter).0
}

/// Instantiate a scheme, returning the freshened type, constraints, and a
/// map from each fresh variable to its kind (Phase 5).
pub fn instantiate_with_kinds(
    scheme: &Scheme,
    counter: &mut TyVarCounter,
) -> (
    Ty,
    Vec<super::ty::Constraint>,
    HashMap<TyVarId, super::kind::Kind>,
) {
    use super::ty::Constraint;
    let mut fresh_kinds = HashMap::new();
    let mapping: HashMap<TyVarId, TyVarId> = scheme
        .bounds
        .iter()
        .enumerate()
        .map(|(i, &v)| {
            let fresh = counter.fresh();
            fresh_kinds.insert(fresh, scheme.kind_at(i));
            (v, fresh)
        })
        .collect();
    let ty = substitute_vars(&scheme.ty, &mapping);
    let constraints = scheme
        .constraints
        .iter()
        .map(|c| Constraint {
            class: c.class.clone(),
            args: c
                .args
                .iter()
                .map(|a| substitute_vars(a, &mapping))
                .collect(),
        })
        .collect();
    let _assoc_projections = scheme
        .assoc_projections
        .iter()
        .map(|p| super::ty::AssocProjection {
            var: mapping.get(&p.var).copied().unwrap_or(p.var),
            name: p.name.clone(),
            args: p
                .args
                .iter()
                .map(|a| substitute_vars(a, &mapping))
                .collect(),
        })
        .collect::<Vec<_>>();
    (ty, constraints, fresh_kinds)
}

/// Walk `ty`, replacing every variable in `mapping` with its mapped
/// value. Variables not in the mapping are left alone.
pub(crate) fn substitute_vars(ty: &Ty, mapping: &HashMap<TyVarId, TyVarId>) -> Ty {
    match ty {
        Ty::Var(v) => match mapping.get(v) {
            Some(&new) => Ty::Var(new),
            None => Ty::Var(*v),
        },
        Ty::Con(_) | Ty::Existential { .. } | Ty::Never => ty.clone(),
        Ty::Fun(a, b) => Ty::Fun(
            Box::new(substitute_vars(a, mapping)),
            Box::new(substitute_vars(b, mapping)),
        ),
        Ty::App(c, args) => Ty::App(
            Box::new(substitute_vars(c, mapping)),
            args.iter().map(|t| substitute_vars(t, mapping)).collect(),
        ),
        Ty::List(inner) => Ty::List(Box::new(substitute_vars(inner, mapping))),
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
                                tys.iter().map(|t| substitute_vars(t, mapping)).collect(),
                            )
                        }
                        crate::typechecking::ty::EnumVariantPayloadTy::Record(fields) => {
                            crate::typechecking::ty::EnumVariantPayloadTy::Record(
                                fields
                                    .iter()
                                    .map(|(n, t)| (n.clone(), substitute_vars(t, mapping)))
                                    .collect(),
                            )
                        }
                    };
                    (n.clone(), new_payload)
                })
                .collect(),
        },
        Ty::Constructor { owner, tag, arity } => Ty::Constructor {
            owner: Box::new(substitute_vars(owner, mapping)),
            tag: *tag,
            arity: *arity,
        },
        Ty::Tuple(tys) => Ty::Tuple(tys.iter().map(|t| substitute_vars(t, mapping)).collect()),
        Ty::Array { element, length } => Ty::Array {
            element: Box::new(substitute_vars(element, mapping)),
            length: *length,
        },
        Ty::Record { fields } => Ty::Record {
            fields: fields
                .iter()
                .map(|(n, t)| (n.clone(), substitute_vars(t, mapping)))
                .collect(),
        },
        Ty::Forall {
            bounds,
            constraints,
            body,
        } => {
            let mut inner = mapping.clone();
            for bound in bounds {
                inner.remove(bound);
            }
            Ty::Forall {
                bounds: bounds.clone(),
                constraints: constraints
                    .iter()
                    .map(|c| super::ty::Constraint {
                        class: c.class.clone(),
                        args: c.args.iter().map(|a| substitute_vars(a, &inner)).collect(),
                    })
                    .collect(),
                body: Box::new(substitute_vars(body, &inner)),
            }
        }
        Ty::Readonly(inner) => Ty::Readonly(Box::new(substitute_vars(inner, mapping))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::typechecking::subst::{Subst, apply_ty_prune};
    use crate::typechecking::ty::{Ty, boolean, int, string};

    fn v(i: u32) -> Ty {
        Ty::Var(TyVarId(i))
    }

    // ---- TyVarCounter ----

    #[test]
    fn counter_starts_at_zero() {
        let c = TyVarCounter::new();
        assert_eq!(c.count(), 0);
    }

    #[test]
    fn counter_mints_distinct_ids() {
        let mut c = TyVarCounter::new();
        let a = c.fresh();
        let b = c.fresh();
        let d = c.fresh();
        assert_ne!(a, b);
        assert_ne!(b, d);
        assert_ne!(a, d);
        assert_eq!(c.count(), 3);
    }

    // ---- Env / Frame basics ----

    #[test]
    fn new_env_is_empty() {
        let env = Env::new();
        assert_eq!(env.depth(), 0);
        assert!(env.lookup("anything").is_none());
    }

    #[test]
    fn push_and_pop_frames() {
        let mut env = Env::new();
        env.push();
        env.push();
        assert_eq!(env.depth(), 2);
        env.pop();
        assert_eq!(env.depth(), 1);
        env.pop();
        assert_eq!(env.depth(), 0);
        assert!(env.pop().is_none());
    }

    #[test]
    fn insert_top_panics_without_frames() {
        let mut env = Env::new();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            env.insert_top("x", Scheme::mono(int()));
        }));
        assert!(result.is_err());
    }

    #[test]
    fn insert_and_lookup_roundtrip() {
        let mut env = Env::new();
        env.push();
        env.insert_top("x", Scheme::mono(int()));
        assert_eq!(env.lookup("x"), Some(&Scheme::mono(int())));
    }

    #[test]
    fn lookup_returns_none_for_unknown() {
        let mut env = Env::new();
        env.push();
        env.insert_top("x", Scheme::mono(int()));
        assert!(env.lookup("y").is_none());
    }

    #[test]
    fn inner_frame_shadows_outer() {
        let mut env = Env::new();
        env.push();
        env.insert_top("x", Scheme::mono(int()));

        env.push();
        env.insert_top("x", Scheme::mono(string()));
        assert_eq!(env.lookup("x"), Some(&Scheme::mono(string())));

        env.pop();
        assert_eq!(env.lookup("x"), Some(&Scheme::mono(int())));
    }

    #[test]
    fn lookup_walks_innermost_first() {
        let mut env = Env::new();
        env.push();
        env.insert_top("a", Scheme::mono(int()));
        env.insert_top("b", Scheme::mono(string()));
        env.push();
        env.insert_top("c", Scheme::mono(boolean()));
        // All three visible from inner frame.
        assert_eq!(env.lookup("a"), Some(&Scheme::mono(int())));
        assert_eq!(env.lookup("b"), Some(&Scheme::mono(string())));
        assert_eq!(env.lookup("c"), Some(&Scheme::mono(boolean())));
    }

    #[test]
    fn same_frame_reinsert_shadows_within_frame() {
        let mut env = Env::new();
        env.push();
        env.insert_top("x", Scheme::mono(int()));
        env.insert_top("x", Scheme::mono(string()));
        assert_eq!(env.lookup("x"), Some(&Scheme::mono(string())));
    }

    // ---- Env::ftv ----

    #[test]
    fn ftv_empty_env_is_empty() {
        let env = Env::new();
        assert!(env.ftv().is_empty());
    }

    #[test]
    fn ftv_excludes_quantified_vars() {
        // Scheme: ∀α. α -> β   -> ftv is {β} only.
        let mut env = Env::new();
        env.push();
        env.insert_top(
            "f",
            Scheme {
                bounds: vec![TyVarId(0)],
                kinds: vec![],
                constraints: vec![],
                assoc_projections: vec![],
                ty: Ty::Fun(Box::new(v(0)), Box::new(v(1))),
            },
        );
        let ftv = env.ftv();
        assert_eq!(ftv, HashSet::from([TyVarId(1)]));
    }

    #[test]
    fn ftv_collects_across_frames() {
        let mut env = Env::new();
        env.push();
        env.insert_top("a", Scheme::mono(v(0)));
        env.push();
        env.insert_top("b", Scheme::mono(v(1)));
        env.push();
        env.insert_top("c", Scheme::mono(v(2)));
        let ftv = env.ftv();
        assert_eq!(ftv, HashSet::from([TyVarId(0), TyVarId(1), TyVarId(2)]));
    }

    // ---- generalize ----

    #[test]
    fn generalize_no_vars_in_type_quantifies_nothing() {
        let env = Env::new();
        let s = generalize(&env, &int());
        assert!(s.bounds.is_empty());
        assert_eq!(s.ty, int());
    }

    #[test]
    fn generalize_empty_env_quantifies_all_vars() {
        let env = Env::new();
        let s = generalize(&env, &v(0));
        assert_eq!(s.bounds, vec![TyVarId(0)]);
    }

    #[test]
    fn generalize_skips_vars_already_in_env() {
        // Env already uses α, so α must NOT be quantified at the let.
        let mut env = Env::new();
        env.push();
        env.insert_top("a", Scheme::mono(v(0)));

        // Infer β -> β where β is fresh; generalize should leave β free
        // because β doesn't appear in env, but α isn't in this type at all.
        let s = generalize(&env, &Ty::Fun(Box::new(v(1)), Box::new(v(1))));
        assert_eq!(s.bounds, vec![TyVarId(1)]);
    }

    #[test]
    fn generalize_partial_overlap() {
        // Env uses α. Type is Fun(α, β). β is quantified, α is not.
        let mut env = Env::new();
        env.push();
        env.insert_top("a", Scheme::mono(v(0)));

        let s = generalize(&env, &Ty::Fun(Box::new(v(0)), Box::new(v(1))));
        assert_eq!(s.bounds, vec![TyVarId(1)]);
        // The bound variables don't include α.
        assert!(!s.bounds.contains(&TyVarId(0)));
    }

    #[test]
    fn generalize_all_in_env_quantifies_nothing() {
        let mut env = Env::new();
        env.push();
        env.insert_top("a", Scheme::mono(v(0)));
        env.insert_top("b", Scheme::mono(v(1)));

        let s = generalize(&env, &Ty::Fun(Box::new(v(0)), Box::new(v(1))));
        assert!(s.bounds.is_empty());
    }

    // ---- instantiate ----

    #[test]
    fn instantiate_mono_scheme_returns_ty_unchanged() {
        let s = Scheme::mono(int());
        let mut counter = TyVarCounter::new();
        let ty = instantiate(&s, &mut counter);
        assert_eq!(ty, int());
        // Mono scheme has no bounds, so no fresh vars are minted.
        assert_eq!(counter.count(), 0);
    }

    #[test]
    fn instantiate_poly_with_one_bound_substitutes() {
        // ∀α. α -> α  ; instantiate should give β -> β for fresh β.
        let s = Scheme {
            bounds: vec![TyVarId(0)],
            kinds: vec![],
            constraints: vec![],
            assoc_projections: vec![],
            ty: Ty::Fun(Box::new(v(0)), Box::new(v(0))),
        };
        let mut counter = TyVarCounter::new();
        let ty = instantiate(&s, &mut counter);
        let fresh = TyVarId(0);
        assert_eq!(ty, Ty::Fun(Box::new(v(fresh.0)), Box::new(v(fresh.0))));
    }

    #[test]
    fn instantiate_poly_with_multiple_bounds() {
        // ∀α β. α -> β  ; instantiate gives γ -> δ.
        let s = Scheme {
            bounds: vec![TyVarId(0), TyVarId(1)],
            kinds: vec![],
            constraints: vec![],
            assoc_projections: vec![],
            ty: Ty::Fun(Box::new(v(0)), Box::new(v(1))),
        };
        let mut counter = TyVarCounter::new();
        let ty = instantiate(&s, &mut counter);
        let gamma = TyVarId(0);
        let delta = TyVarId(1);
        assert_eq!(ty, Ty::Fun(Box::new(v(gamma.0)), Box::new(v(delta.0))));
        assert_eq!(counter.count(), 2);
    }

    #[test]
    fn instantiate_twice_produces_different_vars() {
        // Each instantiation must mint fresh vars so a scheme can be
        // used at independent types (explicit generics, not let-gen).
        let s = Scheme {
            bounds: vec![TyVarId(0)],
            kinds: vec![],
            constraints: vec![],
            assoc_projections: vec![],
            ty: Ty::Fun(Box::new(v(0)), Box::new(v(0))),
        };
        let mut counter = TyVarCounter::new();
        let ty1 = instantiate(&s, &mut counter);
        let ty2 = instantiate(&s, &mut counter);
        // After apply_prune, both should resolve to the same shape, but
        // their single-apply forms use different fresh ids.
        assert_ne!(ty1, ty2);
        // Verify they're both α -> α for some α (pruned).
        assert_eq!(apply_ty_prune(&Subst::empty(), &ty1), ty1.clone());
    }

    #[test]
    fn instantiate_substitutes_inside_nested_funs() {
        // ∀α. (α -> α) -> α -> α
        let s = Scheme {
            bounds: vec![TyVarId(0)],
            kinds: vec![],
            constraints: vec![],
            assoc_projections: vec![],
            ty: Ty::Fun(
                Box::new(Ty::Fun(Box::new(v(0)), Box::new(v(0)))),
                Box::new(Ty::Fun(Box::new(v(0)), Box::new(v(0)))),
            ),
        };
        let mut counter = TyVarCounter::new();
        let ty = instantiate(&s, &mut counter);
        let fresh = TyVarId(0);
        let expected = Ty::Fun(
            Box::new(Ty::Fun(Box::new(v(fresh.0)), Box::new(v(fresh.0)))),
            Box::new(Ty::Fun(Box::new(v(fresh.0)), Box::new(v(fresh.0)))),
        );
        assert_eq!(ty, expected);
    }

    // ---- generalize (test-only; production never calls this) ----

    #[test]
    fn generalize_id_used_at_two_types() {
        // The helper can quantify `α -> α`, but Coil does not run this
        // at `let`. Explicit `fn id<T>(T x) -> T` is the production path.

        let mut env = Env::new();
        let mut counter = TyVarCounter::new();

        // Step 1: infer `fn(x) x` -- α -> α
        let alpha = counter.fresh();
        let id_ty = Ty::Fun(Box::new(v(alpha.0)), Box::new(v(alpha.0)));

        // Step 2: generalize at the let. Empty env, so α is quantified.
        let id_scheme = generalize(&env, &id_ty);
        assert_eq!(id_scheme.bounds, vec![alpha]);

        // Step 3: add `id` to the env.
        env.push();
        env.insert_top("id", id_scheme.clone());

        // Step 4: instantiate `id` twice. The two instantiations should
        // give different fresh vars.
        let id_at_use_1 = instantiate(env.lookup("id").unwrap(), &mut counter);
        let id_at_use_2 = instantiate(env.lookup("id").unwrap(), &mut counter);
        assert_ne!(id_at_use_1, id_at_use_2);

        // Both instantiations are independent α -> α shapes. Full
        // Full unification check is in the infer integration tests.
        // but at this layer we can check the structure.
        fn is_arrow_to_same_var(ty: &Ty) -> bool {
            if let Ty::Fun(a, b) = ty
                && let (Ty::Var(va), Ty::Var(vb)) = (a.as_ref(), b.as_ref())
            {
                return va == vb;
            }
            false
        }
        assert!(is_arrow_to_same_var(&id_at_use_1));
        assert!(is_arrow_to_same_var(&id_at_use_2));
    }

    #[test]
    fn let_polymorphism_recovers_over_generalization_at_block_boundary() {
        // Scenario:
        //   let f = (fn(x) x) in
        //     let a = f 1 in    -- a : int
        //     let b = f "s" in  -- b : string (independent instantiation)
        //     ...
        //
        // After generalizing f at the outer let, the env contains
        // `f : ∀α. α -> α`. The two inner uses get separate
        // instantiations.
        let mut env = Env::new();
        let mut counter = TyVarCounter::new();

        let alpha = counter.fresh();
        let f_ty = Ty::Fun(Box::new(v(alpha.0)), Box::new(v(alpha.0)));

        env.push();
        env.insert_top("f", generalize(&env, &f_ty));

        // First use: f applied to int.
        let f_use1 = instantiate(env.lookup("f").unwrap(), &mut counter);
        let f1_alpha = match &f_use1 {
            Ty::Fun(a, _) => match a.as_ref() {
                Ty::Var(v) => *v,
                _ => panic!("expected var in arg position"),
            },
            _ => panic!("expected fun"),
        };

        // Second use: f applied to string. The fresh var here MUST be
        // different from f1_alpha, otherwise we'd have a unification
        // failure when trying to bind it to both int and string.
        let f_use2 = instantiate(env.lookup("f").unwrap(), &mut counter);
        let f2_alpha = match &f_use2 {
            Ty::Fun(a, _) => match a.as_ref() {
                Ty::Var(v) => *v,
                _ => panic!("expected var in arg position"),
            },
            _ => panic!("expected fun"),
        };
        assert_ne!(f1_alpha, f2_alpha);
    }
}
