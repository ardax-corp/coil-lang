// A named function passed where a generic callee calls it with boxed
// arguments goes wrapped in a closure adapter that unboxes them: the
// adapter is a function of its own with direct MIR, and the call site
// lowers to direct MIR too (`task::blocking_with` was an internal error).
use tests::assert;
use task;

fn sq(int n) -> int {
    return n * n;
}

fn half(float x) -> float {
    return x / 2.0;
}

fn apply<A, T>(A -> T f, A x) -> T {
    return f(x);
}

fn blocking_sq() -> int {
    return match task::blocking_with(sq, 6) {
        Result::Ok(v) => v,
        Result::Err(_) => -1,
    };
}

fn run(unit -> int f) -> int {
    return f();
}

test("blocking_with of a named function") {
    assert!(blocking_sq() == 36);
}

test("blocking_with in a lambda") {
    let k = 1;
    let r = run(
        fn () use (k) {
            match task::blocking_with(sq, 7) {
                Result::Ok(v) => v + k,
                Result::Err(_) => -1,
            }
        },
    );
    assert!(r == 50);
}

test("generic apply of named functions") {
    assert!(apply(sq, 9) == 81);
    assert!(apply(half, 5.0) == 2.5);
    assert!(apply(sq, apply(sq, 2)) == 16);
}
