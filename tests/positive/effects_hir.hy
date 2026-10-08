// E1: effects are solved on HIR. A function that passes a pure lambda to a
// higher-order function is pure, so the counted loops below may split
// across tasks; one whose lambda writes a static stays sequential. Both
// must fold to the sequential sum.
use collections::vec::map;

static let HITS: int = 0;

fn score(int i) -> int {
    let v: Vec<int> = Vec::from([i, i + 1]);
    let ys = map(v, fn (int x) => x * 2);
    return ys[0] + ys[1];
}

fn counted(int x) -> int {
    HITS = HITS + 1;
    return x;
}

fn score_counted(int i) -> int {
    let v: Vec<int> = Vec::from([i, i + 1]);
    let ys = map(v, fn (int x) => counted(x) * 2);
    return ys[0] + ys[1];
}

test("a loop over a function that maps a pure lambda") {
    let acc = 0;
    for i in 0..100000 {
        acc = acc + score(i);
    }
    assert(acc == 20000000000)?;
}

test("a loop over a function that maps an impure lambda") {
    let acc = 0;
    for i in 0..1000 {
        acc = acc + score_counted(i);
    }
    assert(acc == 2000000)?;
    assert(HITS == 2000)?;
}
