// TLS is userland: https://github.com/ardax-corp/coil-tls
//
// Add coil-tls to module roots and native search paths:
//
//   [module]
//   roots = ["./src", "../coil-tls/src"]
//   [ffi]
//   search_paths = ["../coil-tls/native"]
//
// `tls` is a first-party dload stem: no `[ffi] allow` and no lock hash.
// search_paths only locates the file. A missing libtls is LibraryNotFound.
//
// Then:
//   use tls::{client, server};
//   let s = client::enable(tcp, "example.com", { verify: true, ... })?;
//
// `use tls` / `use io::net::tls` without coil-tls on roots does not resolve.
use io::{stdout};
use io::sync::{write_all};
use string::{to_bytes};

fn main() {
    write_all(stdout(), to_bytes("use-coil-tls"));
}
