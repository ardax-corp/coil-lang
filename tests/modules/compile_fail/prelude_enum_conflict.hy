// @expected E0200 — `Option` is the prelude enum and cannot be redeclared.
enum Option {
    Yes,
    No,
}

fn main() {}
