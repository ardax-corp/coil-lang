// A closure whose result holds a type variable nothing pins (`fn () =>
// Ok(2)`: the error type is never named) is a plain closure, not a
// polymorphic `PolyFn` value: its calls pass and return words as any
// closure's do (#1520).
use tests::assert;

test("closure with an open result") {
    let f = fn () => Ok(2);
    let g = match f() {
        Ok(x) => x,
        Err(_) => -1,
    };
    assert!(g == 2);
}

test("closure with a parameter and an open result") {
    let f = fn (int x) => Ok(x + 1);
    let g = match f(4) {
        Ok(x) => x,
        Err(_) => -1,
    };
    assert!(g == 5);
}

test("capturing closure with an open result") {
    let base = 10;
    let f = fn (int x) use (base) => Ok(base + x);
    let a = match f(1) {
        Ok(x) => x,
        Err(_) => -1,
    };
    let b = match f(2) {
        Ok(x) => x,
        Err(_) => -1,
    };
    assert!(a + b == 23);
}

test("closure returning an open none") {
    let f = fn () => None;
    let g = match f() {
        Some(_) => 1,
        None => 0,
    };
    assert!(g == 0);
}
