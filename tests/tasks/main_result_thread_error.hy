// `main` can return `Result<unit, ThreadError>`, take `?` on `channel()`,
// `thread::spawn`, `recv` and `join`, and end with a bare `return Ok(())`
// (#1738): the returned constructor lowers in `main` as it does in a helper.
// @output 7
use thread::{channel, spawn, join, send, recv, Sender, ThreadError};
use io::{stdout, write};
use string::{format, to_bytes};

fn work(Sender tx) -> Result<int, ThreadError> {
    send(tx, 7)?;
    return 1;
}

fn main() -> Result<unit, ThreadError> {
    let (tx, rx) = channel()?;
    let t = spawn(work, tx)?;
    let _ = write(stdout(), to_bytes(format("%i", recv(rx)?)));
    join(t)?;
    return Ok(());
}
