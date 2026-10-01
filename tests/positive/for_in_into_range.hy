class Holder {
    pub start: int,
    pub end: int,
}

impl IntoIterator for Holder {
    type Item = int;
    type IntoIter = Range<int>;
    fn into_iter(Holder h) -> Range<int> {
        return h.start..h.end;
    }
}

fn total(Holder h) -> int {
    let s = 0;
    for x in h {
        s = s + x;
    }
    return s;
}

test("IntoIterator returning a Range") {
    assert(total(new Holder(0, 4)) == 6)?;
    assert(total(new Holder(3, 6)) == 12)?;
    assert(total(new Holder(5, 5)) == 0)?;
}
