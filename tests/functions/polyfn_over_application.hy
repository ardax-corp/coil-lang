// A curried generic function held as a polymorphic value (`PolyFn`) and
// called with more arguments than it declares, through a `let`, a
// function-typed parameter or a `forall` parameter: the declared arguments
// go to the function and the rest to the function it returns. The closure
// one call returns is called on in turn (#1589).
use tests::assert;

fn konst<T>(T a) -> T -> T {
    return fn (T b) use (a) => a;
}

fn konst3<T>(T a) -> T -> T -> T {
    return fn (T b) use (a) => fn (T c) use (a, b) => a;
}

fn at1(int -> int -> int f) -> int {
    return f(1)(2);
}

fn at2(int -> int -> int f) -> int {
    return f(1, 2);
}

fn via(forall T. T -> T -> T f) -> int {
    return f(1, 2);
}

fn at3(int -> int -> int -> int f) -> int {
    return f(1, 2, 3);
}

fn ap(int -> int g) -> int {
    return g(2);
}

test("over-applied through a function-typed parameter") {
    assert!(at1(konst) == 1);
    assert!(at2(konst) == 1);
    assert!(at3(konst3) == 1);
}

test("over-applied through a forall parameter") {
    assert!(via(konst) == 1);
}

test("over-applied through a let") {
    let f = konst;
    let x: int = f(1, 2);
    assert!(x == 1);
    let s: string = f("a", "b");
    assert!(s == "a");
}

test("the closure a PolyFn call returns, called on") {
    let f = konst;
    let g = f(1);
    assert!(g(2) == 1);
    assert!(ap(f(1)) == 1);
}

test("a PolyFn bound at a curried function type") {
    let g: int -> int -> int = konst;
    assert!(g(1, 2) == 1);
    assert!(g(1)(2) == 1);
}
