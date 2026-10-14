// Heap arrays of niche `Option` / `Result` words: each element is the
// word itself (`0` for `None` / `Ok(())`). Direct MIR reads, writes, pushes
// and loops over them as HIR emission does, and a stack array of niche
// words boxes to the same array.
use tests::assert;
use gc::collect;

class Leaf {
    pub v: int,
}

fn opt(int i) -> Option<Leaf> {
    if i % 2 == 0 {
        return Option::None;
    }
    return Option::Some(new Leaf(i));
}

fn check(int x) -> Result<(), string> {
    if x < 0 {
        return Result::Err("neg");
    }
    return Result::Ok(());
}

fn value(Option<Leaf> o) -> int {
    return match o {
        Option::Some(l) => l.v,
        Option::None => 0,
    };
}

fn filled(int n) -> Vec<Option<Leaf>> {
    let out: Vec<Option<Leaf>> = Vec::new();
    let i = 0;
    while i < n {
        out.push(opt(i));
        i = i + 1;
    }
    return out;
}

fn total(Vec<Option<Leaf>> xs) -> int {
    let t = 0;
    for x in xs {
        t = t + value(x);
    }
    return t;
}

fn swap_ends(Vec<Option<Leaf>> xs) {
    let first = xs[0];
    xs[0] = xs[len(xs) - 1];
    xs[len(xs) - 1] = first;
}

fn picked(int k) -> int {
    let hs = [opt(1), opt(2), opt(3)];
    let t = 0;
    for h in hs {
        t = t + value(h);
    }
    return t * 10 + value(hs[k]);
}

fn oks(int a, int b) -> int {
    let rs = [check(a), check(b)];
    let n = 0;
    for r in rs {
        n = n + match r {
            Result::Ok(_) => 1,
            Result::Err(_) => 0,
        };
    }
    return n;
}

test("push, index and loop over niche option elements") {
    let xs = filled(10);
    collect();
    assert!(len(xs) == 10);
    assert!(total(xs) == 25);
    assert!(value(xs[3]) == 3);
    assert!(value(xs[4]) == 0);
    swap_ends(xs);
    assert!(value(xs[0]) == 9 && value(xs[9]) == 0);
    xs[2] = Option::Some(new Leaf(40));
    collect();
    assert!(total(xs) == 65);
}

test("a stack array of niche words boxes to a heap array") {
    assert!(picked(0) == 41);
    assert!(picked(1) == 40);
    assert!(picked(2) == 43);
}

test("unit results as array elements") {
    assert!(oks(1, 2) == 2);
    assert!(oks(-1, 2) == 1);
    assert!(oks(-1, -2) == 0);
}
