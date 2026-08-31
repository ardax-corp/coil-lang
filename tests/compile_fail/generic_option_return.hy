// Expected: E0127 — free generic `Option<T>` return is still a codegen hole.
fn some_of<T>(T x) -> Option<T> {
    return Option::Some(x);
}

fn main() {
    let _ = some_of(7);
}
