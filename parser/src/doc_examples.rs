//! Code examples in `///` doc comments: which fences are coil examples,
//! which lines are hidden, and the program a doc-test runs.
//!
//! The language server renders examples with these rules (hidden lines are
//! dropped on hover) and `coil test` runs them as doc-tests:
//!
//! - A fence opened with ```` ``` ```` or ```` ```coil ```` is an example.
//!   The rustdoc tags `ignore` (not compiled), `no_run` (compiled only) and
//!   `should_panic` (must panic) may follow, comma-separated
//!   (```` ```coil,no_run ````, or alone: ```` ```no_run ````). Any other
//!   tag (```` ```text ````) makes the fence plain text.
//! - Lines starting with `# ` (or a lone `#`) are hidden on hover but are part
//!   of the example (`# use tests::assert;`).
//! - [`DocExample::program`]: `use` lines are hoisted to the top and, when the
//!   example has no `fn main`, the rest is wrapped in `fn main() { … }`.

use crate::ast::{Expression, Output};

/// How a doc-test treats an example.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExampleMode {
    /// Compile and run; passes when `main` returns without a panic.
    Run,
    /// Compile only (```` ```no_run ````).
    NoRun,
    /// Compile and run; passes only when the program panics.
    ShouldPanic,
    /// Not compiled (```` ```ignore ````).
    Ignore,
}

/// The mode of a fence whose opening line is ```` ```info ````, or `None` when
/// the fence is not a coil example (```` ```text ````, ```` ```json ````, …).
pub fn fence_mode(info: &str) -> Option<ExampleMode> {
    let mut mode = ExampleMode::Run;
    for tag in info.split([',', ' ', '\t']).map(str::trim).filter(|t| !t.is_empty()) {
        match tag {
            "coil" => {}
            "ignore" => mode = ExampleMode::Ignore,
            "no_run" if mode != ExampleMode::Ignore => mode = ExampleMode::NoRun,
            "should_panic" if mode == ExampleMode::Run => mode = ExampleMode::ShouldPanic,
            "no_run" | "should_panic" => {}
            _ => return None,
        }
    }
    Some(mode)
}

/// The text after ```` ``` ```` when `line` opens or closes a fence.
pub fn fence_info(line: &str) -> Option<&str> {
    line.trim_start().strip_prefix("```")
}

/// A line inside a coil example that hover hides but the doc-test runs:
/// `# setup` or a lone `#` (not `#[attr]`).
pub fn is_hidden_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    trimmed == "#" || trimmed.starts_with("# ")
}

/// `line` as the doc-test sees it: a hidden line without its `# ` marker
/// (indentation before the `#` is kept).
pub fn unhide(line: &str) -> String {
    if !is_hidden_line(line) {
        return line.to_owned();
    }
    let indent = &line[..line.len() - line.trim_start().len()];
    let rest = line.trim_start();
    let rest = rest.strip_prefix("# ").unwrap_or(rest.strip_prefix('#').unwrap_or(rest));
    format!("{indent}{rest}")
}

/// One fenced coil example in a doc comment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocExample<'a> {
    pub mode: ExampleMode,
    /// Index of the opening fence in the doc lines.
    pub fence: usize,
    /// The lines between the fences, hidden ones included (with their `#`).
    pub lines: Vec<&'a str>,
}

/// Every coil example in `docs` (one entry per `///` line, without the
/// `///`), in order. An unclosed fence runs to the end of the docs.
pub fn examples<'a>(docs: &[&'a str]) -> Vec<DocExample<'a>> {
    let mut out = Vec::new();
    // `Some(Some(example))`: inside a coil example; `Some(None)`: another fence.
    let mut open: Option<Option<DocExample<'a>>> = None;
    for (i, line) in docs.iter().enumerate() {
        if let Some(info) = fence_info(line) {
            match open.take() {
                None => {
                    open = Some(fence_mode(info).map(|mode| DocExample { mode, fence: i, lines: Vec::new() }));
                }
                Some(done) => out.extend(done),
            }
            continue;
        }
        if let Some(Some(example)) = open.as_mut() {
            example.lines.push(line);
        }
    }
    if let Some(Some(example)) = open {
        out.push(example);
    }
    out
}

impl DocExample<'_> {
    /// The program the doc-test compiles: hidden lines shown, `use`
    /// statements first, the rest inside `fn main() { … }` unless the
    /// example declares its own `main`.
    pub fn program(&self) -> String {
        let lines: Vec<String> = self.lines.iter().map(|l| unhide(l)).collect();
        if lines.iter().any(|l| declares_main(l)) {
            let mut text = lines.join("\n");
            text.push('\n');
            return text;
        }
        let mut uses = Vec::new();
        let mut body = Vec::new();
        let mut in_use = false;
        for line in lines {
            let trimmed = line.trim_start();
            let starts_use = trimmed.starts_with("use ") || trimmed.starts_with("pub use ");
            if in_use || starts_use {
                in_use = !trimmed.trim_end().ends_with(';');
                uses.push(trimmed.to_owned());
            } else {
                body.push(line);
            }
        }
        let mut text = String::new();
        for u in &uses {
            text.push_str(u);
            text.push('\n');
        }
        text.push_str("fn main() {\n");
        for line in &body {
            if !line.trim().is_empty() {
                text.push_str("    ");
                text.push_str(line);
            }
            text.push('\n');
        }
        text.push_str("}\n");
        text
    }
}

/// `fn main(` at the start of a line (`pub fn main(` too).
fn declares_main(line: &str) -> bool {
    let t = line.trim_start();
    let t = t.strip_prefix("pub ").map(str::trim_start).unwrap_or(t);
    t.strip_prefix("fn")
        .is_some_and(|rest| rest.starts_with(char::is_whitespace) && rest.trim_start().starts_with("main") && {
            let after = rest.trim_start()["main".len()..].trim_start();
            after.starts_with('(') || after.starts_with('<')
        })
}

/// A declaration with `///` docs: its path inside the file and its doc lines.
#[derive(Debug, Clone)]
pub struct DocItem<'a> {
    /// `f`, `Owner::method`, `Enum::Variant`, `Class::field`.
    pub path: String,
    pub docs: &'a [&'a str],
}

/// Every documented declaration of a parsed file (functions, methods,
/// classes and their fields, enums and their variants, traits and their
/// methods, type aliases, macros), in source order.
pub fn documented_items<'a>(ast: &'a Output<'a>) -> Vec<DocItem<'a>> {
    let mut out = Vec::new();
    collect(ast, "", &mut out);
    out
}

fn join(prefix: &str, name: &str) -> String {
    if prefix.is_empty() { name.to_owned() } else { format!("{prefix}::{name}") }
}

/// `Vec<T>` → `Vec`.
fn bare(name: &str) -> &str {
    name.split('<').next().unwrap_or(name).trim()
}

fn collect<'a>(node: &'a Output<'a>, prefix: &str, out: &mut Vec<DocItem<'a>>) {
    let push = |name: &str, docs: &'a [&'a str], out: &mut Vec<DocItem<'a>>| {
        if !docs.is_empty() {
            out.push(DocItem { path: join(prefix, name), docs });
        }
    };
    match node.1.as_ref() {
        Expression::Program(items) | Expression::Fragment(items) => {
            for item in items {
                collect(item, prefix, out);
            }
        }
        Expression::Expr(inner) | Expression::Statement(inner) | Expression::Method(_, inner) => {
            collect(inner, prefix, out);
        }
        Expression::Function { name, docs, .. } => push(name, docs, out),
        Expression::TypeAlias { name, docs, .. }
        | Expression::AttrDecl { name, docs, .. }
        | Expression::DeriveDecl { name, docs, .. }
        | Expression::FnMacroDecl { name, docs, .. } => push(name, docs, out),
        Expression::Class { name, docs, fields, .. } => {
            push(name, docs, out);
            let owner = join(prefix, name);
            for field in fields {
                if let Expression::Field { name: field_name, docs, .. } = field.1.as_ref()
                    && !docs.is_empty()
                {
                    let field_name = match field_name.1.as_ref() {
                        Expression::Identifier(n) => (*n).to_owned(),
                        other => other.to_string(),
                    };
                    out.push(DocItem { path: join(&owner, &field_name), docs });
                }
            }
        }
        Expression::EnumDecl { name, docs, variants, .. } => {
            push(name, docs, out);
            let owner = join(prefix, name);
            for variant in variants {
                if let Expression::EnumVariant { name: variant, docs, .. } = variant.1.as_ref()
                    && !docs.is_empty()
                {
                    out.push(DocItem { path: join(&owner, variant), docs });
                }
            }
        }
        Expression::TypeClass { name, docs, methods, .. } => {
            push(name, docs, out);
            let owner = join(prefix, name);
            for method in methods {
                collect(method, &owner, out);
            }
        }
        Expression::Implementation { owner, methods, .. } => {
            let owner = join(prefix, bare(owner));
            for method in methods {
                collect(method, &owner, out);
            }
        }
        Expression::TypeClassImpl { args, methods, .. } => {
            let owner = args.first().map(|a| a.1.to_string()).unwrap_or_default();
            let owner = join(prefix, bare(&owner));
            for method in methods {
                collect(method, &owner, out);
            }
        }
        _ => {}
    }
}

/// 1-based line in `source` of the doc line `doc` (a slice the parser took
/// from `source`), or `None` when `doc` does not point into `source`.
pub fn line_of(source: &str, doc: &str) -> Option<usize> {
    let start = source.as_ptr() as usize;
    let at = (doc.as_ptr() as usize).checked_sub(start)?;
    (at <= source.len()).then(|| source.as_bytes()[..at].iter().filter(|b| **b == b'\n').count() + 1)
}

#[cfg(test)]
#[path = "doc_examples.tests.rs"]
mod tests;
