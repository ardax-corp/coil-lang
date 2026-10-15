// Expected: E0117 or E0102 — `?` on a `Result<_, int>` cannot propagate into a `Result<_, string>` function (reported as E0102 today).
fn g() -> Result<int, int> {
    return Result::Ok(1);
}

fn f() -> Result<int, string> {
    let v = g()?;
    return v;
}

fn main() {
    let r = f();
}
