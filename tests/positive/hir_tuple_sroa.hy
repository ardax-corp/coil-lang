// Local tuples split into one local per element in HIR: swaps that read
// the tuple they overwrite, destructuring lets, nested destructures, and
// enum reassignments that read their own payload.
fn fib_pair(int n) -> int {
    let t = (0, 1);
    let i = 0;
    while i < n {
        t = (t[1], t[0] + t[1]);
        i = i + 1;
    }
    return t[0];
}

fn swap_sum(int a, int b) -> int {
    let t = (a, b);
    t = (t[1], t[0]);
    let (x, y) = t;
    return x * 10 + y;
}

fn nested(int a) -> int {
    let (p, (q, r)) = (a, (a + 1, a + 2));
    let { lo, hi } = { lo: p, hi: r };
    return p + q + r + hi - lo;
}

fn discard(int a) -> int {
    let (_, b) = (a * 3, a + 5);
    return b;
}

fn grow(int n) -> int {
    let o = Option::Some(1);
    let i = 0;
    while i < n {
        o = match o {
            Option::Some(v) => Option::Some(v * 2),
            Option::None => Option::None,
        };
        i = i + 1;
    }
    return match o {
        Option::Some(v) => v,
        Option::None => 0,
    };
}

test("swap reads the tuple it overwrites") {
    assert(fib_pair(10) == 55)?;
    assert(swap_sum(1, 2) == 21)?;
}

test("destructuring lets") {
    assert(nested(1) == 8)?;
    assert(discard(4) == 9)?;
}

test("enum reassignment reads its own payload") {
    assert(grow(5) == 32)?;
}
