// @expected E0119 — `with_describe` takes a `TypeDecl`, so it applies to a
// class or enum, not a function.
use derive_macros::with_describe;

#[with_describe(prefix = "p")]
fn plain() -> int {
    return 1;
}

fn main() {}
