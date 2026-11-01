// @expected E0413 — calling a local function value whose body has an effect.
static let N: int = 0;

fn bump(int x) -> int {
    N = N + x;
    return N;
}

pure fn run() -> int {
    let f = fn (int x) => bump(x);
    return f(1);
}

fn main() {
    run();
}
