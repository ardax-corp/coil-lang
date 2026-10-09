// T3 cross-boundary waits: `task::channel` between tasks, `task::blocking`,
// and `thread` channels / join / locks that suspend only the waiting task.
use task::{scope, Scope, TaskError, Channel, ChannelError};
use thread::{spawn, join, send, recv, mutex, with_lock, lock, unlock, ThreadError, channel as thread_channel};
use clock::sleep_ms;

fn ok_or(Result<int, TaskError> r, int fallback) -> int {
    return match r {
        Result::Ok(v) => v,
        Result::Err(_) => fallback,
    };
}

fn got(Result<int, ThreadError> r) -> int {
    return match r {
        Result::Ok(v) => v,
        Result::Err(_) => -1,
    };
}

fn take(Channel<int> ch) -> int {
    return match ch.recv() {
        Result::Ok(v) => v,
        Result::Err(_) => -1,
    };
}

fn put(Channel<int> ch, int v) {
    match ch.send(v) {
        Result::Ok(_) => {},
        Result::Err(_) => panic "send on a closed channel",
    }
}

fn slow_square(int n) -> int {
    sleep_ms(30);
    return n * n;
}

fn slow_seven() -> int {
    sleep_ms(30);
    return 7;
}

test("a full channel suspends the sender until the receiver takes a value") {
    let log: Vec<string> = Vec::new();
    let ch: Channel<int> = task::channel(1);
    let r = scope(
        fn (Scope s) use (ch, log) {
            s
                .spawn(
                    fn () use (ch, log) {
                        put(ch, 1);
                        log.push("sent 1");
                        put(ch, 2);
                        log.push("sent 2");
                        ch.close();
                        0
                    },
                );
            let total = 0;
            task::yield_now();
            log.push("receiving");
            let done = false;
            while !done {
                match ch.recv() {
                    Result::Ok(v) => {
                        total = total + v;
                    },
                    Result::Err(_) => {
                        done = true;
                    },
                }
            }
            total
        },
    );
    assert(ok_or(r, -1) == 3)?;
    assert(log[0] == "sent 1" && log[1] == "receiving" && log[2] == "sent 2")?;
}

test("try_send and try_recv do not wait") {
    let ch: Channel<int> = task::channel(2);
    put(ch, 1);
    put(ch, 2);
    let full = match ch.try_send(3) {
        Result::Err(ChannelError::Full) => true,
        default => false,
    };
    assert(full)?;
    assert(ch.len() == 2 && ch.capacity() == 2)?;
    assert(take(ch) == 1 && take(ch) == 2)?;
    let empty = match ch.try_recv() {
        Result::Err(ChannelError::Empty) => true,
        default => false,
    };
    assert(empty)?;
    ch.close();
    let closed = match ch.try_recv() {
        Result::Err(ChannelError::Closed) => true,
        default => false,
    };
    assert(closed && ch.is_closed())?;
}

test("values are shared, not copied") {
    let ch: Channel<Vec<int>> = task::channel(1);
    let v: Vec<int> = Vec::new();
    let r = scope(
        fn (Scope s) use (ch, v) {
            s
                .spawn(
                    fn () use (ch) {
                        match ch.recv() {
                            Result::Ok(got) => got.push(42),
                            Result::Err(_) => {},
                        }
                        0
                    },
                );
            match ch.send(v) {
                Result::Ok(_) => {},
                Result::Err(_) => {},
            }
            0
        },
    );
    assert(ok_or(r, -1) == 0)?;
    assert(v.len() == 1 && v[0] == 42)?;
}

test("blocking runs on a worker while other tasks keep going") {
    let log: Vec<string> = Vec::new();
    let r = scope(
        fn (Scope s) use (log) {
            let b = s
                .spawn(
                    fn () use (log) {
                        let n = match task::blocking_with(slow_square, 6) {
                            Result::Ok(v) => v,
                            Result::Err(_) => -1,
                        };
                        log.push("blocking done");
                        n
                    },
                );
            s
                .spawn(
                    fn () use (log) {
                        log.push("other task ran");
                        0
                    },
                );
            let seven = match task::blocking(slow_seven) {
                Result::Ok(v) => v,
                Result::Err(_) => -1,
            };
            ok_or(b.join(), -100) + seven
        },
    );
    assert(ok_or(r, -1) == 43)?;
    assert(log[0] == "other task ran" && log[1] == "blocking done")?;
}

fn produce(Sender tx) -> int {
    sleep_ms(20);
    let _ = send(tx, 5);
    sleep_ms(20);
    let _ = send(tx, 6);
    return 0;
}

test("a thread channel recv suspends only the receiving task") {
    let (tx, rx) = thread_channel()?;
    let worker = spawn(produce, tx)?;
    let log: Vec<string> = Vec::new();
    let r = scope(
        fn (Scope s) use (rx, log) {
            let reader = s
                .spawn(
                    fn () use (rx, log) {
                        let a = got(recv(rx));
                        log.push("got first");
                        let b = got(recv(rx));
                        a + b
                    },
                );
            s
                .spawn(
                    fn () use (log) {
                        log.push("ticker");
                        0
                    },
                );
            ok_or(reader.join(), -100)
        },
    );
    join(worker)?;
    assert(ok_or(r, -1) == 11)?;
    assert(log[0] == "ticker" && log[1] == "got first")?;
}

fn bump(int n) -> (int, int) {
    return (n + 1, n);
}

test("a contended with_lock suspends only the waiting task") {
    let m = mutex(10)?;
    lock(m)?;
    let log: Vec<string> = Vec::new();
    let r = scope(
        fn (Scope s) use (m, log) {
            let locker = s
                .spawn(
                    fn () use (m, log) {
                        let before = got(with_lock(m, bump));
                        log.push("locked");
                        before
                    },
                );
            s
                .spawn(
                    fn () use (m, log) {
                        log.push("unlocking");
                        let _ = unlock(m);
                        0
                    },
                );
            ok_or(locker.join(), -100)
        },
    );
    assert(ok_or(r, -1) == 10)?;
    assert(log[0] == "unlocking" && log[1] == "locked")?;
}

test("join on a running thread suspends only the joining task") {
    let log: Vec<string> = Vec::new();
    let r = scope(
        fn (Scope s) use (log) {
            let joiner = s
                .spawn(
                    fn () use (log) {
                        let n = match spawn(slow_square, 4) {
                            Result::Ok(t) => got(join(t)),
                            Result::Err(_) => -1,
                        };
                        log.push("joined");
                        n
                    },
                );
            s
                .spawn(
                    fn () use (log) {
                        log.push("ticker");
                        0
                    },
                );
            ok_or(joiner.join(), -100)
        },
    );
    assert(ok_or(r, -1) == 16)?;
    assert(log[0] == "ticker" && log[1] == "joined")?;
}
