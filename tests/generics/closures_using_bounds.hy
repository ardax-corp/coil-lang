// A closure inside a bounded generic function uses the function's own
// bound: `%v` through `Show`, `<` through `Ord`, `==` through `Eq`,
// arithmetic through `Num`, a call forwarding the bound, a nested
// closure, and a returned closure (#1526).
use tests::assert;
use string::format;

fn show<T: Show>(T x) -> string {
    let f = fn () use (x) => format("<%v>", x);
    return f();
}

fn big<T: Ord>(T a, T b) -> T {
    let f = fn () use (a, b) {
        if a < b {
            return b;
        }
        a
    };
    return f();
}

fn same<T: Eq>(T a, T b) -> bool {
    let f = fn (T c) use (a) => a == c;
    return f(b);
}

fn sum3<T: Num>(T a, T b, T c) -> T {
    let f = fn (T d) use (a, b) => a + b + d;
    return f(c);
}

fn twice_shown<T: Show>(T x) -> string {
    let f = fn () use (x) => show(x) + show(x);
    return f();
}

fn nested<T: Show>(T x) -> string {
    let outer = fn () use (x) {
        let inner = fn () use (x) => format("[%v]", x);
        inner()
    };
    return outer();
}

fn shower<T: Show>(T x) -> unit -> string {
    return fn () use (x) => format("(%v)", x);
}

fn largest<T: Ord>(Vec<T> xs, T start) -> T {
    let pick = fn (T a, T b) {
        if a < b {
            return b;
        }
        a
    };
    let best = start;
    for x in xs {
        best = pick(best, x);
    }
    return best;
}

test("format a bound value in a closure") {
    assert!(show(4) == "<4>");
    assert!(show("s") == "<s>");
    assert!(show(2.5) == "<2.5>");
    assert!(show(true) == "<true>");
}

test("compare bound values in a closure") {
    assert!(big(3, 9) == 9);
    assert!(big(9, 3) == 9);
    assert!(big(1.5, -1.5) == 1.5);
    assert!(same(3, 3));
    assert!(!same("x", "y"));
    assert!(largest([4, 8, 2], 0) == 8);
    assert!(largest([0.5, 2.5, 1.5], 0.0) == 2.5);
}

test("arithmetic on bound values in a closure") {
    assert!(sum3(1, 2, 3) == 6);
    assert!(sum3(0.5, 0.25, 0.25) == 1.0);
}

test("a closure forwards the bound, nests, and is returned") {
    assert!(twice_shown(7) == "<7><7>");
    assert!(nested(5) == "[5]");
    assert!(nested("n") == "[n]");
    let s = shower(6);
    assert!(s() == "(6)");
    let t = shower("t");
    assert!(t() == "(t)");
}
