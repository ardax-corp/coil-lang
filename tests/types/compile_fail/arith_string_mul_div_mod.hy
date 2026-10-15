// Expected: E0102 — `*`, `/` and `%` take numeric operands (#554).
fn main() {
    let a = "a" * "b";
    let b = "a" / "b";
    let c = "a" % "b";
}
