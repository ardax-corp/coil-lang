// #1758: a panic in the function a `thread::spawn` worker starts at runs
// that function's `defer`s once, then fails the join. The worker's entry is
// the floor of its unwind: its pad used to rerun forever, hanging the join.
use tests::assert;
use thread::{Sender, channel, join, recv, send, spawn};

fn dies(Sender tx) -> int {
    defer use (tx) {
        let _ = send(tx, 7);
    }
    panic "worker died";
}

fn dies_in_nested_block(Sender tx) -> int {
    let n = 8;
    {
        defer use (tx, n) {
            let _ = send(tx, n);
        }
    }
    panic "worker died";
}

test("a worker's own defer runs once when it panics") {
    let pair = channel()?;
    let t = spawn(dies, pair[0])?;
    let joined = join(t);
    assert!(joined.is_err());
    assert!(recv(pair[1])? == 7);
}

test("a defer from a nested block of the worker runs once too") {
    let pair = channel()?;
    let t = spawn(dies_in_nested_block, pair[0])?;
    let joined = join(t);
    assert!(joined.is_err());
    assert!(recv(pair[1])? == 8);
}
