// What a method returns through a `readonly` receiver is `readonly` too, so
// it can still be read (#1640). A trait method that declares
// `readonly A self` cannot hand out a mutable alias of `self`, so its
// result keeps its declared type.
use cell::Ref;
use collections::map::HashMap;
use tests::assert;

class Inner {
    pub n: int,
}

class Point {
    pub x: int,
    pub inner: Inner,
}

impl Point {
    pub fn get_inner() -> Inner {
        return self.inner;
    }

    pub fn me() -> Point {
        return self;
    }

    pub fn moved() -> Point {
        return new Point(self.x + 1, new Inner(self.inner.n));
    }
}

trait Fresh<A> {
    fn fresh(readonly A self) -> A {}
}

impl Fresh for Point {
    fn fresh(readonly Point self) -> Point {
        return new Point(self.x, new Inner(self.inner.n));
    }
}

fn sum(readonly Point p) -> int {
    let i = p.get_inner();
    let q = p.me();
    return i.n + q.x + p.moved().x;
}

test("results through a readonly receiver can be read") {
    let p = new Point(1, new Inner(2));
    assert!(sum(p) == 5);
    let r: readonly Point = p;
    assert!((typeof r.get_inner()) == "readonly Inner");
    assert!((typeof r.me()) == "readonly Point");
    assert!((typeof p.me()) == "Point");
}

test("a readonly-self trait method returns its declared type") {
    let r: readonly Point = new Point(1, new Inner(2));
    let c = r.fresh();
    c.x = 7;
    assert!(c.x == 7);
    assert!(r.x == 1);
}

test("Ref and HashMap reads through a readonly value") {
    let cell: readonly Ref<Point> = Ref::new(new Point(3, new Inner(4)));
    assert!(cell.get().inner.n == 4);
    assert!((typeof cell.get()) == "readonly Point");
    let m: HashMap<string, Point> = HashMap::new();
    m.insert("a", new Point(5, new Inner(6)));
    let rm = readonly m;
    assert!(rm.get_or("a", new Point(0, new Inner(0))).x == 5);
    let n: readonly HashMap<string, int> = HashMap::new();
    assert!(n.get_or("z", 9) + 1 == 10);
}

test("a generic result at a readonly type argument") {
    let m: HashMap<string, readonly Point> = HashMap::new();
    m.insert("a", new Point(8, new Inner(9)));
    assert!(m.get_or("a", new Point(0, new Inner(0))).inner.n == 9);
}
