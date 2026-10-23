// Unit test: advancing a task moves Todo -> Doing -> Done.
use tests::assert;
use board::{Status, Task, add_task, advance_task, count_done};

test("advance moves Todo to Doing to Done") {
    let b: Vec<Task> = [];
    let id = add_task(b, "ship");
    assert!(b[0].status == Status::Todo, "starts Todo");
    advance_task(b, id)?;
    assert!(b[0].status == Status::Doing, "now Doing");
    advance_task(b, id)?;
    assert!(b[0].status == Status::Done, "now Done");
    assert!(count_done(b) == 1, "one done");
}

test("unknown id is an error") {
    let b: Vec<Task> = [];
    let r = advance_task(b, 1);
    assert!(r.is_err(), "no task 1");
}
