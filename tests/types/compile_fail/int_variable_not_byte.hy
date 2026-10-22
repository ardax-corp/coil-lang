// @expected E0102 — only literals coerce to `byte`; an unannotated `int`
// variable needs `let x: byte` or a cast.
fn f() -> byte {
    let x = 42;
    return x;
}

fn main() {
    f();
}
