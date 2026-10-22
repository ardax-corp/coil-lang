// @expected E0102 — an `Option` arm after a `Result` arm does not match the `Result` scrutinee.
fn get() -> Result<int, int> {
    return Result::Ok(1);
}

fn main() {
    let n = match get() {
        Result::Ok(v) => v,
        Option::None => 0,
    };
}
