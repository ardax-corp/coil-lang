// Deep `readonly` still allows every read: nested fields, elements, loop
// variables, generic calls and `const` bindings (#1552).
use tests::assert;

class Inner {
    pub n: int,
}

class Point {
    pub x: int,
    pub inner: Inner,
}

fn id<T>(T v) -> T {
    return v;
}

test("nested field reads") {
    let p = readonly new Point(1, new Inner(2));
    let i = p.inner;
    assert!(p.inner.n == 2);
    assert!(i.n == 2);
}

test("element and loop reads") {
    let xs = readonly [new Inner(1), new Inner(2)];
    let sum = xs[0].n;
    for x in xs {
        sum += x.n;
    }
    assert!(sum == 4);
}

test("generic calls keep the value readable") {
    let p = readonly new Point(3, new Inner(4));
    let q = id(p);
    assert!(q.x == 3);
    assert!(id(p.inner).n == 4);
}

test("const readonly bindings") {
    const p = readonly new Point(5, new Inner(6));
    assert!(p.inner.n == 6);
}

test("a readonly binding can be rebound") {
    let i = readonly new Inner(1);
    i = new Inner(2);
    assert!(i.n == 2);
}

class Holder {
    pub o: Option<Inner>,
}

enum Shape {
    Dot(Inner),
}

test("enum payloads under readonly can be read") {
    let h = readonly new Holder(Option::Some(new Inner(7)));
    let n = match h.o {
        Option::Some(i) => i.n,
        Option::None => 0,
    };
    assert!(n == 7);
    let s: readonly Shape = Shape::Dot(new Inner(8));
    let m = match s {
        Shape::Dot(i) => i.n,
    };
    assert!(m == 8);
}
