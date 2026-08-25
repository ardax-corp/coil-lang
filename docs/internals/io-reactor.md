# IO reactor

coil keeps two runtime facets on each root [`Machine`](../../machine/src/vm.rs):

| Facet | Module | Role |
|-------|--------|------|
| CPU | [`reactor.rs`](../../machine/src/reactor.rs) | Work-stealing Coil `Job`s (`spawn` / auto-par) |
| IO | [`io_reactor.rs`](../../machine/src/io_reactor.rs) | handle readiness for streams / TLS |

They share a lifecycle (cloned onto pool workers) but **never** put blocking IO onto stealable CPU jobs.

Host streams store a [`NativeHandle`](../../machine/src/io_handle.rs) (`File` / `TcpStream` / `TcpListener` / `UdpSocket`). The reactor waits on a copyable [`WaitHandle`](../../machine/src/io_handle.rs): Unix `poll(2)` on the fd, Windows `WSAPoll` for sockets and `WaitForSingleObject` for file/stdio handles.

## Async-first model

| Surface | Behavior |
|---------|----------|
| L0 `read` / `write` / `accept` | Always non-blocking; `WouldBlock` when not ready |
| `await_readable` / `await_writable` | **Top-level:** park the VM (`PendingIoWait`) until ready. **Inside a coroutine:** register a waiter and yield so many awaits can share one `poll` |
| `drive()` | Non-blocking `poll_once` on registered async waiters |
| `wait_ready()` | Block until ≥1 registered waiter is ready (batch); no-op when none registered |
| **`block_on(coro)`** (prelude) | Resume until `done`; calls `wait_ready` between resumes |
| Userland `io::sync::{write_all, …}` | Coil loops over L0 + `await_*` ([coil-stdlib IO](https://github.com/ardax-corp/coil-stdlib/blob/main/docs/io.md)) — top-level park path |

Preferred DX — async work, sync boundary:

```coil
use io::{Stream};
async fn copy(Stream a, Stream b) -> Result<(), IoError> {
    // L0 + await_* …
}
fn main() {
    block_on(copy(in, out))?;
}
```

`block_on` is auto-imported from `prelude`. Intermediate `yield`s are discarded;
only the final `return` value is kept. IO `await_*` inside the coroutine yields
cooperatively; `block_on` parks on `wait_ready` between resumes.

## Batching without `block_on`

Multiple coroutine handles can register waiters and share one poll:

```coil
use io::{wait_ready, ...};

fn main() {
    let h1 = serve(c1);
    let h2 = serve(c2);
    while !done(h1) || !done(h2) {
        if !done(h1) { resume h1; }
        if !done(h2) { resume h2; }
        wait_ready();
    }
}
```

Each `await_*` inside `serve` yields after registering interest; `wait_ready`
runs one multiplexed wait over all outstanding handles.

## Waiting on readiness

Top-level `await_*` and sync adapters call
[`IoReactor::wait_fd`](../../machine/src/io_reactor.rs) (via
[`reactor_wait_fd`](../../machine/src/io.rs)). Cooperative awaits use
[`register_wait`](../../machine/src/io_reactor.rs) + yield.
Userland sync adapters (`write_all`, …) reach the park path through top-level
`await_readable` / `await_writable`.

When a CPU reactor is bound (`HostStateGuard`), those blocking waits use
[`wait_fd_helping`](../../machine/src/io_reactor.rs): short poll slices interleaved with
[`Reactor::help_once`](../../machine/src/reactor.rs).

**TLS handshake is different:** `tls_*_enable` waits via
[`reactor_wait_fd_no_help`](../../machine/src/io.rs) so a mid-handshake park
cannot nest-steal the peer `thread::spawn` job onto the same stack (that
deadlocked both sides under `COIL_MAX_WORKER_THREADS=1` — COI-116). The pool
worker still runs the peer while the waiter polls.

After enable, `StreamKind::Tls` IO (`stream_read` / `stream_write` / close)
uses in-tree rustls while the `tls` feature is on, or dloaded `coil_tls_*`
when the stream holds a native session pointer (`dload("tls")` /
`[ffi] search_paths`). WouldBlock from enable still returns that session:
attach (`kind = Tls`), park, then continue on read/write — do not free it
and do not call enable again. WouldBlock from later `.so` IO is the same
tagged `IoError` and parks on the VM reactor; do not handshake on a blocking
`.so` thread. `coil_tls_disable` is close_notify; `coil_tls_free` drops the
session.

## Env / knobs

IO waits inherit the same `Machine` as CPU work; pool size is still
`COIL_MAX_WORKER_THREADS` (CPU facet only).
