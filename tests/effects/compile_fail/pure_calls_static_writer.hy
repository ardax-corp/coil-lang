// @expected E0413 — `twice` is declared `pure` but calls `bump`, which
// writes a static.
static let HITS: int = 0;

fn bump() {
    HITS = HITS + 1;
}

pure fn twice(int x) -> int {
    bump();
    return x * 2;
}
