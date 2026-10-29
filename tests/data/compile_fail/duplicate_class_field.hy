// @expected E0208 — duplicate field in a class declaration (parse E0208).
class C {
    pub x: int,
    pub x: int,
}

fn main() {
    let c = new C(1, 2);
}
