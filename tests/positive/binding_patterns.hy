// for patterns, if let, while let (COI-371).

test("for tuple pattern on dict") {
    let sum = 0;
    let d = { a: 1, b: 2, c: 3 };
    for (k, v) in d {
        sum = sum + v;
    }
    assert(sum == 6)?;
}

test("for wildcard") {
    let n = 0;
    for _ in [1, 2, 3] {
        n = n + 1;
    }
    assert(n == 3)?;
}

test("if let some") {
    let o = Option::Some(7);
    let got = 0;
    if let Option::Some(x) = o {
        got = x;
    }
    assert(got == 7)?;
}

test("if let none else") {
    let o = Option::None;
    let got = 0;
    if let Option::Some(x) = o {
        got = x;
    } else {
        got = -1;
    }
    assert(got == -1)?;
}

test("else if let") {
    let o = Option::None;
    let r = Result::Ok(9);
    let got = 0;
    if let Option::Some(x) = o {
        got = x;
    } else if let Result::Ok(v) = r {
        got = v;
    } else {
        got = -1;
    }
    assert(got == 9)?;
}

test("while let some") {
    let cur = Option::Some(3);
    let acc = 0;
    while let Option::Some(n) = cur {
        acc = acc + n;
        if n == 1 {
            cur = Option::None;
        } else {
            cur = Option::Some(n - 1);
        }
    }
    assert(acc == 6)?;
}
