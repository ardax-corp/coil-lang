//! Whole-program "might this name be written" sidecar for folding a
//! `static let` nothing writes like a `static const`.
//!
//! Conservative: every name that roots an assignment, a compound
//! assignment or a `++` / `--` target anywhere in the compile (a local
//! of the same name included, and the original of a `use` alias) counts
//! as written. Seeded from every AST before any module is emitted, like
//! [`crate::typechecking::fn_value_escape`].

use std::collections::{HashMap, HashSet};

use parser::ast::{Expression, Output};

/// Union into `into` the names `ast` may write.
pub fn collect_written_names(ast: &Output<'_>, into: &mut HashSet<String>) {
    let mut aliases: HashMap<String, String> = HashMap::new();
    let mut written = HashSet::new();
    walk(ast, &mut aliases, &mut written);
    for name in &written {
        if let Some(original) = aliases.get(name) {
            into.insert(original.clone());
        }
    }
    into.extend(written);
}

fn walk(ast: &Output<'_>, aliases: &mut HashMap<String, String>, written: &mut HashSet<String>) {
    match ast.1.as_ref() {
        Expression::Use { name, alias: Some(alias), .. } => {
            aliases.insert(alias.clone(), name.clone());
        }
        Expression::Assignment(target, _) | Expression::CompoundAssign(target, _, _) | Expression::Adjust { target, .. } => {
            roots(target, written);
        }
        _ => {}
    }
    ast.1.for_each_child(&mut |child| walk(child, aliases, written));
}

/// The names `target` writes through: `x`, `m::X`, and the bases of
/// `a.f` / `a[i]`.
fn roots(target: &Output<'_>, written: &mut HashSet<String>) {
    match target.1.as_ref() {
        Expression::Identifier(name) => {
            written.insert((*name).to_string());
        }
        Expression::QualifiedAccess { member, .. } => {
            written.insert((*member).to_string());
        }
        Expression::Access(base, member) => {
            written.insert((*member).to_string());
            roots(base, written);
        }
        Expression::Index(base, _) => roots(base, written),
        Expression::Group(inner) | Expression::Expr(inner) => roots(inner, written),
        _ => {}
    }
}
