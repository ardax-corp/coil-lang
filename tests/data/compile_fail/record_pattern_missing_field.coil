// @expected E0206 — a record pattern binds every declared field.
enum Pt {
    Pt { x: int, y: int },
}

fn f(Pt p) -> int {
    return match p {
        Pt::Pt { x } => x,
    };
}

fn main() {}
