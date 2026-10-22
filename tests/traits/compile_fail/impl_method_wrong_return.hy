// @expected E0102 — the impl's `area` returns string, the trait declares int (#1185).
trait Shape<S> {
    fn area(S self) -> int {}
}

class Sq {
    pub side: int,
}

impl Shape for Sq {
    pub fn area(Sq self) -> string {
        return "big";
    }
}

fn main() {
    let s = new Sq(2);
    let a = s.area();
}
