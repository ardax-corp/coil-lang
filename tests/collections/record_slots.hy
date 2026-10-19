// Records are slotted objects whose slots are the fields sorted by name:
// literals in any field order build the same layout, so a value built in
// one order reads, writes and assigns through a type written in another.
use tests::assert;
use string::format;

fn id<T>(T x) -> T {
    return x;
}

test("literal orders share one layout") {
    let p = { c: "z", a: 1, b: 2.5 };
    let q = { b: 0.5, c: "y", a: 7 };
    assert!(format("%i %v %s", p.a, p.b, p.c) == "1 2.5 z");
    p = q;
    p.a = 9;
    assert!(q.a == 9);
    assert!(format("%i %v %s", p.a, p.b, p.c) == "9 0.5 y");
    let rows = [{ a: 1, b: 1.0, c: "r" }, { c: "s", b: 2.0, a: 2 }];
    rows[0] = p;
    assert!(rows[1].c == "s" && rows[1].a == 2 && rows[0].c == "y");
    assert!(
        format("%v", rows) == "[{ a: 9, b: 0.5, c: y }, { a: 2, b: 2.0, c: s }]",
        format("%v", rows),
    );
}

test("patterns, generics and iteration read slots by name") {
    let r = { z: 3, m: 2, a: 1 };
    let { m, z } = r;
    assert!(m == 2 && z == 3);
    let s = id(r);
    s.z += 10;
    assert!(r.z == 13);
    let names = "";
    let total = 0;
    for (k, v) in r {
        names += k;
        total += v;
    }
    assert!(names == "amz", names);
    assert!(total == 16);
    assert!(format("%v", r) == "{ a: 1, m: 2, z: 13 }", format("%v", r));
}

test("two-word, unit and record fields keep their slots") {
    let r = { opt: Some(4), u: (), inner: { y: 2, x: 1 }, f: 1.5 };
    let o = r.opt ?? 0;
    assert!(o == 4);
    r.opt = None;
    assert!((r.opt ?? 0) == 0);
    let inner = r.inner;
    inner.x = 5;
    assert!(r.inner.x == 5 && r.inner.y == 2);
    assert!(r.len() == 4);
}
