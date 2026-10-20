// `==` / `!=` on a generic class deriving `Eq` in a shared generic body
// whose type parameter is bound by `Ord` (a subclass of `Eq`) builds the
// class's `Eq` dictionary from the `Ord` one's `Eq` superclass and compares
// structurally (#1694).
use tests::assert;

#[derive(Eq)]
class C {
    pub x: int,
}

#[derive(Eq, Ord)]
class K {
    pub x: int,
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

fn boxes(Vec<int> xs) -> Vec<Box<int>> {
    let out: Vec<Box<int>> = Vec::new();
    for x in xs {
        out.push(new Box(x));
    }
    return out;
}

test("generic class under an Ord bound through a function value") {
    let f = box_same;
    assert!(f(new Box(1), new Box(1)));
    assert!(!f(new Box(1), new Box(2)));
    let g = box_differ;
    assert!(!g(new Box(1), new Box(1)));
    assert!(g(new Box(1), new Box(2)));
    let s = box_same;
    assert!(s(new Box(new K(1)), new Box(new K(1))));
    assert!(!s(new Box(new K(1)), new Box(new K(2))));
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

test("direct calls still compare structurally") {
    assert!(box_same(new Box(1), new Box(1)));
    assert!(vec_box_same(boxes([1, 2]), boxes([1, 2])));
}
