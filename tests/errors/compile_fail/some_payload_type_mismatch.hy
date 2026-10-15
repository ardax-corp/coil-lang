// Expected: E0102 — the Some payload must have the declared type.
fn f() -> Option<int> {
    return Option::Some("x");
}

fn main() {
    let r = f();
}
