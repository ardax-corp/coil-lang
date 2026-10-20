// Expected: E0107 — the loop variable over a `readonly` array is `readonly` (#1552).
class Inner {
    pub n: int,
}

fn main() {
    let xs = readonly [new Inner(1)];
    for x in xs {
        x.n = 9;
    }
}
