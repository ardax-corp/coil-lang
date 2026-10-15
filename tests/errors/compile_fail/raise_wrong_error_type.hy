// Expected: E0102 — `raise 5` does not fit the declared `Result<int, string>`.
fn f() -> Result<int, string> {
    raise 5;
}

fn main() {
    let r = f();
}
