// Same rebind rule for `defer` bodies that call disk-module imports.
// Output: ad
use io::stdout;
use io::sync::write_all;
use string::to_bytes;

fn main() {
    defer {
        write_all(stdout(), to_bytes("d"));
    }
    write_all(stdout(), to_bytes("a"));
}
