// Typed inlining of callees that return a two-word Option, Result or enum,
// and enum locals built in place kept as two slots.
// (`if` is a statement, so an `if`-valued local only comes from inlining.)
enum Phase {
    Low(int),
    Mid(int),
    High,
}

fn lookup(int i, int n) -> Option<int> {
    if i < 0 || i >= n {
        return Option::None;
    }
    return Option::Some(i * 2);
}

fn half(int x) -> Result<int, string> {
    if x % 2 != 0 {
        return Result::Err("odd");
    }
    return Result::Ok(x / 2);
}

fn check(int x) -> Result<(), int> {
    if x > 10 {
        return Result::Err(x);
    }
    return Result::Ok(());
}

fn ratio(int a, int b) -> Option<float> {
    if b == 0 {
        return Option::None;
    }
    return Option::Some((a as float) / (b as float));
}

fn phase(int x) -> Phase {
    let k = x % 3;
    if k == 0 {
        return Phase::Low(x);
    }
    if k == 1 {
        return Phase::Mid(x * 10);
    }
    return Phase::High;
}

fn score(Option<int> o) -> int {
    return match o {
        Option::Some(v) => v,
        Option::None => -1,
    };
}

fn sum_lookups(int n) -> int {
    let acc = 0;
    let i = 0 - 2;
    while i < n + 2 {
        acc = acc + match lookup(i, n) {
            Option::Some(x) => x,
            Option::None => 100,
        };
        i = i + 1;
    }
    return acc;
}

test("an Option guard callee in a match scrutinee") {
    // 0+2+4+6+8 for n = 5, plus 100 for each of the four misses.
    assert(sum_lookups(5) == 420, "sum")?;
}

fn quarter(int x) -> Result<int, string> {
    let h = half(x)?;
    return half(h)?;
}

test("a Result callee under ?") {
    assert(match quarter(12) {
        Result::Ok(v) => v,
        Result::Err(_) => -1,
    } == 3, "ok")?;
    assert(match quarter(6) {
        Result::Ok(_) => "",
        Result::Err(e) => e,
    } == "odd", "inner err")?;
    assert(match quarter(7) {
        Result::Ok(_) => "",
        Result::Err(e) => e,
    } == "odd", "outer err")?;
}

fn count_bad(int n) -> int {
    let bad = 0;
    let i = 0;
    while i < n {
        match check(i) {
            Result::Ok(_) => {},
            Result::Err(v) => {
                bad = bad + v;
            },
        }
        i = i + 1;
    }
    return bad;
}

test("a unit Result callee as a statement match") {
    assert(count_bad(14) == 11 + 12 + 13, "sum")?;
}

test("a float Option result, kept and passed on") {
    let r = ratio(3, 4);
    let z = ratio(1, 0);
    assert(match r {
        Option::Some(v) => v == 0.75,
        Option::None => false,
    }, "some")?;
    assert(match z {
        Option::Some(_) => false,
        Option::None => true,
    }, "none")?;
}

fn phases(int n) -> int {
    let acc = 0;
    let i = 0;
    while i < n {
        acc = acc + match phase(i) {
            Phase::Low(v) => v,
            Phase::Mid(v) => v,
            Phase::High => 1000,
        };
        i = i + 1;
    }
    return acc;
}

test("a user pair enum callee with three exits") {
    // i = 0..5: Low 0, Mid 10, High, Low 3, Mid 40, High.
    assert(phases(6) == 0 + 10 + 1000 + 3 + 40 + 1000, "sum")?;
}

test("a variant-built Option local used as a value") {
    let o = Option::Some(7);
    let p = o;
    assert(score(o) == 7, "passed")?;
    assert(score(p) == 7, "copied")?;
    let q: Option<int> = Option::None;
    assert(score(q) == -1, "none")?;
    assert(score(lookup(2, 5)) == 4, "argument")?;
    let k = lookup(1, 5);
    assert(score(k) == 2, "kept")?;
}

fn first_hit(int n) -> int {
    let i = 0;
    while i < n {
        if match lookup(i * 3, n) {
            Option::Some(x) => x > 10,
            Option::None => false,
        } {
            return i;
        }
        i = i + 1;
    }
    return -1;
}

test("an Option callee in an if condition") {
    assert(first_hit(20) == 2, "hit")?;
    assert(first_hit(4) == -1, "miss")?;
}
