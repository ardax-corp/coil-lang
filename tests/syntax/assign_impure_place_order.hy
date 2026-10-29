// Plain assigns through a call index or an index base lower from HIR:
// the value first, then base, then index, each once.
use tests::assert;

fn step(Vec<int> log, int tag, int out) -> int {
    log.push(tag);
    return out;
}

test("call index runs after the value") {
    let log: Vec<int> = Vec::new();
    let a = Vec::from([0, 0, 0]);
    a[step(log, 2, 2)] = step(log, 1, 7);
    assert!(a[2] == 7);
    assert!(log.len() == 2);
    assert!(log[0] == 1);
    assert!(log[1] == 2);
}

test("nested index base") {
    let rows = Vec::from([Vec::from([1, 2]), Vec::from([3, 4])]);
    rows[1][0] = 30;
    assert!(rows[1][0] == 30);
    assert!(rows[0][1] == 2);
}

test("compound assign through an index base") {
    let rows = Vec::from([Vec::from([1, 2]), Vec::from([3, 4])]);
    rows[0][1] += 5;
    assert!(rows[0][1] == 7);
}

// Shared, so the write through the call's result reaches the caller's rows.
shared class Rows {
    pub v: Vec<int>,
}

fn rows_of(Vec<int> log, Rows a) -> Rows {
    log.push(2);
    return a;
}

test("call base for an index") {
    let log: Vec<int> = Vec::new();
    let a = new Rows(Vec::from([0, 0]));
    rows_of(log, a).v[step(log, 3, 1)] = step(log, 1, 5);
    assert!(a.v[1] == 5);
    assert!(log[0] == 1);
    assert!(log[1] == 2);
    assert!(log[2] == 3);
}
