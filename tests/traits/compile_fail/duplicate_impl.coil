// @expected E0119 — two instances of `Shape` for the same type overlap.
trait Shape<S> {
    fn area(S self) -> int {}
}

class Sq {
    pub side: int,
}

impl Shape for Sq {
    fn area(Sq self) -> int {
        return self.side;
    }
}

impl Shape for Sq {
    fn area(Sq self) -> int {
        return 0;
    }
}

fn main() {
    area(new Sq(1));
}
