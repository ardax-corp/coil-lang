// A lambda or named function passed to a generic function: the generic
// body calls it through its closure word.
use collections::vec::{filter, map};

fn keep<T>(Vec<T> xs, T -> bool pred) -> int {
    let n = 0;
    for x in xs {
        if pred(x) {
            n = n + 1;
        }
    }
    return n;
}

fn is_even(int x) -> bool {
    return x % 2 == 0;
}

fn evens(Vec<int> xs) -> int {
    return keep(xs, fn (int x) => x % 2 == 0);
}

fn named(Vec<int> xs) -> int {
    return keep(xs, is_even);
}

test("predicate lambda and named fn") {
    let xs: Vec<int> = Vec::new();
    xs.push(1);
    xs.push(2);
    xs.push(4);
    assert(evens(xs) == 2)?;
    assert(named(xs) == 2)?;
}

test("stdlib map and filter") {
    let xs: Vec<int> = Vec::new();
    xs.push(1);
    xs.push(2);
    xs.push(3);
    let ys = map(xs, fn (int x) => x * 10);
    assert(ys[2] == 30)?;
    assert(len(filter(xs, fn (int x) => x > 1)) == 2)?;
}
