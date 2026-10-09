//! Pipeline stage that gives trait method contracts to every impl (after
//! macro expansion, before typechecking). See `docs/internals/contracts.md`.
//!
//! An impl method gets a copy of its trait method's `requires` / `ensures`,
//! re-parsed as generated text so each copy has spans (and so node ids and
//! effect causes) of its own, with the trait's parameter names renamed to the
//! impl's. The impl may add `ensures` but not `requires`: a caller that only
//! knows the trait cannot see a stronger precondition.

use std::collections::HashMap;
use std::path::PathBuf;

use parser::ast::{ContractKind, Expression, Output};
use reporting::{ErrorCode, Message};

use super::Pipeline;
use super::macros::GeneratedRange;

/// One trait method's parameter names and clauses, as text.
struct TraitMethod {
    params: Vec<String>,
    clauses: Vec<(ContractKind, String, Option<String>)>,
}

/// Methods with clauses, by trait (`module`, `name`) and method name.
type TraitContracts = HashMap<(String, String), HashMap<String, TraitMethod>>;

impl Pipeline {
    /// Copy trait method clauses into the impls of every discovered file.
    pub(super) fn inherit_trait_contracts(&mut self) {
        let files: Vec<PathBuf> = self.processed.clone();
        let mut traits: TraitContracts = HashMap::new();
        for file in &files {
            let module = self.namespace_for(file);
            if let Some(ast) = self.ast_cache.get(file).and_then(|c| c.ast()) {
                collect_traits(ast, &module, &mut traits);
            }
        }
        if traits.is_empty() {
            return;
        }
        for file in &files {
            let module = self.namespace_for(file);
            let Some(cached) = self.ast_cache.get_mut(file) else { continue };
            if cached.contracts_inherited() {
                continue;
            }
            cached.mark_contracts_inherited();
            let Some(ast) = cached.ast() else { continue };
            let imports = file_imports(ast);
            // (impl index, method index, trait key) for each method to extend.
            let mut targets = Vec::new();
            let Expression::Program(items) = ast.1.as_ref() else { continue };
            for (i, item) in items.iter().enumerate() {
                let Expression::TypeClassImpl { class, methods, .. } = item.1.as_ref() else { continue };
                let Some(key) = resolve_trait(class, &module, &imports, &traits) else { continue };
                for (j, m) in methods.iter().enumerate() {
                    if let Some(name) = fn_name(m)
                        && traits[&key].contains_key(name)
                    {
                        targets.push((i, j, key.clone()));
                    }
                }
            }
            let mut messages = Vec::new();
            let mut generated_ranges = Vec::new();
            for (i, j, key) in targets {
                let (name, params, own_requires, site) = {
                    let Some(Expression::Program(items)) = cached.ast().map(|a| a.1.as_ref()) else { break };
                    let Expression::TypeClassImpl { methods, .. } = items[i].1.as_ref() else { continue };
                    let Some(Expression::Function { name, args, contracts, .. }) = function(&methods[j]) else {
                        continue;
                    };
                    let own_requires: Vec<_> = contracts
                        .iter()
                        .filter(|c| c.kind == ContractKind::Requires)
                        .map(|c| c.span.into_range())
                        .collect();
                    // Inherited clauses report at the impl method's header.
                    let start = methods[j].0.start;
                    let end = match function(&methods[j]) {
                        Some(Expression::Function { body: Some(body), .. }) => body.0.start,
                        _ => methods[j].0.end,
                    };
                    let header = cached.source().get(start..end).unwrap_or("");
                    let site = start..start + header.trim_end().len().max(1);
                    (name.to_string(), param_names(args), own_requires, site)
                };
                for range in own_requires {
                    let mut msg = Message::error(
                        ErrorCode::GenericTypeError,
                        format!("`{name}` implements a trait method, so it cannot add `requires`"),
                        range,
                    );
                    msg.with_help(format!(
                        "put the precondition on `{}::{name}`, or drop it from the impl",
                        key.1
                    ));
                    messages.push(msg);
                }
                let tm = &traits[&key][&name];
                let snippet = clause_snippet(&tm.clauses);
                let Ok((generated, range)) = cached.parse_generated(&snippet) else { continue };
                generated_ranges.push(GeneratedRange {
                    file: file.clone(),
                    range,
                    site,
                    origin: format!("the contracts of `{}::{name}`", key.1),
                    text: snippet,
                });
                let Expression::Program(mut gen_items) = *generated.1 else { continue };
                let Some(Expression::Function { contracts: mut copies, .. }) = gen_items.pop().map(|g| *g.1) else {
                    continue;
                };
                let renames: HashMap<&str, &'static str> = tm
                    .params
                    .iter()
                    .zip(params.iter())
                    .filter(|(t, i)| t != i)
                    .map(|(t, i)| (t.as_str(), *i))
                    .collect();
                for c in &mut copies {
                    rename_idents(&mut c.expr, &renames);
                }
                let Some(Expression::Program(items)) = cached.ast_mut().map(|a| a.1.as_mut()) else { break };
                let Expression::TypeClassImpl { methods, .. } = items[i].1.as_mut() else { continue };
                if let Some(Expression::Function { contracts, .. }) = function_mut(&mut methods[j]) {
                    // The trait's clauses first, then the impl's own `ensures`.
                    copies.append(contracts);
                    *contracts = copies;
                }
            }
            cached.push_expand_messages(messages);
            self.generated_ranges.extend(generated_ranges);
        }
    }
}

/// Record every trait method in `ast` that has clauses.
fn collect_traits(ast: &Output<'_>, module: &str, out: &mut TraitContracts) {
    let Expression::Program(items) = ast.1.as_ref() else { return };
    for item in items {
        let Expression::TypeClass { name, methods, .. } = item.1.as_ref() else { continue };
        for m in methods {
            let Some(Expression::Function { name: mname, args, contracts, .. }) = function(m) else { continue };
            if contracts.is_empty() {
                continue;
            }
            let clauses = contracts
                .iter()
                .map(|c| (c.kind, c.text.to_string(), c.message.map(str::to_string)))
                .collect();
            out.entry((module.to_string(), name.to_string()))
                .or_default()
                .insert(mname.to_string(), TraitMethod { params: param_names(args).iter().map(|p| p.to_string()).collect(), clauses });
        }
    }
}

/// `use` items of a file: local name to (module, item name).
fn file_imports(ast: &Output<'_>) -> HashMap<String, (String, String)> {
    fn walk(node: &Output<'_>, out: &mut HashMap<String, (String, String)>) {
        match node.1.as_ref() {
            Expression::Use { path, name, alias } => {
                let local = alias.clone().unwrap_or_else(|| name.clone());
                out.insert(local, (path.join("::"), name.clone()));
            }
            Expression::Fragment(items) => items.iter().for_each(|i| walk(i, out)),
            _ => {}
        }
    }
    let mut out = HashMap::new();
    if let Expression::Program(items) = ast.1.as_ref() {
        items.iter().for_each(|i| walk(i, &mut out));
    }
    out
}

/// The trait an `impl Trait for ..` names: a path, an import, this module's
/// own trait, or else the one trait of that name.
fn resolve_trait(
    class: &str,
    module: &str,
    imports: &HashMap<String, (String, String)>,
    traits: &TraitContracts,
) -> Option<(String, String)> {
    let key = if let Some((m, n)) = class.rsplit_once("::") {
        (m.to_string(), n.to_string())
    } else if let Some((m, n)) = imports.get(class) {
        (m.clone(), n.clone())
    } else {
        (module.to_string(), class.to_string())
    };
    if traits.contains_key(&key) {
        return Some(key);
    }
    let mut same_name = traits.keys().filter(|(_, n)| *n == key.1);
    match (same_name.next(), same_name.next()) {
        (Some(only), None) => Some(only.clone()),
        _ => None,
    }
}

/// `fn __coil_trait_contracts() requires .. ensures .. {}` for re-parsing.
fn clause_snippet(clauses: &[(ContractKind, String, Option<String>)]) -> String {
    let mut s = String::from("fn __coil_trait_contracts()");
    for (kind, text, message) in clauses {
        s.push(' ');
        s.push_str(kind.keyword());
        s.push(' ');
        s.push_str(text);
        if let Some(m) = message {
            s.push_str(", \"");
            s.push_str(m);
            s.push('"');
        }
    }
    s.push_str(" {}");
    s
}

/// Rename free uses of the trait's parameters. A lambda's own parameters
/// may shadow them, so lambdas are left alone.
fn rename_idents(node: &mut Output<'static>, renames: &HashMap<&str, &'static str>) {
    if renames.is_empty() {
        return;
    }
    match node.1.as_mut() {
        Expression::Identifier(id) => {
            if let Some(new) = renames.get(*id) {
                *id = new;
            }
        }
        Expression::Lambda { .. } => {}
        other => other.for_each_child_mut(&mut |child| rename_idents(child, renames)),
    }
}

fn function<'a, 'e>(node: &'a Output<'e>) -> Option<&'a Expression<'e>> {
    match node.1.as_ref() {
        f @ Expression::Function { .. } => Some(f),
        Expression::Method(_, inner) => function(inner),
        _ => None,
    }
}

fn function_mut<'a, 'e>(node: &'a mut Output<'e>) -> Option<&'a mut Expression<'e>> {
    match node.1.as_mut() {
        Expression::Method(_, inner) => function_mut(inner),
        f @ Expression::Function { .. } => Some(f),
        _ => None,
    }
}

fn fn_name<'e>(node: &Output<'e>) -> Option<&'e str> {
    match function(node)? {
        Expression::Function { name, .. } => Some(name),
        _ => None,
    }
}

/// Parameter names of a `Function`'s `args` fragment, in order.
fn param_names<'e>(args: &Output<'e>) -> Vec<&'e str> {
    match args.1.as_ref() {
        Expression::Fragment(items) => items
            .iter()
            .filter_map(|a| match a.1.as_ref() {
                Expression::Argument { name, .. } => Some(*name),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}
