// CALL into_iter/next must use reserved labels when impls follow the user.
use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};

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

impl IntoIterator<Counter> {
    type Item = int;
    type IntoIter = Counter;
    pub fn into_iter(Counter c) -> Counter {
        return c;
    }
}

impl Iterator<Counter> {
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

fn main() {
    write_all(stdout(), to_bytes(format("%i", consume(new Counter(0, 3)))));
}
