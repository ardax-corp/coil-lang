// Expected: E0209 — literal arms cannot cover every `int`.
fn f(int n) -> int {
    return match n {
        1 => 10,
        2 => 20,
    };
}

fn main() {
    let _ = f(1);
}
