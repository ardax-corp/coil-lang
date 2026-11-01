// C2 / Q6: Range parameter as counted cur/end (no heap GetField).
use io::sync::print;
use string::format;

fn range_sum(Range<int> r) -> int {
    let iter = r;
    let acc = 0;
    for x in iter {
        acc = acc + x;
    }
    return acc;
}

fn main() {
    let total = 0;
    let round = 0;
    while round < 96 {
        total = total + range_sum(0..(1 << 14));
        round = round + 1;
    }
    let _ = print(format("%i", total));
}
