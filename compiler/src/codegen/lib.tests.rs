
use super::*;
use parser::Pratt;

#[test]
fn codegen_depth_guard_reports_the_limit_without_unwinding() {
    // See the analogous typechecker test (infer_depth_guard_...) for why
    // this seeds the counter directly rather than compiling a literally
    // deep AST: dropping a deep Box<Expression> chain overflows the
    // stack on its own, independent of do_compile's frame size.
    let ast = Pratt::default()
        .parse("1;")
        .expect("trivial literal parses");
    let mut compiler = Compiler {
        codegen_depth: super::CODEGEN_RECURSION_LIMIT,
        ..Default::default()
    };
    compiler.do_compile(&ast);
    assert!(compiler.codegen_too_deep, "the walk stops at the limit");
    assert!(
        compiler
            .messages
            .iter()
            .any(|m| m.code() == Some(ErrorCode::ExpressionNestingTooDeep)),
        "expected an ExpressionNestingTooDeep diagnostic"
    );
}

/// The HM checker should record diagnostic messages on type errors;
/// `compile` should drain them into the compiler's message list.
#[test]
fn type_errors_appear_in_messages() {
    let mut ast = Pratt::default().parse("x;").expect("parse failed");
    let mut c = Compiler::default();
    c.compile("test", &mut ast);
    assert!(
        !c.messages.is_empty(),
        "expected at least one error message for unknown identifier"
    );
}

/// `register_native` adds the native to the HM checker. A subsequent
/// call to the native type-checks cleanly.
#[test]
fn register_native_visible_to_emitter() {
    use crate::typechecking::ty::{string, unit};
    let mut c = Compiler::default();
    c.register("native_print", &[string()], &unit());
    // Native calls registered with the checker should compile without errors.
    let mut ast = Pratt::default()
        .parse("fn main() { native_print(\"hi\"); }")
        .expect("parse failed");
    c.compile("test", &mut ast);
    let msgs = std::mem::take(&mut c.messages);
    assert!(msgs.is_empty(), "expected no messages, got: {:?}", msgs);
}

#[test]
fn check_program_impl_calls_later_helper_via_compiler_checker() {
    let src = r#"
class Foo { pub v: int, }
impl Foo {
    pub fn bump() -> int { return helper(self.v); }
}
fn helper(int n) -> int { return n + 1; }
fn main() {
    let f = new Foo(41);
    if f.bump() != 42 { panic "bump"; }
}
"#;
    let ast = Pratt::default().parse(src).expect("parse");
    let mut compiler = Compiler::default();
    let _ = compiler.checker.check_program(&ast);
    assert!(
        compiler.checker.messages().is_empty(),
        "unexpected: {:?}",
        compiler
            .checker
            .messages()
            .iter()
            .map(|m| m.message())
            .collect::<Vec<_>>()
    );
}

#[test]
fn compiler_compile_impl_calls_later_helper() {
    let src = r#"
class Foo { pub v: int, }
impl Foo {
    pub fn bump() -> int { return helper(self.v); }
}
fn helper(int n) -> int { return n + 1; }
fn main() {
    let f = new Foo(41);
    if f.bump() != 42 { panic "bump"; }
}
"#;
    let mut ast = Pratt::default().parse(src).expect("parse");
    let mut compiler = Compiler::default();
    compiler.compile("", &mut ast);
    assert!(
        compiler.get_messages().is_empty(),
        "unexpected: {:?}",
        compiler
            .get_messages()
            .iter()
            .map(|m| m.message())
            .collect::<Vec<_>>()
    );
}

// sum types and pattern matching codegen

// Peephole fusion

#[test]
fn break_and_continue_outside_loop_emit_diagnostics() {
    let mut ast = Pratt::default()
        .parse("fn main() { break; continue; }")
        .expect("parse failed");
    let mut compiler = Compiler::default();
    compiler.compile("", &mut ast);
    let rendered = compiler
        .get_messages()
        .iter()
        .map(|m| format!("{:?}", m))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        compiler
            .get_messages()
            .iter()
            .any(|m| m.code() == Some(ErrorCode::CodegenError)
                && m.message().contains("break outside of loop")),
        "expected CodegenError for break, got {rendered}"
    );
    assert!(
        compiler
            .get_messages()
            .iter()
            .any(|m| m.code() == Some(ErrorCode::CodegenError)
                && m.message().contains("continue outside of loop")),
        "expected CodegenError for continue, got {rendered}"
    );
}

// field-access codegen tests
//
// The spec locked in 2 codegen tests for the new
// `Expression::Access` arm. Both verify the code SHAPE
// (MakeEnum → LoadField) and the operand (field_index) so the
// runtime extraction reads the right slot.

#[test]
fn cross_module_does_not_inline_private_method() {
    let util = "class Box { pub n: int, }\n\
             impl Box {\n\
               fn secret() -> int { return 2; }\n\
               pub fn shown() -> int { return 1; }\n\
             }";
    let mut ast = Pratt::default().parse(util).expect("parse util");
    let mut compiler = Compiler::default();
    compiler.compile_module("util", &mut ast);
    let secret = compiler
        .checker()
        .inherent_method_visibility("util::Box::secret")
        .or_else(|| compiler.checker().inherent_method_visibility("Box::secret"));
    let shown = compiler
        .checker()
        .inherent_method_visibility("util::Box::shown")
        .or_else(|| compiler.checker().inherent_method_visibility("Box::shown"));
    assert_eq!(secret, Some(parser::ast::Visibility::Private));
    assert_eq!(shown, Some(parser::ast::Visibility::Public));
}

// growing array builtin codegen tests

// Dims over the `u8` packed ceiling fall back to scalar unroll.

#[test]
fn unescape_coil_string_supports_hex_and_unicode() {
    assert_eq!(unescape_coil_string(r"\x41"), "A");
    assert_eq!(unescape_coil_string(r"\u{42}"), "B");
    assert_eq!(unescape_coil_string("\\\""), "\"");
    assert_eq!(unescape_coil_string(r"\e"), "\x1b");
}

/// A generic class's method clones are kept with their calls resolved
/// and their types qualified (cross-module inlining): a body that only
/// calls, reads fields and indexes is kept; one holding a lambda is not.
#[test]
fn method_clones_keep_resolved_bodies() {
    use crate::hir::HirKind;
    let src = r#"
class Bag<T> {
    pub items: Vec<T>,
}

impl Bag<T> {
    pub fn add(T x) {
        self.items.push(x);
    }
    pub fn at_or(int i, T d) -> T {
        if i < self.items.len() {
            return self.items[i];
        }
        return d;
    }
    pub fn count_if(T -> bool keep) -> int {
        let n = 0;
        for x in self.items {
            if keep(x) {
                n = n + 1;
            }
        }
        return n;
    }
}

fn main() {
    let b: Bag<int> = new Bag(Vec::new());
    b.add(3);
    b.add(4);
    if b.at_or(1, 0) != 4 || b.at_or(9, -1) != -1 {
        panic "at_or";
    }
    if b.count_if(fn (int x) => x > 3) != 1 {
        panic "count_if";
    }
}
"#;
    let mut pipeline = crate::Pipeline::new();
    // Same-module calls would splice the generic bodies and leave no clone.
    pipeline.set_inline_max_cost(0);
    if pipeline.compile_src(src).is_err() {
        panic!("compile failed: {:?}", pipeline.messages().iter().map(|m| m.message()).collect::<Vec<_>>());
    }
    let kept = &pipeline.compiler().hir_resolved_clones;
    let key = |m: &str| kept.keys().find(|k| k.starts_with(&format!("Bag::{m}$"))).cloned();
    let add = key("add").unwrap_or_else(|| panic!("Bag::add not kept: {:?}", kept.keys().collect::<Vec<_>>()));
    let at_or = key("at_or").expect("Bag::at_or kept");
    let resolved = |k: &str| kept[k].exprs.iter().filter(|e| matches!(e.kind, HirKind::Call { callee: crate::hir::Callee::Resolved(_), .. })).count();
    assert!(resolved(&add) >= 1, "Bag::add keeps its push call resolved");
    for k in [&add, &at_or] {
        let b = &kept[k];
        assert!(b.exprs.iter().all(|e| e.node.is_none()), "{k} keeps node ids");
        assert!(
            b.exprs.iter().all(|e| !matches!(e.kind, HirKind::Call { callee: crate::hir::Callee::Named { .. } | crate::hir::Callee::Method { .. }, .. })
                || matches!(&e.kind, HirKind::Call { callee: crate::hir::Callee::Method { name }, .. } if name == "len")),
            "{k} keeps an unresolved call"
        );
    }
    assert!(key("count_if").is_none(), "a body calling a function value is not kept");
}
