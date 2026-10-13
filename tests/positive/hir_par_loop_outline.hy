// An auto-parallel loop moves into its own fork-join function: the
// enclosing body calls it with the accumulator, live captures and bounds,
// and the chunk worker is a body of its own. A const `while` range leaves
// its index at the last value; a dynamic one at the larger of its start
// and end.
use tests::assert;

fn sq(int x) -> int {
    return x * x;
}

// Const `while` range, index read after the loop.
fn const_while() -> int {
    let acc = 0;
    let i = 0;
    while i < 100000 {
        acc = acc + sq(i % 10);
        i = i + 1;
    }
    return acc + i;
}

// Dynamic counted `for` with a live capture.
fn dyn_for(int n, int w) -> int {
    let acc = 0;
    for x in 0..n {
        acc = acc + (x % 3) * w;
    }
    return acc;
}

// Dynamic `while` whose index is read after the loop.
fn dyn_while_index(int n) -> int {
    let acc = 0;
    let i = 0;
    while i < n {
        acc = acc + i % 4;
        i = i + 1;
    }
    return acc * 10 + i;
}

// Two sites in one body, the second after the first.
fn two_sites(int n) -> int {
    let a = 0;
    for x in 0..n {
        a = a + x % 3;
    }
    let b = 0;
    for y in 0..n {
        b = b ^ y;
    }
    return a + b;
}

test("const while site leaves its index") {
    // Each 0..9 cycle squares to 285.
    assert!(const_while() == 10000 * 285 + 100000);
}

test("dynamic for site with a live capture") {
    // 0..90000 is 30000 cycles of 0, 1, 2.
    assert!(dyn_for(90000, 2) == 30000 * 3 * 2);
    assert!(dyn_for(4, 5) == 15);
    assert!(dyn_for(0, 5) == 0);
}

test("dynamic while site with its index read after") {
    assert!(dyn_while_index(80000) == 20000 * 6 * 10 + 80000);
    assert!(dyn_while_index(3) == 33);
}

test("two sites in one body") {
    // 0..60000: 20000 cycles of 0, 1, 2; xor of 0..59999 (a multiple of 4) is 0.
    assert!(two_sites(60000) == 60000);
    assert!(two_sites(5) == 4 + 4);
}
