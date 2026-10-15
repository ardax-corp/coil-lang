// A worker still blocked in `recv` when `main` returns without `join` is
// kept alive by the exit's auto-join, so it receives and prints.
// Output: hi
use thread::{channel, recv, send, spawn, Receiver};
use io::{stdout, write};
use string::{format, to_bytes};

fn worker(Receiver rx) {
    let msg = recv(rx)?;
    write(stdout(), to_bytes(format("%s", msg)));
    return 0;
}

fn main() {
    let pair = channel()?;
    let t = spawn(worker, pair[1])?;
    send(pair[0], "hi")?;
}
