// @expected E0210 — a binding arm matches everything, so later arms are unreachable.
fn f(int n) -> int {
    return match n {
        x => x,
        1 => 2,
    };
}

fn main() {
    let r = f(1);
}
