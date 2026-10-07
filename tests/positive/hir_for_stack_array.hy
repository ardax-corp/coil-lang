// A `for` over a small array literal boxes its frame slots once, right
// before the statement that holds the loop.

test("for over a literal") {
    let xs = [3, 4, 5];
    let s = 0;
    for x in xs {
        s = s + x;
    }
    assert(s == 12)?;
}

test("indexed first, then iterated twice") {
    let xs = [1, 2, 3];
    xs[0] = 10;
    let s = 0;
    for x in xs {
        s = s + x;
    }
    for x in xs {
        s = s + x;
    }
    assert(s == 30)?;
}

test("for inside a branch") {
    let xs = [2, 4];
    let s = 0;
    if s == 0 {
        for x in xs {
            s = s + x;
        }
    }
    assert(s == 6)?;
}

test("writes after the loop reach the boxed array") {
    let xs = [1, 1];
    for x in xs {
        assert(x == 1)?;
    }
    xs[1] = 7;
    assert(xs[0] + xs[1] == 8)?;
}
