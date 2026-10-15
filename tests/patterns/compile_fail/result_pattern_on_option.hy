// Expected: E0102 — `Result::Ok` is not a constructor of `Option`.
fn f(Option<int> o) -> int {
    return match o {
        Result::Ok(v) => v,
        default => 0,
    };
}

fn main() {
    let r = f(Option::None);
}
