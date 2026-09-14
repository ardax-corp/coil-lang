// C2b / Q6 rung 2: user Iterator::next as two-slot Option match + CALL.
use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};

class Counter {
    pub cur: int,
    pub end: int,
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

fn iter_sum(Counter c) -> int {
    let acc = 0;
    for x in c {
        acc = acc + x;
    }
    return acc;
}

fn main() {
    let total = 0;
    let round = 0;
    while round < 96 {
        total = total + iter_sum(new Counter(0, 1 << 14));
        round = round + 1;
    }
    write_all(stdout(), to_bytes(format("%i", total)));
}
