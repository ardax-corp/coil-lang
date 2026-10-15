// Regression: field access on `new Class(...)` must use GetField, and
// `new` must not leave a stash slot between a HostInvoke native-id and
// the field value (StorePop+final LOAD used to bury the id so `send`
// saw the instance address as the native id).
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
    write(stdout(), to_bytes(format("%s", recv(pair[1])?)));
}
