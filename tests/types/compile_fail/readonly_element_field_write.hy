// Expected: E0107 — an element of a `readonly` array is `readonly` too (#1552).
class Inner {
    pub n: int,
}

fn main() {
    let xs = readonly [new Inner(1)];
    xs[0].n = 7;
}
