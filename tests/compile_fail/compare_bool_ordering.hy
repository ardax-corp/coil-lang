// Expected: compile failure — `<` on `bool` (no `Lt` instance) (#554).
fn main() {
    let a = true < false;
}
