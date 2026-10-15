// Expected: E0207 — a `let` record pattern can name only fields the value has.
fn main() {
    let d = { x: 1, y: 2 };
    let { x, z } = d;
}
