// @expected E0107 — a write through what a getter returns from `self` mutates `self` (#1558).
class Inner {
    pub n: int,
}

class Outer {
    pub inner: Inner,
}

impl Outer {
    pub fn get_inner() -> Inner {
        return self.inner;
    }

    pub fn reset() {
        let i = self.get_inner();
        i.n = 0;
    }
}

fn main() {
    let o = readonly new Outer(new Inner(1));
    o.reset();
}
