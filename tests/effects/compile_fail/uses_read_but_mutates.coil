// @expected E0413 — `uses {read}` does not cover writing a static (`mutate`).
static let N: int = 3;

fn bump() -> int uses {read} {
    N = N + 1;
    return N;
}

fn main() {
    bump();
}
