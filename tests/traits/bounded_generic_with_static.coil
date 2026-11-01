// @output 1 4 9
// A `static let` in the file must not change which trait method a
// bounded generic over a class calls.
use io::{stdout, write};
use string::{format, to_bytes};

static let N: int = 0;

trait Area<A> {
    fn area(A self) -> int {}
}

class Sq {
    pub side: int,
}

impl Area for Sq {
    fn area(Sq self) -> int {
        return self.side * self.side;
    }
}

fn first<T: Area>(Vec<T> xs) -> int {
    let x = xs[0];
    return area(x);
}

fn main() {
    let a = first(Vec::from([new Sq(1)]));
    let b = first(Vec::from([new Sq(2), new Sq(5)]));
    let c = first(Vec::from([new Sq(3)])) + N;
    let _ = write(stdout(), to_bytes(format("%i %i %i", a, b, c)));
}
