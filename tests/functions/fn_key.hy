// `symbol::fn_key()` is a compile-time key of the body it is written in: the
// same `int` on every call, whatever the arguments or the caller, and a
// different one in every other function, method, overload and lambda.
use tests::assert;
use symbol::fn_key;

fn plain(int x) -> int {
    return fn_key();
}

fn other(int x) -> int {
    return fn_key();
}

fn twice() -> (int, int) {
    let a = fn_key();
    let b = fn_key();
    return (a, b);
}

fn arity(int x) -> int {
    return fn_key();
}

fn arity(int x, int y) -> int {
    return fn_key();
}

fn ident<T>(T x) -> int {
    return fn_key();
}

fn small() -> int {
    return fn_key();
}

fn through(fn() -> int f) -> int {
    return f();
}

fn lambdas() -> (int, int, int) {
    let f = fn () => fn_key();
    let g = fn () => fn_key();
    return (f(), g(), fn_key());
}

fn nested() -> (int, int) {
    let outer = fn () {
        let inner = fn () => fn_key();
        return inner();
    };
    let again = fn () {
        let inner = fn () => fn_key();
        return inner();
    };
    return (outer(), again());
}

class Counter {
    pub n: int,
}

impl Counter {
    pub fn key() -> int {
        return fn_key();
    }

    pub static fn make_key() -> int {
        return fn_key();
    }
}

trait Keyed<T> {
    fn keyed(T x) -> int {}
}

impl Keyed for int {
    fn keyed(int x) -> int {
        return fn_key();
    }
}

impl Keyed for string {
    fn keyed(string x) -> int {
        return fn_key();
    }
}

test("the same key on every call, whatever the arguments") {
    assert!(plain(1) == plain(2));
    assert!(plain(-5) == plain(1000));
    let (a, b) = twice();
    assert!(a == b);
    assert!(twice() == twice());
}

test("a different key in every function") {
    assert!(plain(1) != other(1));
    assert!(plain(1) != small());
    assert!(arity(1) != arity(1, 2));
}

test("keys are non-negative ints") {
    assert!(plain(0) >= 0);
    assert!(small() >= 0);
}

test("inlining keeps the callee's key") {
    // `small` is inlined here; through a function value it is a real call.
    assert!(small() == through(small));
    assert!(small() != fn_key());
}

test("generic instantiations share their function's key") {
    assert!(ident(1) == ident("one"));
    assert!(ident(1.5) == ident(true));
}

test("each lambda has its own key") {
    let (f, g, outer) = lambdas();
    assert!(f != g);
    assert!(f != outer);
    assert!(lambdas() == lambdas());
    let (a, b) = nested();
    assert!(a != b);
    assert!(nested() == nested());
}

test("methods have their own keys") {
    let c = new Counter(1);
    let d = new Counter(2);
    assert!(c.key() == d.key());
    assert!(c.key() != Counter::make_key());
    assert!(keyed(1) == keyed(2));
    assert!(keyed(1) != keyed("a"));
    assert!(keyed(1) != c.key());
}

test("a test body has a key too") {
    assert!(fn_key() == fn_key());
    assert!(fn_key() != plain(1));
}
