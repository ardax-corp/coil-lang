// Regression: inherent `fn send` must not shadow `thread::send`, so
// `send(self.channel, msg)` typechecks and runs.
// @output hi
use thread::{channel, recv, send, spawn, Sender, Thread};
use io::{stdout, write};
use string::{format, to_bytes};

class ThreadWrapper {
    pub thread: Thread,
    pub channel: Sender,
}

impl ThreadWrapper {
    pub fn send(string msg) {
        send(self.channel, msg)?;
    }
}

fn work() -> int {
    return 0;
}

fn main() {
    let pair = channel()?;
    let t = match spawn(work) {
        Ok(t) => t,
        Err(_) => panic "spawn failed",
    };
    let w = new ThreadWrapper(t, pair[0]);
    w.send("hi")?;
    let _ = write(stdout(), to_bytes(format("%s", recv(pair[1])?)));
}
