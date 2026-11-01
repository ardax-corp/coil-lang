// Batch two cooperative awaits without wrapping each in block_on.
// Each await_* inside an gen fn yields + registers; wait_ready polls both.
//
// @output ok

use io::open;
use io::close;
use io::wait_readable;
use io::wait_ready;
use io::sync::print;
use io::sync::read_to_end;
use string::format;

gen fn slurp(string path) -> Result<int, IoError> {
    let s = open(path, "r")?;
    wait_readable(s)?;
    let bytes = read_to_end(s)?;
    close(s)?;
    return bytes.len();
}

fn main() {
    let h1 = slurp("/etc/hosts");
    let h2 = slurp("/etc/passwd");
    while !done(h1) || !done(h2) {
        if !done(h1) {
            let _ = resume h1;
        }
        if !done(h2) {
            let _ = resume h2;
        }
        wait_ready();
    }
    let _ = print(format("ok"));
}
