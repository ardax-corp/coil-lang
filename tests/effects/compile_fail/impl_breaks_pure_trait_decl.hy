// @expected E0413 — a trait method declared `pure` without a body binds
// every impl: this one writes a static.
static let HITS: int = 0;

trait Area<A> {
    pure fn area(A self) -> int {}
}

class Sq {
    pub side: int,
}

impl Area for Sq {
    fn area(Sq self) -> int {
        HITS = HITS + 1;
        return self.side;
    }
}
