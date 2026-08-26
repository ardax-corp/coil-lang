# TLS ([coil-tls](https://github.com/ardax-corp/coil-tls))

TLS is **userland** in [coil-tls](https://github.com/ardax-corp/coil-tls), not a compiler builtin. rustls lives in that package's native cdylib (`libtls`), loaded with `dload("tls")`. Enable is `dload` + generic `Stream.attach` / `Stream.park` (no TLS-named `StreamKind`, no `io::__tls` HostInvoke). `use tls` / `use io::net::tls` without the package on `[module].roots` is a module-not-found error (COI-210).

## Sibling checkout

Clone [coil-tls](https://github.com/ardax-corp/coil-tls) beside your project and point `coil.toml` at it:

```toml
[module]
roots = ["./src", "../coil-tls/src"]

[ffi]
search_paths = ["../coil-tls/native"]
```

Build the native library in that repo, then:

```coil
use tls::{client, server};

let s = client::enable(tcp, "example.com", ClientOpts{
    verify: true,
    ca_pem: Option::None,
    ca_path: Option::None,
    timeout_ms: 0,
    alpn: "",
})?;
```

Package name is `tls`, so `use tls::{client, server}` matches the old virtual path. coil-http must use `tls::{client,server}::enable` with `ClientOpts` (not leftover HostInvoke, not anonymous records).

Handshake parks stay in the VM (`s.park()` / `reactor_wait_fd_no_help`). Do not move handshake onto a blocking `.so` thread. New packages should `s.attach(ptr, read, write, shutdown, free)` and `s.park()` instead of a VM TLS kind.

`examples/io_tls.hy` is a pointer at this package, not a leftover enable demo.

## Migrating from virtual `io::net::tls` / leftover `io::__tls`

Add coil-tls to `[module].roots`. Recompile any `.hyc` that imported the old virtual module or leftover `io::__tls` (archive **2.14** dropped those HostInvoke slots). Enable attaches a native session onto the same `Stream` via `Stream.attach`.

---

## Related

- [IO streams](io.md)
- [HTTP client](../manual/http-client.md)
- [Getting Started](../manual/getting-started.md)
