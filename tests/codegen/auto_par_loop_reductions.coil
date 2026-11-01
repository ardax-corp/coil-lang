// Auto-par counted loops (`while` and `for … in` ranges) fold to the
// sequential reduction: sums, products, xor, strides, branches, captures,
// body temps, nested blocks, two loops in one body, loops under the grain.
use tests::assert;

static let HITS: int = 0;

fn sq(int i) -> int {
    return i * i;
}

fn twice_i(int i) -> int {
    return i + i;
}

fn step(int i) -> int {
    return i + 3;
}

fn fib(int n) -> int {
    if n <= 1 {
        return n;
    }
    return fib(n - 1) + fib(n - 2);
}

fn twice(int x, int -> int f) -> int {
    return f(f(x));
}

fn bump(int x) -> int {
    HITS = HITS + 1;
    return x;
}

fn score_pure(int i) -> int {
    return twice(i, fn (int x) => x + 1);
}

fn score_impure(int i) -> int {
    return twice(i, fn (int x) => bump(x));
}

fn while_sum_and_index() -> int {
    let acc = 0;
    let i = 0;
    while i < 100 {
        acc = acc + sq(i);
        i = i + 1;
    }
    return acc * 1000 + i;
}

fn run_dynamic(int n) -> int {
    let acc = 0;
    let i = 0;
    while i < n {
        acc = acc + sq(i);
        i = i + 1;
    }
    return acc;
}

fn run_capture(int k) -> int {
    let acc = 0;
    let i = 0;
    while i < 100 {
        acc = acc + k;
        i = i + 1;
    }
    return acc;
}

fn total(int gate) -> int {
    if gate > 0 {
        let acc = 5;
        let i = 0;
        while i < 50 {
            acc = acc + step(i);
            i = i + 1;
        }
        return acc;
    }
    return 0;
}

fn run_for(int n) -> int {
    let acc = 0;
    for x in 0..n {
        acc = acc + sq(x);
    }
    return acc;
}

test("a while sum leaves its index past the range") {
    assert!(while_sum_and_index() == 328350 * 1000 + 100);
}

test("a loop over a pure and an impure lambda call") {
    let acc = 0;
    let i = 0;
    while i < 100 {
        acc = acc + score_pure(i);
        i = i + 1;
    }
    // sum of i + 2 for i in 0..100
    assert!(acc == 5150);
    HITS = 0;
    let b = 0;
    let j = 0;
    while j < 100 {
        b = b + score_impure(j);
        j = j + 1;
    }
    assert!(b == 4950);
    assert!(HITS == 200);
}

test("a parameter bound above and under the grain") {
    assert!(run_dynamic(40) == 20540);
    assert!(run_dynamic(10) == 285);
}

test("an int parameter capture") {
    assert!(run_capture(3) == 300);
}

test("a pure branch in the body") {
    let acc = 0;
    let i = 0;
    while i < 40 {
        if i % 2 == 0 {
            acc = acc + i;
        }
        i = i + 1;
    }
    assert!(acc == 380);
    assert!(i == 40);
}

test("a constant stride") {
    let acc = 0;
    let i = 0;
    while i < 80 {
        acc = acc + i;
        i = i + 2;
    }
    assert!(acc == 1560);
    assert!(i == 80);
}

test("a product seeds later chunks with one") {
    let prod = 3;
    let i = 0;
    while i <= 29 {
        prod = prod * 2;
        i += 1;
    }
    // 3 * 2^30; seeding every chunk with 3 would give 9 * 2^30.
    assert!(prod == 3221225472);
}

test("loop-private temps") {
    let acc = 0;
    let i = 0;
    while i < 64 {
        let d = twice_i(i);
        let w = d + 1;
        acc = acc + w;
        i = i + 1;
    }
    assert!(acc == 4096);
}

test("an accumulator in a nested block of a function") {
    assert!(total(1) == 1380);
    assert!(total(0) == 0);
}

test("two loops in one body") {
    let a = 0;
    let i = 0;
    while i < 30 {
        a = a + sq(i);
        i = i + 1;
    }
    let b = 0;
    let j = 0;
    while j < 40 {
        b = b + j;
        j = j + 1;
    }
    assert!(a == 8555);
    assert!(b == 780);
}

test("a loop over a recursive pure callee") {
    let acc = 0;
    let i = 0;
    while i < 25 {
        acc = acc + fib(i);
        i = i + 1;
    }
    // fib(26) - 1
    assert!(acc == 121392);
}

test("bit operators in the reduction operand") {
    let acc = 0;
    let i = 0;
    while i < 40 {
        acc = acc + ((i << 1) & 7);
        i = i + 1;
    }
    assert!(acc == 120);
}

test("loops at the trip-count floor stay sequential and agree") {
    let acc = 0;
    let i = 0;
    while i < 20 {
        acc = acc + sq(i);
        i = i + 1;
    }
    assert!(acc == 2470);
    let b = 0;
    for x in 0..20 {
        b = b + sq(x);
    }
    assert!(b == 2470);
}

test("counted for ranges") {
    let acc = 0;
    for x in 0..100 {
        acc = acc + sq(x);
    }
    assert!(acc == 328350);
    let incl = 0;
    for x in 1..=60 {
        incl = incl + sq(x);
    }
    assert!(incl == 73810);
    let r = 0..100;
    let local = 0;
    for x in r {
        local = local + sq(x);
    }
    assert!(local == 328350);
}

test("xor, constant scale and commuted add reductions") {
    let x_acc = 0;
    for x in 0..100 {
        x_acc = x_acc ^ sq(x);
    }
    assert!(x_acc == 13452);
    let scale = 3;
    let s_acc = 0;
    for x in 0..100 {
        s_acc = s_acc + scale * sq(x);
    }
    assert!(s_acc == 985050);
    let c_acc = 0;
    for x in 0..100 {
        c_acc = sq(x) + c_acc;
    }
    assert!(c_acc == 328350);
}

test("a dynamic for range") {
    assert!(run_for(100) == 328350);
}
