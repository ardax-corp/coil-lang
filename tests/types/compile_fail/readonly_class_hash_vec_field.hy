// Expected: E0119 — a `Vec` field has no `Hash`, so this readonly class implies only `Eq` (#1558).
readonly class Path {
    pub points: Vec<int>,
}

fn main() {
    let p = new Path(Vec::new());
    let h = p.hash();
}
