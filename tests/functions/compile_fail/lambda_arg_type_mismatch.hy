// Expected: E0102 — a lambda's parameter types are checked at the call.
fn main() {
    let g = fn (int x) => x + 1;
    let r = g("one");
}
