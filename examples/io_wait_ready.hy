// Batch two cooperative awaits without wrapping each in block_on.
// Each await_* inside an gen fn yields + registers; wait_ready polls both.
//
// Output: ok

use io::stdout;
use io::open;
use io::close;
use io::await_readable;
use io::wait_ready;
use io::sync::write_all;
use io::sync::read_to_end;
use string::{format, to_bytes};

gen fn slurp(string path) -> Result<int, IoError> {
    let s = open(path, "r")?;
    await_readable(s)?;
    let bytes = read_to_end(s)?;
    close(s)?;
    return len(bytes);
}

fn main() {
    let h1 = slurp("/etc/hosts");
    let h2 = slurp("/etc/passwd");
    while !done(h1) || !done(h2) {
        if !done(h1) {
            resume h1;
        }
        if !done(h2) {
            resume h2;
        }
        wait_ready();
    }
    write_all(stdout(), to_bytes(format("ok")));
}
