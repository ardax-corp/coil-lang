// Expected: E0102 — a function without `->` returns unit.
fn touch() {
}

fn main() {
    let r: int = touch();
}
