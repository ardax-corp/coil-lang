# 10 — IO streams

coil exposes non-blocking file, stdio, and TCP IO through the virtual
**`io`** module. Import it explicitly (like `ffi`):

```coil
use io::{stdout, open, close, from_bytes};
use io::sync::{write_all, read_to_end};
```

Buffers use the **`byte`** primitive and **`Vec<byte>`** vectors.

---

## `byte` and `Vec<byte>`

| Type | Notes |
|------|--------|
| `byte` | Integer in `0..=255`. Literals coerce when annotated / expected. |
| `Vec<byte>` | Growable byte buffer for `read` / `write`. |

```coil
use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};
fn main() {
    let b: byte = 255;
    let arr = Vec::from([1 as byte, 2 as byte, 3 as byte]);
    write_all(stdout(), to_bytes(format("%i", b)));
    write_all(stdout(), to_bytes(format("%i", len(arr))));
}
```

`byte` implements `Show` and `Eq`. It is **not** in `Num` / `Add` yet — use
`int` for arithmetic and convert at the boundary if needed.

### Text helpers

| Function | Signature | Notes |
|----------|-----------|--------|
| `from_bytes` | `Vec<byte> → Result<string, IoError>` | UTF-8 decode; invalid sequences → `InvalidInput` |
| `to_bytes` | `string → Vec<byte>` | UTF-8 encode (always succeeds) |

```coil
use io::{stdout, from_bytes};
use io::sync::{write_all};
use string::{format, to_bytes};

fn main() {
    let hello = Vec::from([104 as byte, 101 as byte, 108 as byte, 108 as byte, 111 as byte]);
    write_all(stdout(), to_bytes(format("%s", match from_bytes(hello) {
        Result::Ok(s) => s,
        Result::Err(_) => "err",
    })));
}
```

See `examples/io_text.hy`.

---

## Streams

`Stream` is an opaque heap handle. L0 ops never busy-spin: they return
`Err(IoError::WouldBlock)` when the OS would block.

| Function | Signature (simplified) | Behavior |
|----------|------------------------|----------|
| `stdin` / `stdout` / `stderr` | `() -> Stream` | Dup'd process stdio |
| `open(path, mode)` | `→ Result<Stream, IoError>` | Modes: `"r"`, `"w"`, `"a"`, `"rw"` |
| `close(s)` | `→ Result<(), IoError>` | Idempotent close on GC drop too |
| `read` / `write` | L0 | Never busy-spin; `WouldBlock` when not ready |
| `await_readable` / `await_writable` | async await | Top-level parks; inside a coro yields + registers for batch poll |
| `drive` | `() -> int` | Poll registered async waiters once; returns newly-ready count |
| `wait_ready` | `() -> int` | Block until ≥1 registered waiter is ready (multiplex) |
| `block_on` | prelude | Drive an `async fn` handle to completion (see [IO reactor](../../internals/io-reactor.md)) |
| `io::sync::{write_all,read_exact,read_to_end}` | [coil-stdlib](https://github.com/ardax-corp/coil-stdlib/blob/main/docs/io.md) | Blocking adapters over L0 + `await_*` |
| `io::net::tcp::{connect,listen,accept,…}` | TCP | `connect` / `connect_timeout` / `listen` / `accept`, plus address / shutdown helpers |
| `io::net::udp::{bind,send_to,recv_from,…}` | UDP | Datagram sockets; see below |

For stdout text, call `write_all(stdout(), to_bytes(...))`. Blocking adapters
and `io::file` are documented in
[coil-stdlib IO](https://github.com/ardax-corp/coil-stdlib/blob/main/docs/io.md).

TCP and UDP live in nested virtual modules — import them explicitly
(like `ffi::types`):

```coil
use io::{stdout};
use io::net::tcp::{connect};
use io::net::udp::{bind, local_port, send_to};
```

TLS is the [coil-tls](https://github.com/ardax-corp/coil-tls) package, not a
virtual module. See [tls](../../references/tls.md) and `examples/io_tls.hy`.

---

## UDP (`io::net::udp`)

UDP sockets are also `Stream` handles. Prefer the datagram helpers when
you need peer addresses:

| Function | Signature (simplified) | Behavior |
|----------|------------------------|----------|
| `bind(host, port)` | `→ Result<Stream, IoError>` | `port` may be `0` (ephemeral) |
| `local_port(s)` | `→ Result<int, IoError>` | Assigned local port after bind |
| `connect(host, port)` | `→ Result<Stream, IoError>` | Connected peer; then `read` / `write` work |
| `send_to(s, buf, host, port)` | `→ Result<int, IoError>` | Non-blocking `sendto` |
| `recv_from(s, buf)` | `→ Result<(int, string, int), IoError>` | `(nbytes, peer_host, peer_port)` |
| `io::sync::recv_from_wait(s, buf)` | same | Userland: `recv_from` + `await_readable` |

```coil
use io::{close, stdout};
use io::net::udp::{bind, local_port, send_to};
use io::sync::{recv_from_wait, write_all};
use string::{format, to_bytes};

fn main() {
    let server = bind("127.0.0.1", 0)?;
    let port = local_port(server)?;
    let client = bind("127.0.0.1", 0)?;
    let msg = Vec::from([72 as byte, 105 as byte]);
    send_to(client, msg, "127.0.0.1", port)?;
    let buf: Vec<byte> = Vec::from([0 as byte, 0 as byte, 0 as byte, 0 as byte, 0 as byte, 0 as byte, 0 as byte, 0 as byte]);
    let t = recv_from_wait(server, buf)?;
    write_all(stdout(), to_bytes(format("%i", t[0])));
}
```

See `examples/io_udp.hy`.

---

## TCP (`io::net::tcp`)

| Function | Signature (simplified) | Behavior |
|----------|------------------------|----------|
| `connect(host, port)` | `→ Result<Stream, IoError>` | Connected stream; `read` / `write` |
| `connect_timeout(host, port, ms)` | same | Connect deadline; `ms <= 0` waits forever |
| `listen(host, port)` | `→ Result<Stream, IoError>` | Listening socket |
| `accept(s)` | `→ Result<Stream, IoError>` | Non-blocking; `WouldBlock` if empty |
| `io::sync::accept_wait(s)` | same | Userland: `accept` + `await_readable` |
| `peer_addr(s)` / `local_addr(s)` | `→ Result<(string, int), IoError>` | Connected peer / local socket address |
| `set_nodelay(s, enabled)` | `→ Result<(), IoError>` | Toggle `TCP_NODELAY` on TCP streams (attached packages share the fd) |
| `shutdown(s, how)` | `→ Result<(), IoError>` | Half-close: `0` read, `1` write, `2` both |

TLS wrap of a TCP stream is [coil-tls](https://github.com/ardax-corp/coil-tls)
(`use tls::{client, server}`), not a virtual module. See [tls](../../references/tls.md).

---

## File round-trip

```coil
use io::{close, open, stdout};
use io::sync::{read_to_end, write_all};
use string::{format, to_bytes};

fn main() {
    let path = "/tmp/demo.bin";
    let data = Vec::from([72 as byte, 105 as byte]);
    let s = open(path, "w")?;
    write_all(s, data)?;
    close(s)?;

    let s = open(path, "r")?;
    let buf = read_to_end(s)?;
    close(s)?;
    write_all(stdout(), to_bytes(format("%i", len(buf))));
}
```

See `examples/io_file.hy` and `examples/io_eof.hy`.

---

## `Read` / `Write`

The virtual module registers typeclasses **`Read`** and **`Write`** with
`impl` for `Stream`. Free functions (`read`, `write`, …) and trait methods
lower to the same host natives (`HostInvoke`).

---

## Errors

`IoError` variants (unit payloads): `WouldBlock`, `NotFound`,
`PermissionDenied`, `AlreadyClosed`, `InvalidInput`, `Other`, `NotADirectory`,
`AlreadyExists`, `TimedOut`, `Truncated`, `Certificate`, `Handshake`.

`TimedOut` is distinct from `WouldBlock` (deadlines / OS timeouts vs
“try again”). Prefer `?` in `Result`-mode helpers (see
[Error handling](09-error-handling.md)).

---

## Related

- [Built-ins — `io` module](../../references/io.md)
- [Types — `byte`](../../references/types.md)
- [Examples catalog](../examples.md)
