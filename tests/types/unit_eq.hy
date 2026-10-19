// `==` / `!=` on `()` values: unit has one value, so `==` is always true
// and `!=` always false, after both sides run for their effects (#1426).
use tests::assert;

static let calls: int = 0;

fn unit_fn() {}

fn bump() {
    calls = calls + 1;
}

fn ok_is_unit(Result<unit, string> r) -> bool {
    return match r {
        Result::Ok(u) => u == (),
        Result::Err(_) => false,
    };
}

test("unit literals compare equal") {
    let b = () == ();
    assert!(b);
    assert!(!(() != ()));
}

test("a unit call result compares equal to unit") {
    assert!(() == unit_fn());
    assert!(!(unit_fn() != ()));
}

test("a bound unit payload compares equal to unit") {
    assert!(ok_is_unit(Ok(())));
    assert!(!ok_is_unit(Err("no")));
}

test("both sides of a unit comparison run") {
    calls = 0;
    let b = bump() == bump();
    assert!(b);
    assert!(calls == 2);
}
