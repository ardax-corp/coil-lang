// Expected: E0207 — a record pattern may name only the variant's declared fields.
enum Pt {
    Pt { x: int, y: int },
}

fn f(Pt p) -> int {
    return match p {
        Pt::Pt { x, y, z } => x,
    };
}

fn main() {
}
