// Default `dload` allowlist is crypto/tls/regex/time (COI-229). `#[ffi(lib = "c")]`
// needs a host extra stem; a default `coil` run denies libc.

use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};
#[ffi(lib = "c")]
fn strlen(string s) -> int;

fn main() {
    let n = strlen("hello");
    write_all(stdout(), to_bytes(format("%i", n)));
}
