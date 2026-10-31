// @expected E0134 — a `comptime` expression may not touch a `static let`.
static let COUNTER = 1;

fn bump() -> int {
    COUNTER = COUNTER + 1;
    return COUNTER;
}

fn main() {
    let x = comptime bump();
}
