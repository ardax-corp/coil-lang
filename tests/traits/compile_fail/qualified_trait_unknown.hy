// @expected E0202 — `Shape` names no enum, type or trait.
trait Area<T> {
    fn size(T s) -> int {}
}

impl Area for int {
    pub fn size(int n) -> int {
        return n;
    }
}

fn main() {
    let a = Shape::size(1);
}
