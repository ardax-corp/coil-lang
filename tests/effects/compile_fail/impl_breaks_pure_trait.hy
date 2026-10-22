// @expected E0413 — an impl must keep to its trait method's `pure` declaration.
static let N: int = 0;

trait Area<A> {
    pure fn area(A self) -> int {}
}

class Sq {
    pub side: int,
}

impl Area for Sq {
    fn area(Sq self) -> int {
        N = N + 1;
        return self.side * self.side;
    }
}

fn main() {
    area(new Sq(2));
}
