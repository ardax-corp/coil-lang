// @expected E0413 — an object pushed onto a `Vec` is not private, so a
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
    let all: Vec<Counter> = Vec::new();
    all.push(c);
    c.bump();
    return all.len();
}

fn main() {
    let _ = made();
}
