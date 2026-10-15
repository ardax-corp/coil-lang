// A parenthesized receiver `(self).field` reads the field like `self.field`.
// Output: hi
use thread::{channel, recv, send, Sender};
use io::{stdout, write};
use string::{format, to_bytes};

class Worker {
    pub tx: Sender,
}

impl Worker {
    pub fn push(string msg) {
        send(self.tx, msg)?;
    }
}

fn main() {
    let pair = channel()?;
    let w = new Worker(pair[0]);
    w.push("hi")?;
    write(stdout(), to_bytes(format("%s", recv(pair[1])?)));
}
