// @expected E0119 — the later helper `add1` needs `T: Num`; `string` has no
// `Num` instance.
class Foo {
    pub v: string,
}

impl Foo {
    pub fn bump() -> string {
        return add1(self.v);
    }
}

fn add1<T: Num>(T n) -> T {
    return n;
}

fn main() {
    let x = new Foo("x").bump();
}
