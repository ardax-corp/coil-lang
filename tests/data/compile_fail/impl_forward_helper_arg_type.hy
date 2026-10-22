// @expected E0102 — the later helper takes an `int`; the method passes a string.
class Foo {
    pub v: int,
}

impl Foo {
    pub fn bump() -> int {
        return helper("x");
    }
}

fn helper(int n) -> int {
    return n + 1;
}

fn main() {
    let x = new Foo(1).bump();
}
