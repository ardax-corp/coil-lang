// Q6 friend: literal `0..n` for-in as a counted cur/end loop (no range dict).
use io::sync::print;
use string::format;

fn range_sum(int n) -> int {
    let acc = 0;
    for x in 0..n {
        acc = acc + x;
    }
    return acc;
}

fn main() {
    let total = 0;
    let round = 0;
    while round < 96 {
        total = total + range_sum(1 << 14);
        round = round + 1;
    }
    let _ = print(format("%i", total));
}
