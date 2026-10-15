// Expected: E0200 — duplicate enum name.
enum Foo {
    A,
}

enum Foo {
    B,
}

fn main() {}
