// Expected: E0119 — the later helper `twice` has `where Num<T>`; `string` has no
// `Num` instance.
class Foo {
    pub v: string,
}

impl Foo {
    pub fn bump() -> string {
        return twice(self.v);
    }
}

fn twice<T>(T n) -> T where Num<T> {
    return n;
}

fn main() {
    let x = new Foo("x").bump();
}
