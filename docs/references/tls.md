# TLS ([coil-tls](https://github.com/ardax-corp/coil-tls))

TLS is **userland** in [coil-tls](https://github.com/ardax-corp/coil-tls), not a public compiler builtin. rustls lives in that package's native cdylib (`libtls`), loaded with `dload("tls")`.

coil-tls binds leftover HostInvoke (`tls_client_enable` … `tls_alpn_protocol`, ids 25–28 and 121) through **`io::__tls`** (`use io::__tls::client::enable`). That path is not named `tls` or `io::net::tls`: `use tls` / `use io::net::tls` without the package on `[module].roots` is still a module-not-found error (COI-210).

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

Package name is `tls`, so `use tls::{client, server}` matches the old virtual path. `use io::net::tls` / `use tls` without coil-tls on `roots` is a module-not-found error.

coil-http consumes this package the same way (`roots` / `[dependencies]`). Handshake parks stay in the VM (`reactor_wait_fd_no_help`). Do not move handshake onto a blocking `.so` thread.

## Leftover HostInvoke (`io::__tls`)

coil-tls re-exports leftover enable as `tls::client::enable(Stream, host, opts) -> Stream`. Internally it imports `use io::__tls::client::{enable}` (and the matching server / `alpn_protocol` leftovers). Bodies stay dload + `attach_enable_outcome` + park + empty read/write until Ready (no rustls in coil-lang). WouldBlock enable keeps the session; do not retry `enable`. Leftover `disable` sends `coil_tls_disable` (close_notify) on the live fd, then Drop `coil_tls_free`. Stream close / GC do the same when the fd is still usable. Do not import `io::__tls` from application code — use the package.

Unbounded `enable<T>` is not monomorphized: the call site boxes every argument (`Stream` as `ValueTag::Instance`, host as `String`, opts record as `Record`) and the generic body forwards those `Object::Boxed` cells without unboxing. Leftover enable peels one box on stream, host, and opts, then parses the inner instance the same way as a direct call. HostInvoke ids are unchanged.

`examples/tls_thread_loopback.hy` is the leftover client+server enable regression (COI-116): server `enable` in `thread::spawn`, client `enable` on the root, ALPN `h2`. Needs `libtls` on `[ffi] search_paths`.

## Migrating from virtual `io::net::tls`

Add coil-tls to `[module].roots`. Recompile any `.hyc` that imported the old virtual module. HostInvoke ids for leftover `tls_*` natives are unchanged (no archive bump); enable attaches a native session onto the same `Stream`.

---

## Related

- [IO streams](io.md)
- [HTTP client](../manual/http-client.md)
- [Getting Started](../manual/getting-started.md)
