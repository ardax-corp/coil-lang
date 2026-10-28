// A thread that spawns a thread of its own and then waits for it in `recv`
// gets the value: the child is queued on the spawner's own reactor worker,
// so with one worker nothing else could run it while the spawner blocks
// (and the root, also in `recv`, could not help). The wait runs queued jobs
// on a spare thread instead of sitting on the worker.
use tests::assert;
use thread::{spawn, join, send, recv, channel};

fn produce(Sender tx) -> int {
    let _ = send(tx, 5);
    return 0;
}

fn relay(Sender tx) -> int {
    let ends = match channel() {
        Result::Ok(e) => e,
        Result::Err(_) => {
            return -1;
        },
    };
    let child = match spawn(produce, ends[0]) {
        Result::Ok(t) => t,
        Result::Err(_) => {
            return -2;
        },
    };
    let v = match recv(ends[1]) {
        Result::Ok(v) => v,
        Result::Err(_) => -3,
    };
    let _ = join(child);
    let _ = send(tx, v + 1);
    return 0;
}

test("recv from a thread spawned by a thread") {
    let (tx, rx) = channel()?;
    let worker = spawn(relay, tx)?;
    let v = recv(rx)?;
    let _ = join(worker)?;
    assert!(v == 6);
}
