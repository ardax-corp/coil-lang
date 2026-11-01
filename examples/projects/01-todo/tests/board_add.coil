// Unit test: adding tasks grows the board.
use tests::assert;
use board::{Task, add_task, count_done};

test("empty board has no tasks") {
    let b: Vec<Task> = [];
    assert!(b.len() == 0, "empty");
}

test("board grows on add") {
    let b: Vec<Task> = [];
    assert!(add_task(b, "alpha") == 1, "first id");
    assert!(add_task(b, "beta") == 2, "second id");
    assert!(b.len() == 2, "two tasks");
    assert!(count_done(b) == 0, "none done yet");
}
