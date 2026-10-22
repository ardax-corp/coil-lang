// @expected E0119 — `bool` has no ordering, so a `bool`-backed enum cannot
// derive `Ord`; the error is at the derive, not in its output (#1765).
#[repr(bool)]
#[derive(Ord)]
enum E {
    A = false,
    B = true,
}

fn main() {}
