// A trait method with its own type parameters (`fn third<W>(S s, W w)`)
// lowers in every call shape: function-style from an instance method and
// from a test body, and a shared method body passing its own `W` on to a
// generic free fn, declared before or after the impl (#1467).
use tests::assert;

trait Pass<S> {
    fn third<W>(S s, W w) -> W {}
    fn via<W>(S s, W w) -> W {}
    fn wrap<W>(S s, W w) -> W {}
}

class Pt {
    pub x: int,
}

fn ident_before<X>(X x) -> X {
    return x;
}

impl Pass for int {
    pub fn third<W>(int s, W w) -> W {
        return w;
    }
    pub fn via<W>(int s, W w) -> W {
        return third(s, w);
    }
    pub fn wrap<W>(int s, W w) -> W {
        return ident_after(ident_before(w));
    }
}

impl Pass for string {
    pub fn third<W>(string s, W w) -> W {
        return ident_before(w);
    }
    pub fn via<W>(string s, W w) -> W {
        return s.third(w);
    }
    pub fn wrap<W>(string s, W w) -> W {
        return third(s, w);
    }
}

fn ident_after<X>(X x) -> X {
    return x;
}

class Cell {
    pub v: int,
}

impl Cell {
    pub fn keep<W>(W w) -> W {
        return ident_after(w);
    }
}

test("an instance method calls a sibling trait method function-style") {
    assert!(3.via(5) == 5);
    assert!(3.via("five") == "five");
    assert!("s".wrap(2.5) == 2.5);
    assert!(3.via(new Pt(7)).x == 7);
}

test("an instance method passes its own type parameter to a generic fn") {
    assert!(3.wrap(4) == 4);
    assert!(3.wrap("w") == "w");
    assert!(3.wrap(new Pt(8)).x == 8);
    assert!("s".third(true));
    assert!("s".via(9) == 9);
    assert!(new Cell(1).keep("k") == "k");
    assert!(new Cell(1).keep(6) == 6);
}

test("a test body calls the trait method function-style") {
    assert!(third(3, 4) == 4);
    assert!(third(3, "x") == "x");
    assert!(third("a", 1.5) == 1.5);
    assert!(third("a", new Pt(2)).x == 2);
    assert!(3.third(4) == 4);
}
