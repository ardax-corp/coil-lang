// @output disc
use thread::{channel, close, try_recv, ThreadError};
use io::{stdout, write};
use string::{format, to_bytes};

fn main() {
    let pair = channel()?;
    let tx = pair[0];
    let rx = pair[1];
    close(tx)?;
    let msg = match try_recv(rx) {
        Result::Ok(_) => "ok",
        Result::Err(e) => match e {
            ThreadError::Disconnected => "disc",
            default => "other",
        },
    };
    let _ = write(stdout(), to_bytes(format("%s", msg)));
}
