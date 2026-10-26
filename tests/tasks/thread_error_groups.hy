// The `thread` module reports three error sets: channel calls a
// `ChannelError` (shared with `task::Channel`), lock calls a `LockError`,
// and spawn / join / detach a `ThreadError`.
use tests::assert;
use thread::{channel, close, send, try_recv, mutex, lock, try_lock, unlock, spawn, join, detach, ThreadError, ChannelError, LockError};
use task::{scope, Scope, TaskError, Channel};

fn channel_case(Result<int, ChannelError> r) -> string {
    return match r {
        Ok(_) => "ok",
        Err(ChannelError::Closed) => "closed",
        Err(ChannelError::Empty) => "empty",
        Err(ChannelError::Full) => "full",
    };
}

fn lock_case(Result<unit, LockError> r) -> string {
    return match r {
        Ok(_) => "ok",
        Err(LockError::WouldBlock) => "busy",
        Err(LockError::Poisoned) => "poisoned",
        Err(LockError::NotHeld) => "not held",
        Err(LockError::AlreadyHeld) => "already held",
    };
}

fn one() -> int {
    return 1;
}

test("thread channels report ChannelError") {
    let (tx, rx) = channel()?;
    assert!(channel_case(try_recv(rx)) == "empty");
    send(tx, 4)?;
    assert!(channel_case(try_recv(rx)) == "ok");
    close(tx)?;
    assert!(channel_case(try_recv(rx)) == "closed");
}

test("task channels report the same ChannelError") {
    let ch: Channel<int> = task::channel(1);
    assert!(channel_case(ch.try_recv()) == "empty");
    let _ = ch.try_send(1);
    let full = match ch.try_send(2) {
        Err(e) => e == ChannelError::Full,
        Ok(_) => false,
    };
    assert!(full);
}

test("mutex calls report LockError") {
    let a = mutex(0)?;
    let b = mutex(0)?;
    assert!(lock_case(unlock(a)) == "not held");
    assert!(lock_case(lock(a)) == "ok");
    assert!(lock_case(try_lock(a)) == "busy");
    assert!(lock_case(lock(b)) == "already held");
    assert!(lock_case(unlock(b)) == "not held");
    assert!(lock_case(unlock(a)) == "ok");
}

test("join after detach reports ThreadError") {
    let t = spawn(one)?;
    detach(t)?;
    let refused = match join(t) {
        Err(ThreadError::JoinFailed) => true,
        default => false,
    };
    assert!(refused);
}

// A generic value passes the compile-time sendable check; one that turns
// out not to be sendable is a panic, since `ChannelError` has no case for it.
fn forward<T>(T value) -> bool {
    let (tx, rx) = match channel() {
        Ok(pair) => pair,
        Err(_) => panic "channel failed",
    };
    let _ = send(tx, value);
    return true;
}

test("a generic non-sendable send panics") {
    let r = scope(fn (Scope s) {
        let _ = s.spawn(fn () => forward(fn (int n) => n + 1));
        0
    });
    let msg = match r {
        Err(TaskError::Panicked(m)) => m,
        default => "",
    };
    assert!(
        msg == "value is not sendable across threads (a closure, stream, coroutine, gc handle, partially applied function or cycle)",
    );
}
