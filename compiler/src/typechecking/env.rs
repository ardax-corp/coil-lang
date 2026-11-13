//! Scoped environments and scheme instantiation ([`instantiate_with_kinds`]).
//!
//! Production does not let-generalize (not Algorithm W). Bindings are
//! monomorphic unless syntax declares type parameters. `generalize` is
//! test-only.

use std::collections::{HashMap, HashSet};

#[cfg(test)]
use super::ty::ftv_ty;
use super::ty::{Scheme, Ty, TyVarId, ftv_scheme};

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

/// A single scope's bindings, in insertion order, with an index of each
/// name's latest (shadowing) binding: a module frame holds every top-level
/// name, and a linear lookup there made checking quadratic in the number
/// of functions (#1857).
#[derive(Debug, Default, Clone)]
pub struct Frame {
    /// Name, scheme, and whether the binding is a `const`.
    bindings: Vec<(String, Scheme, bool)>,
    /// Name to the index of its latest binding in `bindings`. A name no scope
    /// binds probes every frame, one per module: a fast hash is worth having.
    latest: rustc_hash::FxHashMap<String, usize>,
    /// Per binding, the index of the binding of its name it shadows (the
    /// name's latest when it was bound): what [`Self::drop_since`] falls
    /// back to without a walk of the frame.
    shadowed: Vec<Option<usize>>,
}

impl Frame {
    pub fn new() -> Self {
        Self::default()
    }

    /// Bind `name` to `scheme` in this frame. Later bindings with the same
    /// name shadow earlier ones within the same frame.
    fn insert(&mut self, name: impl Into<String>, scheme: Scheme) {
        let name = name.into();
        let shadowed = self.latest.insert(name.clone(), self.bindings.len());
        self.bindings.push((name, scheme, false));
        self.shadowed.push(shadowed);
    }

    /// Mark the latest binding of `name` in this frame as a `const`.
    pub fn mark_const(&mut self, name: &str) {
        if let Some(&i) = self.latest.get(name) {
            self.bindings[i].2 = true;
        }
    }

    /// Number of bindings in this frame.
    pub fn len(&self) -> usize {
        self.bindings.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bindings.is_empty()
    }

    /// Drop bindings of `name` inserted at or after index `mark` (see
    /// [`Self::len`]), exposing whatever `name` was bound to before.
    ///
    /// Touches only the bindings from `mark` on: a module frame holds every
    /// top-level name, and a walk of it per dropped name made checking
    /// quadratic in the number of impls (#2352).
    pub fn drop_since(&mut self, name: &str, mark: usize) {
        let Some(&latest) = self.latest.get(name).filter(|&&i| i >= mark) else {
            return;
        };
        // `name` falls back to its binding before `mark`.
        let mut before = Some(latest);
        while let Some(i) = before.filter(|&i| i >= mark) {
            before = self.shadowed[i];
        }
        // The bindings after `mark` but `name`'s move down: index those
        // again, and what they shadow inside that tail.
        let tail = self.bindings.split_off(mark);
        let tail_shadowed = self.shadowed.split_off(mark);
        let mut moved: Vec<Option<usize>> = vec![None; tail.len()];
        for (k, (binding, shadowed)) in tail.into_iter().zip(tail_shadowed).enumerate() {
            if binding.0 == name {
                continue;
            }
            let shadowed = match shadowed {
                Some(i) if i >= mark => moved[i - mark],
                other => other,
            };
            let at = self.bindings.len();
            moved[k] = Some(at);
            self.latest.insert(binding.0.clone(), at);
            self.bindings.push(binding);
            self.shadowed.push(shadowed);
        }
        match before {
            Some(i) => self.latest.insert(name.to_string(), i),
            None => self.latest.remove(name),
        };
    }

    /// Look up `name` in this frame only (no parent walk).
    pub fn lookup(&self, name: &str) -> Option<&Scheme> {
        self.latest.get(name).map(|&i| &self.bindings[i].1)
    }

    /// Whether the latest binding of `name` in this frame is a `const`.
    fn binds_const(&self, name: &str) -> Option<bool> {
        self.latest.get(name).map(|&i| self.bindings[i].2)
    }

    /// Iterate bindings in insertion order for tooling such as completions.
    pub fn bindings(&self) -> impl Iterator<Item = (&str, &Scheme)> {
        self.bindings
            .iter()
            .map(|(name, scheme, _)| (name.as_str(), scheme))
    }
}

/// A scoped environment: a stack of frames. The most recently pushed
/// frame is innermost; lookup walks innermost to outermost so that inner
/// bindings shadow outer ones.
#[derive(Debug, Default, Clone)]
pub struct Env {
    frames: Vec<Frame>,
    /// Every name ever bound here. Most lookups in lowering are of names no
    /// scope binds (`int`, an enum's name): one probe answers those, where a
    /// walk probes a frame per module.
    ever_bound: rustc_hash::FxHashSet<String>,
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
    /// Used by lambdas so only explicit `use` captures + params are visible
    /// (imports and module-visible named `fn`s are rebound after this).
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
            for (n, ..) in &frame.bindings {
                names.insert(n.clone());
            }
        }
        names
    }

    /// Return visible names with inner frames shadowing outer frames.
    pub fn visible_names(&self) -> Vec<String> {
        let mut names = Vec::new();
        for frame in self.frames.iter().rev() {
            for (name, ..) in frame.bindings.iter().rev() {
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
        let name = name.into();
        if !self.ever_bound.contains(&name) {
            self.ever_bound.insert(name.clone());
        }
        frame.insert(name, scheme);
    }

    /// Look up a name, walking from the innermost frame to the outermost.
    /// Returns the scheme of the most-recently-bound matching name.
    pub fn lookup(&self, name: &str) -> Option<&Scheme> {
        if !self.ever_bound.contains(name) {
            return None;
        }
        for frame in self.frames.iter().rev() {
            if let Some(s) = frame.lookup(name) {
                return Some(s);
            }
        }
        None
    }

    /// Mark the latest binding of `name` in the innermost frame as a `const`.
    pub fn mark_top_const(&mut self, name: &str) {
        if let Some(frame) = self.frames.last_mut() {
            frame.mark_const(name);
        }
    }

    /// Whether `name` resolves to a `const` binding: a `let`, parameter or
    /// loop binding that shadows a const is not one.
    pub fn is_const(&self, name: &str) -> bool {
        self.frames.iter().rev().find_map(|frame| frame.binds_const(name)).unwrap_or(false)
    }

    /// Index of the innermost frame that binds `name` (0 = outermost).
    pub fn lookup_frame(&self, name: &str) -> Option<usize> {
        self.frames
            .iter()
            .rposition(|frame| frame.lookup(name).is_some())
    }

    /// Free type variables of every scheme in the environment,
    /// excluding each scheme's quantified variables.
    pub fn ftv(&self) -> HashSet<TyVarId> {
        let mut acc = HashSet::new();
        for frame in &self.frames {
            for (_, scheme, _) in &frame.bindings {
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
    (ty, constraints, fresh_kinds)
}

/// Walk `ty`, replacing every free variable in `mapping` with its mapped
/// value. Variables not in the mapping are left alone.
pub(crate) fn substitute_vars(ty: &Ty, mapping: &HashMap<TyVarId, TyVarId>) -> Ty {
    ty.subst_vars(&|v| mapping.get(&v).map(|&new| Ty::Var(new)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::typechecking::subst::{Subst, apply_ty_prune};
    use crate::typechecking::ty::{Ty, boolean, int, string};

    fn v(i: u32) -> Ty {
        Ty::Var(TyVarId(i))
    }

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

    #[test]
    fn new_env_is_empty() {
        let env = Env::new();
        assert_eq!(env.depth(), 0);
        assert!(env.lookup("anything").is_none());
    }

    /// `drop_since` drops only the name's bindings from the mark on, falls
    /// back to its binding before the mark (or none), and keeps the other
    /// names' bindings after the mark, shadowing included.
    #[test]
    fn drop_since_restores_the_binding_before_the_mark() {
        let mut f = Frame::new();
        let ty = |i: u32| Scheme::mono(v(i));
        f.insert("x", ty(1));
        f.insert("y", ty(2));
        f.insert("x", ty(3));
        let mark = f.len();
        f.insert("x", ty(4));
        f.insert("y", ty(5));
        f.insert("x", ty(6));
        f.insert("y", ty(7));
        f.insert("z", ty(8));
        f.drop_since("x", mark);
        assert_eq!(f.lookup("x").map(|s| &s.ty), Some(&v(3)));
        assert_eq!(f.lookup("y").map(|s| &s.ty), Some(&v(7)));
        assert_eq!(f.len(), mark + 3);
        // `y`'s bindings after the mark still shadow in order.
        f.drop_since("y", mark + 1);
        assert_eq!(f.lookup("y").map(|s| &s.ty), Some(&v(5)));
        f.drop_since("y", mark);
        assert_eq!(f.lookup("y").map(|s| &s.ty), Some(&v(2)));
        assert_eq!(f.lookup("z").map(|s| &s.ty), Some(&v(8)));
        // A name first bound after the mark is gone.
        f.drop_since("z", mark);
        assert!(f.lookup("z").is_none());
        assert_eq!(f.len(), mark);
        // A name with no binding from the mark on is left alone.
        f.drop_since("x", mark);
        assert_eq!(f.lookup("x").map(|s| &s.ty), Some(&v(3)));
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
