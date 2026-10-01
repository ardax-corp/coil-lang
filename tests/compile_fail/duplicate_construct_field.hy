// Expected: E0208 — duplicate field in record constructor (parse E0208).
enum E {
    Foo { x: int, y: int },
}

fn main() {
    E::Foo { x: 1, x: 2 };
}
