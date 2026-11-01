// @expected E0413 — a caller pays for the lambda it passes to a higher-order function.
static let N: int = 0;

fn apply(int -> int f, int x) -> int uses {} {
    return f(x);
}

fn bump(int x) -> int {
    N = N + x;
    return N;
}

pure fn run() -> int {
    return apply(fn (int x) => bump(x), 1);
}

fn main() {
    run();
}
