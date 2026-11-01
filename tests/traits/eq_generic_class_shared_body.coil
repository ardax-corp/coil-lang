// `==` / `!=` on a generic class deriving `Eq` in a generic body that runs
// shared (called through a function value) builds the class's `Eq`
// dictionary from the type parameter's and compares by it (#1661).
use tests::assert;

#[derive(Eq)]
class C {
    pub x: int,
}

#[derive(Eq)]
class Box<T> {
    pub v: T,
}

#[derive(Eq)]
class Pair<A, B> {
    pub a: A,
    pub b: B,
}

fn box_same<T: Eq>(Box<T> a, Box<T> b) -> bool {
    return a == b;
}

fn box_differ<T: Eq>(Box<T> a, Box<T> b) -> bool {
    return a != b;
}

fn vec_box_same<T: Eq>(Vec<Box<T>> a, Vec<Box<T>> b) -> bool {
    return a == b;
}

fn opt_box_same<T: Eq>(Option<Box<T>> a, Option<Box<T>> b) -> bool {
    return a == b;
}

fn nested_same<T: Eq>(Vec<Box<Box<T>>> a, Vec<Box<Box<T>>> b) -> bool {
    return a == b;
}

fn pair_same<A: Eq, B: Eq>((Pair<A, B>, A) a, (Pair<A, B>, A) b) -> bool {
    return a == b;
}

fn in_lambda<T: Eq>(Vec<Box<T>> a, Vec<Box<T>> b) -> bool {
    let f = fn () use (a, b) => a != b;
    return f();
}

fn boxes(Vec<int> xs) -> Vec<Box<C>> {
    let out: Vec<Box<C>> = Vec::new();
    for x in xs {
        out.push(new Box(new C(x)));
    }
    return out;
}

test("generic class of int through a function value") {
    let f = box_same;
    assert!(f(new Box(1), new Box(1)));
    assert!(!f(new Box(1), new Box(2)));
    let g = box_differ;
    assert!(!g(new Box(1), new Box(1)));
    assert!(g(new Box(1), new Box(2)));
}

test("generic class of a class through a function value") {
    let f = box_same;
    assert!(f(new Box(new C(1)), new Box(new C(1))));
    assert!(!f(new Box(new C(1)), new Box(new C(2))));
}

test("Vec of a generic class through a function value") {
    let f = vec_box_same;
    assert!(f(boxes([1, 2]), boxes([1, 2])));
    assert!(!f(boxes([1, 2]), boxes([1, 3])));
    assert!(!f(boxes([1, 2]), boxes([1])));
}

test("Option of a generic class through a function value") {
    let f = opt_box_same;
    assert!(f(Some(new Box(new C(1))), Some(new Box(new C(1)))));
    assert!(!f(Some(new Box(new C(1))), Some(new Box(new C(2)))));
    assert!(!f(Some(new Box(new C(1))), None));
}

test("nested generic classes through a function value") {
    let f = nested_same;
    let a: Vec<Box<Box<C>>> = Vec::new();
    a.push(new Box(new Box(new C(1))));
    let b: Vec<Box<Box<C>>> = Vec::new();
    b.push(new Box(new Box(new C(1))));
    assert!(f(a, b));
    b.push(new Box(new Box(new C(1))));
    assert!(!f(a, b));
}

test("two-parameter generic class in a tuple") {
    let f = pair_same;
    assert!(f((new Pair(new C(1), "x"), new C(2)), (new Pair(new C(1), "x"), new C(2))));
    assert!(!f((new Pair(new C(1), "x"), new C(2)), (new Pair(new C(1), "y"), new C(2))));
    assert!(!f((new Pair(new C(1), "x"), new C(2)), (new Pair(new C(1), "x"), new C(3))));
}

test("Vec of a generic class in a lambda") {
    let f = in_lambda;
    assert!(!f(boxes([1]), boxes([1])));
    assert!(f(boxes([1]), boxes([2])));
}
