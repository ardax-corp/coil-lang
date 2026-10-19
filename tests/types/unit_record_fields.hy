// An anonymous record may hold `()` fields: built, read, shown and
// destructured (#1510).
use tests::assert;
use string::format;

fn mark() {}

fn take(unit u) -> int {
    return 7;
}

test("a record with a unit field reads its other fields") {
    let r = { a: 1, u: () };
    assert!(r.a == 1);
}

test("a record with a unit field is shown") {
    let r = { a: 1, u: () };
    let s = format("%v", r);
    assert!(s == "{ a: 1, u: () }", s);
    assert!(format("%v", { u: mark() }) == "{ u: () }");
}

test("a record's unit field is read") {
    let r = { a: 2, u: () };
    assert!(r.u == ());
    assert!(take(r.u) == 7);
}

test("a record with a unit field is destructured") {
    let { a, u } = { a: 4, u: () };
    assert!(a == 4);
    assert!(u == ());
}

test("a record's unit field is assigned") {
    let r = { a: 1, u: () };
    r.u = mark();
    r.a = 5;
    assert!(format("%v", r) == "{ a: 5, u: () }");
    let v = [{ a: 1, u: () }, { a: 2, u: mark() }];
    assert!(v[1].a == 2);
}
