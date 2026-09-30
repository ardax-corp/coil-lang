use thread::{Receiver, Sender, channel, join, recv, send, spawn};
use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

// Request/reply: pass both channel ends to the worker as one tuple.
fn worker((Receiver, Sender) ends) {
    let job = recv(ends[0])?;
    send(ends[1], job)?;
    return 0;
}

fn main() {
    let jobs = channel()?;
    let replies = channel()?;
    let t = spawn(worker, (jobs[1], replies[0]))?;
    send(jobs[0], "ping")?;
    write_all(stdout(), to_bytes(format("%s", recv(replies[1])?)));
    join(t)?;
}
