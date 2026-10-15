// `for x in it` over a user iterator lowered to direct MIR: `next` as a
// two-word `Option<int>`, a niche `Option<string>`, and a boxed
// `Option<(int, int)>`; `break` / `continue` in the body; an `into_iter`
// that returns a different type.
use tests::assert;

class Count {
    pub cur: int,
    pub end: int,
}

impl Iterator for Count {
    type Item = int;
    fn next(Count c) -> Option<int> {
        if c.cur < c.end {
            let v = c.cur;
            c.cur = c.cur + 1;
            return Option::Some(v);
        }
        return Option::None;
    }
}

class Upto {
    pub n: int,
}

impl IntoIterator for Upto {
    type Item = int;
    type IntoIter = Count;
    fn into_iter(Upto u) -> Count {
        return new Count(0, u.n);
    }
}

class Words {
    pub left: int,
    pub word: string,
}

impl IntoIterator for Words {
    type Item = string;
    type IntoIter = Words;
    fn into_iter(Words w) -> Words {
        return w;
    }
}

impl Iterator for Words {
    type Item = string;
    fn next(Words w) -> Option<string> {
        if w.left > 0 {
            w.left = w.left - 1;
            return Option::Some(w.word);
        }
        return Option::None;
    }
}

class Pairs {
    pub i: int,
    pub n: int,
}

impl IntoIterator for Pairs {
    type Item = (int, int);
    type IntoIter = Pairs;
    fn into_iter(Pairs p) -> Pairs {
        return p;
    }
}

impl Iterator for Pairs {
    type Item = (int, int);
    fn next(Pairs p) -> Option<(int, int)> {
        if p.i < p.n {
            p.i = p.i + 1;
            return Option::Some((p.i, p.i * p.i));
        }
        return Option::None;
    }
}

fn sum_upto(int n) -> int {
    let acc = 0;
    for x in new Upto(n) {
        acc = acc + x;
    }
    return acc;
}

fn odd_until(int n, int stop) -> int {
    let acc = 0;
    for x in new Upto(n) {
        if x % 2 == 0 {
            continue;
        }
        if x > stop {
            break;
        }
        acc = acc + x;
    }
    return acc;
}

fn join(int n, string w) -> string {
    let out = "";
    for s in new Words(n, w) {
        out = out + s;
    }
    return out;
}

fn squares(int n) -> int {
    let acc = 0;
    for p in new Pairs(0, n) {
        let (k, sq) = p;
        acc = acc + sq - k;
    }
    return acc;
}

fn grid(int n) -> int {
    let acc = 0;
    for a in new Upto(n) {
        for b in new Upto(a) {
            acc = acc + b;
        }
    }
    return acc;
}

test("two-word next") {
    assert!(sum_upto(100) == 4950);
    assert!(sum_upto(0) == 0);
}

test("break and continue") {
    // 1 + 3 + 5 + 7 + 9
    assert!(odd_until(100, 10) == 25);
    assert!(odd_until(4, 100) == 4);
}

test("niche next") {
    assert!(join(3, "ab") == "ababab");
    assert!(join(0, "x") == "");
}

test("boxed next") {
    // sum of k * k - k for k in 1..=4: 0 + 2 + 6 + 12
    assert!(squares(4) == 20);
    assert!(squares(0) == 0);
}

test("nested loops") {
    // sum over a < 5 of a * (a - 1) / 2: 0 + 0 + 1 + 3 + 6
    assert!(grid(5) == 10);
}
