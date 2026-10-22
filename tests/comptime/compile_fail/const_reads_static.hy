// @expected E0134 — a const initializer may not touch a `static let`.
static let COUNTER = 1;

fn bump() -> int {
    COUNTER = COUNTER + 1;
    return COUNTER;
}

const A = bump();

fn main() {}
