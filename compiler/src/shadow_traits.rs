//! A module trait that takes a rebound builtin trait's short name (#1345).
//!
//! After `use prelude::ops::Eq as PreludeEq;` a module may declare its own
//! `trait Eq`. Traits share one table keyed by short name, so the module's
//! `Eq` would replace the builtin and leave `PreludeEq` naming nothing. In
//! such a module the short name can only mean the module's trait, so every
//! use of it (outside `use` items) is renamed to [`internal_name`] before
//! checking. The builtin keeps its key, its alias and `==`;
//! [`display_names`] spells the renamed trait back in diagnostics.

use std::collections::HashSet;

use parser::ast::{Expression, Output, TypeParam, WhereConstraint};
use reporting::Message;

use crate::attrs::leak;
use crate::typechecking::Checker;

/// The key a module trait named `name` is checked and compiled under.
pub fn internal_name(name: &str) -> String {
    format!("{name}__trait")
}

/// Rename the module traits that take a rebound builtin trait's name.
/// Returns the short names renamed.
pub fn rename(ast: &mut Output<'_>) -> Vec<String> {
    let Expression::Program(children) = ast.1.as_ref() else {
        return Vec::new();
    };
    let mut rebound = HashSet::new();
    for child in children {
        collect_rebound(child, &mut rebound);
    }
    let names: Vec<String> = children
        .iter()
        .filter_map(|c| match c.1.as_ref() {
            Expression::TypeClass { name, .. } if rebound.contains(*name) => Some(name.to_string()),
            _ => None,
        })
        .collect();
    if names.is_empty() {
        return names;
    }
    let renames: Vec<(&'static str, &'static str)> = names
        .iter()
        .map(|n| (leak(n.clone()), leak(internal_name(n))))
        .collect();
    rename_in(ast, &renames);
    names
}

/// Spell each renamed trait by its short name in `msg`.
pub fn display_names(msg: &Message, names: &[String]) -> Message {
    let mut text = msg.message().to_string();
    let mut help = msg.help().clone();
    for name in names {
        let internal = internal_name(name);
        text = text.replace(&internal, name);
        help = help.map(|h| h.replace(&internal, name));
    }
    if text == msg.message() && help == *msg.help() {
        return msg.clone();
    }
    let mut out = Message::new(*msg.kind(), text, msg.range().clone());
    if let Some(code) = msg.code() {
        out.set_code(code);
    }
    if let Some(h) = help {
        out.with_help(h);
    }
    out
}

/// Builtin trait names a `use prelude… as Alias` frees in this module.
fn collect_rebound(node: &Output<'_>, out: &mut HashSet<String>) {
    match node.1.as_ref() {
        Expression::Use { path, name, alias: Some(alias) }
            if path.first().is_some_and(|p| p == "prelude")
                && alias != name
                && Checker::is_builtin_class(name) =>
        {
            out.insert(name.clone());
        }
        Expression::Fragment(items) => items.iter().for_each(|i| collect_rebound(i, out)),
        Expression::Statement(inner) => collect_rebound(inner, out),
        _ => {}
    }
}

fn swap(name: &mut &str, renames: &[(&'static str, &'static str)]) {
    if let Some((_, to)) = renames.iter().find(|(from, _)| from == name) {
        *name = to;
    }
}

fn rename_params(params: &mut [TypeParam<'_>], renames: &[(&'static str, &'static str)]) {
    for p in params {
        for b in &mut p.bounds {
            swap(b, renames);
        }
    }
}

fn rename_wheres(wheres: &mut [WhereConstraint<'_>], renames: &[(&'static str, &'static str)]) {
    for w in wheres {
        swap(&mut w.class, renames);
        for a in &mut w.args {
            rename_in(a, renames);
        }
    }
}

fn rename_in(node: &mut Output<'_>, renames: &[(&'static str, &'static str)]) {
    use Expression as E;
    match node.1.as_mut() {
        // `use` items name the builtin (and other modules) as written.
        E::Use { .. } => return,
        E::Identifier(n) | E::Type(n) => swap(n, renames),
        E::TypeApp { name, .. } => swap(name, renames),
        E::TypeProjection { owner, .. } => swap(owner, renames),
        E::QualifiedAccess { owner, .. } => swap(owner, renames),
        E::Construct { enum_name, .. } => swap(enum_name, renames),
        E::TypeClass { name, type_params, .. } => {
            swap(name, renames);
            rename_params(type_params, renames);
        }
        E::TypeClassImpl { class, type_params, .. } => {
            swap(class, renames);
            rename_params(type_params, renames);
        }
        E::Implementation { what, type_params, .. } => {
            swap(what, renames);
            rename_params(type_params, renames);
        }
        E::Function { type_params, where_constraints, .. } | E::AttrDecl { type_params, where_constraints, .. } => {
            rename_params(type_params, renames);
            rename_wheres(where_constraints, renames);
        }
        E::Class { type_params, .. }
        | E::TypeAlias { type_params, .. }
        | E::EnumDecl { type_params, .. }
        | E::AssocTypeDecl { type_params, .. }
        | E::AssocTypeDef { type_params, .. } => rename_params(type_params, renames),
        E::Forall { params, .. } => rename_params(params, renames),
        _ => {}
    }
    node.1.for_each_child_mut(&mut |c| rename_in(c, renames));
}
