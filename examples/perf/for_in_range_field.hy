// C2b / Q6 rung 1: heap-field Range as counted cur/end (no GetField).
use io::sync::print;
use string::format;

class Holder {
    pub r: Range<int>,
}

fn range_sum(Holder h) -> int {
    let acc = 0;
    for x in h.r {
        acc = acc + x;
    }
    return acc;
}

fn main() {
    let total = 0;
    let round = 0;
    while round < 96 {
        total = total + range_sum(new Holder(0..(1 << 14)));
        round = round + 1;
    }
    let _ = print(format("%i", total));
}
