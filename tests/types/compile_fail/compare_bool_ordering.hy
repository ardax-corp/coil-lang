// Expected: E0102 — `<` on `bool` (no `Lt` instance) (#554).
fn main() {
    let a = true < false;
}
