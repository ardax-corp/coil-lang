// Expected: E0102 — every value packed into `int... xs` must be an `int`.
fn count(int... xs) -> int {
    return len(xs);
}

fn main() {
    let r = count(1, "two", 3);
}
