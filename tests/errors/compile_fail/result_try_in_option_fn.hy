// @expected E0117 — Result `?` cannot propagate out of an Option function.
fn parse() -> Result<int, string> {
    return Result::Ok(1);
}

fn f() -> Option<int> {
    let v = parse()?;
    return Option::Some(v);
}

fn main() {
    let r = f();
}
