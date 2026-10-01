// CALL into_iter/next must use reserved labels when impls follow the user.

class Counter {
    pub cur: int,
    pub end: int,
}

fn consume(Counter c) -> int {
    let s = 0;
    for x in c {
        s = s + x;
    }
    return s;
}

impl IntoIterator for Counter {
    type Item = int;
    type IntoIter = Counter;
    pub fn into_iter(Counter c) -> Counter {
        return c;
    }
}

impl Iterator for Counter {
    type Item = int;
    pub fn next(Counter c) -> Option<int> {
        if c.cur < c.end {
            let v = c.cur;
            c.cur = c.cur + 1;
            return Option::Some(v);
        }
        return Option::None;
    }
}

test("for-in over a user iterator whose impls follow its use") {
    assert(consume(new Counter(0, 3)) == 3)?;
    assert(consume(new Counter(2, 5)) == 9)?;
    assert(consume(new Counter(4, 4)) == 0)?;
}
