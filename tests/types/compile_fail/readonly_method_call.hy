// @expected E0107 — `c.bump()` mutates `self`, so it cannot be called through a `readonly` value (#1558).
class Counter {
    pub n: int,
}

impl Counter {
    pub fn bump() -> int {
        self.n = self.n + 1;
        return self.n;
    }
}

fn main() {
    let c = readonly new Counter(0);
    let n = c.bump();
}
