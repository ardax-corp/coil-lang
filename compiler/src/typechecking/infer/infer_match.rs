//! Match and pattern inference. `infer_inner` remains the dispatcher.

use std::ops::Range;

use parser::ast::{Expression, MatchArm, Output, Pattern, PatternOutput};
use reporting::ErrorCode;

use crate::typechecking::subst::apply_ty_prune;
use crate::typechecking::ty::{Ty, int};

use super::*;

/// How a bare name in a pattern resolved (#1352).
enum BarePattern {
    /// A case of this enum (registry key).
    Ctor(String),
    /// Not a case: the name binds the scrutinee.
    Binding,
    /// Ambiguous or unknown; the error is already reported.
    Failed,
}

impl Checker {
    pub(super) fn infer_match(
        &mut self,
        scrutinee: &Output,
        arms: &[&MatchArm],
        range: Range<usize>,
    ) -> Ty {
        // The match's expected type is its arms'; the scrutinee is an operand.
        let arm_expected = self.current_expected.take();
        let scrutinee_ty = self.infer(scrutinee);
        self.current_expected = arm_expected;
        let resolved_scrutinee = apply_ty_prune(&self.subst, &scrutinee_ty);

        // Set up current_match_lhs for `Expression::Default`
        // (which Decision C preserves but is unreachable in real
        // source, wildcard patterns never reach it).
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

        let mut pattern_failed = false;
        for arm in arms {
            self.push_scope();

            // Pattern AST has no range; body range anchors diagnostics.
            let pattern_range = arm.pattern.0.into_range();
            if matches!(arm.pattern.1, Pattern::Wildcard) {
                let mut msg = Message::error(
                    ErrorCode::UnderscoreMatchArm,
                    "`_` is not a match catch-all; use `default`".to_string(),
                    pattern_range.clone(),
                );
                msg.with_help(
                    "nested `_` in constructors, tuples, and records is still allowed".to_string(),
                );
                self.messages.push(msg);
            }
            let msgs_before = self.messages.len();
            let pat_ty = self.infer_pattern(&arm.pattern, &resolved_scrutinee, &pattern_range);

            // Narrow an Identifier scrutinee to the matched variant so
            // `p.0` / `p.field` inside the arm use tagged field lookup
            // (tuple indices are shared across variants otherwise).
            let mut refined_scrut: Option<(String, Option<Ty>)> = None;
            let arm_pattern = self.resolved_pattern(&arm.pattern);
            if let Expression::Identifier(scrut_name) = scrutinee.1.as_ref()
                && let Pattern::Constructor {
                    enum_name,
                    variant_name,
                    ..
                } = arm_pattern.as_ref()
                && let Some(tag) = self
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

            self.unify(
                &resolved_scrutinee,
                &pat_ty,
                &arm.body.0.into_range(),
                "match pattern against scrutinee",
            );

            // A pattern that failed to check has no coherent coverage
            // shape; skip the exhaustiveness pass for this match.
            pattern_failed |= self.messages[msgs_before..].iter().any(|m| matches!(m.kind(), reporting::MessageKind::ERROR));
            let arm_cov = self.arm_coverage(&arm.pattern, &pattern_range);
            coverage.push(arm_cov);

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

            self.pop_scope();
        }

        self.current_match_lhs = prev;

        // Post-pass exhaustiveness re-applies the current subst to scrutinee_ty.
        if !pattern_failed {
            self.pending_exhaustive.push(PendingExhaustive {
                scrutinee_ty: resolved_scrutinee,
                arms: coverage,
                match_range: range,
            });
        }

        result_ty
    }

    /// Type-check a pattern against an expected type, binding
    /// variables into the current env frame. Returns the pattern's
    /// type, which is the **expected** type (the sum type, not
    /// the constructor type), patterns desugar the scrutinee, so
    /// the pattern's type IS the scrutinee's type. The tag
    /// matching (which determines whether the arm is reachable) is
    /// captured separately in [`ArmCoverage`].
    ///
    /// `pattern_range` is the source range of the pattern itself
    /// or, when not available, a reasonable proxy (the arm's body
    /// range). It is used to anchor pattern-related diagnostics
    /// (`unknown constructor`, `wrong arity`) so ariadne points at
    /// the offending pattern instead of byte 0 of the source.
    pub(super) fn infer_pattern(
        &mut self,
        pattern: &PatternOutput,
        expected_ty: &Ty,
        pattern_range: &Range<usize>,
    ) -> Ty {
        self.infer_pattern_at(pattern, expected_ty, pattern_range, true)
    }

    /// [`Self::infer_pattern`]; `bare_unit` is false for a record field's
    /// shorthand (`Rect { w }`), which always binds.
    fn infer_pattern_at(
        &mut self,
        pattern: &PatternOutput,
        expected_ty: &Ty,
        pattern_range: &Range<usize>,
        bare_unit: bool,
    ) -> Ty {
        use parser::ast::PatternPayload;
        let span = pattern.0;
        // A bare unit case (`None`, `Red`, #1352) is checked as the
        // qualified constructor pattern it names; any other name binds.
        let unit_key;
        let unit_pattern;
        let pattern = match &pattern.1 {
            Pattern::Binding { name } if bare_unit => {
                match self.resolve_bare_pattern(name, span, expected_ty, true, pattern_range) {
                    BarePattern::Ctor(key) => {
                        unit_key = key;
                        unit_pattern = Pattern::Constructor {
                            enum_name: &unit_key,
                            variant_name: name,
                            payload: PatternPayload::Unit,
                        };
                        &unit_pattern
                    }
                    BarePattern::Failed => return expected_ty.clone(),
                    BarePattern::Binding => &pattern.1,
                }
            }
            other => other,
        };
        match pattern {
            Pattern::Wildcard => expected_ty.clone(),
            Pattern::Default => expected_ty.clone(),
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
            Pattern::Integer(_) => {
                self.unify(expected_ty, &int(), pattern_range, "match integer pattern");
                int()
            }
            Pattern::Constructor {
                enum_name,
                variant_name,
                payload,
            } => {
                // Bare `Some(v)` / `Rect { w, h }` (#1352) name no enum.
                let bare_key;
                let enum_name: &str = if enum_name.is_empty() {
                    match self.resolve_bare_pattern(variant_name, span, expected_ty, false, pattern_range) {
                        BarePattern::Ctor(key) => {
                            bare_key = key;
                            &bare_key
                        }
                        _ => {
                            self.bind_unchecked_payload(payload);
                            return expected_ty.clone();
                        }
                    }
                } else {
                    enum_name
                };
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
                        self.bind_unchecked_payload(payload);
                        return expected_ty.clone();
                    }
                };
                // A constructor from another enum can never match. Generic
                // enums get this from the unify in `poly_or_registry_payload`;
                // plain enums carry no type args, so compare the names.
                if !self.is_poly_enum(&enum_str)
                    && let Some(found) = self.scrutinee_enum_key(expected_ty)
                    && found != enum_str
                {
                    self.messages.push(Message::error(
                        ErrorCode::TypeMismatch,
                        format!(
                            "Type mismatch: expected {}, found {} (pattern `{}::{}`)",
                            found, enum_str, enum_str, variant_str
                        ),
                        pattern_range.clone(),
                    ));
                    self.bind_unchecked_payload(payload);
                    return expected_ty.clone();
                }
                let _arity = self
                    .enum_arities
                    .get(&enum_str)
                    .and_then(|a| a.get(tag as usize).copied())
                    .unwrap_or(0);
                // A payload matched out of a `readonly` enum value is
                // read through it (#1552).
                let pruned = apply_ty_prune(&self.subst, expected_ty);
                let (expected_ty, readonly) = match &pruned {
                    Ty::Readonly(inner) => (inner.as_ref(), true),
                    other => (other, false),
                };
                let mut expected_payload = self
                    .poly_or_registry_payload(&enum_str, tag, expected_ty, pattern_range)
                    .unwrap_or(EnumVariantPayloadTy::Unit);
                if readonly {
                    expected_payload = match expected_payload {
                        EnumVariantPayloadTy::Unit => EnumVariantPayloadTy::Unit,
                        EnumVariantPayloadTy::Tuple(tys) => {
                            EnumVariantPayloadTy::Tuple(tys.into_iter().map(|t| self.readonly_view(t)).collect())
                        }
                        EnumVariantPayloadTy::Record(fields) => EnumVariantPayloadTy::Record(
                            fields.into_iter().map(|(n, t)| (n, self.readonly_view(t))).collect(),
                        ),
                    };
                }

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
                    self.bind_unchecked_payload(payload);
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
                            let _ = self.infer_pattern(sub_pat, expected_ty, pattern_range);
                        }
                    }
                    PatternPayload::Record(fields) => {
                        // Build a name → pattern map for the
                        // pattern site, then walk DECLARATION
                        // order. Each declared field must be
                        // present exactly once; the codegen binds
                        // in slot order (= declaration order).
                        let mut pattern_site: std::collections::HashMap<&str, &PatternOutput> =
                            std::collections::HashMap::with_capacity(fields.len());
                        for pf in fields {
                            if pattern_site.insert(pf.name, &pf.pattern).is_some() {
                                self.bind_unchecked_payload(payload);
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
                        for (i, (decl_name, decl_ty)) in decl_fields.iter().enumerate() {
                            let sub_pat = match pattern_site.get(decl_name.as_str()) {
                                Some(p) => *p,
                                None => {
                                    // Fields not checked yet still bind.
                                    for pf in fields {
                                        if !decl_fields[..i].iter().any(|(dn, _)| dn == pf.name) {
                                            self.bind_unchecked_field(pf);
                                        }
                                    }
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
                            // Shorthand `{ w }` always binds `w`.
                            let shorthand = matches!(sub_pat.1, Pattern::Binding { name } if name == decl_name.as_str());
                            let _ = self.infer_pattern_at(sub_pat, decl_ty, pattern_range, !shorthand);
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

                // 4. The pattern's type is the *expected* type
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

    /// Declare every name a constructor pattern's `payload` binds with a
    /// fresh type, for a pattern that failed to check (#1394): its arm
    /// body still sees them, so their uses add no E0100 to the real error.
    fn bind_unchecked_payload(&mut self, payload: &parser::ast::PatternPayload) {
        use parser::ast::PatternPayload;
        match payload {
            PatternPayload::Unit => {}
            PatternPayload::Tuple(parts) => {
                for part in parts {
                    self.bind_unchecked(part, true);
                }
            }
            PatternPayload::Record(fields) => {
                for field in fields {
                    self.bind_unchecked_field(field);
                }
            }
        }
    }

    /// [`Self::bind_unchecked_payload`] for one record field; shorthand
    /// `{ w }` always binds.
    fn bind_unchecked_field(&mut self, field: &parser::ast::PatternField) {
        let shorthand = matches!(field.pattern.1, Pattern::Binding { name } if name == field.name);
        self.bind_unchecked(&field.pattern, !shorthand);
    }

    /// [`Self::bind_unchecked_payload`] for one sub-pattern. A bare unit
    /// case (`None`) binds nothing, by the rule of [`Self::resolve_bare_pattern`].
    fn bind_unchecked(&mut self, pattern: &PatternOutput, bare_unit: bool) {
        let (span, pat) = pattern;
        match pat {
            Pattern::Binding { name } => {
                let is_case = bare_unit
                    && span.start != span.end
                    && name.starts_with(|c: char| c.is_ascii_uppercase())
                    && !self.enums_declaring_variant(name).is_empty();
                if is_case {
                    self.record_bare_case(*span);
                } else {
                    let ty = Ty::Var(self.counter.fresh());
                    self.env.insert_top(name.to_string(), Scheme::mono(ty));
                }
            }
            Pattern::Constructor { payload, .. } => self.bind_unchecked_payload(payload),
            Pattern::Wildcard | Pattern::Default | Pattern::Integer(_) => {}
        }
    }

    /// Resolve a bare case name in a pattern (#1352). The scrutinee's own
    /// enum wins when it declares the case; otherwise the name must be a
    /// case of exactly one enum in scope, as for a bare constructor
    /// expression. A bare name with no payload (`unit`) is a case only when
    /// it starts with an uppercase letter and some enum declares it;
    /// anything else stays a binding.
    fn resolve_bare_pattern(
        &mut self,
        variant: &str,
        span: parser::SimpleSpan,
        expected_ty: &Ty,
        unit: bool,
        pattern_range: &Range<usize>,
    ) -> BarePattern {
        // Synthetic patterns carry an empty span; they never use the sugar.
        if unit && (span.start == span.end || !variant.starts_with(|c: char| c.is_ascii_uppercase())) {
            return BarePattern::Binding;
        }
        let from_scrutinee = self.scrutinee_enum_key(expected_ty).filter(|key| {
            self.enum_tags
                .get(key)
                .is_some_and(|tags| tags.contains_key(variant))
        });
        let enum_key = match from_scrutinee {
            Some(key) => key,
            None => match self.resolve_bare_constructor(variant) {
                BareCtor::Unique(enum_name) => self.resolve_enum_key(&enum_name).unwrap_or(enum_name),
                BareCtor::Ambiguous(enums) => {
                    if unit {
                        self.record_bare_case(span);
                    }
                    self.ambiguous_bare_constructor(variant, &enums, pattern_range.clone());
                    return BarePattern::Failed;
                }
                BareCtor::None if unit => return BarePattern::Binding,
                BareCtor::None => {
                    self.messages.push(Message::error(
                        ErrorCode::UnknownConstructorPattern,
                        format!("Pattern references unknown constructor `{}`", variant),
                        pattern_range.clone(),
                    ));
                    return BarePattern::Failed;
                }
            },
        };
        if unit {
            self.record_bare_case(span);
        }
        self.bare_patterns_by_span
            .insert((span.start, span.end), enum_key.clone());
        BarePattern::Ctor(enum_key)
    }

    /// The registry key of the enum a scrutinee type names, if it names one.
    fn scrutinee_enum_key(&self, ty: &Ty) -> Option<String> {
        let name = match apply_ty_prune(&self.subst, ty) {
            Ty::Con(n) | Ty::Sum { name: n, .. } => n,
            Ty::App(head, _) => match *head {
                Ty::Con(n) => n,
                _ => return None,
            },
            Ty::Constructor { owner, .. } => match *owner {
                Ty::Sum { name, .. } => name,
                _ => return None,
            },
            _ => return None,
        };
        self.enum_tags.contains_key(&name).then_some(name)
    }
}
