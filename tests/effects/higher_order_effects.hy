// A declaration covers what a function does itself: a higher-order
// function's call of its function parameter stays open, and each caller
// pays for what it passes. Passing pure code keeps the caller pure.
use tests::assert;

static let HITS: int = 0;

fn apply(int -> int f, int x) -> int uses {} {
    return f(x);
}

pure fn twice(int -> int f, int x) -> int {
    return f(f(x));
}

pure fn fold(Vec<int> xs, int seed, int -> int -> int step) -> int {
    let acc = seed;
    for x in xs {
        acc = step(acc, x);
    }
    return acc;
}

fn inc(int x) -> int {
    return x + 1;
}

fn bump(int x) -> int {
    HITS = HITS + 1;
    return x + HITS;
}

// Pure: every function it passes is pure.
pure fn pure_caller(int x) -> int {
    let a = apply(fn (int v) => v * 3, x);
    let b = twice(inc, x);
    let xs: Vec<int> = Vec::from([1, 2, 3]);
    let c = fold(xs, 0, fn (int s, int v) => s + v);
    return a + b + c;
}

// Pays for `bump` itself.
fn counting_caller(int x) -> int uses {read, mutate} {
    return apply(bump, x) + twice(fn (int v) => bump(v), x);
}

test("passing pure functions keeps the caller pure") {
    assert!(pure_caller(2) == 6 + 4 + 6);
}

test("a pure higher-order function runs impure arguments") {
    let before = HITS;
    assert!(apply(fn (int v) => bump(v), 0) == before + 1);
    assert!(HITS == before + 1);
    counting_caller(0);
    assert!(HITS == before + 4);
}
