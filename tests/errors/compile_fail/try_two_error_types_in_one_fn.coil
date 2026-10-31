// @expected E0102 — `?` on a call that raises `string` and on one that raises
// `int` would give `c` two error types.
fn a() {
    raise "s";
    return 1;
}

fn b() {
    raise 1;
    return 2;
}

fn c() {
    let _x = a()?;
    let _y = b()?;
    return 0;
}

fn main() {}
