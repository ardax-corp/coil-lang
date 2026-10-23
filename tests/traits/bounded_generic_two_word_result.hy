use tests::assert;

class Count {
    pub n: int,
}

impl Iterator for Count {
    type Item = int;
    pub fn next(Count c) -> Option<int> {
        if c.n >= 2 {
            return Option::None;
        }
        c.n = c.n + 1;
        return Option::Some(c.n);
    }
}

class Once {
    pub done: bool,
}

impl Iterator for Once {
    type Item = string;
    pub fn next(Once o) -> Option<string> {
        if o.done {
            return Option::None;
        }
        o.done = true;
        return Option::Some("once");
    }
}

fn first<I: Iterator>(I it) -> int {
    return match it.next() {
        Option::Some(_) => 1,
        Option::None => 0,
    };
}

fn count<I: Iterator>(I it) -> int {
    let n = 0;
    while let Option::Some(_) = it.next() {
        n = n + 1;
    }
    return n;
}

test("generic caller of a two-word trait result") {
    assert!(first(new Count(0)) == 1);
    assert!(first(new Count(2)) == 0);
    assert!(count(new Count(0)) == 2);
    assert!(first(new Once(false)) == 1);
    assert!(count(new Once(false)) == 1);
}
