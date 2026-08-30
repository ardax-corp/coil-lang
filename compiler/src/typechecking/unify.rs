//! Unification (Robinson's algorithm with occurs check).

use super::env::substitute_vars;
use super::subst::{apply_ty, compose, Subst};
use super::ty::{ftv_ty, option_inner, peel_constructor_refinement, result_ok_err, Ty, TyVarId};

/// Failure modes for unification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnifyError {
    /// Two non-variable types that cannot be unified.
    Mismatch { left: Ty, right: Ty },
    /// `var` occurs in `ty`, so unifying would create an infinite type.
    Occurs { var: TyVarId, ty: Ty },
}

impl std::fmt::Display for UnifyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UnifyError::Mismatch { left, right } => {
                write!(f, "cannot unify {} with {}", left, right)
            }
            UnifyError::Occurs { var, ty } => {
                write!(f, "occurs check failed: t{} occurs in {}", var.raw(), ty)
            }
        }
    }
}

impl std::error::Error for UnifyError {}

/// Unify two types, starting from the empty substitution.
///
/// Convenience wrapper over [`unify_with`] for tests and one-shot
/// unifications.
#[allow(dead_code)] // exposed for tests and one-shot use
pub fn unify(t1: &Ty, t2: &Ty) -> Result<Subst, UnifyError> {
    unify_with(&Subst::empty(), t1, t2)
}

/// Unify two types under an existing substitution.
///
/// Returns the extended substitution. The input `subst` is left
/// unchanged.
pub fn unify_with(subst: &Subst, t1: &Ty, t2: &Ty) -> Result<Subst, UnifyError> {
    // Bring both sides up to date with the current substitution so we
    // always see the most recent bindings when decomposing.
    let t1 = apply_ty(subst, t1);
    let t2 = apply_ty(subst, t2);

    match (t1, t2) {
        // Identical type variables: trivially equal (no occurs check).
        (Ty::Var(v1), Ty::Var(v2)) if v1 == v2 => Ok(subst.clone()),

        // Same type constructor (e.g. `int` with `int`).
        (Ty::Con(a), Ty::Con(b)) if a == b => Ok(subst.clone()),

        // Existentials are nominal at the type level. Concrete-to-existential
        // conversion is a pack operation recorded by inference at value sites.
        (Ty::Existential { class: a }, Ty::Existential { class: b }) if a == b => Ok(subst.clone()),

        // Isorecursive encoding: Ty::Con(name) matches Sum/Constructor of same name.
        (Ty::Con(c_name), Ty::Sum { name, variants })
        | (Ty::Sum { name, variants }, Ty::Con(c_name))
            if c_name == name =>
        {
            let sum = Ty::Sum {
                name: name.clone(),
                variants: variants.clone(),
            };
            unify_with(subst, &sum, &sum)
        }
        (Ty::Con(c_name), ctor @ Ty::Constructor { .. })
        | (ctor @ Ty::Constructor { .. }, Ty::Con(c_name)) => {
            let owner_sum_name = match &ctor {
                Ty::Constructor { owner, .. } => match owner.as_ref() {
                    Ty::Sum { name, .. } => Some(name.clone()),
                    _ => None,
                },
                _ => None,
            };
            if owner_sum_name.as_deref() != Some(c_name.as_str()) {
                return Err(UnifyError::Mismatch {
                    left: ctor,
                    right: Ty::Con(c_name),
                });
            }
            let owner = match &ctor {
                Ty::Constructor { owner, .. } => owner.as_ref().clone(),
                _ => unreachable!(),
            };
            unify_with(subst, &ctor, &owner)
        }

        (Ty::Fun(a1, b1), Ty::Fun(a2, b2)) => {
            let s = unify_with(subst, a1.as_ref(), a2.as_ref())?;
            unify_with(&s, b1.as_ref(), b2.as_ref())
        }

        (
            Ty::Forall {
                bounds: b1,
                constraints: c1,
                body: body1,
            },
            Ty::Forall {
                bounds: b2,
                constraints: c2,
                body: body2,
            },
        ) => {
            if b1.len() != b2.len() {
                return Err(UnifyError::Mismatch {
                    left: Ty::Forall {
                        bounds: b1,
                        constraints: c1,
                        body: body1,
                    },
                    right: Ty::Forall {
                        bounds: b2,
                        constraints: c2,
                        body: body2,
                    },
                });
            }

            let mapping = b2.iter().copied().zip(b1.iter().copied()).collect();
            let renamed_body2 = substitute_vars(&body2, &mapping);
            let mut normalized_c1 = c1
                .iter()
                .map(|c| {
                    (
                        c.class.clone(),
                        c.args
                            .iter()
                            .map(|a| format!("{}", a))
                            .collect::<Vec<_>>()
                            .join(","),
                    )
                })
                .collect::<Vec<_>>();
            let mut normalized_c2 = c2
                .iter()
                .map(|c| {
                    let renamed_args: Vec<_> = c
                        .args
                        .iter()
                        .map(|a| substitute_vars(a, &mapping))
                        .collect();
                    (
                        c.class.clone(),
                        renamed_args
                            .iter()
                            .map(|a| format!("{}", a))
                            .collect::<Vec<_>>()
                            .join(","),
                    )
                })
                .collect::<Vec<_>>();
            normalized_c1.sort();
            normalized_c2.sort();
            if normalized_c1 != normalized_c2 {
                return Err(UnifyError::Mismatch {
                    left: Ty::Forall {
                        bounds: b1,
                        constraints: c1,
                        body: body1,
                    },
                    right: Ty::Forall {
                        bounds: b2,
                        constraints: c2,
                        body: body2,
                    },
                });
            }

            unify_with(subst, &body1, &renamed_body2)
        }

        // List types: unify the element type.
        (Ty::List(a), Ty::List(b)) => unify_with(subst, a.as_ref(), b.as_ref()),

        // Type applications: must have the same constructor and matching
        // arity; then unify args pairwise.
        (Ty::App(c1, args1), Ty::App(c2, args2)) => {
            let s = unify_with(subst, c1.as_ref(), c2.as_ref())?;
            if args1.len() != args2.len() {
                return Err(UnifyError::Mismatch {
                    left: Ty::App(c1, args1),
                    right: Ty::App(c2, args2),
                });
            }
            let mut current = s;
            for (a, b) in args1.iter().zip(args2.iter()) {
                current = unify_with(&current, a, b)?;
            }
            Ok(current)
        }

        // Poly enum annotations are `Ty::App` (Option/Result and user
        // `enum Box<T>`). Constructor owners may still be structural
        // `Ty::Sum` (builtins) — bridge those via payload extraction.
        // User generic constructs use `Ty::App` owners and unify via the
        // Constructor arm below (App ↔ owner).
        (app @ Ty::App(_, _), sum @ Ty::Sum { .. })
        | (sum @ Ty::Sum { .. }, app @ Ty::App(_, _)) => {
            match unify_builtin_app_sum(subst, &app, &sum) {
                Some(result) => result,
                None => Err(UnifyError::Mismatch {
                    left: app,
                    right: sum,
                }),
            }
        }

        // Constructor values unify with an App parent by unifying the
        // App against the constructor's owner (Sum for builtins, App
        // for user generic enums).
        (app @ Ty::App(_, _), ctor @ Ty::Constructor { .. })
        | (ctor @ Ty::Constructor { .. }, app @ Ty::App(_, _)) => {
            let owner = match &ctor {
                Ty::Constructor { owner, .. } => owner.as_ref().clone(),
                _ => unreachable!(),
            };
            match unify_with(subst, &app, &owner) {
                Ok(s) => Ok(s),
                Err(_) => Err(UnifyError::Mismatch {
                    left: app,
                    right: ctor,
                }),
            }
        }

        // Sum types: same name, matching variant count/names/shapes.
        (
            Ty::Sum {
                name: a,
                variants: av,
            },
            Ty::Sum {
                name: b,
                variants: bv,
            },
        ) if a == b => {
            if av.len() != bv.len() {
                return Err(UnifyError::Mismatch {
                    left: Ty::Sum {
                        name: a,
                        variants: av,
                    },
                    right: Ty::Sum {
                        name: b,
                        variants: bv,
                    },
                });
            }
            let mut current = subst.clone();
            for ((an, ap), (bn, bp)) in av.iter().zip(bv.iter()) {
                if an != bn {
                    return Err(UnifyError::Mismatch {
                        left: Ty::Sum {
                            name: a.clone(),
                            variants: av.clone(),
                        },
                        right: Ty::Sum {
                            name: b.clone(),
                            variants: bv.clone(),
                        },
                    });
                }
                if ap.field_count() != bp.field_count() {
                    return Err(UnifyError::Mismatch {
                        left: Ty::Sum {
                            name: a.clone(),
                            variants: av.clone(),
                        },
                        right: Ty::Sum {
                            name: b.clone(),
                            variants: bv.clone(),
                        },
                    });
                }
                if std::mem::discriminant(ap) != std::mem::discriminant(bp) {
                    return Err(UnifyError::Mismatch {
                        left: Ty::Sum {
                            name: a.clone(),
                            variants: av.clone(),
                        },
                        right: Ty::Sum {
                            name: b.clone(),
                            variants: bv.clone(),
                        },
                    });
                }
                let ap_tys = ap.field_types();
                let bp_tys = bp.field_types();
                for (x, y) in ap_tys.iter().zip(bp_tys.iter()) {
                    current = unify_with(&current, x, y)?;
                }
            }
            Ok(current)
        }

        // Constructor vs parent sum (tag/arity must agree).
        (ctor @ Ty::Constructor { .. }, sum @ Ty::Sum { .. })
        | (sum @ Ty::Sum { .. }, ctor @ Ty::Constructor { .. }) => {
            // Re-borrow the constructor parts without consuming the
            // pattern match (we still need `sum` and `ctor` for the
            // final unify).
            let (c_owner, c_tag, c_arity) = match &ctor {
                Ty::Constructor { owner, tag, arity } => (owner.as_ref(), *tag, *arity),
                _ => unreachable!(),
            };
            let (s_name, s_variants) = match &sum {
                Ty::Sum { name, variants } => (name.clone(), variants.clone()),
                _ => unreachable!(),
            };
            let variant = match s_variants.get(c_tag as usize) {
                Some(v) => v,
                None => {
                    return Err(UnifyError::Mismatch {
                        left: ctor,
                        right: sum,
                    });
                }
            };
            if variant.1.field_count() != c_arity {
                return Err(UnifyError::Mismatch {
                    left: ctor,
                    right: sum,
                });
            }
            // The constructor's owner and the sum should be the
            // same type (modulo substitution). Unify to verify.
            let _ = s_name;
            unify_with(subst, c_owner, &sum)
        }

        // Two constructors: join at the owning enum. Same tag keeps the
        // refinement path via owner unify; different tags (e.g. `Rank::Mid`
        // vs `Rank::Low`) still unify so generics / arrays / assignment see
        // one enum type rather than incompatible `::vN` refinements.
        (
            Ty::Constructor {
                owner: o1,
                tag: t1,
                arity: a1,
            },
            Ty::Constructor {
                owner: o2,
                tag: t2,
                arity: a2,
            },
        ) => {
            let _ = (t1, a1, t2, a2);
            unify_with(subst, o1.as_ref(), o2.as_ref())
        }

        // Never unifies with anything (bottom): join absorbs it without binding.
        (Ty::Never, Ty::Never) => Ok(subst.clone()),
        (Ty::Never, _) | (_, Ty::Never) => Ok(subst.clone()),

        // Type variable on either side: bind, with occurs check.
        (Ty::Var(v), t) => bind_var(subst, v, t),
        (t, Ty::Var(v)) => bind_var(subst, v, t),

        // Tuple, array, record
        (Ty::Tuple(tys1), Ty::Tuple(tys2)) => {
            if tys1.len() != tys2.len() {
                return Err(UnifyError::Mismatch {
                    left: Ty::Tuple(tys1),
                    right: Ty::Tuple(tys2),
                });
            }
            let mut current = subst.clone();
            for (a, b) in tys1.iter().zip(tys2.iter()) {
                current = unify_with(&current, a, b)?;
            }
            Ok(current)
        }
        (
            Ty::Array {
                element: e1,
                length: l1,
            },
            Ty::Array {
                element: e2,
                length: l2,
            },
        ) => {
            let len_compatible = match (l1, l2) {
                (super::ty::ArrayLength::Dynamic, _) | (_, super::ty::ArrayLength::Dynamic) => true,
                (super::ty::ArrayLength::Static(n), super::ty::ArrayLength::Static(m)) => n == m,
            };
            if !len_compatible {
                return Err(UnifyError::Mismatch {
                    left: Ty::Array {
                        element: e1,
                        length: l1,
                    },
                    right: Ty::Array {
                        element: e2,
                        length: l2,
                    },
                });
            }
            unify_with(subst, e1.as_ref(), e2.as_ref())
        }
        (Ty::Record { fields: f1 }, Ty::Record { fields: f2 }) => {
            if f1.len() != f2.len() {
                return Err(UnifyError::Mismatch {
                    left: Ty::Record { fields: f1 },
                    right: Ty::Record { fields: f2 },
                });
            }
            // Sort by field name for canonical comparison
            let mut s1 = f1.clone();
            let mut s2 = f2.clone();
            s1.sort_by(|a, b| a.0.cmp(&b.0));
            s2.sort_by(|a, b| a.0.cmp(&b.0));
            let names_eq = s1.iter().map(|(n, _)| n).collect::<Vec<_>>()
                == s2.iter().map(|(n, _)| n).collect::<Vec<_>>();
            if !names_eq {
                return Err(UnifyError::Mismatch {
                    left: Ty::Record { fields: f1 },
                    right: Ty::Record { fields: f2 },
                });
            }
            let mut current = subst.clone();
            for ((_, t1), (_, t2)) in s1.iter().zip(s2.iter()) {
                current = unify_with(&current, t1, t2)?;
            }
            Ok(current)
        }

        (Ty::Readonly(a), Ty::Readonly(b)) => unify_with(subst, a.as_ref(), b.as_ref()),
        (Ty::Readonly(a), b) => unify_with(subst, a.as_ref(), &b),
        (a, Ty::Readonly(b)) => unify_with(subst, &a, b.as_ref()),

        // Anything else: the constructors are incompatible.
        (left, right) => Err(UnifyError::Mismatch { left, right }),
    }
}

fn unify_builtin_app_sum(subst: &Subst, app: &Ty, sum: &Ty) -> Option<Result<Subst, UnifyError>> {
    let Ty::App(con, args) = app else {
        return None;
    };

    // Concrete head: `Option<T>` / `Result<T, E>` annotations vs structural sums.
    if let Ty::Con(name) = con.as_ref() {
        if common::is_builtin_option_enum(name) {
            if args.len() != 1 {
                return Some(Err(UnifyError::Mismatch {
                    left: app.clone(),
                    right: sum.clone(),
                }));
            }
            let Some(inner) = option_inner(sum) else {
                return None;
            };
            return Some(unify_with(subst, &args[0], &inner));
        }

        if common::is_builtin_result_enum(name) {
            if args.len() != 2 {
                return Some(Err(UnifyError::Mismatch {
                    left: app.clone(),
                    right: sum.clone(),
                }));
            }
            let Some((ok, err)) = result_ok_err(sum) else {
                return None;
            };
            let s = match unify_with(subst, &args[0], &ok) {
                Ok(s) => s,
                Err(e) => return Some(Err(e)),
            };
            return Some(unify_with(&s, &args[1], &err));
        }

        return None;
    }

    // Variable head (Phase 5 HKT): unify `F<A>` with builtin Option/Result by
    // binding `F` to the constructor constant, then unifying payload args.
    // Without this, `get(Option::Some(42))` cannot discharge `Container<F>`.
    if let Ty::Var(var) = con.as_ref() {
        if let Some(inner) = option_inner(sum) {
            if args.len() != 1 {
                return Some(Err(UnifyError::Mismatch {
                    left: app.clone(),
                    right: sum.clone(),
                }));
            }
            let s = match bind_var(subst, *var, Ty::Con(common::BUILTIN_OPTION_ENUM.into())) {
                Ok(s) => s,
                Err(e) => return Some(Err(e)),
            };
            return Some(unify_with(&s, &args[0], &inner));
        }

        if let Some((ok, err)) = result_ok_err(sum) {
            if args.len() != 2 {
                return Some(Err(UnifyError::Mismatch {
                    left: app.clone(),
                    right: sum.clone(),
                }));
            }
            let s = match bind_var(subst, *var, Ty::Con(common::BUILTIN_RESULT_ENUM.into())) {
                Ok(s) => s,
                Err(e) => return Some(Err(e)),
            };
            let s = match unify_with(&s, &args[0], &ok) {
                Ok(s) => s,
                Err(e) => return Some(Err(e)),
            };
            return Some(unify_with(&s, &args[1], &err));
        }

        return None;
    }

    None
}

/// Bind `var` to `ty` after the occurs check.
fn bind_var(subst: &Subst, var: TyVarId, ty: Ty) -> Result<Subst, UnifyError> {
    // Do not pin a polymorphic param to a single variant tag — otherwise
    // `min(Rank::Mid, Rank::Low)` binds `T` to `::v1` and rejects `::v0`.
    let ty = peel_constructor_refinement(ty);
    if ftv_ty(&ty).contains(&var) {
        return Err(UnifyError::Occurs { var, ty });
    }
    let new_binding = Subst::singleton(var, ty);
    Ok(compose(subst, &new_binding))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::typechecking::subst::apply_ty_prune;
    use crate::typechecking::ty::{
        array, array_fixed, boolean, float, int, list, string, EnumVariantPayloadTy,
    };

    fn v(i: u32) -> Ty {
        Ty::Var(TyVarId(i))
    }

    fn fun(a: Ty, b: Ty) -> Ty {
        Ty::Fun(Box::new(a), Box::new(b))
    }

    // ---- Basic success cases ----

    #[test]
    fn unify_same_constructor_succeeds() {
        assert_eq!(unify(&int(), &int()).unwrap(), Subst::empty());
        assert_eq!(unify(&float(), &float()).unwrap(), Subst::empty());
        assert_eq!(unify(&string(), &string()).unwrap(), Subst::empty());
        assert_eq!(unify(&boolean(), &boolean()).unwrap(), Subst::empty());
    }

    #[test]
    fn unify_never_absorbs_concrete_without_binding() {
        // Bottom unifies with any type and must not invent a subst binding.
        assert_eq!(unify(&Ty::Never, &int()).unwrap(), Subst::empty());
        assert_eq!(unify(&string(), &Ty::Never).unwrap(), Subst::empty());
        assert_eq!(unify(&Ty::Never, &Ty::Never).unwrap(), Subst::empty());
    }

    #[test]
    fn unify_never_with_var_does_not_bind_var() {
        // Never arms precede Var bind — α stays free so joins stay absorbing.
        let s = unify(&Ty::Never, &v(0)).unwrap();
        assert_eq!(s, Subst::empty());
        assert_eq!(apply_ty(&s, &v(0)), v(0));
    }

    #[test]
    fn unify_var_with_constructor_binds() {
        let s = unify(&v(0), &int()).unwrap();
        assert_eq!(apply_ty(&s, &v(0)), int());
    }

    #[test]
    fn unify_constructor_with_var_binds() {
        let s = unify(&int(), &v(0)).unwrap();
        assert_eq!(apply_ty(&s, &v(0)), int());
    }

    #[test]
    fn unify_var_with_itself_succeeds() {
        // Same variable on both sides — trivially equal, no occurs check.
        assert_eq!(unify(&v(0), &v(0)).unwrap(), Subst::empty());
        assert_eq!(unify(&v(42), &v(42)).unwrap(), Subst::empty());
    }

    #[test]
    fn unify_two_different_vars_binds_left_to_right() {
        // v(0) is on the left, so we bind v(0) → v(1). v(1) is unchanged.
        let s = unify(&v(0), &v(1)).unwrap();
        assert_eq!(apply_ty(&s, &v(0)), v(1));
        assert_eq!(apply_ty(&s, &v(1)), v(1));
    }

    // ---- Failure: mismatch ----

    #[test]
    fn unify_different_constructors_is_mismatch() {
        let err = unify(&int(), &float()).unwrap_err();
        assert!(matches!(err, UnifyError::Mismatch { .. }));
    }

    #[test]
    fn unify_fun_with_int_is_mismatch() {
        let err = unify(&fun(int(), string()), &int()).unwrap_err();
        assert!(matches!(err, UnifyError::Mismatch { .. }));
    }

    #[test]
    fn unify_list_with_non_list_is_mismatch() {
        let err = unify(&list(int()), &int()).unwrap_err();
        assert!(matches!(err, UnifyError::Mismatch { .. }));
    }

    #[test]
    fn unify_app_arity_mismatch_is_mismatch() {
        let foo = Ty::con("Foo");
        let err = unify(
            &Ty::App(Box::new(foo.clone()), vec![v(0)]),
            &Ty::App(Box::new(foo.clone()), vec![int(), boolean()]),
        )
        .unwrap_err();
        assert!(matches!(err, UnifyError::Mismatch { .. }));
    }

    #[test]
    fn unify_app_constructor_mismatch_is_mismatch() {
        let foo = Ty::con("Foo");
        let bar = Ty::con("Bar");
        let err = unify(
            &Ty::App(Box::new(foo), vec![int()]),
            &Ty::App(Box::new(bar), vec![int()]),
        )
        .unwrap_err();
        assert!(matches!(err, UnifyError::Mismatch { .. }));
    }

    // ---- Failure: occurs check ----

    #[test]
    fn occurs_check_rejects_alpha_equals_alpha_to_alpha() {
        // α = α -> α
        let err = unify(&v(0), &fun(v(0), v(0))).unwrap_err();
        assert!(matches!(err, UnifyError::Occurs { .. }));
    }

    #[test]
    fn occurs_check_rejects_var_inside_fun_arg() {
        // α = (α -> int) -> int
        let err = unify(&v(0), &fun(fun(v(0), int()), int())).unwrap_err();
        assert!(matches!(err, UnifyError::Occurs { .. }));
    }

    #[test]
    fn occurs_check_rejects_var_inside_list() {
        // α = List<α>
        let err = unify(&v(0), &list(v(0))).unwrap_err();
        assert!(matches!(err, UnifyError::Occurs { .. }));
    }

    #[test]
    fn occurs_check_rejects_var_inside_app_arg() {
        let foo = Ty::con("Foo");
        let err = unify(&v(0), &Ty::App(Box::new(foo), vec![v(0)])).unwrap_err();
        assert!(matches!(err, UnifyError::Occurs { .. }));
    }

    #[test]
    fn occurs_check_does_not_fire_on_independent_vars() {
        // α = β -> γ : should succeed (α is fresh).
        let s = unify(&v(0), &fun(v(1), v(2))).unwrap();
        assert_eq!(apply_ty(&s, &v(0)), fun(v(1), v(2)));
    }

    // ---- Decomposition ----

    #[test]
    fn unify_fun_decomposes_into_args_and_return() {
        // (α -> β) ~ (int -> bool) binds α = int, β = bool.
        let s = unify(&fun(v(0), v(1)), &fun(int(), boolean())).unwrap();
        assert_eq!(apply_ty(&s, &v(0)), int());
        assert_eq!(apply_ty(&s, &v(1)), boolean());
    }

    #[test]
    fn unify_list_decomposes() {
        let s = unify(&list(v(0)), &list(int())).unwrap();
        assert_eq!(apply_ty(&s, &v(0)), int());
    }

    #[test]
    fn unify_app_decomposes() {
        // Foo<α> ~ Foo<int>
        let foo = Ty::con("Foo");
        let s = unify(
            &Ty::App(Box::new(foo.clone()), vec![v(0)]),
            &Ty::App(Box::new(foo), vec![int()]),
        )
        .unwrap();
        assert_eq!(apply_ty(&s, &v(0)), int());
    }

    #[test]
    fn unify_nested_fun_decomposes_recursively() {
        // ((α -> β) -> γ) ~ ((int -> bool) -> string)
        let lhs = fun(fun(v(0), v(1)), v(2));
        let rhs = fun(fun(int(), boolean()), string());
        let s = unify(&lhs, &rhs).unwrap();
        assert_eq!(apply_ty(&s, &v(0)), int());
        assert_eq!(apply_ty(&s, &v(1)), boolean());
        assert_eq!(apply_ty(&s, &v(2)), string());
    }

    // ---- With existing substitution ----

    #[test]
    fn unify_with_existing_subst_extends_it() {
        // Start with γ = string, unify v(0) with v(2). v(0) should
        // resolve to string.
        let s0 = Subst::singleton(TyVarId(2), string());
        let s = unify_with(&s0, &v(0), &v(2)).unwrap();
        assert_eq!(apply_ty(&s, &v(0)), string());
        assert_eq!(apply_ty(&s, &v(2)), string());
    }

    #[test]
    fn unify_resolves_both_sides_under_existing_subst() {
        // Starting with γ = string, unify Fun(γ, α) with Fun(string, bool).
        // α should resolve to bool; γ already in s0.
        let s0 = Subst::singleton(TyVarId(2), string());
        let s = unify_with(&s0, &fun(v(2), v(0)), &fun(string(), boolean())).unwrap();
        assert_eq!(apply_ty(&s, &v(0)), boolean());
        assert_eq!(apply_ty(&s, &v(2)), string());
    }

    // ---- Algorithm-W-like chaining ----

    #[test]
    fn chained_unifications_propagate_through_subst() {
        // γ ~ α ; α ~ β ; β ~ int
        let s1 = unify(&v(2), &v(0)).unwrap();
        let s2 = unify_with(&s1, &v(0), &v(1)).unwrap();
        let s3 = unify_with(&s2, &v(1), &int()).unwrap();

        // Single-apply sees the next variable in the chain.
        assert_eq!(apply_ty(&s3, &v(0)), v(1));
        assert_eq!(apply_ty(&s3, &v(1)), int());
        assert_eq!(apply_ty(&s3, &v(2)), v(0));

        // Pruned apply sees the fully resolved type.
        assert_eq!(apply_ty_prune(&s3, &v(0)), int());
        assert_eq!(apply_ty_prune(&s3, &v(1)), int());
        assert_eq!(apply_ty_prune(&s3, &v(2)), int());
    }

    #[test]
    fn unify_propagates_through_aliased_vars() {
        // v(0) ~ v(1); then v(0) ~ int.
        let s1 = unify(&v(0), &v(1)).unwrap();
        let s2 = unify_with(&s1, &v(0), &int()).unwrap();

        // Single-apply leaves the alias intact: v(0) is bound to v(1),
        // which is itself bound to int. To get the final type the caller
        // has to reapplied (apply_ty_prune).
        assert_eq!(apply_ty(&s2, &v(0)), v(1));
        assert_eq!(apply_ty(&s2, &v(1)), int());

        // Pruned apply follows the alias chain.
        assert_eq!(apply_ty_prune(&s2, &v(0)), int());
        assert_eq!(apply_ty_prune(&s2, &v(1)), int());
    }

    // ---- Idempotence invariant ----

    fn is_idempotent(s: &Subst) -> bool {
        for (var, ty) in s.iter() {
            if ftv_ty(ty).contains(&var) {
                return false;
            }
        }
        true
    }

    #[test]
    fn result_is_idempotent_simple() {
        let s = unify(&fun(v(0), v(1)), &fun(int(), boolean())).unwrap();
        assert!(
            is_idempotent(&s),
            "substitution should be idempotent: {s:?}"
        );
    }

    #[test]
    fn result_is_idempotent_nested_fun() {
        let s = unify(&v(0), &fun(v(1), v(2))).unwrap();
        assert!(
            is_idempotent(&s),
            "substitution should be idempotent: {s:?}"
        );
    }

    #[test]
    fn result_is_idempotent_chained() {
        let s1 = unify(&v(2), &v(0)).unwrap();
        let s2 = unify_with(&s1, &v(0), &v(1)).unwrap();
        let s3 = unify_with(&s2, &v(1), &int()).unwrap();
        assert!(
            is_idempotent(&s3),
            "substitution should be idempotent: {s3:?}"
        );
    }

    // ---- Sum / Constructor unification ----

    fn sum(name: &str, variants: Vec<(&str, EnumVariantPayloadTy)>) -> Ty {
        Ty::Sum {
            name: name.to_string(),
            variants: variants
                .into_iter()
                .map(|(n, p)| (n.to_string(), p))
                .collect(),
        }
    }

    fn ctor(owner: Ty, tag: u32, arity: usize) -> Ty {
        Ty::Constructor {
            owner: Box::new(owner),
            tag,
            arity,
        }
    }

    #[test]
    fn unify_same_name_empty_sums_succeeds() {
        // enum E { A, B }  ~  enum E { A, B }  → identity.
        let s1 = sum(
            "E",
            vec![
                ("A", EnumVariantPayloadTy::Unit),
                ("B", EnumVariantPayloadTy::Unit),
            ],
        );
        let s2 = sum(
            "E",
            vec![
                ("A", EnumVariantPayloadTy::Unit),
                ("B", EnumVariantPayloadTy::Unit),
            ],
        );
        assert!(unify(&s1, &s2).is_ok());
    }

    #[test]
    fn unify_same_name_sums_with_payloads_succeeds() {
        // enum O { None, Some(int) } ~ enum O { None, Some(int) }
        let s1 = sum(
            "O",
            vec![
                ("None", EnumVariantPayloadTy::Unit),
                ("Some", EnumVariantPayloadTy::Tuple(vec![int()])),
            ],
        );
        let s2 = sum(
            "O",
            vec![
                ("None", EnumVariantPayloadTy::Unit),
                ("Some", EnumVariantPayloadTy::Tuple(vec![int()])),
            ],
        );
        assert!(unify(&s1, &s2).is_ok());
    }

    #[test]
    fn unify_sums_with_polymorphic_payload_binds_vars() {
        // enum E { A } with payload α  ~  enum E { A } with payload int
        // → α = int.
        let s1 = sum("E", vec![("A", EnumVariantPayloadTy::Tuple(vec![v(0)]))]);
        let s2 = sum("E", vec![("A", EnumVariantPayloadTy::Tuple(vec![int()]))]);
        let s = unify(&s1, &s2).unwrap();
        assert_eq!(apply_ty(&s, &v(0)), int());
    }

    #[test]
    fn unify_different_name_sums_is_mismatch() {
        let s1 = sum("E", vec![("A", EnumVariantPayloadTy::Unit)]);
        let s2 = sum("F", vec![("A", EnumVariantPayloadTy::Unit)]);
        assert!(matches!(
            unify(&s1, &s2).unwrap_err(),
            UnifyError::Mismatch { .. }
        ));
    }

    #[test]
    fn unify_sums_with_different_variant_count_is_mismatch() {
        let s1 = sum(
            "E",
            vec![
                ("A", EnumVariantPayloadTy::Unit),
                ("B", EnumVariantPayloadTy::Unit),
            ],
        );
        let s2 = sum("E", vec![("A", EnumVariantPayloadTy::Unit)]);
        assert!(matches!(
            unify(&s1, &s2).unwrap_err(),
            UnifyError::Mismatch { .. }
        ));
    }

    #[test]
    fn unify_sums_with_different_variant_name_is_mismatch() {
        let s1 = sum(
            "E",
            vec![
                ("A", EnumVariantPayloadTy::Unit),
                ("B", EnumVariantPayloadTy::Unit),
            ],
        );
        let s2 = sum(
            "E",
            vec![
                ("A", EnumVariantPayloadTy::Unit),
                ("C", EnumVariantPayloadTy::Unit),
            ],
        );
        assert!(matches!(
            unify(&s1, &s2).unwrap_err(),
            UnifyError::Mismatch { .. }
        ));
    }

    #[test]
    fn unify_sums_with_different_payload_arity_is_mismatch() {
        let s1 = sum("E", vec![("A", EnumVariantPayloadTy::Tuple(vec![int()]))]);
        let s2 = sum(
            "E",
            vec![("A", EnumVariantPayloadTy::Tuple(vec![int(), int()]))],
        );
        assert!(matches!(
            unify(&s1, &s2).unwrap_err(),
            UnifyError::Mismatch { .. }
        ));
    }

    #[test]
    fn unify_sums_with_matching_record_shapes_succeeds() {
        // Two same-named enums where the matching variant has the
        // same record shape on both sides — they unify.
        let s1 = sum(
            "E",
            vec![(
                "A",
                EnumVariantPayloadTy::Record(vec![
                    ("x".to_string(), int()),
                    ("y".to_string(), string()),
                ]),
            )],
        );
        let s2 = sum(
            "E",
            vec![(
                "A",
                EnumVariantPayloadTy::Record(vec![
                    ("x".to_string(), int()),
                    ("y".to_string(), string()),
                ]),
            )],
        );
        assert!(unify(&s1, &s2).is_ok());
    }

    #[test]
    fn unify_sums_with_mismatched_shapes_is_mismatch() {
        // Same variant name, different shapes: `A(int)` (tuple)
        // vs `A { x: int }` (record). These MUST NOT unify — the
        // shape discriminates them, even though the field count
        // matches.
        let s1 = sum("E", vec![("A", EnumVariantPayloadTy::Tuple(vec![int()]))]);
        let s2 = sum(
            "E",
            vec![(
                "A",
                EnumVariantPayloadTy::Record(vec![("x".to_string(), int())]),
            )],
        );
        assert!(matches!(
            unify(&s1, &s2).unwrap_err(),
            UnifyError::Mismatch { .. }
        ));
    }

    #[test]
    fn unify_constructor_with_its_parent_sum_succeeds() {
        // Constructor { tag=1, arity=1 } ~ Sum { Some(int) }
        let s = sum(
            "O",
            vec![
                ("None", EnumVariantPayloadTy::Unit),
                ("Some", EnumVariantPayloadTy::Tuple(vec![int()])),
            ],
        );
        let c = ctor(s.clone(), 1, 1);
        assert!(unify(&c, &s).is_ok());
    }

    #[test]
    fn unify_constructor_with_other_sum_is_mismatch() {
        // Constructor { tag=0, arity=0 } ~ Sum { Some(int) }
        // — wrong arity.
        let s = sum(
            "O",
            vec![
                ("None", EnumVariantPayloadTy::Unit),
                ("Some", EnumVariantPayloadTy::Tuple(vec![int()])),
            ],
        );
        let c = ctor(s.clone(), 0, 0);
        assert!(unify(&c, &s).is_ok()); // None has arity 0, so this works
        let c_bad = ctor(s.clone(), 1, 0);
        assert!(matches!(
            unify(&c_bad, &s).unwrap_err(),
            UnifyError::Mismatch { .. }
        ));
    }

    #[test]
    fn unify_constructors_different_tags_join_at_owner() {
        let s = sum(
            "Rank",
            vec![
                ("Low", EnumVariantPayloadTy::Unit),
                ("Mid", EnumVariantPayloadTy::Unit),
                ("High", EnumVariantPayloadTy::Unit),
            ],
        );
        let mid = ctor(s.clone(), 1, 0);
        let low = ctor(s.clone(), 0, 0);
        assert!(unify(&mid, &low).is_ok());
    }

    #[test]
    fn bind_var_peels_constructor_refinement() {
        let s = sum(
            "Rank",
            vec![
                ("Low", EnumVariantPayloadTy::Unit),
                ("Mid", EnumVariantPayloadTy::Unit),
            ],
        );
        let mid = ctor(s.clone(), 1, 0);
        let subst = unify(&v(0), &mid).unwrap();
        let bound = apply_ty_prune(&subst, &v(0));
        assert_eq!(bound, Ty::Con("Rank".into()));
    }

    #[test]
    fn unify_constructor_with_out_of_range_tag_is_mismatch() {
        // Constructor { tag=5 } ~ Sum with 2 variants — tag out of range.
        let s = sum(
            "O",
            vec![
                ("None", EnumVariantPayloadTy::Unit),
                ("Some", EnumVariantPayloadTy::Tuple(vec![int()])),
            ],
        );
        let c = ctor(s.clone(), 5, 0);
        assert!(matches!(
            unify(&c, &s).unwrap_err(),
            UnifyError::Mismatch { .. }
        ));
    }

    #[test]
    fn unify_recursive_sum_payload_uses_con_not_unfolded() {
        // enum Tree { Leaf, Node(int, Tree, Tree) }
        // The recursive reference is `Ty::Con("Tree")`. The sum
        // itself is `Ty::Sum { name: "Tree", variants: [..] }`.
        // Unifying the sum with itself should not occur-check fail
        // because the payload uses the opaque name reference.
        let tree = Ty::Con("Tree".into());
        let s = sum(
            "Tree",
            vec![
                ("Leaf", EnumVariantPayloadTy::Unit),
                (
                    "Node",
                    EnumVariantPayloadTy::Tuple(vec![int(), tree.clone(), tree]),
                ),
            ],
        );
        assert!(unify(&s, &s).is_ok());
    }

    // ---- Phase 5: HKT App(Var) ↔ builtin Option/Result ----

    #[test]
    fn unify_app_var_with_option_sum_binds_constructor_head() {
        use crate::typechecking::ty::option_ty;
        let app = Ty::App(Box::new(v(0)), vec![v(1)]);
        let opt = option_ty(int());
        let s = unify(&app, &opt).unwrap();
        assert_eq!(
            apply_ty_prune(&s, &v(0)),
            Ty::Con(common::BUILTIN_OPTION_ENUM.into())
        );
        assert_eq!(apply_ty_prune(&s, &v(1)), int());
    }

    #[test]
    fn unify_app_var_with_option_constructor_binds_head_and_payload() {
        use crate::typechecking::ty::option_ty;
        let app = Ty::App(Box::new(v(0)), vec![v(1)]);
        let owner = option_ty(int());
        let some = ctor(owner, 1, 1);
        let s = unify(&app, &some).unwrap();
        assert_eq!(
            apply_ty_prune(&s, &v(0)),
            Ty::Con(common::BUILTIN_OPTION_ENUM.into())
        );
        assert_eq!(apply_ty_prune(&s, &v(1)), int());
    }

    #[test]
    fn unify_app_var_with_binary_concrete_app_binds_by_arity() {
        let app = Ty::App(Box::new(v(0)), vec![v(1), v(2)]);
        let concrete = Ty::App(
            Box::new(Ty::Con(common::BUILTIN_RESULT_ENUM.into())),
            vec![int(), string()],
        );
        let s = unify(&app, &concrete).unwrap();
        assert_eq!(
            apply_ty_prune(&s, &v(0)),
            Ty::Con(common::BUILTIN_RESULT_ENUM.into())
        );
        assert_eq!(apply_ty_prune(&s, &v(1)), int());
        assert_eq!(apply_ty_prune(&s, &v(2)), string());
    }

    // ---- Array length unification (E1) ----

    #[test]
    fn unify_static_array_lengths_equal_succeeds() {
        assert!(unify(&array_fixed(int(), 2), &array_fixed(int(), 2)).is_ok());
    }

    #[test]
    fn unify_static_array_lengths_unequal_is_mismatch() {
        let err = unify(&array_fixed(int(), 2), &array_fixed(int(), 8)).unwrap_err();
        assert!(matches!(err, UnifyError::Mismatch { .. }));
    }

    #[test]
    fn unify_static_array_with_dynamic_succeeds() {
        // Dynamic is the unsized join: Static(n) ↔ Dynamic still unifies.
        assert!(unify(&array_fixed(int(), 2), &array(int())).is_ok());
        assert!(unify(&array(int()), &array_fixed(int(), 2)).is_ok());
    }
}
