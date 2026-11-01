// A function value of type `int -> int -> int` may take both arguments at
// once or one at a time: calling it with more arguments than its closure
// declares applies the declared ones, then calls the function it returns
// with the rest (#1550).
use tests::assert;
use gc::collect;

fn two() -> int -> int -> int {
    return fn (int a, int b) => a * 10 + b;
}

fn curried() -> int -> int -> int {
    return fn (int a) => fn (int b) use (a) => a * 10 + b;
}

fn adder(int a) -> int -> int {
    return fn (int b) use (a) => a * 10 + b;
}

fn at2(int -> int -> int f) -> int {
    return f(1, 2);
}

fn at1(int -> int -> int f) -> int {
    let g = f(1);
    return g(2);
}

fn at3(int -> int -> int -> int f) -> int {
    return f(1, 2, 3);
}

test("both call shapes on a two-parameter closure") {
    assert!(at2(two()) == 12);
    assert!(at1(two()) == 12);
}

test("both call shapes on a curried closure") {
    assert!(at1(curried()) == 12);
    assert!(at2(curried()) == 12);
}

test("a named curried function called with every argument") {
    assert!(adder(1, 2) == 12);
    assert!(at2(adder) == 12);
}

test("over-application splits at each closure's own arity") {
    let k = 5;
    let one_then_two = fn (int a) use (k) => fn (int b, int c) use (a, k) => k * 1000 + a * 100 +
                                                                             b * 10 + c;
    let two_then_one = fn (int a, int b) use (k) => fn (int c) use (a, b, k) => k * 1000 + a * 100 +
                                                                                b * 10 + c;
    let one_at_a_time = fn (int a) => fn (int b) use (a) => fn (int c) use (a, b) => a * 100 +
                                                                                     b * 10 + c;
    assert!(at3(one_then_two) == 5123);
    assert!(at3(two_then_one) == 5123);
    assert!(at3(one_at_a_time) == 123);
}

test("a partial application called with more than it lacks") {
    let f = fn (int a, int b) => fn (int c) use (a, b) => a * 100 + b * 10 + c;
    let g = f(1);
    assert!(g(2, 3) == 123);
}

class Box1 {
    pub v: int,
}

fn join(string -> Box1 -> string f) -> string {
    return f("ab", new Box1(7));
}

test("the waiting arguments survive a collection") {
    let f = fn (string s) {
        let junk = 0;
        while junk < 2000 {
            let b = new Box1(junk);
            junk = junk + b.v - junk + 1;
        }
        collect();
        fn (Box1 b) use (s) => string::format("%s%i", s, b.v)
    };
    assert!(join(f) == "ab7");
}

test("the first call runs before the second") {
    let log = Vec::new();
    let f = fn (int a) use (mut log) {
        log.push(a);
        fn (int b) use (mut log) => log.len() * 10 + b
    };
    assert!(at2(f) == 12);
    assert!(log.len() == 1);
}
