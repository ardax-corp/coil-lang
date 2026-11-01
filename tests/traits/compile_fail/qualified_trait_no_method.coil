// @expected E0101 — trait `Area` declares no method `perimeter`.
trait Area<T> {
    fn size(T s) -> int {}
}

impl Area for int {
    pub fn size(int n) -> int {
        return n;
    }
}

fn main() {
    let a = Area::perimeter(1);
}
