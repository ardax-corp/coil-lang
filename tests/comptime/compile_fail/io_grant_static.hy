// @flags --comptime-read tests/comptime/data
// @expected E0134 — an IO grant does not let a const touch a `static let`.
static let COUNTER = 1;

fn bump() -> int {
    COUNTER = COUNTER + 1;
    return COUNTER;
}

const A = bump();

fn main() {}
