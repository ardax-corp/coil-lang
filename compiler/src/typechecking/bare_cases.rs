//! Bare unit cases in patterns (`None`, `Red`, #1352) for passes that walk
//! the AST after the checker (#1394).
//!
//! The checker decides whether a bare pattern name with no payload is a
//! case or a binding (it is a case when it starts with an uppercase letter
//! and the scrutinee's enum or some enum in scope, local or imported,
//! declares it) and records each case by span; see
//! [`Checker::bare_cases`](crate::typechecking::infer::Checker::bare_cases).
//! This is that record: later passes read it instead of deciding again.

use std::collections::HashSet;
use std::sync::LazyLock;

use parser::ast::{Pattern, PatternOutput, PatternPayload};

/// Spans of the bare pattern names the checker resolved to unit cases.
#[derive(Debug, Default, Clone)]
pub struct BareCases {
    spans: HashSet<(usize, usize)>,
}

impl BareCases {
    /// No cases: every bare pattern name binds (a module the checker has
    /// not seen).
    pub fn none() -> &'static Self {
        static NONE: LazyLock<BareCases> = LazyLock::new(BareCases::default);
        &NONE
    }

    pub(crate) fn insert(&mut self, span: parser::SimpleSpan) {
        self.spans.insert((span.start, span.end));
    }

    /// True when the bare pattern name at `span` is a case, not a binding.
    pub fn is_case(&self, span: parser::SimpleSpan) -> bool {
        self.spans.contains(&(span.start, span.end))
    }

    /// Call `f` with each name `pattern` binds and the span of the
    /// sub-pattern that binds it.
    pub fn each_binder<'s>(
        &self,
        pattern: &PatternOutput<'s>,
        f: &mut dyn FnMut(&'s str, parser::SimpleSpan),
    ) {
        let (span, pat) = pattern;
        match pat {
            Pattern::Binding { name } => {
                if !self.is_case(*span) {
                    f(name, *span);
                }
            }
            Pattern::Constructor { payload, .. } => match payload {
                PatternPayload::Unit => {}
                PatternPayload::Tuple(items) => {
                    for item in items {
                        self.each_binder(item, f);
                    }
                }
                PatternPayload::Record(fields) => {
                    for field in fields {
                        self.each_binder(&field.pattern, f);
                    }
                }
            },
            Pattern::Wildcard | Pattern::Default | Pattern::Integer(_) => {}
        }
    }

    /// The cases the checker records for `ast` checked as a whole program.
    #[cfg(test)]
    pub(crate) fn checked(ast: &parser::ast::Output<'_>) -> Self {
        let mut checker = crate::typechecking::infer::Checker::new();
        let _ = checker.check_program(ast);
        checker.bare_cases().clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use parser::ast::{Expression, Output};

    fn binders(src: &'static str) -> Vec<&'static str> {
        let ast = parser::Pratt::default().parse(src).expect("parse");
        let cases = BareCases::checked(&ast);
        let mut out = Vec::new();
        let mut arms = Vec::new();
        collect_arms(&ast, &mut arms);
        for pattern in arms {
            cases.each_binder(&pattern, &mut |name, _| out.push(name));
        }
        out
    }

    fn collect_arms<'s>(ast: &Output<'s>, out: &mut Vec<PatternOutput<'s>>) {
        if let Expression::Match { arms, .. } = ast.1.as_ref() {
            out.extend(arms.iter().map(|arm| arm.pattern.clone()));
        }
        ast.1.for_each_child(&mut |child| collect_arms(child, out));
    }

    #[test]
    fn declared_and_prelude_cases_do_not_bind() {
        let src = "enum Light {\n    Red,\n    Green,\n}\nfn f(Option<Light> o) -> int {\n    return match o {\n        Some(Red) => 1,\n        Some(other) => 2,\n        None => 3,\n        Missing => 4,\n    };\n}\n";
        assert_eq!(binders(src), ["other", "Missing"]);
    }

    #[test]
    fn record_shorthand_and_nested_payloads() {
        let src = "enum Light {\n    Red,\n}\nenum Box {\n    Item { Red: int, inner: Option<int> },\n}\nfn f(Box b) -> int {\n    return match b {\n        Item { Red, inner: Some(v) } => 1,\n        Box::Item { Red: Red, inner: None } => 2,\n    };\n}\n";
        // `Red: Red` spells the shorthand out, so it binds too (as in the checker).
        assert_eq!(binders(src), ["Red", "v", "Red"]);
    }

    #[test]
    fn synthetic_patterns_always_bind() {
        let cases = BareCases::checked(&parser::Pratt::default().parse("fn main() {}\n").expect("parse"));
        let pattern = parser::ast::spanned_pattern(Pattern::Binding { name: "None" });
        let mut out = Vec::new();
        cases.each_binder(&pattern, &mut |name, _| out.push(name));
        assert_eq!(out, ["None"]);
    }
}
