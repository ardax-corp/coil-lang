// @expected E0119 — unknown attribute names are rejected at compile time.
#[no_such_attribute]
fn f() -> int {
    return 1;
}

fn main() {
    f();
}
