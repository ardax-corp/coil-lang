// A closure inside a generic function is lowered at each instantiation's
// types: capturing or not, capturing values of the type parameter, and
// returning it (#1524).
use tests::assert;
use string::format;

fn one<T>(T x) -> int {
    let f = fn () use (x) => 1;
    return f();
}

fn plain<T>(T x) -> int {
    let f = fn (int a) => a * 2;
    return f(21);
}

fn keep<T>(T x) -> T {
    let f = fn () use (x) => x;
    return f();
}

fn pick<T>(T a, T b, bool first) -> T {
    let f = fn (bool k) use (a, b) {
        if k {
            return a;
        }
        b
    };
    return f(first);
}

fn show<T: Show>(T x) -> string {
    let f = fn () use (x) => format("<%v>", x);
    return f();
}

fn wrap<T>(T x) -> Option<T> {
    let f = fn () use (x) => Some(x);
    return f();
}

fn apply<T>(T x, fn(T) -> T g) -> T {
    let f = fn () use (x, g) => g(x);
    return f();
}

fn twice<T>(T x) -> (T, T) {
    let f = fn () use (x) => keep(x);
    return (f(), f());
}

fn nested<T>(T x) -> T {
    let outer = fn () use (x) {
        let inner = fn () use (x) => x;
        inner()
    };
    return outer();
}

fn or_else<T>(Option<T> o, T d) -> T {
    return match o {
        Some(v) => v,
        None => d,
    };
}

test("a closure in a generic function") {
    assert!(one(3) == 1);
    assert!(one("s") == 1);
    assert!(plain(1.5) == 42);
    assert!(plain("x") == 42);
}

test("a closure returns a captured type parameter value") {
    assert!(keep(7) == 7);
    assert!(keep("seven") == "seven");
    let (a, b) = keep((1, 2));
    assert!(a + b == 3);
    let o = keep(Some(5));
    assert!(or_else(o, 0) == 5);
    let n: Option<int> = keep(None);
    assert!(or_else(n, -1) == -1);
    assert!(keep(2.5) == 2.5);
}

test("a closure picks between captured values") {
    assert!(pick(1, 2, true) == 1);
    assert!(pick("a", "b", false) == "b");
    let (x, y) = pick((1, 2), (3, 4), false);
    assert!(x + y == 7);
}

test("a closure formats a captured value") {
    assert!(show(4) == "<4>");
    assert!(show("q") == "<q>");
}

test("a closure wraps a captured value") {
    assert!(or_else(wrap(3), 0) == 3);
    assert!(or_else(wrap("w"), "") == "w");
}

test("a closure calls a captured function") {
    assert!(apply(4, fn (int v) => v + 1) == 5);
    assert!(apply("a", fn (string v) => v + "b") == "ab");
}

test("a closure calls a generic function") {
    let (a, b) = twice(6);
    assert!(a + b == 12);
    let (c, d) = twice("z");
    assert!(c + d == "zz");
}

test("a closure inside a closure in a generic function") {
    assert!(nested(9) == 9);
    assert!(nested("n") == "n");
}
