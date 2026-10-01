// Expected: compile failure — an `extern "c"` block in an imported module is
// denied like one in the entry file.
use libc_extern::{c_strlen};

fn main() {
    let _ = c_strlen("hello");
}
