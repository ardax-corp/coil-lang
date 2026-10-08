// `if !c { A } else { B }` runs as `if c { B } else { A }`, in return and
// statement position, and an `else if` chain keeps its order.
fn parity(int i) -> int {
    if !(i & 1) {
        return 10;
    } else {
        return 20;
    }
}

fn pick(bool b) -> int {
    let v = 0;
    if !b {
        v = 1;
    } else {
        v = 2;
    }
    return v;
}

fn chain(bool a, bool b) -> int {
    if !a {
        return 1;
    } else if b {
        return 2;
    } else {
        return 3;
    }
}

test("negated if conditions swap their branches") {
    assert(parity(4) == 10)?;
    assert(parity(5) == 20)?;
    assert(pick(false) == 1)?;
    assert(pick(true) == 2)?;
    assert(chain(false, true) == 1)?;
    assert(chain(true, true) == 2)?;
    assert(chain(true, false) == 3)?;
}
