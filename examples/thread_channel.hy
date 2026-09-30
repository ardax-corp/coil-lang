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
    write_all(stdout(), to_bytes(format("%s", recv(rx)?)));
    join(t)?;
}
