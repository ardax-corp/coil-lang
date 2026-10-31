// @expected E0413 — a loop `invariant` must have no effects; `bump` writes a static.
static let HITS: int = 0;

fn bump() -> bool {
    HITS = HITS + 1;
    return true;
}

fn main() {
    let i = 0;
    while i < 3
        invariant bump()
    {
        i = i + 1;
    }
}
