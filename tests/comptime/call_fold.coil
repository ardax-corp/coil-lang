// At -O2 a pure call with constant arguments is computed while compiling.
// One that cannot be (too long, panics, reads a static) stays a call, so
// every result here is the same as at -O0.
use string::format;
use tests::assert;

const K = 7;

const BASE = fib(10);

fn fib(int n) -> int {
    if n <= 1 {
        return n;
    }
    return fib(n - 1) + fib(n - 2);
}

fn half(float x) -> float {
    return x / 2.0;
}

fn is_even(int n) -> bool {
    return n % 2 == 0;
}

fn greet(string who, int times) -> string {
    let out = "";
    let i = 0;
    while i < times {
        out = format("%s%s!", out, who);
        i = i + 1;
    }
    return out;
}

fn plus_k(int x) -> int {
    return x + K;
}

fn plus_base(int x) -> int {
    return x + BASE;
}

fn boom(int n) -> int {
    if n > 0 {
        panic "no";
    }
    return n;
}

static let counter = 0;

fn read_counter(int x) -> int {
    return counter + x;
}

fn main() {
    assert!(fib(20) == 6765);
    assert!(fib(32) == 2178309);
    assert!(half(5.0) == 2.5);
    assert!(is_even(10));
    assert!(!is_even(7));
    assert!(greet("hi", 3) == "hi!hi!hi!");
    assert!(plus_k(1) == 8);
    assert!(plus_base(1) == 56);
    let n = 0;
    if counter > 0 {
        n = boom(1);
    }
    assert!(n == 0);
    counter = 5;
    assert!(read_counter(1) == 6);
    assert!(fib(black_box(10)) == 55);
}
