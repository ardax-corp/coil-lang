// Expected: E0119 — an impl must provide every trait method without a default (`name`).
trait Shape<S> {
    fn area(S self) -> int;
    fn name(S self) -> string;
}

class Sq {
    pub side: int,
}

impl Shape for Sq {
    fn area(Sq self) -> int {
        return self.side;
    }
}

fn main() {
    area(new Sq(1));
}
