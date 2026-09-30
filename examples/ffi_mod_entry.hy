// COI-19: extern in an imported module + Vec before repeat calls.
use io::stdout;
use io::sync::write_all;
use string::{format, to_bytes};
use ffi_mod::sys::run_twice;

fn main() {
    let n = run_twice();
    let _ = write_all(stdout(), to_bytes(format("%v\n", n)));
}
