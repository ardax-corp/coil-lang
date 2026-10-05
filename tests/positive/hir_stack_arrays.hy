// HIR lowering of `let a = [..]` locals kept in frame slots: literal and
// computed indexes, stores, `%` indexes, and selects as binary operands.

fn pick(int i) -> int {
    let a = [10, 20, 30, 40];
    return a[i];
}

fn sum_twice(int i) -> int {
    let a = [10, 20, 30, 40];
    let x = a[i];
    return x + a[i] + a[i];
}

fn both_sides(int i, int j) -> int {
    let a = [1, 2, 4, 8];
    return a[i] * 100 + a[j];
}

fn bump(int i) -> int {
    let a = [0, 0, 0];
    a[i] = 5;
    a[i] += 2;
    a[0] = a[0] + 1;
    return a[0] * 100 + a[1] * 10 + a[2];
}

fn wrap(int i) -> int {
    let a = [1, 2, 3];
    a[i % 3] += 10;
    return a[i % 3];
}

fn floats(int i) -> float {
    let a = [1.5, 2.5];
    a[i] = a[i] * 2.0;
    return a[0] + a[1];
}

fn flags(int i) -> bool {
    let a = [false, true, false];
    return a[i] && !a[2];
}

fn count(int n) -> int {
    let a = [0, 0, 0, 0];
    let i = 0;
    while i < n {
        a[i % 4] += i;
        i += 1;
    }
    return a[0] + a[1] * 10 + a[2] * 100 + a[3] * 1000;
}

fn nested(int i) -> int {
    let idx = [2, 0, 1];
    let vals = [7, 8, 9];
    return vals[idx[i]];
}

test("literal and computed reads") {
    assert(pick(0) == 10)?;
    assert(pick(3) == 40)?;
    assert(sum_twice(2) == 90)?;
    assert(both_sides(3, 1) == 802)?;
    assert(nested(0) == 9)?;
    assert(nested(1) == 7)?;
}

test("stores and compound stores") {
    assert(bump(0) == 800)?;
    assert(bump(1) == 170)?;
    assert(bump(2) == 107)?;
    assert(floats(1) == 6.5)?;
    assert(flags(1))?;
    assert(!flags(0))?;
}

test("modulo indexes stay in range") {
    assert(wrap(4) == 12)?;
    assert(wrap(-1) == 13)?;
    assert(count(8) == 10 * 1000 + 8 * 100 + 6 * 10 + 4)?;
}
