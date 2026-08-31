//! Remove harness-only declarations from the AST when compiling for production.

use parser::ast::{Expression, Output};

/// Drop top-level `test("…") { … }` blocks and `#[test]` functions.
pub fn strip_test_declarations(ast: &mut Output<'_>) {
    let Expression::Program(children) = ast.1.as_mut() else {
        return;
    };
    children.retain(|child| !is_test_top_level_decl(child));
}

fn is_test_top_level_decl(node: &Output<'_>) -> bool {
    match node.1.as_ref() {
        Expression::TestCase { .. } => true,
        Expression::Function { attrs, .. } => attrs.iter().any(|a| a.name == "test"),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use parser::Pratt;

    #[test]
    fn strip_removes_test_blocks_and_attributed_functions() {
        let mut ast = Pratt::default()
            .parse(
                r#"
#[test]
fn hidden() { assert(true)?; }
test("block") { assert(true)?; }
fn main() { }
"#,
            )
            .expect("parse");
        strip_test_declarations(&mut ast);
        let Expression::Program(children) = ast.1.as_ref() else {
            panic!("expected program");
        };
        assert_eq!(children.len(), 1);
        assert!(
            matches!(children[0].1.as_ref(), Expression::Function { name, .. } if *name == "main")
        );
    }

    /// Production stripping must not drop ordinary functions or `attr`
    /// declarations — only harness `#[test]` fns and `test("…")` blocks.
    #[test]
    fn strip_keeps_attr_decls_and_non_test_functions() {
        let mut ast = Pratt::default()
            .parse(
                r#"
attr log<T>(fn(...args) -> T target, string message, ...args) -> T {
    return target(...args);
}
#[log(message = "x")]
fn keep_me() -> int { return 1; }
#[test]
fn drop_me() { assert(true)?; }
fn main() { }
"#,
            )
            .expect("parse");
        strip_test_declarations(&mut ast);
        let Expression::Program(children) = ast.1.as_ref() else {
            panic!("expected program");
        };
        let names: Vec<&str> = children
            .iter()
            .filter_map(|c| match c.1.as_ref() {
                Expression::AttrDecl { name, .. } => Some(*name),
                Expression::Function { name, .. } => Some(*name),
                _ => None,
            })
            .collect();
        assert_eq!(names, ["log", "keep_me", "main"]);
    }

    #[test]
    fn strip_removes_signature_only_test_functions() {
        let mut ast = Pratt::default()
            .parse(
                r#"
#[test]
fn sig_only() { assert(true)?; }
fn main() { }
"#,
            )
            .expect("parse");
        strip_test_declarations(&mut ast);
        let Expression::Program(children) = ast.1.as_ref() else {
            panic!("expected program");
        };
        assert_eq!(children.len(), 1);
        assert!(
            matches!(children[0].1.as_ref(), Expression::Function { name, .. } if *name == "main")
        );
    }
}
