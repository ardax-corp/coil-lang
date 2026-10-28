// A trait default method with a `readonly T self` receiver calls another
// trait method on `self` (#2087).
use tests::assert;

trait Area<T> {
    fn area(readonly T self) -> float {}
    fn twice(readonly T self) -> float {
        return self.area() * 2.0;
    }
}

class Sq {
    pub s: float,
}

class Rect {
    pub w: float,
    pub h: float,
}

impl Area for Sq {
    fn area(readonly Sq self) -> float {
        return self.s * self.s;
    }
}

impl Area for Rect {
    fn area(readonly Rect self) -> float {
        return self.w * self.h;
    }
}

fn doubled<T: Area>(readonly T x) -> float {
    return x.twice();
}

test("default method with readonly self calls self.area() on a square") {
    let q = new Sq(2.0);
    assert!(q.twice() == 8.0);
}

test("default method with readonly self calls self.area() on a rect") {
    let r = new Rect(2.0, 3.0);
    assert!(r.twice() == 12.0);
}

test("default method with readonly self through a bounded generic") {
    assert!(doubled(new Sq(3.0)) == 18.0);
    assert!(doubled(new Rect(1.5, 2.0)) == 6.0);
}
