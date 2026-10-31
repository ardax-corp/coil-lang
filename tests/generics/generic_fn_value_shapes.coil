// Generic fns read as values (bare `T` params, a `T` nested in a tuple
// return) and a free generic fn whose `Option<T>` return type is inferred.
use tests::assert;

fn both<T>(T a, T b) -> T {
    return b;
}

fn wrap<T>(T a) -> (T, int) {
    return (a, 1);
}

fn some_of<T>(T x) {
    return Option::Some(x);
}

test("a generic fn with bare type params as a value") {
    let f = both;
    assert!(f(1, 2) == 2);
}

test("a generic fn returning a tuple with T as a value") {
    let f = wrap;
    let (a, n) = f(7);
    assert!(a == 7);
    assert!(n == 1);
}

test("an inferred Option<T> return") {
    let r = match some_of(7) {
        Option::Some(v) => v,
        Option::None => 0,
    };
    assert!(r == 7);
}
