// @expected E0119 — a derive applies to a class or enum, so
// `#[derive(Eq)]` on a function is an error instead of being ignored (#1202).
#[derive(Eq)]
fn f() -> int {
    return 1;
}

fn main() {}
