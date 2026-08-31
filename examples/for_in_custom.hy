// Expected output: 012
//
// User-defined IntoIterator + Iterator on a class. `next` mutates the
// heap instance in place so state advances across resumes.

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

fn main() {
    let c = new Counter(0, 3);
    for x in c {
        write_all(stdout(), to_bytes(format("%i", x)));
    }
}
