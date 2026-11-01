// @expected E0201 — bare `Foo(x)` names a case of both `A` and `B`, and the
// `int` scrutinee picks neither; write `A::Foo` or `B::Foo`.
enum A {
    Foo(int),
}

enum B {
    Foo(int),
}

fn f(int n) -> int {
    return match n {
        Foo(x) => x,
        default => 0,
    };
}

fn main() {
    let r = f(1);
}
