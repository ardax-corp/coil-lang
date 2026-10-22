// @expected E0119 — `C::x` names an instance field, read on a value (`c.x`), not through the class (#1726).
class C {
    pub const x: int = 1,
}

fn main() {
    let v = C::x;
}
