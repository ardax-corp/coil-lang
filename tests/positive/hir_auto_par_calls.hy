// Calls to a pure recursive fork site reach its parallel worker from HIR
// bodies too: a literal argument calls the worker directly, a computed one
// branches on the site's cutoff. A `while` reduce stays a parallel loop.
// Results must match the sequential run.
fn fib(int n) -> int {
    if n < 2 {
        return n;
    }
    return fib(n - 1) + fib(n - 2);
}

fn add(int a, int b, int c) -> int {
    if a < 0 {
        return 0;
    }
    return a + b + c;
}

fn fib_of(int n) -> int {
    return fib(n);
}

test("literal and computed fork-site calls") {
    assert(fib(25) == 75025)?;
    let n = 24;
    assert(fib(n) == 46368)?;
    assert(fib(n - 20) == 3)?;
    assert(fib_of(23) == 28657)?;
}

test("while reduce over a pure call") {
    let acc = 0;
    let i = 0;
    while i < 20000 {
        acc = acc + add(i % 7 - 3, i % 5, i);
        i = i + 1;
    }
    let seq = 0;
    let j = 0;
    while j < 20000 {
        let a = j % 7 - 3;
        if a >= 0 {
            seq = seq + a + j % 5 + j;
        }
        j = j + 1;
    }
    assert(acc == seq)?;
}
