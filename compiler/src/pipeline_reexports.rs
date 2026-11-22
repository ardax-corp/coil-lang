//! Re-exports (`pub use`, #2864): after discovery, every `use m::x` whose
//! module `m` only re-exports `x` (`pub use a::{x};` in `m`) is rewritten to
//! import the item from the module that declares it, so the checker and
//! every later stage see a plain import of `a::x`. The spelling as written
//! is kept in [`Expression::Use`]'s `written` and restored before each run,
//! so a cached AST follows a later edit of `m`.

use std::collections::HashMap;
use std::path::PathBuf;

use parser::ast::{Expression, Output};

use super::Pipeline;
use crate::module_roots::resolve_use_in_roots;

/// An imported item: its module path and name.
type ItemPath = (Vec<String>, String);

/// What one module re-exports: local name -> the item it imports.
type Exports = HashMap<String, ItemPath>;

impl Pipeline {
    /// Rewrite every top-level `use` of a re-exported name in the discovered
    /// files (see the module docs).
    pub(super) fn resolve_reexports(&mut self) {
        let files: Vec<PathBuf> = self.worklist.iter().map(|w| w.file.clone()).collect();
        let mut exports: HashMap<PathBuf, Exports> = HashMap::new();
        for file in &files {
            let Some(ast) = self.ast_cache.get_mut(file).and_then(|c| c.ast_mut()) else {
                continue;
            };
            restore_written(ast);
            let mut out = Exports::new();
            collect_exports(ast, &mut out);
            if !out.is_empty() {
                exports.insert(file.clone(), out);
            }
        }
        if exports.is_empty() {
            return;
        }
        let virtual_modules = crate::typechecking::VirtualModules::new();
        let resolve = |path: &[String], name: &str| -> Option<ItemPath> {
            let mut at = (path.to_vec(), name.to_string());
            let mut moved = false;
            // A chain of re-exports is at most one hop per module; a longer
            // one is a cycle, which discovery reports.
            for _ in 0..=files.len() {
                if at.1 == "*"
                    || virtual_modules.resolves_use(&at.0, &at.1)
                    || crate::macros::embedded_use(&at.0, &at.1).is_some()
                {
                    break;
                }
                let Some(file) = resolve_use_in_roots(&self.roots, &self.project_root, &at.0, &at.1) else {
                    break;
                };
                let Some(next) = exports.get(&file).and_then(|e| e.get(&at.1)) else {
                    break;
                };
                at = next.clone();
                moved = true;
            }
            moved.then_some(at)
        };
        let mut rewrites: Vec<(PathBuf, Vec<Rewrite>)> = Vec::new();
        for file in &files {
            let Some(ast) = self.ast_cache.get(file).and_then(|c| c.ast()) else {
                continue;
            };
            let mut found = Vec::new();
            plan_rewrites(ast, &mut Vec::new(), &resolve, &mut found);
            if !found.is_empty() {
                rewrites.push((file.clone(), found));
            }
        }
        for (file, found) in rewrites {
            let Some(ast) = self.ast_cache.get_mut(&file).and_then(|c| c.ast_mut()) else {
                continue;
            };
            for r in found {
                apply_rewrite(ast, &r.at, r.to);
            }
        }
    }
}

/// A `use` to rewrite: its position (child indices from the file's root)
/// and the item it imports.
struct Rewrite {
    at: Vec<usize>,
    to: ItemPath,
}

fn children_mut<'a, 'e>(node: &'a mut Output<'e>) -> Option<&'a mut Vec<Output<'e>>> {
    match node.1.as_mut() {
        Expression::Program(children) | Expression::Fragment(children) | Expression::Block(children) => {
            Some(children)
        }
        _ => None,
    }
}

fn children<'a, 'e>(node: &'a Output<'e>) -> Option<&'a Vec<Output<'e>>> {
    match node.1.as_ref() {
        Expression::Program(children) | Expression::Fragment(children) | Expression::Block(children) => {
            Some(children)
        }
        _ => None,
    }
}

/// Put back the spelling of every `use` an earlier run rewrote.
fn restore_written(node: &mut Output<'_>) {
    if let Some(children) = children_mut(node) {
        for child in children {
            restore_written(child);
        }
        return;
    }
    if let Expression::Use { path, name, alias, written, .. } = node.1.as_mut()
        && let Some(w) = written.take()
    {
        (*path, *name, *alias) = *w;
    }
}

/// The names `node`'s top-level `pub use`s re-export.
fn collect_exports(node: &Output<'_>, out: &mut Exports) {
    if let Some(children) = children(node) {
        for child in children {
            collect_exports(child, out);
        }
        return;
    }
    if let Expression::Use { path, name, alias, public: true, .. } = node.1.as_ref()
        && name != "*"
    {
        let local = alias.clone().unwrap_or_else(|| name.clone());
        out.entry(local).or_insert_with(|| (path.clone(), name.clone()));
    }
}

fn plan_rewrites(
    node: &Output<'_>,
    at: &mut Vec<usize>,
    resolve: &dyn Fn(&[String], &str) -> Option<ItemPath>,
    out: &mut Vec<Rewrite>,
) {
    if let Some(children) = children(node) {
        for (i, child) in children.iter().enumerate() {
            at.push(i);
            plan_rewrites(child, at, resolve, out);
            at.pop();
        }
        return;
    }
    if let Expression::Use { path, name, .. } = node.1.as_ref()
        && let Some(to) = resolve(path, name)
    {
        out.push(Rewrite { at: at.clone(), to });
    }
}

fn apply_rewrite(root: &mut Output<'_>, at: &[usize], to: ItemPath) {
    let mut node = root;
    for &i in at {
        let Some(child) = children_mut(node).and_then(|c| c.get_mut(i)) else {
            return;
        };
        node = child;
    }
    if let Expression::Use { path, name, alias, written, .. } = node.1.as_mut() {
        let local = alias.clone().unwrap_or_else(|| name.clone());
        let orig = (std::mem::take(path), std::mem::take(name), alias.clone());
        (*path, *name) = to;
        if *name != local {
            *alias = Some(local);
        }
        *written = Some(Box::new(orig));
    }
}
