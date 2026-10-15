// Expected: E0413 — reading a `static let` is the `read` effect, so a `pure fn` cannot.
static let N: int = 3;

pure fn get() -> int {
    return N;
}

fn main() {
    get();
}
