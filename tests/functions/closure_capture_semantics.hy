// `use (…)` captures a copy of each value at the time the lambda is made: a
// scalar, a string, a class object or a Vec alike (copy-on-write, so the copy
// is made only when one side writes). `use (mut x)` borrows `x` instead: the
// closure and the outer scope write one object. State a closure keeps
// between calls lives in a `Ref<T>`.
use tests::assert;
use cell::Ref;

class Box {
    pub n: int,
}

fn make_counter(int start) -> unit -> int {
    let b = Ref::new(new Box(start));
    return fn () use (b) {
        let box = b.get();
        box.n = box.n + 1;
        b.set(box);
        return box.n;
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

test("a captured class object is a copy") {
    let c = new Box(0);
    let bump = fn () use (c) {
        c.n = c.n + 1;
        return c.n;
    };
    assert!(bump() == 1);
    assert!(bump() == 1);
    assert!(c.n == 0);
}

test("a borrowed class object is shared") {
    let c = new Box(0);
    let bump = fn () use (mut c) {
        c.n = c.n + 1;
        return c.n;
    };
    bump();
    assert!(bump() == 2);
    assert!(c.n == 2);
    c.n = 10;
    assert!(bump() == 11);
}

test("a captured Vec is a copy") {
    let v: Vec<int> = [];
    let add = fn (int x) use (v) {
        v.push(x);
        return v.len();
    };
    assert!(add(1) == 1);
    assert!(add(2) == 1);
    assert!(v.len() == 0);
}

test("a borrowed Vec is shared") {
    let v: Vec<int> = [];
    let add = fn (int x) use (mut v) {
        v.push(x);
        return v.len();
    };
    add(1);
    assert!(add(2) == 2);
    assert!(v.len() == 2);
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
    let inc = fn () use (mut b) {
        b.n = b.n + 1;
        return b.n;
    };
    let read = fn () use (mut b) => b.n;
    inc();
    inc();
    assert!(read() == 2);
}
