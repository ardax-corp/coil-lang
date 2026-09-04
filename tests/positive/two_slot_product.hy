// Direct CALL/RETURN two-slot ABI for arity-2 immediate products.
// `(int, int)` moves `[a, b]` (second on top) instead of boxing ObjTuple.
// Destructure / bind+index stay on the pair; box only at escape.
// Mixed-heap and wider tuples stay boxed.

fn pair(int i) -> (int, int) {
    return (i, i + 1);
}

fn mixed(int n, float f) -> (int, float) {
    return (n, f);
}

fn forward(int i) -> (int, int) {
    return pair(i);
}

fn take((int, int) p) -> int {
    return p[0] + p[1];
}

fn boxed_triple() -> (int, int, int) {
    return (1, 2, 3);
}

fn mixed_heap(int n) -> (int, string) {
    return (n, "x");
}

test("destructure two-slot product call") {
    let (a, b) = pair(4);
    assert(a == 4)?;
    assert(b == 5)?;
}

test("bind then index two-slot product") {
    let p = pair(7);
    assert(p[0] == 7)?;
    assert(p[1] == 8)?;
}

test("pass-through two-slot product return") {
    let (a, b) = forward(2);
    assert(a + b == 5)?;
}

test("mixed immediate product") {
    let (n, f) = mixed(3, 1.5);
    assert(n == 3)?;
    assert(f == 1.5)?;
}

test("escape boxes for heap consumer") {
    assert(take(pair(1)) == 3)?;
}

test("wider and mixed-heap products stay heap-shaped") {
    let t = boxed_triple();
    assert(t[0] + t[1] + t[2] == 6)?;
    let (n, s) = mixed_heap(9);
    assert(n == 9)?;
    assert(s == "x")?;
}
