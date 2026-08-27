// TLS is userland: https://github.com/ardax-corp/coil-tls
//
// Add coil-tls to module roots and native search paths:
//
//   [module]
//   roots = ["./src", "../coil-tls/src"]
//   [ffi]
//   search_paths = ["../coil-tls/native"]
//   allow = ["tls"]
//
// `tls` needs `[ffi] allow` plus `trusted = true` on the coil-tls dep
// (or a matching `[[package.native]] sha256`). search_paths only locates
// the file. A missing libtls that passed the gate is LibraryNotFound.
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
