// @output 9\n
// A typed `Receiver<int>` gives `recv` its value type, so arithmetic on
// the received value needs no annotation (#1845).
use io::sync::println;
use string::format;
use thread::{channel, recv, send, Receiver, Sender, ThreadError};

fn square(Receiver<int> rx) -> Result<int, ThreadError> {
    let n = recv(rx)?;
    return Ok(n * n);
}

fn run() -> Result<int, ThreadError> {
    let ends: (Sender<int>, Receiver<int>) = channel()?;
    send(ends[0], 3)?;
    return square(ends[1]);
}

fn main() {
    println(format("%i", run().ok() ?? 0))?;
}
