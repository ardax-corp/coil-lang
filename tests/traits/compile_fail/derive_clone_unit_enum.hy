// @expected E0119 — `Clone` is not derivable, not even on a plain unit enum.
#[derive(Clone)]
enum Color {
    Red,
}

fn main() {}
