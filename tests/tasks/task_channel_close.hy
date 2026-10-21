// Closing a `task::channel`: `send` fails with `Closed`, `recv` drains the
// queued values first, and a task waiting in `send` or `recv` wakes up with
// `Closed`.
use tests::assert;
use task::{scope, Scope, TaskError, Channel, ChannelError};

fn is_closed_err(Result<unit, ChannelError> r) -> bool {
    return match r {
        Result::Err(ChannelError::Closed) => true,
        default => false,
    };
}

fn recv_code(Channel<int> ch) -> int {
    return match ch.recv() {
        Result::Ok(v) => v,
        Result::Err(ChannelError::Closed) => -1,
        Result::Err(_) => -2,
    };
}

test("send after close fails with Closed") {
    let ch: Channel<int> = task::channel(4);
    assert!(!is_closed_err(ch.send(1)));
    assert!(recv_code(ch) == 1);
    ch.close();
    assert!(ch.is_closed());
    assert!(is_closed_err(ch.send(1)));
    assert!(is_closed_err(ch.try_send(1)));
    assert!(ch.len() == 0);
}

test("recv drains queued values before reporting Closed") {
    let ch: Channel<int> = task::channel(3);
    let _ = ch.send(10);
    let _ = ch.send(20);
    ch.close();
    assert!(ch.len() == 2);
    assert!(recv_code(ch) == 10);
    assert!(recv_code(ch) == 20);
    assert!(recv_code(ch) == -1);
    assert!(recv_code(ch) == -1);
}

test("close wakes a task waiting in recv") {
    let ch: Channel<int> = task::channel(1);
    let r = scope(fn (Scope s) use (ch) {
        let reader = s.spawn(fn () use (ch) => recv_code(ch));
        task::yield_now();
        ch.close();
        match reader.join() {
            Result::Ok(v) => v,
            Result::Err(_) => -100,
        }
    });
    assert!(r == Result::Ok(-1));
}

test("close wakes a task waiting in send") {
    let ch: Channel<int> = task::channel(1);
    let r = scope(fn (Scope s) use (ch) {
        let _ = ch.send(1);
        let writer = s.spawn(fn () use (ch) => is_closed_err(ch.send(2)));
        task::yield_now();
        ch.close();
        match writer.join() {
            Result::Ok(b) => b,
            Result::Err(_) => false,
        }
    });
    assert!(r == Result::Ok(true));
    // The value queued before the close is still there.
    assert!(recv_code(ch) == 1);
    assert!(recv_code(ch) == -1);
}

test("closing twice is harmless") {
    let ch: Channel<string> = task::channel(1);
    ch.close();
    ch.close();
    assert!(ch.is_closed());
    let closed = match ch.try_recv() {
        Result::Err(ChannelError::Closed) => true,
        default => false,
    };
    assert!(closed);
}

test("every receiver waiting on a closed channel wakes") {
    let ch: Channel<int> = task::channel(2);
    let r = scope(fn (Scope s) use (ch) {
        let a = s.spawn(fn () use (ch) => recv_code(ch));
        let b = s.spawn(fn () use (ch) => recv_code(ch));
        let c = s.spawn(fn () use (ch) => recv_code(ch));
        task::yield_now();
        let _ = ch.send(5);
        ch.close();
        let total = 0;
        for t in [a, b, c] {
            total = total + match t.join() {
                Result::Ok(v) => v,
                Result::Err(_) => -100,
            };
        }
        total
    });
    // One receiver gets 5, the other two see Closed (-1 each).
    assert!(r == Result::Ok(3));
}
