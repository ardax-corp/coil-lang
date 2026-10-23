// A method call whose only target is a trait instance method, with no
// inherent method of that name, runs the instance (#1938).
// @output 1 2 none
use io::{stdout, write};
use string::{format, to_bytes};

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

fn main() {
    let c = new Count(0);
    let a = c.next() ?? 0;
    let b = c.next() ?? 0;
    let end = match c.next() {
        Option::Some(_) => "some",
        Option::None => "none",
    };
    let _ = write(stdout(), to_bytes(format("%i %i %s", a, b, end)));
}
