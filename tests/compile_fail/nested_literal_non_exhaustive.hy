// Expected: compile failure — `Some(200)` does not cover every `Some`
// payload, so the match is non-exhaustive (#595).
fn code(Option<int> o) -> int {
    return match o {
        Option::Some(200) => 1,
        Option::None => 0,
    };
}

fn main() {
    let _ = code(Option::Some(3));
}
