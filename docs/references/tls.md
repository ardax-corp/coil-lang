# TLS ([coil-tls](https://github.com/ardax-corp/coil-tls))

TLS is **userland** in [coil-tls](https://github.com/ardax-corp/coil-tls), not a compiler builtin. rustls lives in that package's native cdylib (`libtls`), loaded with `dload("tls")`.

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

let s = client::enable(tcp, "example.com", {
    verify: true,
    ca_pem: Option::None,
    ca_path: Option::None,
    timeout_ms: 0,
    alpn: "",
})?;
```

Package name is `tls`, so `use tls::{client, server}` matches the old virtual path. `use io::net::tls` without coil-tls on `roots` is a module-not-found error.

coil-http consumes this package the same way (`roots` / `[dependencies]`), not a HostInvoke virtual module.

Handshake parks stay in the VM (`reactor_wait_fd_no_help`). Do not move handshake onto a blocking `.so` thread.

## Migrating from virtual `io::net::tls`

Add coil-tls to `[module].roots`. Recompile any `.hyc` that imported the old virtual module. HostInvoke ids for leftover `tls_*` natives are unchanged (no archive bump); enable attaches a native session onto the same `Stream`.

---

## Related

- [IO streams](io.md)
- [HTTP client](../manual/http-client.md)
- [Getting Started](../manual/getting-started.md)
