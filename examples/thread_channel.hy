// Threads: send a value over a channel.
//
// Output: hello

use thread::{Sender, channel, join, recv, send, spawn};
use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};

fn producer(Sender tx) {
    send(tx, "hello")?;
}

fn main() {
    let pair = channel()?;
    let tx = pair[0];
    let rx = pair[1];
    let t = spawn(producer, tx)?;
    let _ = write_all(stdout(), to_bytes(format("%s", recv(rx)?)));
    join(t)?;
}
