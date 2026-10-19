// A closure inside a generic function is lowered at each instantiation's
// types: capturing or not, capturing values of the type parameter, and
// returning it, called in place, passed on or returned (#1524).
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

fn hold<T>(T x) -> T {
    let f = fn () use (x) => x;
    return f();
}

fn both<T>(T x) -> (T, T) {
    let f = fn () use (x) => (x, x);
    return f();
}

fn tag<T>(T x) -> (T, int) {
    let f = fn (int n) use (x) => (x, n);
    return f(7);
}

fn shown<T: Show>(T x) -> string {
    let f = fn () use (x) => x;
    return format("<%v>", f());
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

fn app<U>(U x, U -> U g) -> U {
    return g(x);
}

fn via<T>(T x) -> T {
    return app(x, fn (T v) => v);
}

fn via2<T>(T x, T y) -> T {
    return app(x, fn (T v) use (y) => y);
}

fn maker<T>(T x) -> unit -> T {
    return fn () use (x) => x;
}

fn at_int(unit -> int f) -> int {
    return f();
}

fn at_str(unit -> string f) -> string {
    return f();
}

fn at_pair(unit -> (int, int) f) -> int {
    let (a, b) = f();
    return a + b;
}

fn at_opt(unit -> Option<int> f) -> int {
    return match f() {
        Some(v) => v,
        None => -1,
    };
}

class Cell {
    pub v: int,
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
    assert!(hold(true));
    assert!(hold(new Cell(6)).v == 6);
    let v = hold([1, 2, 3]);
    assert!(len(v) == 3);
    assert!(v[2] == 3);
    let r: Result<int, string> = hold(Err("bad"));
    let m = match r {
        Ok(_) => "",
        Err(e) => e,
    };
    assert!(m == "bad");
}

test("a closure in a bounded generic function") {
    assert!(shown(4) == "<4>");
    assert!(shown("q") == "<q>");
}

test("a closure picks between captured values") {
    assert!(pick(1, 2, true) == 1);
    assert!(pick("a", "b", false) == "b");
    let (x, y) = pick((1, 2), (3, 4), false);
    assert!(x + y == 7);
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

test("a closure passed to a generic call in a generic function") {
    assert!(via(5) == 5);
    assert!(via("v") == "v");
    assert!(via(0.5) == 0.5);
    assert!(via2(5, 6) == 6);
    assert!(via2("a", "b") == "b");
    let (a, b) = via2((1, 2), (3, 4));
    assert!(a * b == 12);
    assert!(or_else(via(Some(3)), 0) == 3);
}

test("a closure returned from a generic function") {
    assert!(at_int(maker(41)) == 41);
    assert!(at_str(maker("s")) == "s");
    assert!(at_pair(maker((3, 4))) == 7);
    assert!(at_opt(maker(Some(8))) == 8);
    assert!(at_opt(maker(None)) == -1);
}

test("a closure returns a tuple of a type parameter") {
    let (a, b) = both(5);
    assert!(a + b == 10);
    let (c, d) = both("p");
    assert!(c + d == "pp");
    let (e, n) = tag("q");
    assert!(e == "q" && n == 7);
    let (g, m) = tag(1.5);
    assert!(g == 1.5 && m == 7);
}

test("a generic function with a closure read as a value") {
    let t = both;
    let (a, b) = t(5);
    let (c, d) = t("p");
    assert!(a + b == 10);
    assert!(c + d == "pp");
    let k = keep;
    assert!(k(3) == 3);
    assert!(k("z") == "z");
}

test("a closure returns a heap tuple") {
    let s = "s";
    let f = fn () use (s) => (s, s);
    let (a, b) = f();
    assert!(a + b == "ss");
}
