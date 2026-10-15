// A trait method can declare its effects. Calls through the trait (an
// existential) get the declared effects, so a `pure` trait
// method can be called from a `pure fn`, and a `uses {…}` one from a
// function that declares those effects.
use tests::assert;

static let LOADS: int = 0;

trait Area<A> {
    pure fn area(A self) -> int {}
}

trait Source<S> {
    fn load(S self) -> int uses {read, mutate} {}
}

class Sq {
    pub side: int,
}

class Rect {
    pub w: int,
    pub h: int,
}

impl Area for Sq {
    fn area(Sq self) -> int {
        return self.side * self.side;
    }
}

impl Area for Rect {
    fn area(Rect self) -> int {
        return self.w * self.h;
    }
}

impl Source for Sq {
    fn load(Sq self) -> int {
        LOADS = LOADS + 1;
        return self.side;
    }
}

// An impl may do less than the declaration allows.
impl Source for Rect {
    fn load(Rect self) -> int {
        return self.w;
    }
}

pure fn total(Area a, Area b) -> int {
    return area(a) + area(b);
}

pure fn total_methods(Area a, Area b) -> int {
    return a.area() + b.area();
}

fn load_both(Source a, Source b) -> int uses {read, mutate} {
    return load(a) + load(b);
}

test("pure trait methods through existentials") {
    assert!(total(new Sq(3), new Rect(2, 5)) == 19);
    assert!(total_methods(new Rect(1, 1), new Sq(2)) == 5);
}

test("a uses trait method through existentials") {
    let before = LOADS;
    assert!(load_both(new Sq(4), new Rect(6, 1)) == 10);
    assert!(LOADS == before + 1);
}
