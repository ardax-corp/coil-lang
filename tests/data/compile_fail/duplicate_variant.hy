// Expected: E0201 — one enum may declare a variant name once.
enum A {
    Foo,
    Foo,
}

fn main() {}
