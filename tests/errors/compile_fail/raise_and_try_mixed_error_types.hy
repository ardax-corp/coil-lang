// Expected: E0117 or E0102 — a `raise "s"` conflicts with `?` on a `Result<_, int>` in one function (reported as E0102 today).
fn g() -> Result<int, int> {
    return Result::Ok(1);
}

fn f() {
    let v = g()?;
    raise "s";
}

fn main() {
    let r = f();
}
