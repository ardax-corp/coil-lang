// `main` can return `Result<unit, ThreadError>`, take `?` on
// `thread::spawn` and `join`, and end with a bare `return Ok(())` (#1738):
// the returned constructor lowers in `main` as it does in a helper. Channel
// calls report a `ChannelError`, so a helper returning one takes their `?`.
// @output 7
use thread::{channel, spawn, join, send, recv, Sender, ThreadError, ChannelError};
use io::{stdout, write};
use string::{format, to_bytes};

fn work(Sender tx) -> Result<int, ChannelError> {
    send(tx, 7)?;
    return 1;
}

fn main() -> Result<unit, ThreadError> {
    let (tx, rx) = match channel() {
        Result::Ok(pair) => pair,
        Result::Err(_) => panic "channel failed",
    };
    let t = spawn(work, tx)?;
    let got = match recv(rx) {
        Result::Ok(v) => v,
        Result::Err(_) => -1,
    };
    let _ = write(stdout(), to_bytes(format("%i", got)));
    join(t)?;
    return Ok(());
}
