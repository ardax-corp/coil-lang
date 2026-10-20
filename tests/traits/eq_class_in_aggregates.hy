// `==` / `!=` on an array, `Vec`, tuple, `Option`, enum payload or record
// compares a class element with the class's `Eq` instance when it has one
// (derived or written), and by identity when it has none. A class deriving
// `Eq` with a `Vec<C>` field compares that field the same way (#1629).
use tests::assert;

#[derive(Eq)]
class Q {
    pub x: int,
}

#[derive(Eq)]
class WQ {
    pub v: Vec<Q>,
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

class P {
    pub x: int,
}

enum Slot {
    Empty,
    Full(Q),
}

enum Wrap<T> {
    W(T),
    Nothing,
}

enum Named {
    At { q: Q, n: int },
}

enum List {
    Nil,
    Cons(Q, List),
}

fn same<T: Eq>(T a, T b) -> bool {
    return a == b;
}

fn differ<T: Eq>(T a, T b) -> bool {
    return a != b;
}

fn vec_same<T: Eq>(Vec<T> a, Vec<T> b) -> bool {
    return a == b;
}

fn nan() -> float {
    let z = 0.0;
    return z / z;
}

fn qs(Vec<int> xs) -> Vec<Q> {
    let v: Vec<Q> = Vec::new();
    for x in xs {
        v.push(new Q(x));
    }
    return v;
}

fn empty() -> Vec<Q> {
    return Vec::new();
}

test("vec of Eq class") {
    let c: Vec<Q> = Vec::new();
    c.push(new Q(1));
    let d: Vec<Q> = Vec::new();
    d.push(new Q(1));
    assert!(new Q(1) == new Q(1));
    assert!(c == d);
    assert!(!(c != d));
    assert!(qs([1, 2]) != qs([1, 3]));
    assert!(qs([1, 2]) != qs([1]));
    assert!(empty() == empty());
}

test("field of Vec<Eq class>") {
    assert!(new WQ(qs([1])) == new WQ(qs([1])));
    assert!(new WQ(qs([1])) != new WQ(qs([2])));
    assert!(!(new WQ(qs([1, 2])) != new WQ(qs([1, 2]))));
}

test("nested Vec, array, tuple, Option and record") {
    let a: Vec<Vec<Q>> = Vec::new();
    a.push(qs([1, 2]));
    let b: Vec<Vec<Q>> = Vec::new();
    b.push(qs([1, 2]));
    assert!(a == b);
    b.push(empty());
    assert!(a != b);
    assert!([new Q(1), new Q(2)] == [new Q(1), new Q(2)]);
    assert!([new Q(1)] != [new Q(2)]);
    assert!([[new Q(3)]] == [[new Q(3)]]);
    assert!((new Q(1), 2) == (new Q(1), 2));
    assert!((new Q(1), 2) != (new Q(2), 2));
    assert!((1, (new Q(4), "s")) == (1, (new Q(4), "s")));
    let o: Option<Q> = Some(new Q(5));
    let p: Option<Q> = Some(new Q(5));
    let n: Option<Q> = None;
    assert!(o == p);
    assert!(o != n);
    assert!(Some(new Q(1)) != Some(new Q(2)));
    assert!({ q: new Q(1), k: 2 } == { q: new Q(1), k: 2 });
    assert!({ q: new Q(1), k: 2 } != { q: new Q(9), k: 2 });
}

test("enum payload of an Eq class") {
    assert!(Slot::Full(new Q(1)) == Slot::Full(new Q(1)));
    assert!(Slot::Full(new Q(1)) != Slot::Full(new Q(2)));
    assert!(Slot::Full(new Q(1)) != Slot::Empty);
}

test("generic, record-payload and recursive enums") {
    let a: Wrap<Q> = Wrap::W(new Q(1));
    let b: Wrap<Q> = Wrap::W(new Q(1));
    let c: Wrap<Q> = Wrap::W(new Q(2));
    assert!(a == b);
    assert!(a != c);
    assert!(Named::At { q: new Q(1), n: 2 } == Named::At { q: new Q(1), n: 2 });
    assert!(Named::At { q: new Q(1), n: 2 } != Named::At { q: new Q(2), n: 2 });
    let l = List::Cons(new Q(1), List::Cons(new Q(2), List::Nil));
    let m = List::Cons(new Q(1), List::Cons(new Q(2), List::Nil));
    assert!(l == m);
    assert!(l != List::Cons(new Q(1), List::Nil));
}

test("Result, Vec of Option, and floats beside a class") {
    let r: Result<Q, string> = Ok(new Q(1));
    let s: Result<Q, string> = Ok(new Q(1));
    let e: Result<Q, string> = Err("x");
    assert!(r == s);
    assert!(r != e);
    let v: Vec<Option<Q>> = Vec::new();
    v.push(Some(new Q(1)));
    v.push(None);
    let w: Vec<Option<Q>> = Vec::new();
    w.push(Some(new Q(1)));
    w.push(None);
    assert!(v == w);
    let n = nan();
    assert!((new Q(1), n) != (new Q(1), n));
    assert!((new Q(1), 0.0) == (new Q(1), -0.0));
}

test("a written Eq instance is used too") {
    assert!([new Money(5)] == [new Money(5)]);
    assert!((new Money(5), 1) != (new Money(6), 1));
}

test("through an Eq bound") {
    assert!(same(qs([1, 2]), qs([1, 2])));
    assert!(!same(qs([1, 2]), qs([2, 1])));
    assert!(differ(qs([1]), qs([2])));
    assert!(!differ(qs([7]), qs([7])));
    assert!(same((new Q(1), 2), (new Q(1), 2)));
    assert!(same([new Q(1)], [new Q(1)]));
    assert!(same(Some(new Q(3)), Some(new Q(3))));
    assert!(same({ a: new Q(3) }, { a: new Q(3) }));
    assert!(vec_same(qs([4]), qs([4])));
    assert!(!vec_same(qs([4]), qs([5])));
    let s = same;
    assert!(s(qs([3]), qs([3])));
    assert!(!s(qs([3]), qs([4])));
}

test("a Vec, Option or record of primitives meets an Eq bound") {
    let v: Vec<int> = Vec::new();
    v.push(1);
    let w: Vec<int> = Vec::new();
    w.push(1);
    assert!(same(v, w));
    w.push(2);
    assert!(differ(v, w));
    let o: Option<int> = Some(1);
    assert!(same(o, Some(1)));
    assert!(differ(o, None));
    assert!(same({ a: 1, b: "x" }, { a: 1, b: "x" }));
}

test("a class without Eq still compares by identity") {
    let x = new P(1);
    assert!([x] == [x]);
    assert!([new P(1)] != [new P(1)]);
    let v: Vec<P> = Vec::new();
    v.push(new P(1));
    let w: Vec<P> = Vec::new();
    w.push(new P(1));
    assert!(v != w);
    assert!((x, 1) == (x, 1));
    assert!((new P(1), 1) != (new P(1), 1));
}
