// `Ord` implies `Eq` (#1716): `==` / `!=` inside a `T: Ord` body use `T`'s
// `Eq`, so a generic class deriving `Eq` compares a class field by its `Eq`
// both in the mono clone (a direct call) and in the shared body (a call
// through a function value), and a `Vec<T>` compares its elements the same
// way. The `shared_*` functions are only ever called through a function
// value, so no mono clone stands in for their shared body.
use tests::assert;

#[derive(Eq, Ord)]
class K {
    pub v: int,
}

// `Eq` that ignores `note`, so identity and field-by-field compare both
// differ from it.
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

impl Lt for Money {
    pub fn lt(Money a, Money b) -> bool {
        return a.cents < b.cents;
    }
}

impl Le for Money {
    pub fn le(Money a, Money b) -> bool {
        return a.cents <= b.cents;
    }
}

impl Gt for Money {
    pub fn gt(Money a, Money b) -> bool {
        return a.cents > b.cents;
    }
}

impl Ge for Money {
    pub fn ge(Money a, Money b) -> bool {
        return a.cents >= b.cents;
    }
}

impl Ord for Money {
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

fn vec_same<T: Ord>(Vec<T> a, Vec<T> b) -> bool {
    return a == b;
}

fn shared_box_same<T: Ord>(Box<T> a, Box<T> b) -> bool {
    return a == b;
}

fn shared_box_differ<T: Ord>(Box<T> a, Box<T> b) -> bool {
    return a != b;
}

fn shared_vec_same<T: Ord>(Vec<T> a, Vec<T> b) -> bool {
    return a == b;
}

fn shared_vec_box_same<T: Ord>(Vec<Box<T>> a, Vec<Box<T>> b) -> bool {
    return a == b;
}

fn ks(Vec<int> xs) -> Vec<K> {
    let out: Vec<K> = Vec::new();
    for x in xs {
        out.push(new K(x));
    }
    return out;
}

test("Box of a class under an Ord bound, mono clone") {
    assert!(box_same(new Box(new K(1)), new Box(new K(1))));
    assert!(!box_same(new Box(new K(1)), new Box(new K(2))));
    assert!(!box_differ(new Box(new K(1)), new Box(new K(1))));
    assert!(box_differ(new Box(new K(1)), new Box(new K(2))));
    assert!(box_same(new Box(new Money(1, 2)), new Box(new Money(1, 3))));
    assert!(!box_same(new Box(new Money(1, 2)), new Box(new Money(2, 2))));
}

test("Box of a class under an Ord bound, shared body") {
    let f = shared_box_same;
    assert!(f(new Box(new K(1)), new Box(new K(1))));
    assert!(!f(new Box(new K(1)), new Box(new K(2))));
    let g = shared_box_differ;
    assert!(!g(new Box(new K(1)), new Box(new K(1))));
    assert!(g(new Box(new K(1)), new Box(new K(2))));
    let h = shared_box_same;
    assert!(h(new Box(new Money(1, 2)), new Box(new Money(1, 3))));
    assert!(!h(new Box(new Money(1, 2)), new Box(new Money(2, 2))));
}

fn kboxes(Vec<int> xs) -> Vec<Box<K>> {
    let out: Vec<Box<K>> = Vec::new();
    for x in xs {
        out.push(new Box(new K(x)));
    }
    return out;
}

test("Vec of a class under an Ord bound") {
    assert!(vec_same(ks([1, 2]), ks([1, 2])));
    assert!(!vec_same(ks([1, 2]), ks([1, 3])));
    let f = shared_vec_same;
    assert!(f(ks([1, 2]), ks([1, 2])));
    assert!(!f(ks([1, 2]), ks([1, 3])));
    assert!(!f(ks([1, 2]), ks([1])));
    let g = shared_vec_box_same;
    assert!(g(kboxes([1, 2]), kboxes([1, 2])));
    assert!(!g(kboxes([1, 2]), kboxes([1, 3])));
}

test("primitives in a Box under an Ord bound") {
    let g = shared_box_same;
    assert!(g(new Box("x"), new Box("x")));
    assert!(!g(new Box("x"), new Box("y")));
    assert!(g(new Box(1.5), new Box(1.5)));
    assert!(!g(new Box(1.5), new Box(2.5)));
}
