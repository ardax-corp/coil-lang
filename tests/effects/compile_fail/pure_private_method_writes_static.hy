// Expected: E0413 — a method that also writes a static is `mutate` even on
// a private receiver (#1648).
static let BUMPS: int = 0;

class Counter {
    pub n: int,
}

impl Counter {
    pub fn bump() {
        self.n = self.n + 1;
        BUMPS = BUMPS + 1;
    }
}

pure fn made() -> int {
    let c = new Counter(0);
    c.bump();
    return c.n;
}

fn main() {
    let _ = made();
}
