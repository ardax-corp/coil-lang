// @expected E0100 — loop clauses see the variables around the loop, not a `for` loop's own binding.
fn main() {
    let s = 0;
    for k in 0..3
        invariant k < 3
    {
        s = s + k;
    }
}
