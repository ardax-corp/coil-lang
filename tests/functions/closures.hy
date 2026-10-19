// Lambdas with captures, named functions read as values, and calls through
// function values with word, string, class and enum arguments and results.
use tests::assert;
use gc::collect;

class Cell {
    pub v: int,
}

fn apply(int -> int f, int x) -> int {
    return f(x);
}

fn applyf(float -> float f, float x) -> float {
    return f(x);
}

fn test_both(int -> bool p, int a, int b) -> bool {
    return p(a) && p(b);
}

fn shout(string -> string f, string s) -> string {
    return f(s) + "!";
}

fn half(int n) -> Option<int> {
    if n % 2 == 1 {
        return Option::None;
    }
    return Option::Some(n / 2);
}

fn parse(int n) -> Result<int, string> {
    if n < 0 {
        raise "negative";
    }
    return n * 10;
}

fn first_half(int -> Option<int> f, int x) -> int {
    return f(x) ?? -1;
}

fn parsed(int -> Result<int, string> f, int x) -> int {
    return match f(x) {
        Result::Ok(v) => v,
        Result::Err(_) => -1,
    };
}

fn inc(int x) -> int {
    return x + 1;
}

fn sum_with(int -> int f, int n) -> int {
    let total = 0;
    for i in 0..n {
        total = total + f(i);
    }
    return total;
}

fn scaled(int k, int n) -> int {
    let f = fn (int x) use (k) => x * k;
    return sum_with(f, n);
}

fn greet(string name) -> string {
    let g = fn (string s) use (name) => s + ", " + name;
    return shout(g, "hello");
}

fn boxed(int v) -> int {
    let b = new Cell(v);
    let get = fn (int x) use (b) => b.v + x;
    collect();
    return apply(get, 1);
}

fn many(int n) -> int {
    let acc = 0;
    let tag = "k";
    for i in 0..n {
        let f = fn (int x) use (i, tag) => x + i + tag.len();
        if i % 64 == 0 {
            collect();
        }
        acc = acc + apply(f, 1);
    }
    return acc;
}

fn named(int x) -> int {
    let f = inc;
    return f(f(x));
}

fn pick(bool hi) -> int -> int {
    if hi {
        return inc;
    }
    return fn (int x) => x - 1;
}

test("captures of a word and a string") {
    assert!(scaled(3, 4) == 18);
    assert!(greet("coil") == "hello, coil!");
}

test("a capture of a class object across a collection") {
    assert!(boxed(41) == 42);
}

test("closures made in a loop") {
    assert!(many(200) == 200 * 2 + 199 * 200 / 2);
}

test("named functions as values") {
    assert!(named(5) == 7);
    assert!(apply(inc, 9) == 10);
    assert!(sum_with(inc, 3) == 6);
    assert!(apply(pick(true), 4) == 5);
    assert!(apply(pick(false), 4) == 3);
}

test("float and bool results") {
    assert!(applyf(fn (float x) => x * 0.5, 3.0) == 1.5);
    assert!(test_both(fn (int x) => x > 0, 1, 2));
    assert!(!test_both(fn (int x) => x > 0, 1, -2));
}

test("enum results through a function value") {
    assert!(first_half(half, 8) == 4);
    assert!(first_half(half, 7) == -1);
    assert!(parsed(parse, 4) == 40);
    assert!(parsed(parse, -4) == -1);
}
