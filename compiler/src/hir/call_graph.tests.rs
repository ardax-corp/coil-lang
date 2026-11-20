//! Which functions of a small program sit on a call cycle.

use std::collections::HashSet;

use crate::hir::build_module;
use crate::typechecking::infer::Checker;

fn recursive(src: &str) -> HashSet<String> {
    let owned = Box::leak(src.to_string().into_boxed_str());
    let ast = parser::Pratt::default().parse(owned).expect("parse");
    let mut checker = Checker::new();
    let _ = checker.check_program(&ast);
    assert!(checker.messages().iter().all(|m| *m.kind() != reporting::MessageKind::ERROR), "{:?}", checker.messages());
    let sidecar = checker.typed_sidecar();
    build_module(&checker, &sidecar, "", &ast).recursive_fns(&checker, "")
}

fn names(set: &HashSet<String>) -> Vec<&str> {
    let mut out: Vec<&str> = set.iter().map(String::as_str).collect();
    out.sort_unstable();
    out
}

#[test]
fn a_self_call_is_a_cycle() {
    let rec = recursive(
        "
fn count(int n) -> int { if n == 0 { return 0; } return count(n - 1); }
fn main() { let _ = count(3); }
",
    );
    assert_eq!(names(&rec), ["count"]);
}

#[test]
fn a_mutual_cycle_lists_every_member_and_no_caller() {
    let rec = recursive(
        "
fn ping(int n) -> int { if n == 0 { return 0; } return pong(n - 1); }
fn pong(int n) -> int { return pang(n); }
fn pang(int n) -> int { return ping(n); }
fn leaf(int n) -> int { return n + 1; }
fn enter(int n) -> int { return ping(leaf(n)); }
fn main() { let _ = enter(3); }
",
    );
    assert_eq!(names(&rec), ["pang", "ping", "pong"]);
}

#[test]
fn a_method_is_not_a_node() {
    // `join(self)` in the method names the function, which does not call
    // back: no cycle on the name.
    let rec = recursive(
        "
class T {
    pub x: int,
}
fn join(T t) -> int { return t.x; }
impl T {
    pub fn join() -> int {
        return join(self);
    }
    pub fn down(int n) -> int {
        if n == 0 { return self.x; }
        return self.down(n - 1);
    }
}
fn main() { return; }
",
    );
    assert_eq!(names(&rec), [] as [&str; 0]);
}

#[test]
fn a_call_inside_a_lambda_is_not_the_function_s() {
    let rec = recursive(
        "
fn apply(int -> int f, int x) -> int { return f(x); }
fn twice(int n) -> int { return apply(fn (int k) => twice(k), n); }
fn main() { let _ = apply(fn (int k) => k, 1); }
",
    );
    assert_eq!(names(&rec), [] as [&str; 0]);
}
