// Loop length proofs across impure calls: a callee that only writes fields or
// elements keeps `len(v)` invariant; one that pushes must not.
class Tally {
    pub hits: int,
}

/// `[start, start + 1, …]` with `n` elements, as a growable `Vec`.
fn seq(int start, int n) -> Vec<int> {
    let v: Vec<int> = Vec::new();
    let i = 0;
    while i < n {
        v.push(start + i);
        i = i + 1;
    }
    return v;
}

fn tally() -> Tally {
    return new Tally(0);
}

fn count(Tally t, int x) -> int {
    t.hits = t.hits + 1;
    return x * 2;
}

fn poke(Vec<int> v, int i) -> int {
    v[i] = v[i] + 1;
    return 0;
}

fn grow(Vec<int> v) -> int {
    if len(v) < 10 {
        v.push(len(v));
    }
    return 0;
}

fn shrink(Vec<int> v) -> int {
    if len(v) > 2 {
        v.pop();
    }
    return 0;
}

test("field-writing callee keeps the scan") {
    let v = seq(1, 4);
    let t = tally();
    let s = 0;
    let i = 0;
    while i < len(v) {
        s = s + count(t, v[i]);
        i = i + 1;
    }
    assert(s == 20)?;
    assert(t.hits == 4)?;
}

test("element-writing callee keeps the scan") {
    let v = seq(1, 3);
    let i = 0;
    while i < len(v) {
        poke(v, i);
        i = i + 1;
    }
    assert(v[0] == 2)?;
    assert(v[2] == 4)?;
}

test("pushing callee is seen by the loop bound") {
    let v = seq(0, 1);
    let i = 0;
    while i < len(v) {
        grow(v);
        i = i + 1;
    }
    assert(i == 10)?;
    assert(len(v) == 10)?;
}

test("popping callee is seen by the loop bound") {
    let v = seq(5, 5);
    let s = 0;
    let i = 0;
    while i < len(v) {
        s = s + v[i];
        shrink(v);
        i = i + 1;
    }
    assert(i == 3)?;
    assert(s == 18)?;
}
