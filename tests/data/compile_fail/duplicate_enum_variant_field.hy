// Expected: E0208 — duplicate field in enum variant decl (parse E0208).
enum E {
    Foo { x: int, x: int },
}

fn main() {}
