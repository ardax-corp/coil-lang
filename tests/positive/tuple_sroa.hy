// Local tuples read with constant indices become slots (no allocation);
// escaping ones are rebuilt once. Semantics must not change either way.
fn pair_sum(int n) -> int {
    let total = 0;
    let i = 0;
    while i < n {
        let t = (i, i + 1);
        total = total + t[1] - t[0];
        i = i + 1;
    }
    return total;
}

fn make(int a) -> (int, int) {
    let t = (a, a * 2);
    return t;
}

fn first((int, int) p) -> int {
    return p[0];
}

fn snapshot() -> int {
    let x = 1;
    let t = (x, x + 1);
    x = 100;
    return t[0] + t[1] + x;
}

test("private tuple in a loop") {
    assert(pair_sum(10) == 10)?;
}

test("tuple escapes by return") {
    let p = make(4);
    assert(p[0] == 4)?;
    assert(p[1] == 8)?;
}

test("tuple escapes to a call after local reads") {
    let t = (7, 9);
    let s = t[0] + t[1];
    assert(s == 16)?;
    assert(first(t) == 7)?;
}

test("tuple keeps its values when the source slot changes") {
    assert(snapshot() == 103)?;
}

test("destructuring still works") {
    let (a, b) = make(3);
    assert(a == 3)?;
    assert(b == 6)?;
}

fn id(int x) -> int {
    return x;
}

fn wide_tuple(int i) -> int {
    let t = (id(i), id(i + 1), id(i + 2), id(i + 3), id(i + 4));
    return t[0] * 10000 + t[1] * 1000 + t[2] * 100 + t[3] * 10 + t[4];
}

fn wide_strings() -> string {
    let t = ("a", "b", "c", "d", "e");
    return t[0] + t[1] + t[2] + t[3] + t[4];
}

test("five-element tuples keep element order") {
    assert(wide_tuple(1) == 12345)?;
    assert(wide_strings() == "abcde")?;
}
