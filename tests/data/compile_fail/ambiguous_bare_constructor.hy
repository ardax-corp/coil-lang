// Expected: E0201 — a bare `Foo` is ambiguous when two enums declare it;
// write `A::Foo` or `B::Foo`.
enum A {
    Foo,
}

enum B {
    Foo,
}

fn main() {
    let x = Foo;
}
