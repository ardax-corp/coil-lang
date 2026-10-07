// `for _ in xs` steps every item without binding it.

fn count_range(Range<int> r) -> int {
    let n = 0;
    for _ in r {
        n = n + 1;
    }
    return n;
}

fn count_array([int] xs) -> int {
    let n = 0;
    for _ in xs {
        n = n + 1;
    }
    return n;
}

test("wildcard over a range value") {
    assert(count_range(2..7) == 5)?;
}

test("wildcard over a literal range") {
    let n = 0;
    for _ in 0..3 {
        n = n + 2;
    }
    assert(n == 6)?;
}

test("wildcard over an array") {
    assert(count_array([4, 5, 6]) == 3)?;
}

test("wildcard over a vec") {
    let v = Vec::from([1, 2, 3, 4]);
    let n = 0;
    for _ in v {
        n = n + 1;
    }
    assert(n == 4)?;
}
