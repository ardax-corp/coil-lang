// TLS is userland: https://github.com/ardax-corp/coil-tls
//
// Add coil-tls to the module roots and the native search path:
//
//   coil --root src --root ../coil-tls/src --ffi-search-path ../coil-tls/native \
//     --allow-dload tls --dload-trusted tls app.hy
//
// `tls` needs `--allow-dload tls` plus `--dload-trusted tls` (or
// `--dload-pin tls=SHA256`). `--ffi-search-path` only locates the file.
// Without allow, `dload` is `LibraryDenied`. A missing libtls that passed
// the gate is `LibraryNotFound`.
//
// Then:
//   use tls::{client, server};
//   let s = client::enable(tcp, "example.com", { verify: true, ... })?;
//
// `use tls` / `use io::net::tls` without coil-tls on roots does not resolve.
//
// Output: use-coil-tls

use io::stdout;
use io::sync::write_all;
use string::to_bytes;

fn main() {
    write_all(stdout(), to_bytes("use-coil-tls"));
}
