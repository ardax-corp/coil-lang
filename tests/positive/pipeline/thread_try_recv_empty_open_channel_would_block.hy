// Output: wb
use thread::{channel, try_recv, ThreadError};
use io::{stdout, write};
use string::{format, to_bytes};

fn main() {
    let pair = channel()?;
    let rx = pair[1];
    let msg = match try_recv(rx) {
        Result::Ok(_) => "ok",
        Result::Err(e) => match e {
            ThreadError::WouldBlock => "wb",
            default => "other",
        },
    };
    write(stdout(), to_bytes(format("%s", msg)));
}
