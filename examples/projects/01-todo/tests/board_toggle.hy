// Unit test: advancing a task moves Todo -> Doing -> Done.
// Bind `board[i]` before `.status` — chained index+field in `assert` has
// been flaky under the test harness.
use tests::assert;
use board::{add_task, advance_task, count_done, empty_board};

test("advance moves Todo to Doing to Done") {
    let b = empty_board();
    b = add_task(b, "ship");
    let t0 = b[1];
    assert!(t0.status == 0, "starts Todo");
    advance_task(b, 1);
    let t1 = b[1];
    assert!(t1.status == 1, "now Doing");
    advance_task(b, 1);
    let t2 = b[1];
    assert!(t2.status == 2, "now Done");
    assert!(count_done(b) == 1, "one done");
}
