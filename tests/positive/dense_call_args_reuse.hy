// A call whose later argument is computed from earlier ones (`c, 1, c - 1`)
// or repeats one (`x, x`) passes every argument, not the shared operands once.
fn g(int a, int b, int c) -> int {
    if c == 0 {
        return a * 10 + b;
    }
    return g(c, 1, c - 1);
}

fn pair(int a, int b) -> int {
    return a * 10 + b;
}

fn same(int x, int n) -> int {
    if n == 0 {
        return x;
    }
    return same(pair(x, x) % 97, n - 1);
}

test("later argument computed from earlier ones") {
    assert(g(0, 0, 3) == 11)?;
    assert(g(5, 5, 0) == 55)?;
}

test("repeated argument") {
    assert(pair(3, 3) == 33)?;
    assert(same(1, 2) == 11 * 11 % 97)?;
}
