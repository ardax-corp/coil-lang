// Expected: E0117 — Option `?` cannot propagate out of a Result function.
fn find() -> Option<int> {
    return Option::None;
}

fn f() -> Result<int, string> {
    let v = find()?;
    return Result::Ok(v);
}

fn main() {
    let r = f();
}
