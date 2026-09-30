//! Scope-aware resolution of function-local names (tooling only).
//!
//! [`SymbolIndex`](crate::SymbolIndex) is name-keyed: fine for top-level
//! decls, wrong for locals (every `p` in the project is "the same" `p`).
//! This walks function bodies with real lexical scopes and binds each
//! identifier use to its `let` / parameter / loop / pattern / capture
//! binding. Lambdas and `defer` isolate the env: only their `use (…)`
//! captures see outer locals.

use std::ops::Range;

use parser::ast::{
    Expression, LetPattern, Output, Pattern, PatternPayload,
};

/// One local binding: its declaration site and every use that resolves to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalBinding {
    pub name: String,
    pub decl: Range<usize>,
    pub uses: Vec<Range<usize>>,
}

impl LocalBinding {
    /// Declaration first, then uses in source order.
    pub fn occurrences(&self) -> impl Iterator<Item = &Range<usize>> {
        std::iter::once(&self.decl).chain(self.uses.iter())
    }

    pub fn contains(&self, offset: usize) -> bool {
        self.occurrences().any(|r| r.start <= offset && offset <= r.end)
    }
}

/// Every local binding in `source` (empty when it does not parse).
pub fn local_bindings(source: &str) -> Vec<LocalBinding> {
    let Ok(ast) = parser::Pratt::default().parse(source) else {
        return Vec::new();
    };
    local_bindings_in(source, &ast)
}

/// Same as [`local_bindings`] for an already-parsed `ast` of `source`.
pub fn local_bindings_in(source: &str, ast: &Output<'_>) -> Vec<LocalBinding> {
    let mut resolver = Resolver {
        source,
        bindings: Vec::new(),
        scopes: Vec::new(),
    };
    resolver.walk(ast);
    for binding in &mut resolver.bindings {
        binding.uses.sort_by_key(|r| r.start);
        binding.uses.dedup();
    }
    resolver.bindings
}

/// The binding whose declaration or use covers `offset`.
pub fn binding_at(source: &str, offset: usize) -> Option<LocalBinding> {
    local_bindings(source)
        .into_iter()
        .find(|binding| binding.contains(offset))
}

enum Frame {
    /// Ordinary lexical scope.
    Scope(Vec<(String, usize)>),
    /// Lambda / defer boundary: lookups stop here (captures are re-bound
    /// in the scope just above it).
    Barrier,
}

struct Resolver<'s> {
    source: &'s str,
    bindings: Vec<LocalBinding>,
    scopes: Vec<Frame>,
}

impl<'s> Resolver<'s> {
    fn range_of(&self, name: &str, fallback: Range<usize>) -> Range<usize> {
        let base = self.source.as_ptr() as usize;
        let ptr = name.as_ptr() as usize;
        if ptr >= base && ptr + name.len() <= base + self.source.len() {
            let start = ptr - base;
            start..start + name.len()
        } else {
            fallback
        }
    }

    fn push(&mut self) {
        self.scopes.push(Frame::Scope(Vec::new()));
    }

    fn pop(&mut self) {
        self.scopes.pop();
    }

    fn lookup(&self, name: &str) -> Option<usize> {
        for frame in self.scopes.iter().rev() {
            match frame {
                Frame::Barrier => return None,
                Frame::Scope(names) => {
                    if let Some((_, idx)) = names.iter().rev().find(|(n, _)| n == name) {
                        return Some(*idx);
                    }
                }
            }
        }
        None
    }

    /// Declare `name` in the innermost scope. Outside any function the name
    /// is a global and not tracked here.
    fn bind(&mut self, name: &str, fallback: Range<usize>) {
        if name == "_" || name.is_empty() {
            return;
        }
        let decl = self.range_of(name, fallback);
        let idx = self.bindings.len();
        let Some(Frame::Scope(names)) = self.scopes.last_mut() else {
            return;
        };
        names.push((name.to_owned(), idx));
        self.bindings.push(LocalBinding {
            name: name.to_owned(),
            decl,
            uses: Vec::new(),
        });
    }

    fn use_name(&mut self, name: &str, fallback: Range<usize>) {
        if let Some(idx) = self.lookup(name) {
            let range = self.range_of(name, fallback);
            self.bindings[idx].uses.push(range);
        }
    }

    fn bind_args(&mut self, args: &Output<'_>) {
        let mut stack = vec![args];
        while let Some(node) = stack.pop() {
            match node.1.as_ref() {
                Expression::Argument { name, .. } => {
                    self.bind(name, node.0.start..node.0.end);
                }
                other => other.for_each_child(&mut |child| stack.push(child)),
            }
        }
    }

    fn bind_let_pattern(&mut self, pattern: &LetPattern<'_>, fallback: Range<usize>) {
        match pattern {
            LetPattern::Wildcard => {}
            LetPattern::Binding { name } => self.bind(name, fallback),
            LetPattern::Tuple(items) => {
                for item in items {
                    self.bind_let_pattern(item, fallback.clone());
                }
            }
            LetPattern::Record(fields) => {
                for field in fields {
                    self.bind_let_pattern(&field.pattern, fallback.clone());
                }
            }
        }
    }

    fn bind_pattern(&mut self, pattern: &(parser::SimpleSpan, Pattern<'_>)) {
        let span = pattern.0.start..pattern.0.end;
        match &pattern.1 {
            Pattern::Binding { name } => self.bind(name, span),
            Pattern::Constructor { payload, .. } => match payload {
                PatternPayload::Unit => {}
                PatternPayload::Tuple(items) => {
                    for item in items {
                        self.bind_pattern(item);
                    }
                }
                PatternPayload::Record(fields) => {
                    for field in fields {
                        self.bind_pattern(&field.pattern);
                    }
                }
            },
            Pattern::Wildcard | Pattern::Default | Pattern::Integer(_) => {}
        }
    }

    /// Capture list of a lambda / defer: each name is a use of the outer
    /// binding, re-bound (same binding) past the barrier.
    fn enter_isolated(&mut self, captures: &[&str], span: Range<usize>) {
        let mut rebinds = Vec::new();
        for capture in captures {
            if let Some(idx) = self.lookup(capture) {
                let range = self.range_of(capture, span.clone());
                self.bindings[idx].uses.push(range);
                rebinds.push(((*capture).to_owned(), idx));
            }
        }
        self.scopes.push(Frame::Barrier);
        self.scopes.push(Frame::Scope(rebinds));
    }

    fn leave_isolated(&mut self) {
        self.scopes.pop();
        self.scopes.pop();
    }

    fn walk(&mut self, node: &Output<'_>) {
        let span = node.0.start..node.0.end;
        match node.1.as_ref() {
            Expression::Identifier(name) => self.use_name(name, span),
            Expression::Function { args, returns, body, .. } => {
                self.push();
                self.bind_args(args);
                if let Some(returns) = returns {
                    self.walk(returns);
                }
                if let Some(body) = body {
                    self.walk(body);
                }
                self.pop();
            }
            Expression::Lambda { args, captures, body } => {
                self.enter_isolated(captures, span);
                self.bind_args(args);
                self.walk(body);
                self.leave_isolated();
            }
            Expression::Defer { captures, body } => {
                self.enter_isolated(captures, span);
                self.walk(body);
                self.leave_isolated();
            }
            Expression::Block(items) => {
                self.push();
                for item in items {
                    self.walk(item);
                }
                self.pop();
            }
            Expression::Fragment(items)
                if matches!(items.first().map(|i| i.1.as_ref()), Some(Expression::Variable(..))) =>
            {
                // `let x = x + 1`: the initializer sees the outer `x`.
                for item in &items[1..] {
                    self.walk(item);
                }
                if let Expression::Variable(name, annotation) = items[0].1.as_ref() {
                    if let Some(annotation) = annotation {
                        self.walk(annotation);
                    }
                    self.bind(name, items[0].0.start..items[0].0.end);
                }
            }
            Expression::LetDestructure { pattern, rhs } => {
                self.walk(rhs);
                self.bind_let_pattern(pattern, span);
            }
            Expression::Loop { identifier, pattern, iterable, body } => {
                self.walk(iterable);
                self.push();
                if let Some(identifier) = identifier
                    && let Expression::Identifier(name) = identifier.1.as_ref()
                {
                    self.bind(name, identifier.0.start..identifier.0.end);
                }
                if let Some(pattern) = pattern {
                    self.bind_let_pattern(pattern, span);
                }
                self.walk(body);
                self.pop();
            }
            Expression::Match { scrutinee, arms } => {
                self.walk(scrutinee);
                for arm in arms {
                    self.push();
                    self.bind_pattern(&arm.pattern);
                    self.walk(&arm.body);
                    self.pop();
                }
            }
            Expression::IfLet { scrutinee, then_arm, else_arm: other }
            | Expression::WhileLet { scrutinee, then_arm, on_miss: other } => {
                self.walk(scrutinee);
                for arm in [then_arm, other] {
                    self.push();
                    self.bind_pattern(&arm.pattern);
                    self.walk(&arm.body);
                    self.pop();
                }
            }
            other => other.for_each_child(&mut |child| self.walk(child)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn occurrences_at(source: &str, needle: &str, nth: usize) -> Vec<&'static str> {
        let offset = source.match_indices(needle).nth(nth).unwrap().0;
        let binding = binding_at(source, offset).expect("binding");
        binding
            .occurrences()
            .map(|r| {
                let line = source[..r.start].matches('\n').count();
                Box::leak(format!("{line}:{}", &source[r.clone()]).into_boxed_str()) as &str
            })
            .collect()
    }

    #[test]
    fn let_binding_and_uses() {
        let src = "fn main() {\n    let p = 1;\n    let q = p + p;\n}\n";
        assert_eq!(occurrences_at(src, "p", 0), ["1:p", "2:p", "2:p"]);
    }

    #[test]
    fn shadowing_splits_bindings() {
        let src = "fn main() {\n    let x = 1;\n    let x = x + 1;\n    let y = x;\n}\n";
        // Outer x: its decl and the use inside the shadowing initializer.
        assert_eq!(occurrences_at(src, "x", 0), ["1:x", "2:x"]);
        // Inner x: its decl and the later use.
        assert_eq!(occurrences_at(src, "x", 1), ["2:x", "3:x"]);
    }

    #[test]
    fn same_name_in_two_functions_is_two_bindings() {
        let src = "fn a() {\n    let p = 1;\n    let _ = p;\n}\nfn b() {\n    let p = 2;\n    let _ = p;\n}\n";
        assert_eq!(occurrences_at(src, "p", 0), ["1:p", "2:p"]);
        assert_eq!(occurrences_at(src, "p", 2), ["5:p", "6:p"]);
    }

    #[test]
    fn params_loops_and_patterns() {
        let src = "fn f(int n) -> int {\n    for i in [1, 2] {\n        let _ = i + n;\n    }\n    match Some(n) {\n        Option::Some(v) => { return v; },\n        Option::None => { return n; },\n    };\n}\n";
        assert_eq!(occurrences_at(src, "n) ->", 0), ["0:n", "2:n", "4:n", "6:n"]);
        assert_eq!(occurrences_at(src, "i ", 0), ["1:i", "2:i"]);
        assert_eq!(occurrences_at(src, "v)", 0), ["5:v", "5:v"]);
    }

    #[test]
    fn lambda_sees_outer_only_through_captures() {
        let src = "fn main() {\n    let k = 1;\n    let f = fn(int a) use (k) { return a + k; };\n}\n";
        assert_eq!(occurrences_at(src, "k", 0), ["1:k", "2:k", "2:k"]);
    }

    #[test]
    fn globals_are_not_locals() {
        let src = "fn helper() -> int { return 1; }\nfn main() {\n    let _ = helper();\n}\n";
        let offset = src.match_indices("helper").nth(1).unwrap().0;
        assert!(binding_at(src, offset).is_none());
    }
}
