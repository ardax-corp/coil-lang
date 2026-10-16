// Expected: E0001 — patterns take no bool literals (`true` would bind a name).
fn f(bool b) -> int {
    return match b {
        true => 1,
        false => 0,
    };
}

fn main() {
    let r = f(true);
}
