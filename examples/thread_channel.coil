// Threads: send a value over a channel.
//
// Channel calls fail with a `ChannelError` and `spawn` / `join` with a
// `ThreadError`, so `main`'s `?`s take the channel errors and the thread
// calls are matched.
//
// @output hello

use thread::{Sender, channel, join, recv, send, spawn};
use io::sync::print;
use string::format;

fn producer(Sender tx) {
    send(tx, "hello")?;
}

fn main() {
    let (tx, rx) = channel()?;
    let t = match spawn(producer, tx) {
        Ok(t) => t,
        Err(e) => panic format("spawn failed: %v", e),
    };
    let _ = print(format("%s", recv(rx)?));
    match join(t) {
        Ok(sent) => sent?,
        Err(e) => panic format("join failed: %v", e),
    }
}
