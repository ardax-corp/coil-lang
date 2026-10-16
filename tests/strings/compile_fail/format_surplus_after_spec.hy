// Expected: E0119 — `a=%i` has one specifier, so the second argument is surplus.
fn main() {
    let s = string::format("a=%i", 1, 2);
}
