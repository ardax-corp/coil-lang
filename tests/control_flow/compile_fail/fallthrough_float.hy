// Expected: E0111 — an empty `-> float` body needs a `return`.
fn f() -> float {}

fn main() {
    let _ = f();
}
