// Expected: E0205 — a unit variant takes no payload.
enum Color {
    Red,
    Blue,
}

fn main() {
    let c = Color::Red(1);
}
