// Expected: E0209 — `Color::Blue` is not covered.
enum Color {
    Red,
    Green,
    Blue,
}

fn f(Color c) -> int {
    return match c {
        Color::Red => 1,
        Color::Green => 2,
    };
}

fn main() {
    let r = f(Color::Red);
}
