// Expected: compile failure — free generic Option return (E0127).
fn some_of<T>(T x) -> Option<T> {
    return Option::Some(x);
}

fn main() {
    let _ = some_of(7);
}
