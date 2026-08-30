//! Bind `use` / `mod` to interned [`DefId`]s.
//!
//! Runs after parse / [`Pipeline::enqueue_uses`](crate::pipeline::Pipeline)
//! discovery, at the start of check. Does **not** crawl the filesystem —
//! the file graph is already built; this walk only intern defs in the
//! current AST and alias imported names to DefIds minted when the
//! defining module was resolved (dependency order).

use std::collections::HashMap;

use parser::ast::{Expression, Output};

use super::def_id::{DefId, DefInterner, DefKind, ModuleId};
use super::virtual_modules::VirtualModules;

/// Intern top-level defs in `ast` and bind `use` aliases onto `local_defs`.
pub fn resolve(
    interner: &mut DefInterner,
    module: ModuleId,
    ast: &Output,
    virtual_modules: &VirtualModules,
    local_defs: &mut HashMap<String, DefId>,
) {
    intern_items(interner, module, ast, local_defs);
    bind_uses(interner, ast, virtual_modules, local_defs);
}

fn unwrap<'a>(node: &'a Output<'a>) -> &'a Output<'a> {
    let mut current = node;
    for _ in 0..8 {
        current = match current.1.as_ref() {
            Expression::Statement(inner)
            | Expression::ExprStatement(inner)
            | Expression::Expr(inner)
            | Expression::Group(inner) => inner,
            _ => break,
        };
    }
    current
}

fn intern_items(
    interner: &mut DefInterner,
    module: ModuleId,
    ast: &Output,
    local_defs: &mut HashMap<String, DefId>,
) {
    let ast = unwrap(ast);
    match ast.1.as_ref() {
        Expression::Program(children)
        | Expression::Fragment(children)
        | Expression::Block(children) => {
            for child in children {
                intern_items(interner, module, child, local_defs);
            }
        }
        Expression::Function { name, .. } => {
            let id = interner.intern(module, DefKind::Fn, name);
            local_defs.entry((*name).to_string()).or_insert(id);
        }
        Expression::Class { name, .. } => {
            let id = interner.intern(module, DefKind::Class, name);
            local_defs.entry((*name).to_string()).or_insert(id);
        }
        Expression::EnumDecl { name, .. } => {
            let id = interner.intern(module, DefKind::Enum, name);
            local_defs.entry((*name).to_string()).or_insert(id);
        }
        Expression::TypeAlias { name, .. } => {
            let id = interner.intern(module, DefKind::TypeAlias, name);
            local_defs.entry((*name).to_string()).or_insert(id);
        }
        Expression::StaticDecl { name, .. } => {
            let id = interner.intern(module, DefKind::Static, name);
            local_defs.entry((*name).to_string()).or_insert(id);
        }
        Expression::ExternBlock { declarations, .. } => {
            for decl in declarations {
                let _ = interner.intern(module, DefKind::Ffi, decl.name);
            }
        }
        Expression::Implementation { owner, methods, .. } => {
            for method in methods {
                intern_method(interner, module, owner, method, local_defs);
            }
        }
        Expression::Module(name, _body) => {
            let path = match interner.module_path(module) {
                Some("") => name.clone(),
                Some(parent) => format!("{parent}::{name}"),
                None => name.clone(),
            };
            let _ = interner.intern_module(&path);
        }
        _ => {}
    }
}

fn intern_method(
    interner: &mut DefInterner,
    module: ModuleId,
    owner: &str,
    node: &Output,
    local_defs: &mut HashMap<String, DefId>,
) {
    let node = unwrap(node);
    match node.1.as_ref() {
        Expression::Method(_, body) => intern_method(interner, module, owner, body, local_defs),
        Expression::Function { name, .. } => {
            let qual = format!("{owner}::{name}");
            let id = interner.intern(module, DefKind::Method, &qual);
            local_defs.entry(qual).or_insert(id);
        }
        _ => {}
    }
}

fn bind_uses(
    interner: &mut DefInterner,
    ast: &Output,
    virtual_modules: &VirtualModules,
    local_defs: &mut HashMap<String, DefId>,
) {
    let ast = unwrap(ast);
    match ast.1.as_ref() {
        Expression::Program(children)
        | Expression::Fragment(children)
        | Expression::Block(children) => {
            for child in children {
                bind_uses(interner, child, virtual_modules, local_defs);
            }
        }
        Expression::Use { path, name, alias } => {
            if name == "*" {
                return;
            }
            if virtual_modules.resolves_use(path, name) {
                return;
            }
            let module_ns = path.join("::");
            let Some(mid) = interner.module_id(&module_ns) else {
                return;
            };
            let Some(id) = interner.get(mid, name) else {
                return;
            };
            let local = alias.clone().unwrap_or_else(|| name.clone());
            // A local def of the same short name wins (intern ran first).
            local_defs.entry(local).or_insert(id);
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use parser::Pratt;

    fn parse(src: &str) -> parser::ast::Output<'static> {
        // Leak the source so the AST can outlive this helper in tests.
        let owned: &'static str = Box::leak(src.to_string().into_boxed_str());
        Pratt::default().parse(owned).expect("parse failed")
    }

    #[test]
    fn resolve_interns_two_fns_with_distinct_ids() {
        let ast = parse("fn foo() {}\nfn bar() {}\n");
        let mut intern = DefInterner::new();
        let m = intern.intern_module("");
        let mut local = HashMap::new();
        resolve(&mut intern, m, &ast, &VirtualModules::new(), &mut local);
        let foo = *local.get("foo").expect("foo interned");
        let bar = *local.get("bar").expect("bar interned");
        assert_ne!(foo, bar);
    }

    #[test]
    fn resolve_same_logical_def_twice_same_id() {
        let ast = parse("fn foo() {}\n");
        let mut intern = DefInterner::new();
        let m = intern.intern_module("");
        let mut local = HashMap::new();
        resolve(&mut intern, m, &ast, &VirtualModules::new(), &mut local);
        let first = *local.get("foo").unwrap();
        let mut local2 = HashMap::new();
        resolve(&mut intern, m, &ast, &VirtualModules::new(), &mut local2);
        assert_eq!(*local2.get("foo").unwrap(), first);
    }

    #[test]
    fn resolve_use_binds_same_def_id_as_defining_module() {
        let def_ast = parse("fn add(int a, int b) -> int { return a + b; }\n");
        let use_ast = parse("use math::add;\nfn main() { add(1, 2); }\n");
        let mut intern = DefInterner::new();
        let math = intern.intern_module("math");
        let entry = intern.intern_module("");
        let mut math_locals = HashMap::new();
        resolve(
            &mut intern,
            math,
            &def_ast,
            &VirtualModules::new(),
            &mut math_locals,
        );
        let defined = *math_locals.get("add").expect("defining add");
        let mut entry_locals = HashMap::new();
        resolve(
            &mut intern,
            entry,
            &use_ast,
            &VirtualModules::new(),
            &mut entry_locals,
        );
        let imported = *entry_locals.get("add").expect("imported add");
        assert_eq!(imported, defined);
    }
}
