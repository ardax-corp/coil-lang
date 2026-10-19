// A nullary function type `fn() -> R` means `unit -> R`, the same as the
// arrow spelling, in every type position (#1525).
use tests::assert;

type Thunk = fn() -> int;

class Lazy {
    pub make: fn() -> int,
}

fn at(fn() -> int f) -> int {
    return f();
}

fn at_arrow(unit -> int f) -> int {
    return f();
}

fn at_alias(Thunk f) -> int {
    return f();
}

fn konst(int k) -> fn() -> int {
    return fn () use (k) => k;
}

fn maker<T>(T x) -> fn() -> T {
    return fn () use (x) => x;
}

fn run_twice(fn(fn() -> int g) -> int run) -> int {
    return run(fn () => 20) + run(fn () => 1);
}

test("nullary fn type as a parameter and return type") {
    assert!(at(fn () => 1) == 1);
    assert!(at_arrow(fn () => 2) == 2);
    assert!(at(konst(3)) == 3);
    assert!(at_arrow(konst(4)) == 4);
    let m = maker("hi");
    assert!(m() == "hi");
    assert!(at(maker(5)) == 5);
    assert!(run_twice(fn (fn() -> int g) => g() * 2) == 42);
}

test("nullary fn type in let, alias, generic arg and field") {
    let g: fn() -> int = fn () => 2;
    assert!(g() == 2);
    let t: Thunk = fn () => 6;
    assert!(at_alias(t) == 6);
    assert!(at(t) == 6);
    let fs: Vec<fn() -> int> = [fn () => 7, fn () => 8];
    assert!(fs[0]() + fs[1]() == 15);
    let l = new Lazy(fn () => 9);
    let mk = l.make;
    assert!(mk() == 9);
}
