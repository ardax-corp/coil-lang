// Expected: E0107 — a `readonly` array cannot be passed to a plain `[int; 3]` parameter (#1552).
fn z([int; 3] v) {
    v[0] = 42;
}

fn main() {
    let xs = readonly [1, 2, 3];
    z(xs);
}
