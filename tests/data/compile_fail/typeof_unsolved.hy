// Expected: E0119 — `typeof` needs a ground type; a bare `Option::None` has none.
fn main() {
    let s = typeof Option::None;
}
