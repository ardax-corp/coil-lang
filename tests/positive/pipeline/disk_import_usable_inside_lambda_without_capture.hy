// Disk-module imports (e.g. `io::sync::write_all`) are file-level globals.
// After `take_and_isolate` they must be rebound like virtual imports — not
// treated as missing captures inside lambdas.
// Output: ok
use io::stdout;
use io::sync::write_all;
use string::to_bytes;

fn main() {
    let f = fn () => write_all(stdout(), to_bytes("ok"));
    f()?;
}
