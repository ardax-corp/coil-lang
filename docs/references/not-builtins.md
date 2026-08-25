# What is NOT a builtin

Compiler virtual modules cover systems I/O, threads, crypto, time, env, FFI,
and IEEE float math. Collections, text/bytes helpers, decimal parse, path, blocking
IO adapters, whole-file helpers, regex, TLS, and HTTP are **not** HostInvoke/opcodes — they
live in userland packages ([coil-stdlib](https://github.com/ardax-corp/coil-stdlib),
[coil-regex](https://github.com/ardax-corp/coil-regex),
[coil-tls](https://github.com/ardax-corp/coil-tls), …).

Still not a compiler builtin (and not coil-stdlib either):

| Category | Examples | Where to look |
|----------|----------|----------------|
| Raw memory | `alloc`, `free` | [`gc::Root` / `gc::Weak`](gc.md) |
| Regex | PCRE2 | [coil-regex](https://github.com/ardax-corp/coil-regex) ([regex](regex.md)) |
| TLS | rustls | [coil-tls](https://github.com/ardax-corp/coil-tls) ([tls](tls.md)) |
| HTTP in the VM | opcodes / natives | [coil-http](https://github.com/ardax-corp/coil-http) via [spool](../manual/http-client.md); HTTPS uses [coil-tls](tls.md) |

Use **`io`** for streams, **FFI** for C libraries, or **host natives** when embedding the VM in Rust.

---

## Related

- [coil-stdlib docs](https://github.com/ardax-corp/coil-stdlib/blob/main/docs/README.md)
- [io](io.md)
- [tls](tls.md)
- [ffi](ffi.md)
- [host-natives](host-natives.md)
