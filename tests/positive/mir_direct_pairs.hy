// Two-word pairs in bodies lowered straight from HIR to MIR: enum
// parameters, results and locals as `[payload, tag]`, matches on them
// (bound, wildcard and identity arms, returned and joined), float
// payloads, ranges as `[start, end]` and two-element tuples.

enum Phase {
    Low(int),
    Mid(int),
    High(int),
    Off,
}

fn phase_of(int i) -> Phase {
    if i % 4 == 0 {
        return Phase::Low(i);
    }
    if i % 4 == 1 {
        return Phase::Mid(i * 2);
    }
    if i % 4 == 2 {
        return Phase::High(i * 3);
    }
    return Phase::Off;
}

fn score(Phase p) -> int {
    return match p {
        Phase::Low(v) => v,
        Phase::Mid(x) => x + 1,
        Phase::High(y) => y + 2,
        Phase::Off => -1,
    };
}

fn half(int i) -> Option<int> {
    if i % 2 == 0 {
        return Option::Some(i / 2);
    }
    return Option::None;
}

fn ratio(int i) -> Option<float> {
    if i == 0 {
        return Option::None;
    }
    return Option::Some(1.0 / (i as float));
}

fn checked(int a, int b) -> Result<int, int> {
    if b == 0 {
        return Result::Err(a);
    }
    return Result::Ok(a / b);
}

fn res_of(int i) -> Result<int, int> {
    if i % 3 == 0 {
        return Result::Err(i);
    }
    return checked(100, i % 3);
}

fn phases(int n) -> int {
    let acc = 0;
    for i in 0..n {
        let p = phase_of(i);
        acc = acc + score(p);
        let tagged = match p {
            Phase::Off => 100,
            default => 0,
        };
        acc = acc + tagged;
    }
    return acc;
}

fn halves(int n) -> int {
    let acc = 0;
    let i = 0;
    while i < n {
        acc = acc + match half(i) {
            Option::Some(x) => x,
            Option::None => 0,
        };
        i = i + 1;
    }
    return acc;
}

fn ratios(int n) -> float {
    let acc = 0.0;
    for i in 0..n {
        let r = ratio(i);
        match r {
            Option::Some(x) => {
                acc = acc + x;
            },
            Option::None => {
                acc = acc - 1.0;
            },
        }
    }
    return acc;
}

fn divs(int n) -> int {
    let acc = 0;
    for i in 0..n {
        let c = res_of(i);
        acc = acc + match c {
            Result::Ok(v) => v,
            Result::Err(e) => -e,
        };
    }
    return acc;
}

fn span(int n) -> Range<int> {
    return 1..n;
}

fn span_sum(int n) -> int {
    let r = span(n);
    let acc = 0;
    for x in r {
        acc = acc + x;
    }
    return acc;
}

fn minmax(int a, int b) -> (int, int) {
    if a < b {
        return (a, b);
    }
    return (b, a);
}

fn spread(int n) -> int {
    let acc = 0;
    for i in 0..n {
        let (lo, hi) = minmax(i, n - i);
        acc = acc + hi - lo;
    }
    return acc;
}

test("enum pairs from direct MIR") {
    assert(score(Phase::High(4)) == 6)?;
    assert(phases(8) == 244)?;
    assert(halves(10) == 10)?;
    assert(ratios(3) == 0.5)?;
    assert(divs(7) == 291)?;
}

test("range and tuple pairs from direct MIR") {
    assert(span_sum(5) == 10)?;
    assert(spread(5) == 13)?;
}
