// C2b / Q6 rung 1: array-held Range as counted cur/end (no GetField).
use io::sync::print;
use string::format;

fn range_sum(Vec<Range<int>> rs) -> int {
    let acc = 0;
    for x in rs[0] {
        acc = acc + x;
    }
    return acc;
}

fn main() {
    let total = 0;
    let round = 0;
    while round < 96 {
        total = total + range_sum([0..(1 << 14)]);
        round = round + 1;
    }
    let _ = print(format("%i", total));
}
