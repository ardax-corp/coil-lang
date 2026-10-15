// Expected: E0102 — a class `invariant` must be a `bool`.
class C invariant self.n {
    pub n: int,
}

fn main() {
    let c = new C(1);
}
