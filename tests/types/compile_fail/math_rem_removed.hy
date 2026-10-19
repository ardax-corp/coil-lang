// Expected: E0101 — `rem` is no longer in the prelude; write `x % y`.
fn main() {
    let _ = rem(7.5, 2.0);
}
