// @expected E0102 — a returned literal coerces to `byte` only in 0..=255.
fn f() -> byte {
    return 300;
}

fn main() {}
