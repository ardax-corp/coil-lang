// A trait method with a body is a default: an impl that omits it gets the
// default, which can call the impl's own methods; an impl that writes it
// replaces it. The same method is reached directly, through a bound and
// through an existential.
use tests::assert;

trait Shape<S> {
    fn area(S self) -> int {}

    fn scaled(S self, int k) -> int {
        return area(self) * k;
    }

    fn describe(S self) -> string {
        return "area " + show(area(self));
    }
}

class Sq {
    pub side: int,
}

class Rect {
    pub w: int,
    pub h: int,
}

impl Shape for Sq {
    fn area(Sq self) -> int {
        return self.side * self.side;
    }
}

impl Shape for Rect {
    fn area(Rect self) -> int {
        return self.w * self.h;
    }

    fn describe(Rect self) -> string {
        return "rect " + show(self.w) + "x" + show(self.h);
    }
}

fn label<T: Shape>(T s) -> string {
    return describe(s);
}

fn double_area<T: Shape>(T s) -> int {
    return s.scaled(2);
}

fn label_any(Shape s) -> string {
    return describe(s);
}

test("an omitted default uses the impl's methods") {
    let s = new Sq(3);
    assert!(s.scaled(2) == 18);
    assert!(scaled(s, 3) == 27);
    assert!(s.describe() == "area 9");
}

test("an impl that writes the method replaces the default") {
    let r = new Rect(2, 5);
    assert!(r.describe() == "rect 2x5");
    assert!(r.scaled(3) == 30);
}

test("defaults and overrides through a bound") {
    assert!(label(new Sq(2)) == "area 4");
    assert!(label(new Rect(1, 4)) == "rect 1x4");
    assert!(double_area(new Rect(1, 4)) == 8);
}

test("defaults and overrides through an existential") {
    assert!(label_any(new Sq(1)) == "area 1");
    assert!(label_any(new Rect(3, 3)) == "rect 3x3");
}
