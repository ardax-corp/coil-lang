// @output 42\n9\n5\n
// Thread handles carry their value type (#1905): an annotation names it,
// a bare name infers it, and `recv` / `join` give it back.
use io::sync::println;
use string::format;
use thread::{channel, join, mutex, recv, send, spawn, with_lock, Mutex, Receiver, Sender, Thread, ThreadError, ChannelError, LockError};

fn answer() -> int {
    return 42;
}

fn square(Receiver<int> rx) -> int {
    return match recv(rx) {
        Ok(n) => n * n,
        Err(_) => 0,
    };
}

fn bump(Mutex counter) {
    with_lock(counter, fn (int n) => (n + 1, 0))?;
}

fn loaded() -> Result<(Sender<int>, Receiver<int>), ChannelError> {
    let ends: (Sender<int>, Receiver<int>) = channel()?;
    send(ends[0], 3)?;
    return Ok(ends);
}

fn counter() -> Result<Mutex<int>, LockError> {
    return mutex(4);
}

fn current(Mutex<int> m) -> Result<int, LockError> {
    return with_lock(m, fn (int n) => (n, n));
}

fn run() -> Result<string, ThreadError> {
    let t: Thread<int> = spawn(answer)?;
    let first = join(t)?;

    let ends = match loaded() {
        Ok(pair) => pair,
        Err(_) => panic "channel failed",
    };
    let sq = spawn(square, ends[1])?;
    let second = join(sq)?;

    let m = match counter() {
        Ok(m) => m,
        Err(_) => panic "mutex failed",
    };
    let b = spawn(bump, m)?;
    let _ = join(b)?;
    let third = current(m).ok() ?? -1;
    return Ok(format("%i\n%i\n%i", first, second, third));
}

fn main() {
    println(run().ok() ?? "failed")?;
}
