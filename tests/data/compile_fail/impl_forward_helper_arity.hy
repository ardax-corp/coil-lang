// @expected E0119 — the later helper takes one argument; the method passes two.
class Foo {
    pub v: int,
}

impl Foo {
    pub fn bump() -> int {
        return helper(self.v, 2);
    }
}

fn helper(int n) -> int {
    return n + 1;
}

fn main() {
    let x = new Foo(1).bump();
}
