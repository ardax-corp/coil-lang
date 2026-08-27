// strlen.hy — User-defined binding to a C library function via
// the source-level `extern` block syntax. Demonstrates that
// integrating with 3rd-party libraries like libc, libcurl,
// or openssl is just writing coil code — NO VM
// rebuild, NO Rust closures to register, NO manual dload/
// declare/invoke ceremony.
//
// Default `dload` allowlist is crypto/tls/regex/time (COI-229). `extern "c"`
// needs a host extra stem; a default `coil` run denies libc.
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
