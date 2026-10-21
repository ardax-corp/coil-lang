// `==` / `!=` on a generic class deriving `Eq` in a generic body that runs
// shared (called through a function value) whose type parameter has no
// `Eq` bound of its own: a bound whose trait has `Eq` as a superclass
// (`T: Keyed`, or `T: Ord`, #1716) gives the class's `Eq` dictionary its
// `Eq` methods; the class compares field by field (#1694).
use tests::assert;

#[derive(Eq)]
class C {
    pub x: int,
}

class Money {
    pub cents: int,
    pub note: int,
}

impl Eq for Money {
    pub fn eq(Money a, Money b) -> bool {
        return a.cents == b.cents;
    }
    pub fn ne(Money a, Money b) -> bool {
        return a.cents != b.cents;
    }
}

trait Keyed<T: Eq> {
    fn key(T x) -> int {}
}

impl Keyed for Money {
    pub fn key(Money m) -> int {
        return m.cents;
    }
}

#[derive(Eq)]
class Box<T> {
    pub v: T,
}

fn box_same<T: Ord>(Box<T> a, Box<T> b) -> bool {
    return a == b;
}

fn box_differ<T: Ord>(Box<T> a, Box<T> b) -> bool {
    return a != b;
}

fn vec_box_same<T: Ord>(Vec<Box<T>> a, Vec<Box<T>> b) -> bool {
    return a == b;
}

fn opt_box_same<T: Ord>(Option<Box<T>> a, Option<Box<T>> b) -> bool {
    return a == b;
}

fn mixed_same<A: Ord, B: Eq>(Box<A> a, Box<A> b, Box<B> c, Box<B> d) -> bool {
    return a == b && c == d;
}

fn keyed_same<T: Keyed>(Box<T> a, Box<T> b) -> bool {
    return a == b;
}

fn keyed_vec_same<T: Keyed>(Vec<Box<T>> a, Vec<Box<T>> b) -> bool {
    return a == b;
}

fn boxes(Vec<int> xs) -> Vec<Box<int>> {
    let out: Vec<Box<int>> = Vec::new();
    for x in xs {
        out.push(new Box(x));
    }
    return out;
}

fn money(int cents, int note) -> Vec<Box<Money>> {
    let out: Vec<Box<Money>> = Vec::new();
    out.push(new Box(new Money(cents, note)));
    return out;
}

test("generic class under an Ord bound through a function value") {
    let f = box_same;
    assert!(f(new Box(1), new Box(1)));
    assert!(!f(new Box(1), new Box(2)));
    assert!(f(new Box(1.5), new Box(1.5)));
    assert!(!f(new Box(1.5), new Box(2.5)));
    let g = box_differ;
    assert!(!g(new Box(1), new Box(1)));
    assert!(g(new Box(1), new Box(2)));
}

test("Vec of a generic class under an Ord bound") {
    let f = vec_box_same;
    assert!(f(boxes([1, 2]), boxes([1, 2])));
    assert!(!f(boxes([1, 2]), boxes([1, 3])));
    assert!(!f(boxes([1, 2]), boxes([1])));
}

test("Option of a generic class under an Ord bound") {
    let f = opt_box_same;
    assert!(f(Some(new Box(1)), Some(new Box(1))));
    assert!(!f(Some(new Box(1)), Some(new Box(2))));
    assert!(!f(Some(new Box(1)), None));
}

test("Ord and Eq bounds together") {
    let f = mixed_same;
    assert!(f(new Box(1), new Box(1), new Box(new C(2)), new Box(new C(2))));
    assert!(!f(new Box(1), new Box(3), new Box(new C(2)), new Box(new C(2))));
    assert!(!f(new Box(1), new Box(1), new Box(new C(2)), new Box(new C(4))));
}

test("a bound whose trait has Eq as a superclass") {
    let f = keyed_same;
    assert!(f(new Box(new Money(1, 2)), new Box(new Money(1, 3))));
    assert!(!f(new Box(new Money(1, 2)), new Box(new Money(2, 2))));
    let g = keyed_vec_same;
    assert!(g(money(1, 2), money(1, 3)));
    assert!(!g(money(1, 2), money(4, 2)));
}
