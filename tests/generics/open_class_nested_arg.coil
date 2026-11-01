// An open generic class instance (`Slot<T>`) passed to another generic
// inside a type argument, `Vec<U>` at `U = Slot<T>` (coil-lang#2149):
// both shared bodies agree on the element, and trait calls on the
// element still dispatch.
use tests::assert;

class Slot<T> {
    pub v: T,
}

class Pair<T> {
    pub r: Result<T, string>,
}

trait Weight<W> {
    fn weight(W x) -> int {}
}

class Bag {
    pub n: int,
}

impl Weight for Bag {
    pub fn weight(Bag b) -> int {
        return b.n * 10;
    }
}

fn one<U>(U x) -> Vec<U> {
    let v: Vec<U> = Vec::new();
    v.push(x);
    return v;
}

fn slots<T>(T x) -> Vec<Slot<T>> {
    let v = one(new Slot(x));
    v.push(new Slot(x));
    return v;
}

fn pairs<T>(T x) -> Vec<Pair<T>> {
    return one(new Pair(Result::Ok(x)));
}

fn first_slot<T>(Vec<Slot<T>> v) -> Slot<T> {
    let w = one(v[0]);
    return w[0];
}

fn total<U: Weight>(Vec<U> v) -> int {
    let n = 0;
    for x in v {
        n = n + x.weight();
    }
    return n;
}

fn bags<T>(T x, int n) -> Vec<Bag> {
    let v = one(new Bag(n));
    v.push(new Bag(n + 1));
    return v;
}

test("an open class instance in a Vec through a shared body") {
    let v = slots(5);
    assert!(v.len() == 2);
    assert!(v[0].v == 5);
    assert!(v[1].v == 5);
    let s = slots("a");
    assert!(s[0].v == "a");
    assert!(first_slot(s).v == "a");
}

test("an open class holding an open Result") {
    let p = pairs(7);
    let ok = match p[0].r {
        Result::Ok(n) => n == 7,
        Result::Err(_) => false,
    };
    assert!(ok);
}

test("trait calls on class elements built in a shared body") {
    let b = bags("x", 2);
    assert!(total(b) == 50);
    assert!(total(one(new Bag(4))) == 40);
}
