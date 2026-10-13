// Regression: parenthesized `(self).field` must still emit GetField.
// Pre-fix, `receiver_type` ignored `Group`, so Access fell through to
// `LoadField(0)` and `send` received a bogus value (often looking like
// the class instance was passed instead of the field).
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
