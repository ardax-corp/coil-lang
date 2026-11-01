// Indirect calls of closures: an exact call moves its arguments above the
// captures in place (more captures than arguments and the other way round),
// a partial call keeps the captures, and a generic function value merges
// its dictionaries.
use tests::assert;

fn call2(int -> int -> int f, int a, int b) -> int {
    return f(a, b);
}

fn call4(int -> int -> int -> int -> int f) -> int {
    return f(1, 2, 3, 4);
}

fn sum<T: Num>(T a, T b) -> T {
    return a + b;
}

test("more captures than arguments") {
    let x = 100;
    let y = 20;
    let z = 3;
    let f = fn (int a, int b) use (x, y, z) => x + y + z + a * 1000 + b * 10000;
    assert!(call2(f, 4, 5) == 54123);
}

test("more arguments than captures") {
    let k = 7;
    let f = fn (int a, int b, int c, int d) use (k) => k * (a + 10 * b + 100 * c + 1000 * d);
    assert!(call4(f) == 7 * 4321);
}

test("calls in a loop keep the captures") {
    let k = 3;
    let f = fn (int a, int b) use (k) => a + b + k;
    let acc = 0;
    let i = 0;
    while i < 1000 {
        acc = call2(f, acc, 1) % 1000003;
        i = i + 1;
    }
    assert!(acc == 4000);
}

test("generic function value") {
    let add = sum;
    let i = 0;
    let acc = 0;
    while i < 100 {
        acc = add(acc, i);
        i = i + 1;
    }
    assert!(acc == 4950);
}
