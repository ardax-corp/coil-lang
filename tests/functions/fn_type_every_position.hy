// The `fn(T x) -> R` / `fn(T) -> R` function-type spelling works in every
// type position, alongside the arrow spelling `T -> R` (#1389, #1391).
use tests::assert;

type IntFn = fn(int x) -> int;
type BinFn = fn(int, int) -> int;

class Holder {
    pub op: fn(int) -> int,
    pub base: int,
}

fn apply(fn(int x) -> int f, int v) -> int {
    return f(v);
}

fn apply_unnamed(fn(int) -> int f, int v) -> int {
    return f(v);
}

fn apply_paren((fn(int x) -> int) f, int v) -> int {
    return f(v);
}

fn apply_arrow(int -> int f, int v) -> int {
    return f(v);
}

fn apply_alias(IntFn f, int v) -> int {
    return f(v);
}

fn combine(BinFn f, int a, int b) -> int {
    return f(a, b);
}

fn twice(fn(fn(int) -> int g, int v) -> int run, int v) -> int {
    return run(fn (int x) => x * 2, v);
}

fn mk(int k) -> fn(int x) -> int {
    return fn (int y) => y + k;
}

test("fn type as a parameter type") {
    assert!(apply(fn (int x) => x + 1, 4) == 5);
    assert!(apply_unnamed(fn (int x) => x * 3, 4) == 12);
    assert!(apply_paren(fn (int x) => x - 1, 4) == 3);
    assert!(apply_arrow(fn (int x) => x + 10, 4) == 14);
    assert!(apply(mk(5), 1) == 6);
    assert!(twice(apply, 21) == 42);
}

test("fn type as a type alias target") {
    let f: IntFn = fn (int x) => x + 100;
    assert!(apply_alias(f, 1) == 101);
    assert!(apply(f, 2) == 102);
    assert!(combine(fn (int a, int b) => a * b, 6, 7) == 42);
}

test("fn type in let annotations and generic args") {
    let g: fn(int) -> int = fn (int x) => x * x;
    assert!(g(9) == 81);
    let fs: Vec<fn(int) -> int> = [fn (int x) => x + 1, fn (int x) => x * 10];
    assert!(fs[0](1) == 2);
    assert!(fs[1](2) == 20);
}

test("fn type as a class field type") {
    let h = new Holder(fn (int x) => x + 7, 3);
    assert!(h.op(h.base) == 10);
}
