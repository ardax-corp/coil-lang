// Expected: E0104 — no compiler-provided `to_string` on an enum either; call
// `show()`.
enum Color {
    Red,
}

fn main() {
    let _ = Color::Red.to_string();
}
