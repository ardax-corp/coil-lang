// @expected E0413 — a `requires` clause must have no effects; `bump` writes a static.
static let HITS: int = 0;

fn bump() -> bool {
    HITS = HITS + 1;
    return true;
}

fn f(int x) -> int
    requires bump()
{
    return x;
}

fn main() {
    f(1);
}
