// Named and spread arguments to a generic free function land in their
// parameters, as for a non-generic one; named ones run in source order (#1680).
use tests::assert;
use string::format;

trait Reset<R> {
    fn reset(R self) {}

    fn val(R self) -> int {
        return 0;
    }
}

class P {
    pub x: int,
}

impl Reset for P {
    fn reset(P self) {
        self.x = 0;
    }

    fn val(P self) -> int {
        return self.x;
    }
}

fn put<A: Reset, B: Reset>(A a, B b) -> int {
    let n = a.val() * 10 + b.val();
    b.reset();
    return n;
}

fn pair<A: Show, B: Show>(A a, B b) -> string {
    return format("%v-%v", a, b);
}

fn first<T>(T a, T b, T c) -> T {
    return a;
}

fn mk<T>(T a, T b) -> Vec<T> {
    let v: Vec<T> = Vec::new();
    v.push(a);
    v.push(b);
    return v;
}

fn joined<T: Show>(string sep, T... xs) -> string {
    let s = "";
    for x in xs {
        s = s + format("%v", x) + sep;
    }
    return s;
}

static let order: string = "";

fn t(string s, int v) -> int {
    order = order + s;
    return v;
}

test("named arguments to a bounded generic function") {
    let p = new P(1);
    let q = new P(2);
    assert!(put(b: q, a: p) == 12);
    assert!(q.x == 0);
    assert!(p.x == 1);
    assert!(put(new P(3), b: new P(4)) == 34);
}

test("named arguments to an unbounded generic function") {
    assert!(pair(b: "y", a: "x") == "x-y");
    assert!(pair(b: 2, a: "x") == "x-2");
    assert!(first(c: 3, b: 2, a: 1) == 1);
}

test("named arguments to a generic function run in source order") {
    order = "";
    assert!(first(c: t("c", 3), a: t("a", 1), b: t("b", 2)) == 1);
    assert!(order == "cab");
}

test("spread arguments to a generic function") {
    let p = new P(5);
    let q = new P(6);
    let tup = (p, q);
    assert!(put(...tup) == 56);
    assert!(q.x == 0);
    assert!(first(...[7, 8, 9]) == 7);
    assert!(first(1, ...(2, 3)) == 1);
    assert!(pair(...("u", 4)) == "u-4");
}

test("a spread is the only call of a generic function at its types") {
    let w = mk(...(1.5, 2.5));
    assert!(w[0] == 1.5 && w[1] == 2.5);
    let t = (true, false);
    let b = mk(...t);
    assert!(b[0] && !b[1]);
    let v = mk(b: 8, a: 7);
    assert!(v[0] == 7 && v[1] == 8);
}

test("a named fixed argument ahead of a generic rest pack") {
    assert!(joined(sep: ";", 1, 2) == "1;2;");
    assert!(joined(",", ...[3, 4]) == "3,4,");
}
