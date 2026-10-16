// Expected: E0210 — `Result::Ok(Option::Some(_))` is already matched by the
// binding arm `Result::Ok(Option::Some(v))` before it.
fn unwrap(Result r) -> int {
    return match r {
        Result::Ok(Option::Some(v)) => v,
        Result::Ok(Option::Some(_)) => 0,
        Result::Ok(Option::None) => -1,
        Result::Err(_) => -2,
    };
}

fn main() {
    let _ = unwrap(Result::Ok(Option::Some(1)));
}
