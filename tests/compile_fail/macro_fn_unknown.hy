// Expected: E0119 — no `macro nope` is in scope.
fn main() {
    let x = nope!(1);
}
