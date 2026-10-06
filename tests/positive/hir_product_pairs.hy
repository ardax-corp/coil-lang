// Arity-2 tuples of immediates return as two words. The HIR path builds
// them from literals, forwards them, binds and indexes them in two slots,
// and boxes them only where a heap tuple is needed.
fn pair(int i) -> (int, int) {
    return (i, i * 10);
}

fn mixed(int n) -> (int, float) {
    if n > 0 {
        return (n, 0.5);
    }
    return (0, 1.5);
}

fn forward(int i) -> (int, int) {
    return pair(i + 1);
}

fn from_heap((int, int) t) -> (int, int) {
    return t;
}

fn swap(int i) -> (int, int) {
    let p = pair(i);
    return (p[1], p[0]);
}

fn total((int, int) t) -> int {
    return t[0] + t[1];
}

test("destructure a product call") {
    let (a, b) = pair(3);
    assert(a == 3)?;
    assert(b == 30)?;
    let (_, d) = forward(1);
    assert(d == 20)?;
    let (e, _) = mixed(2);
    assert(e == 2)?;
}

test("index a product local") {
    let p = pair(4);
    assert(p[0] == 4)?;
    assert(p[1] == 40)?;
    let s = swap(2);
    assert(s[0] == 20)?;
    assert(s[1] == 2)?;
}

test("a product boxes for a heap consumer") {
    let p = pair(5);
    assert(total(p) == 55)?;
    assert(total(pair(1)) == 11)?;
    let (x, y) = from_heap((7, 8));
    assert(x + y == 15)?;
}

test("mixed immediate lanes") {
    let (n, f) = mixed(0);
    assert(n == 0)?;
    assert(f == 1.5)?;
    let m = mixed(3);
    assert(m[1] == 0.5)?;
}
