# `io` virtual module

Non-blocking file / stdio / TCP / UDP streams. **Not** auto-imported:

```coil
use io::{stdout, open, read, write, close};
use io::sync::{write_all, read_to_end};   // optional blocking adapters (coil-stdlib)
```

| Export | Kind | Notes |
|--------|------|-------|
| `Stream` | Opaque type | Heap handle; closed on GC drop |
| `IoError` | Builtin enum | `WouldBlock`, `NotFound`, `PermissionDenied`, `AlreadyClosed`, `InvalidInput`, `Other`, `NotADirectory`, `AlreadyExists`, `TimedOut`, `Truncated`, `Certificate`, `Handshake` |
| `Read` / `Write` | Typeclasses | `impl` for `Stream`; methods = free functions |
| `stdin` / `stdout` / `stderr` | `() -> Stream` | Dup'd fds |
| `open` / `close` / `read` / `write` / `write_from` | L0 | Never busy-spin; `read` → `Result<Option<int>, IoError>` (`None` = EOF); `write_from(s, buf, offset)` writes `buf[offset..]` without allocating a suffix array |
| `await_readable` / `await_writable` | Async await | Top-level parks VM; inside a coro registers + yields (batch via `wait_ready`) |
| `drive` | `() -> int` | Poll async waiters once (non-blocking) |
| `wait_ready` | `() -> int` | Block until ≥1 registered waiter is ready |
| `block_on` | Prelude | `block_on(coro) -> Y` — auto-imported; drives `async fn` to completion |
| `from_bytes` / `to_bytes` | Text aliases | UTF-8 `Vec<byte> ↔ string` (`from_bytes` → `Result<string, IoError>`); also exported by [`string`](string.md) |
| `io::net::tcp::{connect,connect_timeout,listen,accept}` | TCP | Nested module — `use io::net::tcp::{connect, listen, …};`; timeout `ms <= 0` waits forever |
| `io::net::tcp::{peer_addr,local_addr,set_nodelay,shutdown}` | TCP helpers | Address tuples, `TCP_NODELAY`, and half-close (`0` read, `1` write, `2` both) |
| `io::net::udp::{bind,connect,send_to,recv_from,local_port}` | UDP | Nested module; `recv_from` → `(nbytes, host, port)` |

TLS is **not** a virtual module. Use the [coil-tls](https://github.com/ardax-corp/coil-tls) package (`use tls::{client, server}`) with `libtls` on `[ffi] search_paths`. See [tls](tls.md).

## Userland sync adapters (`io::sync`)

Blocking helpers and whole-file IO are **coil-stdlib**, not host natives:
[IO adapters](https://github.com/ardax-corp/coil-stdlib/blob/main/docs/io.md)
(`write_all`, `read_to_end`, `print` / `println`, `io::file::{read_text, …}`).

```coil
use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};
```

Prefer `async fn` + prelude `block_on` when structuring concurrent IO.

`connect` / `connect_timeout` try **every** DNS result under one absolute
deadline. `listen` / UDP `bind` still use the first resolved address — prefer
an explicit IP (e.g. `127.0.0.1`) when family order matters.

OS `TimedOut` maps to `IoError::TimedOut` (not `WouldBlock`).

Buffers are **`Vec<byte>`**. Use `string::{from_bytes, to_bytes}` for text; `io::{from_bytes, to_bytes}` remain aliases. Use `write_all(stdout(), to_bytes(...))` for stdout text. HTTP is userland [coil-http](https://github.com/ardax-corp/coil-http) — install via [spool](../manual/http-client.md). TLS for HTTPS is [coil-tls](tls.md).

See [Tutorial 10 — IO streams](../manual/tutorial/10-io-streams.md) and `examples/io_*.hy`.

---

## Related

- [IO tutorial](../manual/tutorial/10-io-streams.md)
- [coil-stdlib IO adapters](https://github.com/ardax-corp/coil-stdlib/blob/main/docs/io.md)
- [io::fs](io-fs.md)
- [string](string.md)
