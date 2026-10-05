// Range values under HIR: `[start, end]` params, locals and results, boxed
// fields, and the `to_vec` thunk.

fn total(Range<int> r) -> int {
    let acc = 0;
    for x in r {
        acc = acc + x;
    }
    return acc;
}

fn total_inc(RangeInclusive<int> r) -> int {
    let acc = 0;
    for x in r {
        acc = acc + x;
    }
    return acc;
}

fn pass(Range<int> r) -> int {
    let copy = r;
    return total(copy);
}

fn upto(int n) -> Range<int> {
    return 0..n;
}

fn through(int n) -> RangeInclusive<int> {
    let r = 1..=n;
    return r;
}

fn shifted(Range<int> r, int k) -> int {
    return total(r) + k;
}

class Span {
    pub r: Range<int>,
}

fn reset(Span s, int n) {
    s.r = 0..n;
}

fn span_total(Span s) -> int {
    let acc = 0;
    for x in s.r {
        acc = acc + x;
    }
    return acc;
}

test("range params and copies") {
    assert(total(0..5) == 10)?;
    assert(total_inc(0..=4) == 10)?;
    assert(pass(2..5) == 9)?;
    assert(shifted(0..4, 10) == 16)?;
}

test("returned ranges") {
    let acc = 0;
    for x in upto(5) {
        acc = acc + x;
    }
    assert(acc == 10)?;
    let r = upto(4);
    assert(total(r) == 6)?;
    assert(total_inc(through(4)) == 10)?;
}

test("boxed range fields") {
    let s = new Span(1..4);
    assert(span_total(s) == 6)?;
    reset(s, 10);
    assert(span_total(s) == 45)?;
}

test("range locals") {
    let r = 3..6;
    let acc = 0;
    for x in r {
        acc = acc + x;
    }
    assert(acc == 12)?;
    let w = 0..2;
    w = 0..3;
    let n = 0;
    for x in w {
        n = n + 1 + x;
    }
    assert(n == 6)?;
}

test("to_vec") {
    let v = (2..5).to_vec();
    assert(v.len() == 3)?;
    assert(v[0] == 2)?;
    let r = 0..=3;
    let w = r.to_vec();
    assert(w.len() == 4)?;
    assert(w[3] == 3)?;
    let f = (1.0..3.0).to_vec();
    assert(f.len() == 2)?;
    assert(f[1] == 2.0)?;
    let u = upto(3).to_vec();
    assert(u.len() == 3)?;
}

test("float range values") {
    let r = 0.5..3.5;
    let acc = 0.0;
    for x in r {
        acc = acc + x;
    }
    assert(acc == 4.5)?;
}
