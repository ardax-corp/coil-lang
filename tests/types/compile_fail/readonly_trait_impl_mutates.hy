// @expected E0107 — an impl may not mutate a parameter its trait declares `readonly` (#1558).
trait Area<A> {
    fn area(readonly A self) -> int {}
}

class Sq {
    pub s: int,
}

impl Area for Sq {
    fn area(Sq self) -> int {
        self.s = self.s + 1;
        return self.s * self.s;
    }
}

fn main() {
    let n = new Sq(2).area();
}
