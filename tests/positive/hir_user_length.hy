// `len` of a class with a `Length` instance calls the instance's method.

class Bag {
    pub items: Vec<int>,
    pub extra: int,
}

impl Length for Bag {
    fn len(Bag b) -> int {
        return b.items.len() + b.extra;
    }
}

test("len of a new object") {
    assert(len(new Bag(Vec::from([1, 2, 3]), 1)) == 4)?;
}

test("len of a local") {
    let b = new Bag(Vec::from([5]), 0);
    let n = len(b) * 10 + len("ab");
    assert(n == 12)?;
}

test("len in a loop bound") {
    let b = new Bag(Vec::from([1, 1]), 2);
    let s = 0;
    for i in 0..len(b) {
        s = s + i;
    }
    assert(s == 6)?;
}
