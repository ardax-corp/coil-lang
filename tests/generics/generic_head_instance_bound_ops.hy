// #971: in an instance for a generic head (`Duo<T: Add>`), the impl's `T`
// stays a type parameter across the constructor's arguments, so both
// `x.a + y.a` and `x.b + y.b` resolve through the `Add` bound.
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
