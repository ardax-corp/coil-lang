// Reading a field straight off `new Class(...)` yields that field's value
// (here a channel sender that is then used).
// Output: hi
use thread::{channel, recv, send, Sender};
use io::{stdout, write};
use string::{format, to_bytes};

class Worker {
    pub tx: Sender,
}

fn main() {
    let pair = channel()?;
    send((new Worker(pair[0])).tx, "hi")?;
    let _ = write(stdout(), to_bytes(format("%s", recv(pair[1])?)));
}
