// @expected E0413 — `uses {}` means the same as `pure`.
static let N: int = 3;

fn get() -> int uses {} {
    return N;
}

fn main() {
    get();
}
