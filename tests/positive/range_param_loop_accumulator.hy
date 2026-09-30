// A loop-carried accumulator stays one variable when a Range parameter's
// copies are folded away (slot_promote peel-floor raise at -O2).

fn rsum(Range<int> r) -> int {
    let s = 0;
    for x in r {
        s = s + x;
    }
    return s;
}

fn rsum_inclusive(RangeInclusive<int> r) -> int {
    let s = 0;
    for x in r {
        s = s + x;
    }
    return s;
}

fn rcount_after(Range<int> r, int base) -> int {
    let n = base;
    for _ in r {
        n = n + 1;
    }
    return n;
}

test("sum over a Range parameter") {
    assert(rsum(0..4) == 6)?;
    assert(rsum(3..3) == 0)?;
    assert(rsum(1..11) == 55)?;
}

test("sum over a RangeInclusive parameter") {
    assert(rsum_inclusive(0..=4) == 10)?;
}

test("accumulator seeded from a parameter") {
    assert(rcount_after(0..5, 10) == 15)?;
    assert(rcount_after(2..2, 10) == 10)?;
}
