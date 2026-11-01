// @expected E0001 — a whole-arm tuple pattern is not a match pattern; use `let (a, b) = …`.
fn f(int a, int b) -> int {
    return match (a, b) {
        (0, y) => y,
        default => 0,
    };
}

fn main() {
    let r = f(0, 1);
}
