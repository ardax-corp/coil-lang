// @expected E0209 — `Result::Ok(Option::None)` is not covered.
fn f(Result<Option<int>, string> r) -> int {
    return match r {
        Result::Ok(Option::Some(v)) => v,
        Result::Err(_) => 0,
    };
}

fn main() {
    let r = f(Result::Err("x"));
}
