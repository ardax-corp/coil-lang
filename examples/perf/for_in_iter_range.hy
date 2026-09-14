// C2b / Q6 rung 2: user into_iter returning Range uses the counted latch.
use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};

class Holder {
    pub start: int,
    pub end: int,
}

impl IntoIterator for Holder {
    type Item = int;
    type IntoIter = Range<int>;
    pub fn into_iter(Holder h) -> Range<int> {
        return h.start..h.end;
    }
}

fn range_sum(Holder h) -> int {
    let acc = 0;
    for x in h {
        acc = acc + x;
    }
    return acc;
}

fn main() {
    let total = 0;
    let round = 0;
    while round < 96 {
        total = total + range_sum(new Holder(0, 1 << 14));
        round = round + 1;
    }
    write_all(stdout(), to_bytes(format("%i", total)));
}
