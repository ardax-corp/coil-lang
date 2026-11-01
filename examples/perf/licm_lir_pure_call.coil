// CPU: a loop that adds an invariant pure call and an invariant int
// division each iteration, in a body that returns an object allocated
// after the loop (dense refuses that shape, so the body stays on LIR).
// MIR LICM must move both out on LIR: the call and the division can trap,
// so they move behind a copy of the loop test.
use io::sync::print;
use string::format;

class Total {
    pub value: int,
}

fn weight(int n) -> int {
    let k = 0;
    let s = n;
    while k < 16 {
        s = s + (s % 7) * k;
        k = k + 1;
    }
    return s;
}

fn sum(int n, int d) -> Total {
    let s = 0;
    let i = 0;
    while i < n {
        s = s + weight(n) + n / d + i;
        i = i + 1;
    }
    return new Total(s);
}

fn main() {
    let total = 0;
    let r = 0;
    while r < 40 {
        total = total + sum(50000, 3 + r).value;
        r = r + 1;
    }
    let _ = print(format("%i", total));
}
