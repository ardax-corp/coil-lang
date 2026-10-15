// A trait instance for a generic head (`impl Add for Duo<T: Add>`) and a
// trait's default body are compiled once per concrete type they are used
// at: operator, method and function-style calls, `%v` and dictionaries of a
// ground type reach the clone, whose bound calls are ground instance calls.
// Uses through an open type still go to the shared body.

use tests::assert;
use string::format;

class Duo<T> {
    pub a: T,
    pub b: T,
}

fn plus<T: Add>(T a, T b) -> T {
    return a + b;
}

impl Add for Duo<T: Add> {
    pub fn add(Duo<T> x, Duo<T> y) -> Duo<T> {
        return new Duo(plus(x.a, y.a), plus(x.b, y.b));
    }
}

trait Measure<S> {
    fn size(S x) -> int {}

    fn twice(S x) -> int {
        return x.size() * 2;
    }

    fn bigger(S x, S y) -> bool {
        return x.size() > y.size();
    }
}

impl Measure for int {
    pub fn size(int x) -> int {
        return x;
    }
}

impl Measure for string {
    pub fn size(string s) -> int {
        return s.len();
    }
}

impl Measure for Duo<T: Measure> {
    pub fn size(Duo<T> d) -> int {
        return d.a.size() + d.b.size();
    }
}

impl Show for Duo<T: Show> {
    pub fn show(Duo<T> d) -> string {
        return format("<%v|%v>", d.a, d.b);
    }
}

fn total<U: Measure>(U x, U y) -> int {
    return x.size() + y.twice();
}

fn sum_duos(int n) -> Duo<int> {
    let acc = new Duo(0, 0);
    for i in 0..n {
        acc = acc + new Duo(i, 1);
    }
    return acc;
}

test("operators reach the per-type clone") {
    let d = sum_duos(4);
    assert!(d.a == 6);
    assert!(d.b == 4);
    let f = new Duo(1.5, 2.0) + new Duo(0.5, 1.0);
    assert!(f.a == 2.0);
    assert!(f.b == 3.0);
}

test("method, function-style and show calls on clones") {
    let d = new Duo(3, 4);
    assert!(d.size() == 7);
    assert!(size(new Duo("ab", "c")) == 3);
    assert!(new Duo(new Duo(1, 2), new Duo(3, 4)).size() == 10);
    assert!(format("%v", d) == "<3|4>");
    assert!(format("%v", new Duo("x", "y")) == "<x|y>");
}

test("default bodies are cloned per instance") {
    assert!(5.twice() == 10);
    assert!("abc".twice() == 6);
    assert!(new Duo(1, 2).twice() == 6);
    assert!(9.bigger(2));
    assert!(!"a".bigger("bc"));
}

test("dictionaries of a ground type name the clones") {
    assert!(total(2, 3) == 8);
    assert!(total("a", "bb") == 5);
    assert!(total(new Duo(1, 1), new Duo(2, 3)) == 12);
}
