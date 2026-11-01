// @expected E0107 — enum record fields are immutable; build a new variant instead.
enum Pt {
    Pt { x: int, y: int },
}

fn main() {
    let p = Pt::Pt { x: 1, y: 2 };
    p.x = 5;
}
