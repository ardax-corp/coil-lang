// Each lambda a body plans is queued as a function of its own when the
// body lowers to MIR, not when its stack IL is emitted: lambdas on a cold
// path, in a small function inlined into its caller, inside a loop, and
// passed to a generic callee that boxes their arguments.
use tests::assert;

fn map_all<T, U>(Vec<T> xs, T -> U f) -> Vec<U> {
    let out = Vec::new();
    for x in xs {
        out.push(f(x));
    }
    return out;
}

fn twice(int x) -> int {
    let f = fn (int y) => y * 2;
    return f(x);
}

fn cold(int x) -> int {
    if x < 0 {
        let g = fn (int y) use (x) => y - x;
        return g(1);
    }
    return x;
}

fn looped(int n) -> int {
    let total = 0;
    for i in 0..n {
        let f = fn (int y) use (i) => y + i;
        total = total + f(1);
    }
    return total;
}

fn boxed_args(int k) -> int {
    let ys = map_all(Vec::from([1, 2, 3]), fn (int x) use (k) => x * k);
    return ys[0] + ys[1] + ys[2];
}

test("inlined callee") {
    assert!(twice(21) == 42);
    assert!(twice(1) + twice(2) == 6);
}

test("cold path") {
    assert!(cold(-3) == 4);
    assert!(cold(5) == 5);
}

test("in a loop") {
    assert!(looped(4) == 10);
}

test("generic callee boxes the arguments") {
    assert!(boxed_args(10) == 60);
}
