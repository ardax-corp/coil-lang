// Memory: 200k live heap pairs in a `Vec`. Tuple words live inside the
// object (no separate `Vec` allocation per tuple).
use io::sync::print;
use string::format;

fn build(int n) -> Vec<(int, int)> {
    let pairs: Vec<(int, int)> = Vec::with_capacity(n);
    let i = 0;
    while i < n {
        pairs.push((i, i * 7 % 13));
        i = i + 1;
    }
    return pairs;
}

fn weigh(Vec<(int, int)> pairs) -> int {
    let total = 0;
    let i = 0;
    while i < pairs.len() {
        let p = pairs[i];
        total = total + p[0] - p[1];
        i = i + 1;
    }
    return total;
}

fn main() {
    let total = 0;
    let round = 0;
    while round < 4 {
        total = total + weigh(build(200000));
        round = round + 1;
    }
    let _ = print(format("%i", total));
}
