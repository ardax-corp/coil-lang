// Expected: E0102 — in result mode `return v` becomes `Ok(v)`, so `v` must be the success type.
fn f(int n) -> Result<int, string> {
    if n > 0 {
        raise "e";
    }
    return "s";
}

fn main() {
    let r = f(1);
}
