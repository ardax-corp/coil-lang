// @expected E0413 — an object captured by a lambda is not private, so a
// method that mutates it is `mutate` (#1648).
class Counter {
    pub n: int,
}

impl Counter {
    pub fn bump() {
        self.n = self.n + 1;
    }
}

pure fn made() -> int {
    let c = new Counter(0);
    c.bump();
    let get = fn () use (c) => c.n;
    return get();
}

fn main() {
    let _ = made();
}
