// Expected: E0209 — `Option::Some(Color::Green)` is not covered.
enum Color {
    Red,
    Green,
}

fn f(Option<Color> c) -> int {
    return match c {
        Option::Some(Color::Red) => 1,
        Option::None => 0,
    };
}

fn main() {
    let r = f(Option::None);
}
