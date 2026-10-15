// Each type parameter has its own bounds, and a generic with bounds can
// call another generic whose bounds are a subset of them.
use tests::assert;

fn dist<A: Num + Ord>(A lo, A hi) -> A {
    if hi < lo {
        return lo - hi;
    }
    return hi - lo;
}

fn spread<A: Num + Ord + Show, B: Show>(A lo, A hi, B tag) -> string {
    let shown = show(dist(lo, hi));
    return show(tag) + ":" + shown;
}

fn pair_max<A: Ord, B: Ord>(A a1, A a2, B b1, B b2) -> (A, B) {
    let a = a1;
    if a2 > a1 {
        a = a2;
    }
    let b = b1;
    if b2 > b1 {
        b = b2;
    }
    return (a, b);
}

test("bounds on two parameters") {
    assert!(spread(3, 10, "d") == "d:7");
    assert!(spread(10, 3, true) == "true:7");
    assert!(spread(0.5, 2.0, 1) == "1:1.5");
}

test("independent bounds pick their own instances") {
    let (a, b) = pair_max(1, 4, 2.5, 0.5);
    assert!(a == 4);
    assert!(b == 2.5);
}
