// @expected E0102 — the Ok payload must have the declared success type.
fn f() -> Result<int, string> {
    return Result::Ok("x");
}

fn main() {
    let r = f();
}
