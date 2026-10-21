// Expected: E0413 — a method that returns its receiver hands out a mutable
// alias, so the object is not private and mutating it is `mutate` (#1648).
class Counter {
    pub n: int,
}

impl Counter {
    pub fn me() -> Counter {
        return self;
    }

    pub fn bump() {
        self.n = self.n + 1;
    }
}

pure fn made() -> int {
    let c = new Counter(0);
    let d = c.me();
    c.bump();
    return d.n;
}

fn main() {
    let _ = made();
}
