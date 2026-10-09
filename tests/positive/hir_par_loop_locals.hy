// An auto-parallel loop's body runs in the chunk worker's own frame: its
// `let`s bind there, and the index, accumulator and live captures arrive as
// the worker's slots. `sum_halves` (an `Option` callee) stays sequential.

fn half(int x) -> Option<int> {
    if x % 2 == 0 {
        return Option::Some(x / 2);
    }
    return Option::None;
}

fn sum_halves(int n) -> int {
    let acc = 0;
    let i = 0;
    while i < n {
        let h = half(i);
        let v = match h {
            Option::Some(x) => x,
            Option::None => 1,
        };
        acc = acc + v;
        i = i + 1;
    }
    return acc;
}

fn sum_mod(int n) -> int {
    let acc = 0;
    let i = 0;
    while i < n {
        let k = i % 5;
        let m = k * k;
        acc = acc + m;
        i = i + 1;
    }
    return acc;
}

fn weighted(int n, int w) -> int {
    let acc = 0;
    for i in 0..n {
        let k = i % 7;
        acc = acc + k * w;
    }
    return acc;
}

test("while site with body locals") {
    assert(sum_halves(200000) == 5000050000)?;
    assert(sum_halves(10) == 15)?;
    assert(sum_halves(0) == 0)?;
}

test("while site with body locals in the worker") {
    // Each cycle of 0..4 squares to 30.
    assert(sum_mod(100000) == 20000 * 30)?;
    assert(sum_mod(3) == 5)?;
}

test("counted for site with a live capture") {
    // 0..140000 is 20000 full cycles of 0..6 (sum 21).
    assert(weighted(140000, 3) == 20000 * 21 * 3)?;
    assert(weighted(7, 2) == 42)?;
}
