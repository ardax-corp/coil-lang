// Same rebind rule for `defer` bodies that call disk-module imports.
// @output ad
use io::stdout;
use io::sync::write_all;
use string::to_bytes;

fn main() {
    defer {
        let _ = write_all(stdout(), to_bytes("d"));
    }
    let _ = write_all(stdout(), to_bytes("a"));
}
