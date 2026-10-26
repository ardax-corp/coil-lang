// A worker thread fed jobs over a channel until it receives "stop".
//
// `main`'s `?`s take the `ThreadError` of `spawn` / `join`; the channel
// calls report a `ChannelError`, which `submit` turns into a panic.
//
// @output a,b,

use thread::{Sender, Thread, channel, join, send, spawn};
use pool::worker::run_jobs;
use string::format;

class Worker {
    pub thread: Thread,
    pub tx: Sender,
}

impl Worker {
    pub fn submit(string job) {
        match send(self.tx, job) {
            Ok(_) => {},
            Err(e) => panic format("send failed: %v", e),
        }
    }

    pub fn join() {
        join(self.thread)?;
    }
}

fn main() {
    let pair = match channel() {
        Ok(p) => p,
        Err(e) => panic format("channel failed: %v", e),
    };
    let t = spawn(run_jobs, pair[1])?;
    let w = new Worker(t, pair[0]);
    w.submit("a");
    w.submit("b");
    w.submit("stop");
    w.join()?;
}
