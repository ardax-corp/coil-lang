// @expected E0101 — the method calls a function no module declares.
class Foo {
    pub v: int,
}

impl Foo {
    pub fn bump() -> int {
        return missing(self.v);
    }
}

fn main() {
    let x = new Foo(1).bump();
}
