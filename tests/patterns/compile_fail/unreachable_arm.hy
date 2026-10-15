// Expected: E0210 — unreachable match arm.
enum Color {
    Red,
    Blue,
}

fn main() {
    match Color::Red {
        Color::Red => 1,
        Color::Blue => 2,
        Color::Red => 3,
    };
}
