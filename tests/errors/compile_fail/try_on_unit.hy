// Expected: E0114 — a unit call result cannot be unwrapped with `?`.
fn touch() {}

fn f() -> Option<int> {
    let v = touch()?;
    return Option::Some(1);
}

fn main() {
    let r = f();
}
