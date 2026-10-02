// Expected: E0210 — `Some(_)` already covers `Some(200)` (#595).
fn code(Option<int> o) -> int {
    return match o {
        Option::Some(_) => 1,
        Option::Some(200) => 2,
        Option::None => 0,
    };
}

fn main() {
    let _ = code(Option::Some(3));
}
