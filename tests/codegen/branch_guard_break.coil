// A `break` under `!flag` and under a two-local compare: the branch
// fusions keep the guard's sense, so each loop leaves on its first trip.
use tests::assert;

fn not_flag() -> int {
    let done = false;
    let n = 0;
    while n < 10 {
        if !done {
            break;
        }
        n = n + 1;
    }
    return n;
}

fn two_locals() -> int {
    let v: Vec<int> = Vec::new();
    v.push(1);
    v.push(2);
    let a = v[0];
    let b = v[1];
    let n = 0;
    while n < 10 {
        if a < b {
            break;
        }
        n = n + 1;
    }
    return n;
}

test("break under a negated flag") {
    assert!(not_flag() == 0);
}

test("break under a two-local compare") {
    assert!(two_locals() == 0);
}
