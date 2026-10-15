// Expected: E0119 — constructors are `Enum::Variant`; `Color.Red` is a field access.
enum Color {
    Red,
    Blue,
}

fn main() {
    let c = Color.Red;
}
