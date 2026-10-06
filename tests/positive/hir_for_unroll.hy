// Short literal ranges unroll: the body runs once per value with the
// binding set to it. Longer ranges, and bodies with their own `break` or
// `continue`, keep the counted loop.
fn short() -> int {
    let s = 0;
    for i in 0..4 {
        let sq = i * i;
        s += sq;
    }
    return s;
}

fn inclusive() -> int {
    let s = 0;
    for i in 1..=3 {
        s = s * 10 + i;
    }
    return s;
}

fn empty() -> int {
    let s = 7;
    for i in 5..5 {
        s += i;
    }
    return s;
}

fn long() -> int {
    let s = 0;
    for i in 0..9 {
        s += i;
    }
    return s;
}

fn early() -> int {
    let s = 0;
    for i in 0..4 {
        if i == 2 {
            break;
        }
        s += 1;
    }
    return s;
}

fn nested_break() -> int {
    let s = 0;
    for i in 0..3 {
        for j in 0..100 {
            if j > i {
                break;
            }
            s += 1;
        }
    }
    return s;
}

test("short literal ranges unroll") {
    assert(short() == 14)?;
    assert(inclusive() == 123)?;
    assert(empty() == 7)?;
}

test("long ranges and own jumps keep the loop") {
    assert(long() == 36)?;
    assert(early() == 2)?;
    assert(nested_break() == 6)?;
}
