// @expected E0208 — a `static` field may not reuse an instance field's name.
class C {
    pub x: int,
    pub static x: int = 0,
}

fn main() {
    let c = new C(1);
}
