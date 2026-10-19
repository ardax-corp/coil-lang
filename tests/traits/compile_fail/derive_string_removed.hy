// Expected: E0119 — the `String` derive was removed; `Show` is the display
// trait (derive `Show`, call `show()`).
#[derive(String)]
enum Color {
    Red,
}

fn main() {}
