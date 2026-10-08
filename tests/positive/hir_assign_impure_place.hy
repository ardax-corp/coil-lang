// Plain assigns through a call index or an index base lower from HIR:
// the value first, then base, then index, each once.
fn step(Vec<int> log, int tag, int out) -> int {
    log.push(tag);
    return out;
}

test("call index runs after the value") {
    let log: Vec<int> = Vec::new();
    let a = Vec::from([0, 0, 0]);
    a[step(log, 2, 2)] = step(log, 1, 7);
    assert(a[2] == 7)?;
    assert(len(log) == 2)?;
    assert(log[0] == 1)?;
    assert(log[1] == 2)?;
}

test("nested index base") {
    let rows = Vec::from([Vec::from([1, 2]), Vec::from([3, 4])]);
    rows[1][0] = 30;
    assert(rows[1][0] == 30)?;
    assert(rows[0][1] == 2)?;
}

test("compound assign through an index base") {
    let rows = Vec::from([Vec::from([1, 2]), Vec::from([3, 4])]);
    rows[0][1] += 5;
    assert(rows[0][1] == 7)?;
}

fn rows_of(Vec<int> log, Vec<int> a) -> Vec<int> {
    log.push(2);
    return a;
}

test("call base for an index") {
    let log: Vec<int> = Vec::new();
    let a = Vec::from([0, 0]);
    rows_of(log, a)[step(log, 3, 1)] = step(log, 1, 5);
    assert(a[1] == 5)?;
    assert(log[0] == 1)?;
    assert(log[1] == 2)?;
    assert(log[2] == 3)?;
}

test("compound assign through computed index bases") {
    let rows = Vec::from([Vec::from([1, 2]), Vec::from([3, 4])]);
    let i = 1;
    rows[i][0] *= 10;
    rows[i - 1][i] -= 2;
    assert(rows[1][0] == 30)?;
    assert(rows[0][1] == 0)?;
    let grid = [[1.5, 2.0], [3.0, 4.0]];
    grid[1][1] += 0.5;
    assert(grid[1][1] == 4.5)?;
}
