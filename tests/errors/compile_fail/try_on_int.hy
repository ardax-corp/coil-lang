// @expected E0114 — `?` needs an Option or a Result operand.
fn f() -> Option<int> {
    let n = 3;
    let v = n?;
    return Option::Some(v);
}

fn main() {
    let r = f();
}
