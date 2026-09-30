// Expected: compile failure — no `macro nope` is in scope.
fn main() {
    let x = nope!(1);
}
