// Output: jf
use thread::{detach, join, spawn, ThreadError};
use io::{stdout, write};
use string::{format, to_bytes};

fn work() -> int {
    return 1;
}

fn main() {
    let t = spawn(work)?;
    detach(t)?;
    let msg = match join(t) {
        Result::Ok(_) => "joined",
        Result::Err(e) => match e {
            ThreadError::JoinFailed => "jf",
            default => "other",
        },
    };
    write(stdout(), to_bytes(format("%s", msg)));
}
