// Integer-literal match hot loop: sixteen cases, so a binary search tests
// about four literals per value where a linear chain tests up to sixteen.
use io::sync::print;
use string::format;

fn cost(int op) -> int {
    return match op {
        0 => 3,
        1 => 5,
        2 => 7,
        3 => 11,
        4 => 13,
        5 => 17,
        6 => 19,
        7 => 23,
        8 => 29,
        9 => 31,
        10 => 37,
        11 => 41,
        12 => 43,
        13 => 47,
        14 => 53,
        15 => 59,
        default => 1,
    };
}

fn run() -> int {
    let acc = 0;
    let i = 0;
    while i < 3000000 {
        acc = acc + cost(i % 17);
        i = i + 1;
    }
    return acc;
}

fn main() {
    let _ = print(format("%i", run()));
}
