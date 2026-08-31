use thread::{Sender, Thread, channel, join, send, spawn};
use pool::worker::run_jobs;

class Worker {
    pub thread: Thread,
    pub tx: Sender,
}

impl Worker {
    pub fn submit(string job) {
        send(self.tx, job)?;
    }

    pub fn join() {
        join(self.thread)?;
    }
}

fn main() {
    let pair = channel()?;
    let t = spawn(run_jobs, pair[1])?;
    let w = new Worker(t, pair[0]);
    w.submit("a")?;
    w.submit("b")?;
    w.submit("stop")?;
    w.join()?;
}
