// Expected: E0111 — an empty `-> bool` body needs a `return`.
fn flag() -> bool {}

fn main() {
    let _ = flag();
}
