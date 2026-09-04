//! Conservative "does this name ever appear as a function value"
//! sidecar fact for the two-slot CALL/RETURN classifier.
//!
//! `CallIndirect` / `MakeFn` / `MakePolyFn` / FFI callback / thread-spawn
//! / coroutine targets keep the one-word boxed ABI — a two-word direct
//! CALL/RETURN function must not have its address taken. Rather than
//! synthesizing a unary wrapper at every such site, this walk fails
//! closed: any name used as a value (not the direct callee of a `Call`)
//! is poisoned so the classifier refuses to widen it.
//!
//! Proof is **whole-program**: [`collect_fn_value_escaped`] is seeded from
//! every AST in the compile (package use-graph plus in-memory entry)
//! *before* any module is emitted. A per-file walk alone cannot see
//! `file A` take `&f` while `file B` defines `f` with a two-slot RETURN.

use std::collections::{HashMap, HashSet};

use parser::ast::{EnumConstructPayload, Expression, Output};

use super::infer::Checker;

/// Fill [`Checker::fn_value_escaped`] for `ast` after inference.
///
/// This is the per-module sidecar snapshot. Package-wide proof lives on
/// [`crate::Compiler::set_fn_value_escaped_program`], seeded before emit.
pub fn analyze_fn_value_escape(checker: &mut Checker, ast: &Output<'_>) {
    checker.fn_value_escaped.clear();
    collect_fn_value_escaped(ast, &mut checker.fn_value_escaped);
}

/// Union names that appear as a function *value* in `ast` into `into`.
///
/// Walks bare identifiers, [`Expression::QualifiedAccess`], and method
/// names on [`Expression::Access`] / [`Expression::OptionalAccess`] when
/// those nodes are not the `name` of a [`Expression::Call`]. `use` aliases
/// (`use m::{f as g}`) map a poisoned local back to the imported stem/FQN.
pub fn collect_fn_value_escaped(ast: &Output<'_>, into: &mut HashSet<String>) {
    let mut aliases: HashMap<String, Vec<String>> = HashMap::new();
    collect_use_aliases(ast, &mut aliases);
    walk(ast, false, into, &aliases);
}

fn collect_use_aliases(ast: &Output<'_>, aliases: &mut HashMap<String, Vec<String>>) {
    if let Expression::Use { path, name, alias } = ast.1.as_ref() {
        if name != "*" {
            let local = alias.clone().unwrap_or_else(|| name.clone());
            let fqn = if path.is_empty() {
                name.clone()
            } else {
                format!("{}::{name}", path.join("::"))
            };
            let entry = aliases.entry(local).or_default();
            if !entry.iter().any(|s| s == name) {
                entry.push(name.clone());
            }
            if !entry.iter().any(|s| s == &fqn) {
                entry.push(fqn);
            }
        }
        return;
    }
    walk_children(ast, &mut |child| collect_use_aliases(child, aliases));
}

fn poison(into: &mut HashSet<String>, aliases: &HashMap<String, Vec<String>>, name: &str) {
    into.insert(name.to_string());
    if let Some(mapped) = aliases.get(name) {
        for m in mapped {
            into.insert(m.clone());
        }
    }
}

/// `in_call_name` is true exactly for the `name` position of a `Call` — the
/// one context that does not escape.
fn walk(
    ast: &Output<'_>,
    in_call_name: bool,
    into: &mut HashSet<String>,
    aliases: &HashMap<String, Vec<String>>,
) {
    match ast.1.as_ref() {
        Expression::Identifier(n) if !in_call_name => {
            poison(into, aliases, n);
        }
        Expression::QualifiedAccess { owner, member } if !in_call_name => {
            poison(into, aliases, member);
            poison(into, aliases, &format!("{owner}::{member}"));
        }
        Expression::Access(recv, method) | Expression::OptionalAccess(recv, method) => {
            if !in_call_name {
                poison(into, aliases, method);
            }
            walk(recv, false, into, aliases);
        }
        Expression::Call { name, args } => {
            walk(name, true, into, aliases);
            if let Some(args) = args {
                for a in args {
                    walk(a, false, into, aliases);
                }
            }
        }
        Expression::Construct { fields, .. } => match fields {
            EnumConstructPayload::Unit => {}
            EnumConstructPayload::Tuple(args) => {
                for a in args {
                    walk(a, false, into, aliases);
                }
            }
            EnumConstructPayload::Record(parts) => {
                for p in parts {
                    walk(&p.value, false, into, aliases);
                }
            }
        },
        _ => walk_children(ast, &mut |child| walk(child, false, into, aliases)),
    }
}

fn walk_children(ast: &Output<'_>, f: &mut dyn FnMut(&Output<'_>)) {
    match ast.1.as_ref() {
        Expression::Program(items)
        | Expression::Block(items)
        | Expression::Fragment(items)
        | Expression::List(items)
        | Expression::Array(items)
        | Expression::Tuple(items)
        | Expression::If(items)
        | Expression::Declare(items)
        | Expression::Invoke(items) => {
            for item in items {
                f(item);
            }
        }
        Expression::Module(_, inner)
        | Expression::Statement(inner)
        | Expression::Expr(inner)
        | Expression::ExprStatement(inner)
        | Expression::Group(inner)
        | Expression::Return(inner)
        | Expression::ImplicitReturn(inner)
        | Expression::Raise(inner)
        | Expression::Try(inner)
        | Expression::Negate(inner)
        | Expression::Not(inner)
        | Expression::LogicalNot(inner)
        | Expression::Positive(inner)
        | Expression::Cast(inner, _)
        | Expression::TypeOf(inner)
        | Expression::Readonly(inner)
        | Expression::Yield(inner)
        | Expression::YieldFrom(inner)
        | Expression::Panic(inner)
        | Expression::Member(inner)
        | Expression::Method(_, inner)
        | Expression::NamedArg(_, inner)
        | Expression::Defer { body: inner, .. }
        | Expression::Spread(inner)
        | Expression::Dload(inner)
        | Expression::Done(inner)
        | Expression::Noop(inner) => f(inner),
        Expression::Add(a, b)
        | Expression::Sub(a, b)
        | Expression::Mul(a, b)
        | Expression::Div(a, b)
        | Expression::Mod(a, b)
        | Expression::Pow(a, b)
        | Expression::Shl(a, b)
        | Expression::Shr(a, b)
        | Expression::Xor(a, b)
        | Expression::And(a, b)
        | Expression::BitAnd(a, b)
        | Expression::Or(a, b)
        | Expression::BitOr(a, b)
        | Expression::Eq(a, b)
        | Expression::Neq(a, b)
        | Expression::Le(a, b)
        | Expression::Gt(a, b)
        | Expression::Leq(a, b)
        | Expression::Geq(a, b)
        | Expression::Coalesce(a, b)
        | Expression::Assignment(a, b)
        | Expression::CompoundAssign(a, _, b)
        | Expression::TypeFun(a, b)
        | Expression::Range {
            start: a, end: b, ..
        } => {
            f(a);
            f(b);
        }
        Expression::Match { scrutinee, arms } => {
            f(scrutinee);
            for arm in arms {
                f(&arm.body);
            }
        }
        Expression::Branch(cond, body) => {
            if let Some(c) = cond {
                f(c);
            }
            f(body);
        }
        Expression::Loop {
            identifier,
            iterable,
            body,
        } => {
            if let Some(id) = identifier {
                f(id);
            }
            f(iterable);
            f(body);
        }
        Expression::Instantiate(class, args) => {
            f(class);
            if let Some(args) = args {
                for a in args {
                    f(a);
                }
            }
        }
        Expression::Index(base, idx) => {
            f(base);
            if let Some(idx) = idx {
                f(idx);
            }
        }
        Expression::LetDestructure { rhs, .. } => f(rhs),
        Expression::Adjust { target, .. } => f(target),
        Expression::Resume(t, arg) => {
            f(t);
            if let Some(a) = arg {
                f(a);
            }
        }
        Expression::Dict(fields) => {
            for field in fields {
                f(&field.value);
            }
        }
        Expression::Function { body, args, .. } => {
            f(args);
            if let Some(body) = body {
                f(body);
            }
        }
        Expression::Lambda { body, .. } => f(body),
        Expression::TestCase { body, .. } => f(body),
        Expression::Implementation { methods, .. } => {
            for m in methods {
                f(m);
            }
        }
        Expression::StaticDecl { ty, init, .. } => {
            if let Some(t) = ty {
                f(t);
            }
            f(init);
        }
        Expression::Variable(_, ty) => {
            if let Some(t) = ty {
                f(t);
            }
        }
        Expression::Constant(init, ty) => {
            f(init);
            if let Some(t) = ty {
                f(t);
            }
        }
        Expression::Argument { ty, .. } => {
            if let Some(t) = ty {
                f(t);
            }
        }
        Expression::TypeFnSig { params, ret } => {
            f(params);
            f(ret);
        }
        Expression::AttrDecl {
            args,
            returns,
            body,
            ..
        } => {
            f(args);
            if let Some(r) = returns {
                f(r);
            }
            f(body);
        }
        Expression::TypeApp { args, .. } | Expression::TypeProjection { args, .. } => {
            for a in args {
                f(a);
            }
        }
        Expression::Class { fields, .. } => {
            for field in fields {
                f(field);
            }
        }
        Expression::TypeClassImpl { methods, .. } => {
            for m in methods {
                f(m);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use parser::Pratt;
    use std::collections::HashSet;

    fn escaped(src: &str) -> HashSet<String> {
        let owned = Box::leak(src.to_string().into_boxed_str());
        let ast = Pratt::default().parse(owned).expect("parse");
        let mut names = HashSet::new();
        collect_fn_value_escaped(&ast, &mut names);
        names
    }

    fn escaped_via_checker(src: &str) -> HashSet<String> {
        let owned = Box::leak(src.to_string().into_boxed_str());
        let ast = Pratt::default().parse(owned).expect("parse");
        let mut c = Checker::new();
        let _ = c.check_program(&ast);
        c.fn_value_escaped_names().clone()
    }

    #[test]
    fn direct_call_does_not_escape() {
        let src = r#"
fn f(int n) -> int { return n; }
fn main() { let _ = f(1); }
"#;
        assert!(!escaped(src).contains("f"));
        assert!(!escaped_via_checker(src).contains("f"));
    }

    #[test]
    fn bare_reference_escapes() {
        let src = r#"
fn f(int n) -> int { return n; }
fn main() { let g = f; let _ = g(1); }
"#;
        assert!(escaped(src).contains("f"));
    }

    #[test]
    fn passed_as_argument_escapes() {
        let src = r#"
fn f(int n) -> int { return n; }
fn main() {
    let g = f;
    let arr = [g];
    let _ = arr[0](1);
}
"#;
        assert!(escaped(src).contains("f"));
    }

    #[test]
    fn qualified_access_as_value_escapes() {
        let src = r#"
fn main() {
    let g = arith::div;
    let _ = g(1, 1);
}
"#;
        let names = escaped(src);
        assert!(names.contains("div"), "{names:?}");
        assert!(names.contains("arith::div"), "{names:?}");
    }

    #[test]
    fn qualified_call_does_not_escape_callee() {
        let src = r#"
fn main() { let _ = arith::div(1, 1); }
"#;
        let names = escaped(src);
        assert!(!names.contains("div"), "{names:?}");
        assert!(!names.contains("arith::div"), "{names:?}");
    }

    #[test]
    fn method_as_value_escapes() {
        let src = r#"
fn main() {
    let c = new C();
    let g = c.checked;
    let _ = g(1);
}
"#;
        assert!(escaped(src).contains("checked"));
    }

    #[test]
    fn method_call_does_not_escape_method_name() {
        let src = r#"
fn main() {
    let c = new C();
    let _ = c.checked(1);
}
"#;
        assert!(!escaped(src).contains("checked"));
    }

    #[test]
    fn use_alias_poisons_imported_stem() {
        let src = r#"
use arith::{div as d};
fn main() {
    let g = d;
    let _ = g(1, 1);
}
"#;
        let names = escaped(src);
        assert!(names.contains("d"), "{names:?}");
        assert!(names.contains("div"), "{names:?}");
        assert!(names.contains("arith::div"), "{names:?}");
    }

    #[test]
    fn union_across_asts_closes_cross_file_hole() {
        let def = r#"
fn div(int a, int b) -> Result<int, int> {
    return Result::Ok(a / b);
}
"#;
        let take = r#"
fn main() {
    let f = div;
    let _ = f(10, 2);
}
"#;
        let mut names = HashSet::new();
        let def_src = Box::leak(def.to_string().into_boxed_str());
        let take_src = Box::leak(take.to_string().into_boxed_str());
        collect_fn_value_escaped(&Pratt::default().parse(def_src).unwrap(), &mut names);
        collect_fn_value_escaped(&Pratt::default().parse(take_src).unwrap(), &mut names);
        assert!(names.contains("div"));
    }
}
