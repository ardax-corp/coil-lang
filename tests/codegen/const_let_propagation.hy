// A local bound once to a literal and never written again reads as that
// literal, so arithmetic, comparisons and an inlined callee's branches on
// it fold. A local written again (in a branch, a loop or a later `let`)
// keeps its reads, and a branch with an effect still runs when taken.
use tests::assert;

static let calls: int = 0;

fn bump() {
    calls = calls + 1;
}

fn pick(int mode) -> int {
    if mode == 0 {
        return 10;
    }
    if mode == 1 {
        return 20;
    }
    return 30;
}

test("a literal local folds through an inlined callee") {
    let m = 1;
    let a = pick(m);
    let b = a * 2 + 1;
    if b > 100 {
        bump();
    }
    assert!(b == 41);
    assert!(calls == 0);
}

test("a taken branch on a literal local keeps its effect") {
    let before = calls;
    let limit = 3;
    if limit > 2 {
        bump();
    }
    assert!(calls == before + 1);
}

test("a local written in a branch keeps its reads") {
    let m = 1;
    let x = calls + 5;
    if x > 0 {
        m = 2;
    }
    assert!(m * 2 == 4);
}

test("a local written in a loop keeps its reads") {
    let i = 0;
    let sum = 0;
    while i < 4 {
        sum = sum + i;
        i = i + 1;
    }
    assert!(sum == 6);
    assert!(i == 4);
}

test("float and bool literals propagate") {
    let half = 0.5;
    let on = true;
    let r = half * 4.0;
    assert!(r == 2.0);
    assert!(on && r > 1.0);
}
