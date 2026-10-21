// A generic function's recursive call has its declared return type, so a
// method call on the result needs no annotation (#1191).
use io::{stdout, write};
use string::{format, to_bytes};

// Output: 3 2 1 3

fn reversed<T>(Vec<T> xs) -> Vec<T> {
    if xs.len() == 0 {
        return xs;
    }
    let rest: Vec<T> = Vec::new();
    for i in 1..xs.len() {
        rest.push(xs[i]);
    }
    let r = reversed(rest);
    r.push(xs[0]);
    return r;
}

fn depth(int n) -> Option<int> {
    if n == 0 {
        return Option::Some(0);
    }
    let d = depth(n - 1);
    return Option::Some((d ?? 0) + 1);
}

fn main() {
    let xs: Vec<int> = Vec::from([1, 2, 3]);
    let r = reversed(xs);
    let d = depth(3) ?? -1;
    let _ = write(stdout(), to_bytes(format("%i %i %i %i\n", r[0], r[1], r[2], d)));
}
