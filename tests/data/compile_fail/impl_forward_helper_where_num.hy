// Expected: E0119 — the later helper `twice` has `T: Num`; `string` has no
// `Num` instance.
class Foo {
    pub v: string,
}

impl Foo {
    pub fn bump() -> string {
        return twice(self.v);
    }
}

fn twice<T: Num>(T n) -> T {
    return n;
}

fn main() {
    let x = new Foo("x").bump();
}
