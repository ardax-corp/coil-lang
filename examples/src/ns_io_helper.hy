// Helper for tests/host/io_try_in_dependency.hy: a dependency that does
// stream IO with `?` on every host call.
use io::{close, open, write, IoError};

fn write_greeting(string path) -> Result<(), IoError> {
    let h: byte = 104;
    let i: byte = 105;
    let nl: byte = 10;
    let payload = Vec::from([h, i, nl]);
    let w = open(path, "w")?;
    write(w, payload)?;
    close(w)?;
    return;
}
