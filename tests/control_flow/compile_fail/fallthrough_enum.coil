// @expected E0111 — an empty body returning a user enum needs a `return`.
enum Color {
    Red,
    Blue,
}

fn bad_adt() -> Color {}

fn main() {
    let _ = bad_adt();
}
