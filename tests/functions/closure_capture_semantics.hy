// `use (…)` captures copy a scalar or string at the time the lambda is made,
// while a captured class object or Vec is shared with the outer scope; each
// closure made by a call gets its own captures.
use tests::assert;

class Box {
    pub n: int,
}

fn make_counter(int start) -> unit -> int {
    let b = new Box(start);
    return fn () use (b) {
        b.n = b.n + 1;
        return b.n;
    };
}

test("a captured int is the value at creation") {
    let k = 1;
    let f = fn () use (k) => k * 10;
    k = 5;
    assert!(f() == 10);
    assert!(k == 5);
}

test("a captured string is the value at creation") {
    let s = "ab";
    let f = fn (string t) use (s) => s + t;
    s = "zz";
    assert!(f("c") == "abc");
}

test("a captured class object is shared") {
    let c = new Box(0);
    let bump = fn () use (c) {
        c.n = c.n + 1;
        return c.n;
    };
    bump();
    assert!(bump() == 2);
    assert!(c.n == 2);
    c.n = 10;
    assert!(bump() == 11);
}

test("a captured Vec is shared") {
    let v: Vec<int> = [];
    let add = fn (int x) use (v) {
        v.push(x);
        return len(v);
    };
    add(1);
    assert!(add(2) == 2);
    assert!(len(v) == 2);
    assert!(v[1] == 2);
}

test("each returned closure has its own state") {
    let c = make_counter(0);
    c();
    c();
    assert!(c() == 3);
    let d = make_counter(100);
    assert!(d() == 101);
    assert!(c() == 4);
}

test("two closures over one object see each other's writes") {
    let b = new Box(0);
    let inc = fn () use (b) {
        b.n = b.n + 1;
        return b.n;
    };
    let read = fn () use (b) => b.n;
    inc();
    inc();
    assert!(read() == 2);
}
