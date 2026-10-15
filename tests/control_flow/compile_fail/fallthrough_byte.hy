// Expected: E0111 — an empty `-> byte` body needs a `return`.
fn b() -> byte {}

fn main() {
    let _ = b();
}
