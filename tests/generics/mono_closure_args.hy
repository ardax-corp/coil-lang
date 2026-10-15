// A generic function called with a ground closure argument is cloned at
// its type arguments like any other ground call: the clone calls the
// closure with its own words, no boxing or adapter. A type parameter bound
// to a function type stays on the shared body.
use tests::assert;

fn apply_to<T, U>(T x, T -> U f) -> U {
    return f(x);
}

fn fold<T>(Vec<T> xs, T init, T -> T -> T f) -> T {
    let acc = init;
    for x in xs {
        acc = f(acc, x);
    }
    return acc;
}

fn twice<T>(T x, T -> T f) -> T {
    return apply_to(apply_to(x, f), f);
}

fn half(float y) -> float {
    return y / 2.0;
}

test("lambdas over int, string and float") {
    assert!(apply_to(21, fn (int y) => y * 2) == 42);
    assert!(apply_to("abc", fn (string s) => len(s)) == 3);
    assert!(apply_to(5.0, half) == 2.5);
}

test("a capture and a two-argument closure") {
    let k = 7;
    assert!(apply_to(3, fn (int y) use (k) => y + k) == 10);
    let xs: Vec<int> = Vec::new();
    xs.push(1);
    xs.push(2);
    xs.push(3);
    assert!(fold(xs, 0, fn (int a, int b) => a + b) == 6);
    let words: Vec<string> = Vec::new();
    words.push("a");
    words.push("b");
    assert!(fold(words, ">", fn (string a, string b) => a + b) == ">ab");
}

test("a clone calling a generic with its closure") {
    assert!(twice(5, fn (int y) => y + 1) == 7);
    assert!(twice("x", fn (string s) => s + s) == "xxxx");
}

test("a function-typed type argument") {
    let triple = fn (int y) => y * 3;
    assert!(apply_to(triple, fn (int -> int g) => g(2)) == 6);
}
