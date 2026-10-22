// @output 14
// A trait method called in method form (`a.area()`) gets the trait's
// declared effects, as `area(a)` does, on a bound or a concrete receiver.
use io::{stdout, write};
use string::{format, to_bytes};

trait Area<A> {
    pure fn area(A self) -> int {}
}

class Sq {
    pub side: int,
}

impl Area for Sq {
    fn area(Sq self) -> int {
        return self.side * self.side;
    }
}

pure fn via_bound<T: Area>(T a) -> int {
    return a.area();
}

pure fn via_concrete(Sq a) -> int {
    return a.area();
}

pure fn via_call(Sq a) -> int {
    return area(a);
}

fn main() {
    let total = via_bound(new Sq(2)) + via_concrete(new Sq(3)) + via_call(new Sq(1));
    let _ = write(stdout(), to_bytes(format("%i", total)));
}
