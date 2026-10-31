// A curried function given every argument at once, when it is imported
// (by its bare name, an alias or its path) or generic (over-applied past
// its declared parameters, or through a type parameter instantiated at a
// function type): the declared arguments go to the named call and the rest
// to the function it returns (#1569).
use tests::assert;
use curry_provider::{adder, adder as add, pair_with};

fn konst<T>(T a) -> T -> T {
    return fn (T b) use (a) => a;
}

fn id<T>(T x) -> T {
    return x;
}

fn add2(int a, int b) -> int {
    return a * 10 + b;
}

fn local_adder(int a) -> int -> int {
    return fn (int b) use (a) => a * 10 + b;
}

test("an imported curried function by its bare name") {
    assert!(adder(1, 2) == 12);
    assert!(add(3, 4) == 34);
    assert!(curry_provider::adder(5, 6) == 56);
}

test("an imported curried function one call at a time") {
    let g = adder(1);
    assert!(g(2) == 12);
    assert!(adder(1)(2) == 12);
}

test("a generic curried function given every argument") {
    assert!(konst(1, 2) == 1);
    assert!(konst("x", "y") == "x");
    assert!(pair_with(1, 2) == "1,2");
    assert!(curry_provider::pair_with("a", "b") == "a,b");
}

test("a generic result instantiated at a function type") {
    assert!(id(add2, 1, 2) == 12);
    assert!(id(local_adder, 1, 2) == 12);
}
