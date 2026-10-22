// @expected E0107 — a `readonly Area` allows only trait methods declared `readonly A self` (#1558).
trait Area<A> {
    fn area(readonly A self) -> int {}
    fn grow(A self) {}
}

class Sq {
    pub s: int,
}

impl Area for Sq {
    fn area(Sq self) -> int {
        return self.s * self.s;
    }

    fn grow(Sq self) {
        self.s = self.s + 1;
    }
}

fn main() {
    let a: readonly Area = readonly new Sq(2);
    let n = a.area();
    a.grow();
}
