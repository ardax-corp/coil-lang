// Expected: E0111 — an empty `-> Option<int>` body needs a `return`.
fn opt() -> Option<int> {}

fn main() {
    let _ = opt();
}
