// @expected E0119 — an impl may only define the trait's methods; `perimeter` is not one.
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

    fn perimeter(Sq self) -> int {
        return 4 * self.side;
    }
}

fn main() {
    area(new Sq(1));
}
