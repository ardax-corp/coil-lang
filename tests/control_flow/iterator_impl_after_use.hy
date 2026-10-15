// A class is iterable in `for … in` even when its `IntoIterator` and
// `Iterator` impls come after the function that loops over it.
use tests::assert;

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

test("impl after use") {
    let n = consume(new Counter(0, 3));
    assert!((typeof n) == "int");
    assert!(n == 3);
}
