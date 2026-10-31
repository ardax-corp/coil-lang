// A `const` can hold the `Result` of `channel()` and an `Option` of a scalar
// (#1714): a builtin enum application whose payloads hold no mutable interior
// is fully constant and does not warn E0119 (see compiler/tests/diagnostics.rs).
// @output 7 3
use thread::{channel, send, recv, ChannelError};
use io::{stdout, write};
use string::{format, to_bytes};

fn run() -> Result<int, ChannelError> {
    const ends = channel();
    const n: Option<int> = Some(3);
    const pair = ends?;
    send(pair[0], 7)?;
    const got = recv(pair[1])?;
    let _ = write(stdout(), to_bytes(format("%i %i", got, n.unwrap_or(0))));
    return 0;
}

fn main() {
    let _ = run();
}
