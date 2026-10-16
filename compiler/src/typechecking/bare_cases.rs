//! Bare unit cases in patterns (`None`, `Red`, #1352) for passes that run
//! without the checker (#1394).
//!
//! The checker reads a bare name with no payload as a case when it starts
//! with an uppercase letter and some enum in scope declares it; anything
//! else binds. Without the checker, "in scope" is approximated by every
//! `enum` declared in the AST at hand plus the prelude `Option` / `Result`
//! cases. Enums imported from another module are not visible here, so a
//! bare case of one still reads as a binding.

use std::collections::HashSet;

use parser::ast::{Expression, Output, Pattern, PatternField, PatternOutput, PatternPayload};

/// Prelude cases usable bare without any `enum` in the AST.
const PRELUDE_CASES: &[&str] = &["Some", "None", "Ok", "Err"];

/// Case names a bare pattern can refer to (see the module docs).
#[derive(Debug, Default, Clone)]
pub struct BareCases {
    names: HashSet<String>,
}

impl BareCases {
    /// Every case declared by an `enum` in `ast`, plus the prelude cases.
    pub fn of(ast: &Output<'_>) -> Self {
        let mut names: HashSet<String> = PRELUDE_CASES.iter().map(|n| (*n).to_string()).collect();
        collect_cases(ast, &mut names);
        Self { names }
    }

    /// True when the bare pattern name `name` at `span` is a case, not a
    /// binding. Synthetic patterns (empty span) always bind, as in the
    /// checker.
    pub fn is_case(&self, name: &str, span: parser::SimpleSpan) -> bool {
        span.start != span.end
            && name.starts_with(|c: char| c.is_ascii_uppercase())
            && self.names.contains(name)
    }

    /// Call `f` with each name `pattern` binds and the span of the
    /// sub-pattern that binds it.
    pub fn each_binder<'s>(
        &self,
        pattern: &PatternOutput<'s>,
        f: &mut dyn FnMut(&'s str, parser::SimpleSpan),
    ) {
        self.binders_at(pattern, true, f);
    }

    fn binders_at<'s>(
        &self,
        pattern: &PatternOutput<'s>,
        bare_unit: bool,
        f: &mut dyn FnMut(&'s str, parser::SimpleSpan),
    ) {
        let (span, pat) = pattern;
        match pat {
            Pattern::Binding { name } => {
                if !(bare_unit && self.is_case(name, *span)) {
                    f(name, *span);
                }
            }
            Pattern::Constructor { payload, .. } => match payload {
                PatternPayload::Unit => {}
                PatternPayload::Tuple(items) => {
                    for item in items {
                        self.binders_at(item, true, f);
                    }
                }
                PatternPayload::Record(fields) => {
                    for field in fields {
                        self.binders_at(&field.pattern, !is_shorthand(field), f);
                    }
                }
            },
            Pattern::Wildcard | Pattern::Default | Pattern::Integer(_) => {}
        }
    }
}

/// Record shorthand `{ w }` always binds `w`, even when `w` names a case.
fn is_shorthand(field: &PatternField<'_>) -> bool {
    matches!(field.pattern.1, Pattern::Binding { name } if name == field.name)
}

fn collect_cases(ast: &Output<'_>, out: &mut HashSet<String>) {
    if let Expression::EnumDecl { variants, .. } = ast.1.as_ref() {
        for variant in variants {
            if let Expression::EnumVariant { name, .. } = variant.1.as_ref() {
                out.insert((*name).to_string());
            }
        }
        return;
    }
    ast.1.for_each_child(&mut |child| collect_cases(child, out));
}
