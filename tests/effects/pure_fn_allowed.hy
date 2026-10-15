// A `pure fn` may index, `assert`, panic, build and change objects it made
// itself, read its arguments, and call other effect-free code, declared or
// inferred. Panics are not effects.
use tests::assert;
use task::{scope, Scope, TaskError};

class Pt {
    pub x: int,
    pub y: int,
}

// Not declared: inferred pure.
fn sq(int n) -> int {
    return n * n;
}

pure fn squares(int n) -> Vec<int> {
    let out: Vec<int> = Vec::new();
    for i in 0..n {
        out.push(sq(i));
    }
    return out;
}

pure fn moved(Pt p, int dx) -> Pt {
    let q = new Pt(p.x, p.y);
    q.x = q.x + dx;
    return q;
}

pure fn third([int; 4] xs) -> int {
    return xs[2];
}

pure fn checked(int n) -> int {
    if n < 0 {
        panic "negative";
    }
    return n;
}

pure fn sum_pairs(Vec<(int, int)> ps) -> int {
    let s = 0;
    for p in ps {
        let (a, b) = p;
        s = s + a * b;
    }
    return s;
}

pure fn compose(int x) -> int {
    let inc = fn (int v) => v + 1;
    let dbl = fn (int v) => v * 2;
    return dbl(inc(x));
}

pure fn greeting(string name) -> string {
    return "hi " + name;
}

fn outcome(unit -> int body) -> string {
    return match scope(fn (Scope s) use (body) {
        let _ = s.spawn(body);
        0
    }) {
        Result::Ok(_) => "ok",
        Result::Err(TaskError::Panicked(m)) => m,
        Result::Err(_) => "other",
    };
}

test("building and changing its own objects") {
    let v = squares(4);
    assert!(len(v) == 4);
    assert!(v[3] == 9);
    let p = new Pt(1, 2);
    let q = moved(p, 5);
    assert!(q.x == 6 && q.y == 2);
    assert!(p.x == 1);
}

test("indexing and reading arguments") {
    assert!(third([1, 2, 3, 4]) == 3);
    let ps: Vec<(int, int)> = Vec::from([(1, 2), (3, 4)]);
    assert!(sum_pairs(ps) == 14);
}

test("local lambdas and strings") {
    assert!(compose(3) == 8);
    assert!(greeting("bo") == "hi bo");
}

test("a pure fn may panic") {
    assert!(checked(3) == 3);
    let m = outcome(fn () => checked(0 - 1));
    assert!(m == "negative", m);
}
