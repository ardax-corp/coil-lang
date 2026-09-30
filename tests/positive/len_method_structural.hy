// `x.len()` is the canonical length on every structural type, not only
// `Vec`: fixed arrays, arrays, tuples and strings.
fn count([int] xs) -> int {
    return xs.len();
}

test("fixed array len method") {
    let xs = [1, 2, 3];
    assert(xs.len() == 3)?;
    assert(xs.len() == len(xs))?;
}

test("array parameter len method") {
    assert(count([4, 5, 6, 7]) == 4)?;
}

test("tuple and string len method") {
    let t = (1, "a", 2.0);
    assert(t.len() == 3)?;
    let s = "coil";
    assert(s.len() == 4)?;
}

test("vec len method unchanged") {
    let v: Vec<int> = Vec::new();
    v.push(1);
    v.push(2);
    assert(v.len() == 2)?;
}
