// Operands that branch (`match`, `??`) keep their order and their values
// when they meet in a binary operator.

fn lookup(int i, int n) -> Option<int> {
    if i < 0 || i >= n {
        return Option::None;
    }
    return Option::Some(i * 2);
}

fn pick(int a) -> int {
    let x = Option::Some(a);
    let y: Option<int> = Option::None;
    return (x ?? 10) + (y ?? 20);
}

fn diff_match(int a, int b) -> int {
    return match lookup(a, 7) {
        Option::Some(v) => v,
        Option::None => -1,
    } - match lookup(b, 7) {
        Option::Some(v) => v,
        Option::None => -1,
    };
}

test("coalesce on locals as operands") {
    let x = Option::Some(2);
    let y: Option<int> = Option::None;
    assert((x ?? -1) + (y ?? -1) == 1)?;
    assert((x ?? 10) + (x ?? 20) == 4)?;
    assert((y ?? 10) + (y ?? 20) == 30)?;
    assert(pick(5) == 25)?;
}

test("coalesce on calls as operands") {
    let b = 3;
    assert((lookup(1, 7) ?? -1) + (lookup(-1, 7) ?? -1) == 1)?;
    assert((lookup(1, 7) ?? -1) - (lookup(3, 7) ?? -1) == -4)?;
    assert(b - (lookup(1, 7) ?? -1) == 1)?;
    assert((lookup(1, 7) ?? -1) - b == -1)?;
}

test("match operands keep their order") {
    assert(diff_match(1, 3) == -4)?;
    assert(diff_match(3, 1) == 4)?;
    assert(diff_match(9, 1) == -3)?;
}

test("match arms still bind from slots") {
    let x = Option::Some(4);
    let r = match x {
        Option::Some(v) => v + 1,
        Option::None => 0,
    };
    assert(r + (x ?? 0) == 9)?;
}
