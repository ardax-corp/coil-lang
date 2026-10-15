// Expected: E0119 — `for` over a dict needs every value to share one type.
fn main() {
    for (k, v) in { a: 1, b: "x" } {
    }
}
