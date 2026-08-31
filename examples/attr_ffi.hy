// attr_ffi.hy — `extern "c"` block for compile-time libc bindings.
//
// Expected output: `5` (strlen of "hello").

use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};
extern "c" {
    fn strlen(string s) -> int;
}

fn main() {
    let n = strlen("hello");
    write_all(stdout(), to_bytes(format("%i", n)));
}
