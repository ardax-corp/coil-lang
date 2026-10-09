// Typed inlining reaches calls the hoisting cannot move: the right side
// of `||` / `&&` runs its callee in place, only when the left side lets
// it. Calls inside an inlined body inline in the next round, and test
// bodies inline like functions.

static let calls = 0;

fn three() -> int {
    return 3;
}

fn digit(int b) -> int {
    if b >= 48 && b <= 57 {
        return b - 48;
    }
    return -1;
}

fn is_digit(int b) -> bool {
    return digit(b) >= 0;
}

fn counted(int b) -> bool {
    calls = calls + 1;
    let d = b * 2;
    return d > 10;
}

fn alnum(int b) -> bool {
    return b == 95 || is_digit(b);
}

fn guarded(int b) -> bool {
    return b > 0 && counted(b);
}

fn classify(int s, [int] t) -> int {
    if s == three() {
        return 1;
    }
    let a = digit(t[s]);
    return a;
}

test("a call on the right of || runs only when needed") {
    assert(alnum(95))?;
    assert(alnum(55))?;
    assert(!alnum(65))?;
}

test("a call on the right of && keeps its effect conditional") {
    calls = 0;
    assert(!guarded(0))?;
    assert(calls == 0)?;
    assert(guarded(9))?;
    assert(!guarded(2))?;
    assert(calls == 2)?;
}

test("calls inside an inlined callee inline too") {
    assert(classify(3, [1, 2]) == 1)?;
    assert(classify(0, [50, 2]) == 2)?;
    assert(classify(1, [50, 120]) == -1)?;
}

test("a negated call") {
    let n = 0;
    let i = 40;
    while i < 70 {
        if !is_digit(i) {
            n = n + 1;
        }
        i = i + 1;
    }
    assert(n == 20)?;
}
