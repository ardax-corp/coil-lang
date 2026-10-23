// Threads: send a value over a channel.
//
// @output hello

use thread::{Sender, channel, join, recv, send, spawn};
use io::sync::print;
use string::format;

fn producer(Sender tx) {
    send(tx, "hello")?;
}

fn main() {
    let pair = channel()?;
    let tx = pair[0];
    let rx = pair[1];
    let t = spawn(producer, tx)?;
    let _ = print(format("%s", recv(rx)?));
    (join(t)?)?;
}
