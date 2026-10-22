// @expected E0107 — a mutable object held by a readonly class is read-only through it (#1558).
class Counter {
    pub n: int,
}

readonly class Holder {
    pub c: Counter,
}

fn bump(Counter c) {
    c.n = c.n + 1;
}

fn main() {
    let h = new Holder(new Counter(0));
    bump(h.c);
}
