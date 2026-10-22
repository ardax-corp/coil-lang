// @expected E0001 — `uses {…}` names effects from the fixed list; `disk` is not one.
fn f(int x) -> int uses {disk} {
    return x;
}

fn main() {
    f(1);
}
