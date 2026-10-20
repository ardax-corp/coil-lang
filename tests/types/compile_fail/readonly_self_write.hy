// Expected: E0107 — `self` declared `readonly` cannot be written, even inside the impl (#1558).
trait Area<A> {
    fn area(readonly A self) -> int {}
}

class Sq {
    pub s: int,
}

impl Area for Sq {
    fn area(readonly Sq self) -> int {
        self.s = 3;
        return self.s;
    }
}

fn main() {
    let n = new Sq(2).area();
}
