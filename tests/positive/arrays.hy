// Array literals, index, mutation, Vec push, len, for-in.
test("literal index") {
    let a = [10, 20, 30];
    assert(a[0] == 10)?;
    assert(a[1] == 20)?;
    assert(a[2] == 30)?;
}

test("store index") {
    let a = [1, 2, 3];
    a[1] = 99;
    assert(a[1] == 99)?;
    assert(a[0] == 1)?;
}

// Heap `a[i] = v` spills only the RHS before compiling array/index. Eval order
// must stay value → index (array is a plain local here).
fn record_step(Vec<int> log, int id, int ret) -> int {
    log.push(id);
    return ret;
}

test("indexed assign evaluates RHS before index") {
    let log: Vec<int> = Vec::new();
    let a = Vec::from([0, 0, 0]);
    a[record_step(log, 2, 1)] = record_step(log, 1, 42);
    assert(a[1] == 42)?;
    assert(log[0] == 1)?;
    assert(log[1] == 2)?;
}

test("compound index assign") {
    let a = [10, 20, 30];
    a[1] += 5;
    assert(a[1] == 25)?;
}

test("len of literal") {
    let a = [1, 2, 3, 4];
    assert(len(a) == 4)?;
}

test("push grows vec") {
    let a = Vec::from([1]);
    a.push(2);
    a.push(3);
    assert(len(a) == 3)?;
    assert(a[0] == 1)?;
    assert(a[1] == 2)?;
    assert(a[2] == 3)?;
}

test("empty then push") {
    let a: Vec<int> = Vec::new();
    a.push(7);
    a.push(8);
    assert(len(a) == 2)?;
    assert(a[0] == 7)?;
    assert(a[1] == 8)?;
}

test("for in array") {
    let sum = 0;
    for x in [1, 2, 3, 4] {
        sum = sum + x;
    }
    assert(sum == 10)?;
}

test("nested array index") {
    let a = [[1, 2], [3, 4]];
    assert(a[0][1] == 2)?;
    assert(a[1][0] == 3)?;
}

// Runtime Index / StoreIndex contract (coil-website /docs/references/arrays):
// variable OOB read → -1; OOB write → no-op (no panic).
test("variable oob index yields minus one") {
    let a = [10, 20, 30];
    let hi = 3;
    let neg = 0 - 1;
    assert(a[hi] == -1)?;
    assert(a[neg] == -1)?;
}

test("variable oob store is noop") {
    let a = [10, 20, 30];
    let hi = 99;
    a[hi] = 7;
    assert(a[0] == 10)?;
    assert(a[1] == 20)?;
    assert(a[2] == 30)?;
}
