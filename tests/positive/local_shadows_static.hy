// A parameter, `let`, pattern / loop binding, or capture named like a
// static shadows it: reads and writes go to the local and the static is
// untouched. Codegen used to prefer the static slot for every bare name.
fn add2(int n) -> int {
    return n + 2;
}

static let n: int = 3;

static const c: int = 5;

static let xs: Vec<int> = Vec::new();

fn bump(int n) -> int {
    n += 4;
    n = n * 2;
    return n;
}

fn times(int c) -> int {
    return c * 10;
}

fn fill(Vec<int> xs) -> int {
    xs.push(1);
    xs.push(2);
    xs[0] = 9;
    return len(xs) + xs[0];
}

fn get() -> int {
    return n;
}

test("parameter declared before the static") {
    assert(add2(5) == 7)?;
}

test("parameter reads and writes") {
    assert(bump(1) == 10)?;
    assert(times(4) == 40)?;
    assert(n == 3)?;
    assert(c == 5)?;
}

test("collection parameter") {
    let v: Vec<int> = Vec::new();
    assert(fill(v) == 11)?;
    assert(len(xs) == 0)?;
}

test("let, match, and loop bindings") {
    let r = 0;
    {
        let n = 100;
        r = n;
    }
    assert(r == 100)?;
    let m = match Option::Some(40) {
        Option::Some(n) => n + 2,
        Option::None => 0,
    };
    assert(m == 42)?;
    let s = 0;
    for n in 0..5 {
        s = s + n;
    }
    assert(s == 10)?;
    assert(n == 3)?;
}

test("lambda parameter and capture") {
    let double = fn (int n) => n * 2;
    assert(double(5) == 10)?;
    let n = 10;
    let add = fn (int y) use (n) => n + y;
    assert(add(1) == 11)?;
    assert(get() == 3)?;
}

test("static stays reachable") {
    n = 8;
    assert(get() == 8)?;
    assert(c + 1 == 6)?;
}
