// @expected E0100 — a `comptime` expression cannot use a local variable.
fn sq(int x) -> int {
    return x * x;
}

fn main() {
    let n = 3;
    let x = comptime sq(n);
}
