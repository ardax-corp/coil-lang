// @expected E0107 — `twice` mutates `self` through `bump`, so it cannot be called on a `readonly` value (#1558).
class Counter {
    pub n: int,
}

impl Counter {
    pub fn bump() {
        self.n = self.n + 1;
    }

    pub fn twice() {
        self.bump();
        self.bump();
    }
}

fn main() {
    let c = readonly new Counter(1);
    c.twice();
}
