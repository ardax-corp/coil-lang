// Destructuring under HIR: `let` tuple and record patterns, and for-in
// patterns over arrays and dicts.

fn swap_sum(int a, int b) -> int {
    let (x, y) = (b, a);
    return x * 10 + y;
}

fn nested(int v) -> int {
    let ((a, b), c) = ((v, v + 1), v + 2);
    let { left, right } = { left: a + b, right: c };
    let (_, keep) = (left, right);
    return left * 100 + keep;
}

fn pairs_total() -> int {
    let total = 0;
    for (i, s) in [(1, "a"), (2, "bb"), (3, "ccc")] {
        total = total + i * len(s);
    }
    return total;
}

fn dict_values() -> int {
    let total = 0;
    for (_, v) in { a: 1, b: 2, c: 4 } {
        total = total + v;
    }
    return total;
}

fn dict_entries() -> int {
    let n = 0;
    for entry in { a: 5, b: 6 } {
        n = n + entry[1];
    }
    return n;
}

test("let patterns") {
    assert(swap_sum(1, 2) == 21)?;
    assert(nested(3) == 705)?;
}

test("for-in patterns") {
    assert(pairs_total() == 14)?;
    assert(dict_values() == 7)?;
    assert(dict_entries() == 11)?;
}
