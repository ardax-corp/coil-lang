// Plain scalar calls in a body lowered straight from HIR to MIR: int,
// float and bool arguments and results, a call made for its effect, a
// self call, and calls inside a loop.

fn sq(int x) -> int {
    return x * x;
}

fn scale(float x, float k) -> float {
    return x * k + 0.5;
}

fn odd(int x) -> bool {
    return x % 2 == 1;
}

fn pick(bool c, int a, int b) -> int {
    if c {
        return a;
    }
    return b;
}

fn fact(int n) -> int {
    if n <= 1 {
        return 1;
    }
    return n * fact(n - 1);
}

fn noop(int x) {
    let y = x + 1;
}

fn sum_squares(int n) -> int {
    let total = 0;
    for i in 0..n {
        noop(i);
        total = total + sq(i) + pick(odd(i), 1, 0);
    }
    return total;
}

fn scaled_sum(int n) -> float {
    let acc = 0.0;
    let i = 0;
    while i < n {
        acc = acc + scale(i as float, 2.0);
        i = i + 1;
    }
    return acc;
}

test("scalar calls from a direct MIR loop") {
    assert(sum_squares(10) == 290)?;
    assert(scaled_sum(4) == 14.0)?;
    assert(fact(10) * 2 == 7257600)?;
}
