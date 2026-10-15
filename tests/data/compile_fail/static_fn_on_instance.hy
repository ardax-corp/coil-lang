// Expected: E0119 — a `static fn` is called as `C::make()`, not on an instance.
class C {
    pub v: int,
}

impl C {
    pub static fn make() -> C {
        return new C(1);
    }
}

fn main() {
    let c = new C(2);
    let d = c.make();
}
