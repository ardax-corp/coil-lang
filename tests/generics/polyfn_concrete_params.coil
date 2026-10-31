// #1089: a generic function read as a value (`PolyFn`) passes its concrete
// parameters unboxed; only bare type-parameter ones are boxed.
use tests::assert;

fn second<T: Num>(T a, int n) -> int {
    return n;
}

fn first_concrete<T>(int n, T a) -> int {
    return n * 2;
}

fn count_to<T>(T tag, int n) -> int {
    let i = 0;
    while i < n {
        i += 1;
    }
    return i;
}

fn mixed<T: Num>(T a, int n, T b) -> T {
    return a + b;
}

test("concrete parameter after a type parameter") {
    let g = second;
    assert!(g(1, 5) == 5);
}

test("concrete parameter before a type parameter") {
    let g = first_concrete;
    assert!(g(21, "x") == 42);
}

test("loop bounded by a concrete parameter") {
    let g = count_to;
    assert!(g(true, 3) == 3);
}

test("bare type parameters still box and unbox") {
    let g = mixed;
    assert!(g(2, 100, 3) == 5);
}
