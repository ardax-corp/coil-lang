// @expected E0413 — writing a `static let` is the `mutate` effect.
static let N: int = 3;

pure fn set(int v) {
    N = v;
}

fn main() {
    set(1);
}
