// Dense bodies branch on a combined bool register (`a || b`, `a && b`)
// as `slot != 0`, and compute a join's scalar φ incoming straight
// into the join's register. Both must keep every arm's value and every
// branch direction.
use tests::assert;

fn outside(int i, int n) -> Option<int> {
    if i < 0 || i >= n {
        return Option::None;
    }
    return Option::Some(i * 2);
}

fn both(int a, int b) -> int {
    if a > 0 && b > 0 {
        return a + b;
    }
    return 0 - 1;
}

fn one_of(int a, int b) -> int {
    if (a > 0) != (b > 0) {
        return 1;
    }
    return 0;
}

fn step(int i) -> Result<int, int> {
    if i % 5 == 0 {
        return Result::Err(0 - 1);
    }
    return Result::Ok((i % 10) + 1);
}

fn chain(int i) -> Result<int, int> {
    let a = step(i)?;
    let b = step(i + 3)?;
    return Result::Ok(a + b);
}

fn pick(int i) -> float {
    let x = 0.0;
    if i % 3 == 0 {
        x = (i as float) * 0.5;
    } else if i % 3 == 1 {
        x = (i as float) + 0.25;
    } else {
        x = 0.0 - (i as float);
    }
    return x;
}

fn reused(int i) -> int {
    // `t` feeds the join and is read again after it.
    let t = i * 3;
    let r = 0;
    if i % 2 == 0 {
        r = t;
    } else {
        r = t + 1;
    }
    return r + t;
}

test("or of two compares branches both ways") {
    let acc = 0;
    let i = 0 - 3;
    while i < 12 {
        acc = acc + match outside(i, 7) {
            Option::Some(x) => x,
            Option::None => 100,
        };
        i = i + 1;
    }
    // inside 0..7: 2 * (0 + ... + 6) = 42; outside: 3 + 5 = 8 times 100
    assert!(acc == 842);
}

test("and / not-equal of compares") {
    assert!(both(2, 3) == 5);
    assert!(both(0, 3) == 0 - 1);
    assert!(both(2, 0 - 3) == 0 - 1);
    assert!(one_of(1, 0) == 1);
    assert!(one_of(0, 1) == 1);
    assert!(one_of(1, 1) == 0);
    assert!(one_of(0, 0) == 0);
}

test("try chain joins keep each arm's value") {
    let acc = 0;
    let i = 0;
    while i < 100 {
        acc = acc + match chain(i) {
            Result::Ok(v) => v,
            Result::Err(e) => e,
        };
        i = i + 1;
    }
    // period 10: 68 per period (examples/perf/result_try_churn.hy)
    assert!(acc == 680);
}

test("float else-if chain join") {
    assert!(pick(6) == 3.0);
    assert!(pick(7) == 7.25);
    assert!(pick(8) == 0.0 - 8.0);
    let s = 0.0;
    let i = 0;
    while i < 9 {
        s = s + pick(i);
        i = i + 1;
    }
    // 0 + 1.25 - 2 + 1.5 + 4.25 - 5 + 3 + 7.25 - 8
    assert!(s == 2.25);
}

test("an incoming read after the join keeps its register") {
    assert!(reused(4) == 24);
    assert!(reused(5) == 31);
}
