// `==` / `!=` on an aggregate of a type parameter in a generic body that
// runs shared (called through a function value) compares the elements with
// the parameter's `Eq` instance, as its monomorphic clone does (#1642).
use tests::assert;

#[derive(Eq)]
class C {
    pub x: int,
}

class Money {
    pub cents: int,
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

#[derive(Eq)]
class D {
    pub x: int,
}

impl Keyed for D {
    pub fn key(D d) -> int {
        return d.x;
    }
}

#[derive(Eq)]
class Box<T> {
    pub v: Vec<T>,
}

enum Wrap<T> {
    W(T),
    Nothing,
}

fn ord_same<T: Keyed>(Vec<T> a, Vec<T> b) -> bool {
    return a == b;
}

fn wrap_same<T: Eq>(Wrap<T> a, Wrap<T> b) -> bool {
    return a == b;
}

fn in_lambda<T: Eq>(Vec<T> a, Vec<T> b) -> bool {
    let f = fn () use (a, b) => a == b;
    return f();
}

fn box_same<T: Eq>(Box<T> a, Box<T> b) -> bool {
    return a == b;
}

fn pair_same<T: Eq, U: Eq>(Vec<(T, U)> a, Vec<(T, U)> b) -> bool {
    return a == b;
}

fn same<T: Eq>(T a, T b) -> bool {
    return a == b;
}

fn vec_same<T: Eq>(Vec<T> a, Vec<T> b) -> bool {
    return a == b;
}

fn vec_differ<T: Eq>(Vec<T> a, Vec<T> b) -> bool {
    return a != b;
}

fn nested_same<T: Eq>(Vec<Vec<T>> a, Vec<Vec<T>> b) -> bool {
    return a == b;
}

fn tuple_same<T: Eq>(T a, T b) -> bool {
    return (a, 1) == (b, 1);
}

fn option_same<T: Eq>(Option<T> a, Option<T> b) -> bool {
    return a == b;
}

fn result_same<T: Eq>(Result<T, string> a, Result<T, string> b) -> bool {
    return a == b;
}

fn record_same<T: Eq>(T a, T b) -> bool {
    return { v: a, k: 2 } == { v: b, k: 2 };
}

fn array_same<T: Eq>(T a, T b) -> bool {
    return [a, a] == [b, b];
}

fn cs(Vec<int> xs) -> Vec<C> {
    let v: Vec<C> = Vec::new();
    for x in xs {
        v.push(new C(x));
    }
    return v;
}

test("Vec of an Eq class through a function value") {
    let f = vec_same;
    assert!(vec_same(cs([1]), cs([1])));
    assert!(f(cs([1]), cs([1])));
    assert!(f(cs([1, 2]), cs([1, 2])));
    assert!(!f(cs([1, 2]), cs([1, 3])));
    assert!(!f(cs([1]), cs([1, 1])));
    let g = vec_differ;
    assert!(!g(cs([1]), cs([1])));
    assert!(g(cs([1]), cs([2])));
}

test("a written Eq instance and primitives") {
    let f = vec_same;
    let a: Vec<Money> = Vec::new();
    a.push(new Money(5));
    let b: Vec<Money> = Vec::new();
    b.push(new Money(5));
    assert!(f(a, b));
    b.push(new Money(6));
    assert!(!f(a, b));
    let h = vec_same;
    let v: Vec<int> = Vec::new();
    v.push(1);
    let w: Vec<int> = Vec::new();
    w.push(1);
    assert!(h(v, w));
    w.push(2);
    assert!(!h(v, w));
}

test("nested Vec, tuple, Option, Result, record and array") {
    let n = nested_same;
    let a: Vec<Vec<C>> = Vec::new();
    a.push(cs([1, 2]));
    let b: Vec<Vec<C>> = Vec::new();
    b.push(cs([1, 2]));
    assert!(n(a, b));
    b.push(cs([3]));
    assert!(!n(a, b));
    let t = tuple_same;
    assert!(t(new C(1), new C(1)));
    assert!(!t(new C(1), new C(2)));
    let o = option_same;
    let p: Option<C> = Some(new C(3));
    let q: Option<C> = Some(new C(3));
    let r: Option<C> = None;
    assert!(o(p, q));
    assert!(!o(p, r));
    let s = result_same;
    let x: Result<C, string> = Ok(new C(4));
    let y: Result<C, string> = Ok(new C(4));
    let z: Result<C, string> = Err("e");
    assert!(s(x, y));
    assert!(!s(x, z));
    let rec = record_same;
    assert!(rec(new C(5), new C(5)));
    assert!(!rec(new C(5), new C(6)));
    let arr = array_same;
    assert!(arr(new C(7), new C(7)));
    assert!(!arr(new C(7), new C(8)));
}

test("a subclass bound") {
    let f = ord_same;
    let a: Vec<D> = Vec::new();
    a.push(new D(1));
    let b: Vec<D> = Vec::new();
    b.push(new D(1));
    assert!(f(a, b));
    b.push(new D(1));
    assert!(!f(a, b));
}

test("generic enum without Eq") {
    let f = wrap_same;
    let a: Wrap<C> = Wrap::W(new C(1));
    let b: Wrap<C> = Wrap::W(new C(1));
    let c: Wrap<C> = Wrap::W(new C(2));
    assert!(f(a, b));
    assert!(!f(a, c));
}

test("lambda") {
    let f = in_lambda;
    assert!(f(cs([1]), cs([1])));
    assert!(!f(cs([1]), cs([2])));
}

test("derived Eq of a generic class with a Vec<T> field") {
    assert!(new Box(cs([1])) == new Box(cs([1])));
    assert!(box_same(new Box(cs([1])), new Box(cs([1]))));
    assert!(!box_same(new Box(cs([1])), new Box(cs([2]))));
    let s = same;
    assert!(s(new Box(cs([1])), new Box(cs([1]))));
    assert!(!s(new Box(cs([1])), new Box(cs([2]))));
}

test("two parameters") {
    let f = pair_same;
    let a: Vec<(C, string)> = Vec::new();
    a.push((new C(1), "a"));
    let b: Vec<(C, string)> = Vec::new();
    b.push((new C(1), "a"));
    assert!(f(a, b));
    b.push((new C(1), "a"));
    assert!(!f(a, b));
}
