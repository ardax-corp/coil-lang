// @expected E0117 — only a test body takes `?` on any `Result` or `Option`; a
// helper returning `Result` cannot `?` an `Option`.
fn first(Vec<int> xs) -> Option<int> {
    if xs.len() == 0 {
        return Option::None;
    }
    return Option::Some(xs[0]);
}

fn helper() -> Result<int, string> {
    let v = first(Vec::new())?;
    return v;
}

test("helper keeps its own mode") {
    let _ = helper();
}
