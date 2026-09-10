// C2: free-fn Range param and returned Range for-in.
fn sum_param(Range<int> r) -> int {
    let acc = 0;
    for x in r {
        acc = acc + x;
    }
    return acc;
}

fn make(int n) -> Range<int> {
    return 0..n;
}

fn sum_ret(int n) -> int {
    let acc = 0;
    for x in make(n) {
        acc = acc + x;
    }
    return acc;
}

fn sum_inc(RangeInclusive<int> r) -> int {
    let acc = 0;
    for x in r {
        acc = acc + x;
    }
    return acc;
}

test("param range for-in") {
    assert_eq(sum_param(0..5), 10);
}

test("returned range for-in") {
    assert_eq(sum_ret(5), 10);
}

test("inclusive range param") {
    assert_eq(sum_inc(0..=4), 10);
}
