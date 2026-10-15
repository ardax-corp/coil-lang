// Expected: E0210 — `Result::Ok(Option::None)` is already matched by the arm before it.
fn f(Result<Option<int>, string> r) -> int {
    return match r {
        Result::Ok(Option::None) => 0,
        Result::Ok(Option::None) => 1,
        Result::Ok(Option::Some(v)) => v,
        Result::Err(_) => -1,
    };
}

fn main() {
    let r = f(Result::Err("x"));
}
