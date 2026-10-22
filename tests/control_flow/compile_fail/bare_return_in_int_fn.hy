// @expected E0102 — `return;` returns unit, not the declared `int`.
fn f() -> int {
    return;
}

fn main() {
    f();
}
