// @output 42\n9\n5\n
// Thread handles carry their value type (#1905): an annotation names it,
// a bare name infers it, and `recv` / `join` give it back.
use io::sync::println;
use string::format;
use thread::{channel, join, mutex, recv, send, spawn, with_lock, Mutex, Receiver, Sender, Thread};

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

fn run() -> Result<string, ThreadError> {
    let t: Thread<int> = spawn(answer)?;
    let first = join(t)?;

    let ends: (Sender<int>, Receiver<int>) = channel()?;
    send(ends[0], 3)?;
    let sq = spawn(square, ends[1])?;
    let second = join(sq)?;

    let m = mutex(4)?;
    let b = spawn(bump, m)?;
    (join(b)?)?;
    let third = with_lock(m, fn (int n) => (n, n))?;
    return Ok(format("%i\n%i\n%i", first, second, third));
}

fn main() {
    println(run() ?? "failed")?;
}
