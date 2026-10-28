// OS threads: `send` / `recv` keep FIFO order across a thread, a request /
// reply pair of channels round-trips, `with_lock` serializes increments from
// several threads, and a `thread::recv` inside a task suspends only that task.
use tests::assert;
use thread::{Sender, Receiver, Mutex, spawn, join, channel, send, recv, close, mutex, with_lock, ChannelError, LockError};
use task::{scope, Scope, TaskError};
use clock::sleep_ms;

fn produce(Sender tx) -> Result<int, ChannelError> {
    let i = 0;
    while i < 5 {
        send(tx, i * 10)?;
        i += 1;
    }
    close(tx)?;
    return 5;
}

fn doubler((Receiver, Sender) ends) -> Result<int, ChannelError> {
    let n = recv(ends[0])?;
    send(ends[1], n * 2)?;
    return 0;
}

fn bump_many(Mutex m) -> Result<int, LockError> {
    let i = 0;
    while i < 100 {
        with_lock(m, fn (int n) => (n + 1, 0))?;
        i += 1;
    }
    return 0;
}

fn late_reply(Sender tx) -> Result<int, ChannelError> {
    sleep_ms(30);
    send(tx, 7)?;
    return 0;
}

test("values sent from a thread arrive in order") {
    let pair = channel()?;
    let t = spawn(produce, pair[0])?;
    let got: Vec<int> = Vec::new();
    let i = 0;
    while i < 5 {
        got.push(recv(pair[1])?);
        i += 1;
    }
    assert!(got[0] == 0 && got[1] == 10 && got[4] == 40);
    assert!(join(t)? == Result::Ok(5));
}

test("a request and reply channel pair round-trips") {
    let jobs = channel()?;
    let replies = channel()?;
    let t = spawn(doubler, (jobs[1], replies[0]))?;
    send(jobs[0], 21)?;
    assert!(recv(replies[1])? == 42);
    (join(t)?)?;
}

test("with_lock serializes increments from several threads") {
    let m = mutex(0)?;
    let a = spawn(bump_many, m)?;
    let b = spawn(bump_many, m)?;
    let c = spawn(bump_many, m)?;
    (join(a)?)?;
    (join(b)?)?;
    (join(c)?)?;
    assert!(with_lock(m, fn (int x) => (x, x))? == 300);
}

test("a thread recv inside a task lets the other tasks run") {
    let log: Vec<string> = Vec::new();
    let pair = channel()?;
    let tx = pair[0];
    let rx = pair[1];
    let worker = spawn(late_reply, tx)?;
    let r = scope(fn (Scope s) use (mut log, rx) {
        let waiter = s.spawn(fn () use (mut log, rx) {
            let v = match recv(rx) {
                Result::Ok(v) => v,
                Result::Err(_) => -1,
            };
            log.push("received");
            v
        });
        s.spawn(fn () use (mut log) {
            log.push("other ran");
            0
        });
        match waiter.join() {
            Result::Ok(v) => v,
            Result::Err(_) => -1,
        }
    });
    (join(worker)?)?;
    assert!(r == Result::Ok(7));
    assert!(log[0] == "other ran" && log[1] == "received");
}
