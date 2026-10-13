// Aliased disk imports must rebind under the local alias name.
// Output: ok
use io::stdout;
use io::sync::write_all as wa;
use string::to_bytes;

fn main() {
    let f = fn () => wa(stdout(), to_bytes("ok"));
    f()?;
}
