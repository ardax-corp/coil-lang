// @expected E0413 — an object returned mutable after a call that mutates it
// is not kept private: the method call is `mutate` (#1648).
class Counter {
    pub n: int,
}

impl Counter {
    pub fn bump() {
        self.n = self.n + 1;
    }
}

pure fn made() -> Counter {
    let c = new Counter(0);
    c.bump();
    return c;
}

fn main() {
    let _ = made();
}
