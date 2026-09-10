// COI-349 C1: self two-slot CALL/RETURN (non-tail + tail).
// Same `[payload, tag]` ABI as helper B3; keep/refuse is the cost gate.

#[max_depth(32)]
fn walk(int n) -> Option<int> {
    if n <= 0 {
        return Option::Some(0);
    }
    let r = walk(n - 1);
    let x = match r {
        Option::Some(v) => v,
        Option::None => 0,
    };
    return Option::Some(x + 1);
}

#[max_depth(32)]
fn walk_tail(int n, int acc) -> Option<int> {
    if n <= 0 {
        return Option::Some(acc);
    }
    return walk_tail(n - 1, acc + 1);
}

#[max_depth(32)]
fn pair_walk(int n) -> (int, int) {
    if n <= 0 {
        return (0, 1);
    }
    let (a, b) = pair_walk(n - 1);
    return (a + 1, b);
}

test("self two-slot Option walk") {
    assert(match walk(5) {
        Option::Some(v) => v == 5,
        Option::None => false,
    })?;
    assert(match walk(0) {
        Option::Some(v) => v == 0,
        Option::None => false,
    })?;
}

test("self two-slot Option tail") {
    assert(match walk_tail(7, 0) {
        Option::Some(v) => v == 7,
        Option::None => false,
    })?;
}

test("self two-slot product walk") {
    let (a, b) = pair_walk(4);
    assert(a == 4 && b == 1)?;
}
