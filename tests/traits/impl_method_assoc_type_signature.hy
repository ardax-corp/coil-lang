// An impl method whose signature goes through the trait's associated type
// (`Option<Item>`) is checked with the instance's `type Item` substituted:
// a matching impl compiles and runs, with `type Item` before or after the
// method (#2086).
use tests::assert;

class Up {
    pub cur: int,
    pub end: int,
}

impl Iterator for Up {
    type Item = int;
    fn next(Up u) -> Option<int> {
        if u.cur < u.end {
            let v = u.cur;
            u.cur = u.cur + 1;
            return Option::Some(v);
        }
        return Option::None;
    }
}

class Rep {
    pub left: int,
    pub word: string,
}

impl Iterator for Rep {
    fn next(Rep r) -> Option<string> {
        if r.left > 0 {
            r.left = r.left - 1;
            return Option::Some(r.word);
        }
        return Option::None;
    }
    type Item = string;
}

impl IntoIterator for Rep {
    type Item = string;
    type IntoIter = Rep;
    fn into_iter(Rep r) -> Rep {
        return r;
    }
}

test("assoc-typed next, Item first") {
    let u = new Up(0, 3);
    let acc = 0;
    for x in u {
        acc = acc + x;
    }
    assert!(acc == 3);
}

test("assoc-typed next, Item after") {
    let out = "";
    for s in new Rep(2, "ab") {
        out = out + s;
    }
    assert!(out == "abab");
}
