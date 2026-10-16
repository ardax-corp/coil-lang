// Expected: E0102 — `Option` patterns cannot match a `Result` scrutinee.
fn get() -> Result<string, int> {
    return Result::Ok("x");
}

fn main() {
    let n = match get() {
        Option::Some(_) => 1,
        Option::None => 0,
    };
}
