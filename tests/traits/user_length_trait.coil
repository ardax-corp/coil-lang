// `len` of a class with a `Length` instance calls the instance's method.
use tests::assert;

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
    assert!((new Bag(Vec::from([1, 2, 3]), 1)).len() == 4);
}

test("len of a local") {
    let b = new Bag(Vec::from([5]), 0);
    let n = b.len() * 10 + "ab".len();
    assert!(n == 12);
}

test("len in a loop bound") {
    let b = new Bag(Vec::from([1, 1]), 2);
    let s = 0;
    for i in 0..b.len() {
        s = s + i;
    }
    assert!(s == 6);
}
