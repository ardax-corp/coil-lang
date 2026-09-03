//! Conservative "does this name ever appear as a bare function value"
//! sidecar fact for the two-slot CALL/RETURN classifier.
//!
//! `CallIndirect` / `MakeFn` / `MakePolyFn` / FFI callback / coroutine
//! targets keep the one-word boxed ABI (task cut) — a two-word direct
//! CALL/RETURN function must not have its address taken and pointed at
//! directly. Rather than synthesizing a unary wrapper at every such site,
//! this walk fails closed: any bare identifier that is not the direct
//! callee of a `Call` poisons that name, so the classifier can simply
//! refuse to widen it. Scoped to the current compile unit (per-file, same
//! as [`super::local_escape`]) — a name only used as a value in a
//! different file is a known gap, not a soundness hole within one file.

use parser::ast::{EnumConstructPayload, Expression, Output};

use super::infer::Checker;

/// Fill [`Checker::fn_value_escaped`] for `ast` after inference.
pub fn analyze_fn_value_escape(checker: &mut Checker, ast: &Output<'_>) {
    checker.fn_value_escaped.clear();
    walk(checker, ast, true);
}

/// `in_call_name` is true exactly for the `name` position of a `Call` — the
/// one context that does not escape.
fn walk(checker: &mut Checker, ast: &Output<'_>, in_call_name: bool) {
    match ast.1.as_ref() {
        Expression::Identifier(n) if !in_call_name => {
            checker.fn_value_escaped.insert((*n).to_string());
        }
        Expression::Call { name, args } => {
            walk(checker, name, true);
            if let Some(args) = args {
                for a in args {
                    walk(checker, a, false);
                }
            }
        }
        Expression::Access(recv, _) | Expression::OptionalAccess(recv, _) => {
            walk(checker, recv, false);
        }
        Expression::Construct { fields, .. } => match fields {
            EnumConstructPayload::Unit => {}
            EnumConstructPayload::Tuple(args) => {
                for a in args {
                    walk(checker, a, false);
                }
            }
            EnumConstructPayload::Record(parts) => {
                for p in parts {
                    walk(checker, &p.value, false);
                }
            }
        },
        _ => walk_children(ast, &mut |child| walk(checker, child, false)),
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
        | Expression::If(items) => {
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
        | Expression::Spread(inner) => f(inner),
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
        Expression::Function { body, .. } => {
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
}
