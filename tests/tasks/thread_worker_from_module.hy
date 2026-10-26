// A worker thread whose body lives in another module and uses `?` on its
// channel reads, fed jobs until "stop"; the entry's `?`s share the link.
// `main`'s `?`s all take `ThreadError`, so its channel calls match instead.
// @output a,b,
use thread::{Sender, Thread, channel, join, send, spawn};
use pool::worker::run_jobs;

class Worker {
    pub thread: Thread,
    pub tx: Sender,
}

impl Worker {
    pub fn submit(string job) {
        match send(self.tx, job) {
            Ok(_) => {},
            Err(_) => panic "send failed",
        }
    }

    pub fn join() {
        join(self.thread)?;
    }
}

fn main() {
    let pair = match channel() {
        Ok(p) => p,
        Err(_) => panic "channel failed",
    };
    let t = spawn(run_jobs, pair[1])?;
    let w = new Worker(t, pair[0]);
    w.submit("a");
    w.submit("b");
    w.submit("stop");
    w.join()?;
}
