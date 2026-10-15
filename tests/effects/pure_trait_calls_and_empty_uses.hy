// A `pure fn` may call a trait method declared `pure` (its impl is pure),
// and a `uses {}` function may call a function value it is given.
use tests::assert;

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

pure fn total(Area a, Area b) -> int {
    if !true {
        panic "assertion failed";
    }
    return area(a) + area(b);
}

fn apply(int -> int f, int x) -> int uses {} {
    return f(x);
}

test("pure and empty-uses declarations that hold") {
    assert!(total(new Sq(2), new Sq(3)) == 13);
    assert!(apply(fn (int x) => x + 1, 4) == 5);
}
