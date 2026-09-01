//! Match and pattern inference. `infer_inner` remains the dispatcher.

use std::ops::Range;

use parser::ast::{Expression, MatchArm, Output, Pattern};
use reporting::ErrorCode;

use crate::typechecking::subst::apply_ty_prune;
use crate::typechecking::ty::Ty;

use super::*;

impl Checker {
    // ---- Match ----

    pub(super) fn infer_match(&mut self, scrutinee: &Output, arms: &[MatchArm], range: Range<usize>) -> Ty {
        let scrutinee_ty = self.infer(scrutinee);
        let resolved_scrutinee = apply_ty_prune(&self.subst, &scrutinee_ty);

        // Set up current_match_lhs for `Expression::Default`
        // (which Decision C preserves but is unreachable in real
        // source — wildcard patterns never reach it).
        let prev = self.current_match_lhs.replace(scrutinee_ty.clone());

        let mut result_ty = Ty::Var(self.counter.fresh());
        let mut first = true;
        let mut coverage: Vec<ArmCoverage> = Vec::with_capacity(arms.len());

        if arms.is_empty() {
            self.current_match_lhs = prev;
            return self.error(
                ErrorCode::GenericTypeError,
                "match has no arms".to_string(),
                range,
            );
        }

        for arm in arms {
            // Step 1: each arm gets a fresh env frame so the
            // pattern's bindings don't leak.
            self.push_scope();

            // Step 2: type the pattern, binding variables. The
            // pattern AST doesn't carry its own range today, so we
            // pass the arm's body range as a reasonable proxy for
            // error anchoring — it's close enough that ariadne
            // points near the offending pattern instead of at byte
            // 0 of the source.
            let pattern_range = arm.pattern.0.into_range();
            let pat_ty = self.infer_pattern(&arm.pattern.1, &resolved_scrutinee, &pattern_range);

            // Narrow an Identifier scrutinee to the matched variant so
            // `p.0` / `p.field` inside the arm use tagged field lookup
            // (tuple indices are shared across variants otherwise).
            let mut refined_scrut: Option<(String, Option<Ty>)> = None;
            if let Expression::Identifier(scrut_name) = scrutinee.1.as_ref()
                && let Pattern::Constructor {
                    enum_name,
                    variant_name,
                    ..
                } = &arm.pattern.1
            {
                if let Some(tag) = self
                    .enum_tags
                    .get(*enum_name)
                    .and_then(|t| t.get(*variant_name).copied())
                {
                    let arity = self
                        .enum_arities
                        .get(*enum_name)
                        .and_then(|a| a.get(tag as usize).copied())
                        .unwrap_or(0);
                    let owner = match &resolved_scrutinee {
                        Ty::Sum { .. } => resolved_scrutinee.clone(),
                        Ty::Constructor { owner, .. } => owner.as_ref().clone(),
                        Ty::Con(name) => {
                            let variant_names =
                                self.enums.get(name.as_str()).cloned().unwrap_or_default();
                            let payloads = self
                                .enum_payloads
                                .get(name.as_str())
                                .cloned()
                                .unwrap_or_default();
                            let variants: Vec<(String, EnumVariantPayloadTy)> =
                                variant_names.into_iter().zip(payloads).collect();
                            Ty::Sum {
                                name: name.clone(),
                                variants,
                            }
                        }
                        other => other.clone(),
                    };
                    let ctor = Ty::Constructor {
                        owner: Box::new(owner),
                        tag,
                        arity,
                    };
                    let prev_cg = self.codegen_var_types.get(*scrut_name).cloned();
                    self.env
                        .insert_top((*scrut_name).to_string(), Scheme::mono(ctor.clone()));
                    self.codegen_var_types
                        .insert((*scrut_name).to_string(), ctor);
                    refined_scrut = Some(((*scrut_name).to_string(), prev_cg));
                }
            }

            // Step 3: unify pattern type with scrutinee.
            self.unify(
                &resolved_scrutinee,
                &pat_ty,
                &arm.body.0.into_range(),
                "match pattern against scrutinee",
            );

            // Step 4: capture coverage info.
            let arm_cov = self.arm_coverage(&arm.pattern.1, &pattern_range);
            coverage.push(arm_cov);

            // Step 5: infer body, unify with result.
            let body_ty = self.infer(&arm.body);
            if let Some((name, prev_cg)) = refined_scrut {
                match prev_cg {
                    Some(ty) => {
                        self.codegen_var_types.insert(name, ty);
                    }
                    None => {
                        self.codegen_var_types.remove(&name);
                    }
                }
            }
            if first {
                result_ty = body_ty;
                first = false;
            } else {
                result_ty = self.join_ty(
                    &result_ty,
                    &body_ty,
                    &arm.body.0.into_range(),
                    "match arm body",
                );
            }

            // Step 6: pop the per-arm env frame.
            self.pop_scope();
        }

        self.current_match_lhs = prev;

        // Record for the post-pass exhaustiveness check. The
        // scrutinee type stored here is the resolved (pruned)
        // version at the time of the match; the post-pass will
        // re-apply the current substitution to handle any
        // variables bound by intervening code.
        self.pending_exhaustive.push(PendingExhaustive {
            scrutinee_ty: resolved_scrutinee,
            arms: coverage,
            match_range: range,
        });

        result_ty
    }

    /// Type-check a pattern against an expected type, binding
    /// variables into the current env frame. Returns the pattern's
    /// type, which is the **expected** type (the sum type, not
    /// the constructor type) — patterns desugar the scrutinee, so
    /// the pattern's type IS the scrutinee's type. The tag
    /// matching (which determines whether the arm is reachable) is
    /// captured separately in [`ArmCoverage`].
    ///
    /// `pattern_range` is the source range of the pattern itself —
    /// or, when not available, a reasonable proxy (the arm's body
    /// range). It is used to anchor pattern-related diagnostics
    /// (`unknown constructor`, `wrong arity`) so ariadne points at
    /// the offending pattern instead of byte 0 of the source.
    pub(super) fn infer_pattern(
        &mut self,
        pattern: &Pattern,
        expected_ty: &Ty,
        pattern_range: &Range<usize>,
    ) -> Ty {
        use parser::ast::PatternPayload;
        match pattern {
            Pattern::Wildcard => {
                // Wildcard matches anything, binds nothing. The
                // body's bindings (if any) come from nested
                // patterns; wildcard itself has no payload.
                expected_ty.clone()
            }
            Pattern::Binding { name } => {
                // `name => body` binds `name` to the scrutinee in
                // the arm's env. This makes the arm cover every
                // case (Rust semantics). Also record in the
                // codegen side-table so `e.kind` on a match-bound
                // enum (e.g. `Result::Err(e)`) emits LoadField,
                // not GetField.
                let pruned = apply_ty_prune(&self.subst, expected_ty);
                self.env
                    .insert_top(name.to_string(), Scheme::mono(pruned.clone()));
                self.record_codegen_var_type(name.to_string(), pruned.clone());
                pruned
            }
            Pattern::Constructor {
                enum_name,
                variant_name,
                payload,
            } => {
                // 1. Look up the variant's tag in the registry.
                let enum_str = self
                    .resolve_enum_key(enum_name)
                    .unwrap_or_else(|| enum_name.to_string());
                let variant_str = variant_name.to_string();
                let tag_opt = self
                    .enum_tags
                    .get(&enum_str)
                    .and_then(|t| t.get(&variant_str).copied());
                let tag = match tag_opt {
                    Some(t) => t,
                    None => {
                        // Unknown constructor in a pattern is an
                        // error. Record the error and return the
                        // expected type so the arm body is still
                        // processed.
                        self.messages.push(Message::error(
                            ErrorCode::UnknownConstructorPattern,
                            format!(
                                "Pattern references unknown constructor `{}::{}`",
                                enum_str, variant_str
                            ),
                            pattern_range.clone(),
                        ));
                        return expected_ty.clone();
                    }
                };
                let _arity = self
                    .enum_arities
                    .get(&enum_str)
                    .and_then(|a| a.get(tag as usize).copied())
                    .unwrap_or(0);
                let expected_payload = self
                    .poly_or_registry_payload(&enum_str, tag, expected_ty, pattern_range)
                    .unwrap_or(EnumVariantPayloadTy::Unit);

                let (shape_matches, same_shape_with_wrong_arity) =
                    match (&expected_payload, payload) {
                        (EnumVariantPayloadTy::Unit, PatternPayload::Unit) => (true, false),
                        (EnumVariantPayloadTy::Tuple(_), PatternPayload::Tuple(parts)) => {
                            let want = expected_payload.field_count();
                            (parts.len() == want, parts.len() != want)
                        }
                        (EnumVariantPayloadTy::Record(_), PatternPayload::Record(_)) => {
                            // Defer the arity check to the
                            // field-by-field pass below, which
                            // produces more specific diagnostics.
                            (true, false)
                        }
                        _ => (false, false),
                    };
                if !shape_matches {
                    if same_shape_with_wrong_arity {
                        return self.error_with_help(
                            ErrorCode::GenericTypeError,
                            format!(
                                "Constructor pattern `{}::{}` expects {} sub-patterns, got {}",
                                enum_str,
                                variant_str,
                                expected_payload.field_count(),
                                match payload {
                                    PatternPayload::Unit => 0,
                                    PatternPayload::Tuple(parts) => parts.len(),
                                    PatternPayload::Record(fields) => fields.len(),
                                },
                            ),
                            pattern_range.clone(),
                            Some("check the variant's declared payload arity".to_string()),
                        );
                    }
                    return self.error_with_help(
                        ErrorCode::PayloadShapeMismatch, format!(
                            "Constructor pattern `{}::{}` payload shape mismatch (declared as {}, pattern uses {})",
                            enum_str,
                            variant_str,
                            payload_kind_name(&expected_payload),
                            match payload {
                                PatternPayload::Unit => "unit",
                                PatternPayload::Tuple(_) => "tuple",
                                PatternPayload::Record(_) => "record",
                            },
                        ),
                        pattern_range.clone(),
                        Some("check the variant's declared payload shape".to_string()),
                    );
                }

                // 3. Recurse into each sub-pattern with the
                // corresponding payload type. The payload type
                // comes from the pre-pass's `enum_payloads`
                // (already resolved, e.g. `int` for
                // `Option::Some(int)`).
                match payload {
                    PatternPayload::Unit => {}
                    PatternPayload::Tuple(parts) => {
                        let expected_tys = expected_payload.field_types();
                        for (sub_pat, expected_ty) in parts.iter().zip(expected_tys.iter()) {
                            let _ = self.infer_pattern(&sub_pat.1, expected_ty, pattern_range);
                        }
                    }
                    PatternPayload::Record(fields) => {
                        // Build a name → pattern map for the
                        // pattern site, then walk DECLARATION
                        // order. Each declared field must be
                        // present exactly once; the codegen binds
                        // in slot order (= declaration order).
                        let mut pattern_site: std::collections::HashMap<&str, &Pattern> =
                            std::collections::HashMap::with_capacity(fields.len());
                        for pf in fields {
                            if pattern_site.insert(pf.name, &pf.pattern.1).is_some() {
                                return self.error_with_help(
                                    ErrorCode::DuplicateField,
                                    format!(
                                        "Duplicate field `{}` in record pattern `{}::{}`",
                                        pf.name, enum_str, variant_str,
                                    ),
                                    pattern_range.clone(),
                                    Some("each field must appear exactly once".to_string()),
                                );
                            }
                        }
                        let EnumVariantPayloadTy::Record(decl_fields) = &expected_payload else {
                            unreachable!()
                        };
                        for (decl_name, decl_ty) in decl_fields.iter() {
                            let sub_pat = match pattern_site.get(decl_name.as_str()) {
                                Some(p) => *p,
                                None => {
                                    return self.error_with_help(
                                        ErrorCode::MissingField,
                                        format!(
                                            "Missing field `{}` in record pattern `{}::{}`",
                                            decl_name, enum_str, variant_str,
                                        ),
                                        pattern_range.clone(),
                                        Some(format!(
                                            "add `{0}: _` (or `{0}: binding`) to the pattern",
                                            decl_name,
                                        )),
                                    );
                                }
                            };
                            let _ = self.infer_pattern(sub_pat, decl_ty, pattern_range);
                        }
                        // Check for unknown field names.
                        for pf in fields {
                            if !decl_fields.iter().any(|(dn, _)| dn == pf.name) {
                                return self.error_with_help(
                                    ErrorCode::UnknownField,
                                    format!(
                                        "Unknown field `{}` in record pattern `{}::{}`",
                                        pf.name, enum_str, variant_str,
                                    ),
                                    pattern_range.clone(),
                                    Some(format!(
                                        "the declared fields are: {}",
                                        decl_fields
                                            .iter()
                                            .map(|(n, _)| format!("`{}`", n))
                                            .collect::<Vec<_>>()
                                            .join(", "),
                                    )),
                                );
                            }
                        }
                    }
                }

                // 4. The pattern's type is the *expected* type —
                // patterns desugar the scrutinee, so the pattern
                // returns whatever the scrutinee had. (If the
                // scrutinee was a Ty::Constructor for a specific
                // tag, the pattern is still of that same type;
                // exhaustiveness checking will report the
                // arm as unreachable.)
                expected_ty.clone()
            }
        }
    }
}
