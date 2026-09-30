// Static initializers may call functions: generic and stdlib calls
// (packed call targets resolved when the setup region is spliced),
// inlined / self-recursive callees (their peeled bodies stay in the
// initializer), and functions declared after the static.
fn id<T>(T x) -> T {
    return x;
}

fn fib(int n) -> int {
    if n < 2 {
        return n;
    }
    return fib(n - 1) + fib(n - 2);
}

static let xs: Vec<int> = Vec::new();

static let seeded: Vec<int> = Vec::with_capacity(4);

static let k: int = id(7);

static let f: int = fib(15);

static let fwd: int = later(4);

static let base: int = len(xs) + 3;

static let names: Option<Vec<string>> = Option::Some(Vec::new());

fn later(int n) -> int {
    let s = 0;
    let i = 0;
    while i < n {
        s = s + i;
        i = i + 1;
    }
    return s;
}

test("static initializers with calls") {
    assert(k == 7)?;
    assert(f == 610)?;
    assert(fwd == 6)?;
    assert(base == 3)?;
    xs.push(f);
    seeded.push(k);
    assert(len(xs) == 1)?;
    assert(xs[0] == 610)?;
    assert(seeded[0] == 7)?;
    let m = match names {
        Option::Some(v) => len(v),
        Option::None => -1,
    };
    assert(m == 0)?;
}
