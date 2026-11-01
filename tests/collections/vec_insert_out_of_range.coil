// `Vec::insert` past the end (or at a negative index) panics with a plain
// bounds message naming the index and length, not an internal host-call
// error (#1078). Inserting at `len` appends.
use tests::assert;
use task::{scope, Scope, TaskError};

fn message(Result<int, TaskError> r) -> string {
    return match r {
        Result::Ok(_) => "ok",
        Result::Err(TaskError::Panicked(m)) => m,
        Result::Err(_) => "other",
    };
}

// Run `body` as a child task: its panic message, or "ok".
fn outcome(unit -> int body) -> string {
    return message(scope(fn (Scope s) use (body) {
        let _ = s.spawn(body);
        0
    }));
}

fn insert_at(int i) -> int {
    let v: Vec<int> = Vec::new();
    v.push(1);
    v.insert(i, 2);
    return v.len();
}

test("insert at 0 and at len") {
    assert!(insert_at(0) == 2);
    assert!(insert_at(1) == 2);
}

test("insert past the end panics with the index and length") {
    let m = outcome(fn () => insert_at(5));
    assert!(m == "Vec::insert: index 5 out of bounds (len 1)", m);
}

test("insert at a negative index panics") {
    let m = outcome(fn () => insert_at(-1));
    assert!(m == "Vec::insert: index -1 out of bounds (len 1)", m);
}
