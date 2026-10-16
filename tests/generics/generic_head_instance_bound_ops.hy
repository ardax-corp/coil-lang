// #971: in an instance for a generic head (`Duo<T: Add>`), the impl's `T`
// stays a type parameter across the constructor's arguments, so both
// `x.a + y.a` and `x.b + y.b` (and `.add` calls) resolve through the `Add` bound.
use tests::assert;

class Duo<T> {
    pub a: T,
    pub b: T,
}

impl Add for Duo<T: Add> {
    pub fn add(Duo<T> x, Duo<T> y) -> Duo<T> {
        return new Duo(x.a + y.a, x.b + y.b);
    }
}

test("bound operator in a generic-head instance") {
    let d = new Duo(1, 2) + new Duo(3, 4);
    assert!(d.a == 4);
    assert!(d.b == 6);
}

test("float instantiation") {
    let d = new Duo(1.5, 2.0) + new Duo(0.5, 1.0);
    assert!(d.a == 2.0);
    assert!(d.b == 3.0);
}

class Pair<T> {
    pub a: T,
    pub b: T,
}

// Two bound method calls on `T` in one body resolve through the bound, as
// the operators above do.
impl Add for Pair<T: Add> {
    pub fn add(Pair<T> x, Pair<T> y) -> Pair<T> {
        return new Pair(x.a.add(y.a), x.b.add(y.b));
    }
}

test("two bound method calls in a generic-head instance") {
    let p = new Pair(1, 2) + new Pair(3, 4);
    assert!(p.a == 4);
    assert!(p.b == 6);
}
