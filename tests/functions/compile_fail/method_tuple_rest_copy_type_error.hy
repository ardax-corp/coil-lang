// @expected E0102 — the copy of method `add_first` for a `(string)` pack adds
// a string to an int; the `(int)` copy alone would compile (coil-lang#1754).
class H {
    pub n: int,
}

impl H {
    pub fn add_first(... xs) -> int {
        return xs[0] + self.n;
    }
}

fn main() {
    let h = new H(1);
    let a = h.add_first(1);
    let b = h.add_first("s");
}
