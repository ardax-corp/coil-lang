// A disk-module import (`io::sync::write_all`) is visible inside a lambda body
// without a `use (…)` capture, like a virtual-module import.
// Output: ok
use io::stdout;
use io::sync::write_all;
use string::to_bytes;

fn main() {
    let f = fn () => write_all(stdout(), to_bytes("ok"));
    f()?;
}
