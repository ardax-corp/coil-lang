// Expected: E0103 — a function returning itself would need an infinite type.
fn bad() {
    return bad;
}

fn main() {}
