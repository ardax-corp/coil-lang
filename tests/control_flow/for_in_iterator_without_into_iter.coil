// A type that is an `Iterator` but not `IntoIterator` loops on its own
// `next` (coil-stdlib#57: `for e in map.iter()`).
// @output 6 15 ab 3
use io::{stdout, write};
use string::{format, to_bytes};

class Count {
    pub n: int,
}

impl Iterator for Count {
    type Item = int;
    pub fn next(Count c) -> Option<int> {
        if c.n >= 3 {
            return Option::None;
        }
        c.n = c.n + 1;
        return Option::Some(c.n);
    }
}

class Repeat<T> {
    pub left: int,
    pub item: T,
}

impl Iterator for Repeat<T> {
    type Item = T;
    pub fn next(Repeat<T> r) -> Option<T> {
        if r.left <= 0 {
            return Option::None;
        }
        r.left = r.left - 1;
        return Option::Some(r.item);
    }
}

class Pair {
    pub key: string,
    pub value: int,
}

class Pairs {
    pub items: Vec<Pair>,
    pub at: int,
}

impl Iterator for Pairs {
    type Item = Pair;
    pub fn next(Pairs p) -> Option<Pair> {
        if p.at >= p.items.len() {
            return Option::None;
        }
        p.at = p.at + 1;
        return Option::Some(p.items[p.at - 1]);
    }
}

fn main() {
    let total = 0;
    for x in new Count(0) {
        total = total + x;
    }
    let fives = 0;
    for x in new Repeat(3, 5) {
        fives = fives + x;
    }
    let text = "";
    for s in new Repeat(2, "ab") {
        text = s;
    }
    let items: Vec<Pair> = Vec::new();
    items.push(new Pair("a", 1));
    items.push(new Pair("b", 2));
    let sum = 0;
    for p in new Pairs(items, 0) {
        sum = sum + p.value;
    }
    let _ = write(stdout(), to_bytes(format("%i %i %s %i", total, fives, text, sum)));
}
