# IO reactor

coil keeps two runtime facets on each root [`Machine`](../../machine/src/vm.rs):

| Facet | Module | Role |
|-------|--------|------|
| CPU | [`reactor.rs`](../../machine/src/reactor.rs) | Work-stealing Coil `Job`s (`spawn` / auto-par) |
| IO | [`io_reactor.rs`](../../machine/src/io_reactor.rs) | handle readiness for streams / attached package IO |

They share a lifecycle (cloned onto pool workers) but **never** put blocking IO onto stealable CPU jobs.

Host streams store a [`NativeHandle`](../../machine/src/io_handle.rs) (`File` / `TcpStream` / `TcpListener` / `UdpSocket`). The reactor waits on a copyable [`WaitHandle`](../../machine/src/io_handle.rs): Unix `poll(2)` on the fd, Windows `WSAPoll` for sockets and `WaitForSingleObject` for file/stdio handles.

## Async-first model

| Surface | Behavior |
|---------|----------|
| L0 `read` / `write` / `accept` | Always non-blocking; `WouldBlock` when not ready |
| `wait_readable` / `wait_writable` (old names `await_readable` / `await_writable` still work) | Inside a `task::scope` with other tasks: suspend the **task** and run others ([tasks](tasks.md)). Otherwise: park the VM (`PendingIoWait`) until ready, inside a generator too (a generator never yields because of IO) |
| `drive()` / `wait_ready()` | **Deprecated** (warning `E0129`). Leftovers from manual multiplexing; nothing registers waiters for them any more. Use `task::scope` |
| **`block_on(coro)`** (prelude) | **Deprecated** (warning `E0129`). Resume until `done`. IO inside parks as above |
| Userland `io::sync::{write_all, …}` | Coil loops over L0 + `wait_readable` / `wait_writable` ([coil-stdlib IO](https://github.com/ardax-corp/coil-stdlib/blob/main/docs/io.md)), so they work unchanged in and out of tasks |

Concurrent IO uses tasks:

```coil
use task::{scope, Scope};

fn main() {
    let r = scope(fn (Scope s) {
        let a = s.spawn(fn () => serve(c1));
        let b = s.spawn(fn () => serve(c2));
        0
    });
}
```

Each IO wait inside `serve` suspends its task; the scheduler polls every
waiting task's handle at once when none is ready to run.

## Waiting on readiness

The VM park path and sync adapters call
[`IoReactor::wait_fd`](../../machine/src/io_reactor.rs) (via
[`reactor_wait_fd`](../../machine/src/io.rs)). A task wait uses
[`register_wait`](../../machine/src/io_reactor.rs) on the scheduler's own
`IoReactor` and [`take_ready`](../../machine/src/io_reactor.rs) after
`wait_any`.

When a CPU reactor is bound (`HostStateGuard`), those blocking waits use
[`wait_fd_helping`](../../machine/src/io_reactor.rs): short poll slices interleaved with
[`Reactor::help_once`](../../machine/src/reactor.rs).

**Attached package handshake is different:** userland `Stream.attach` +
`Stream.park` parks via
[`reactor_wait_fd_no_help`](../../machine/src/io.rs) and pumps one native
step per `read` / `write` until the handshake completes,
so a mid-handshake park cannot nest-steal the peer `thread::spawn` job onto
the same stack (that deadlocked both sides under `COIL_MAX_WORKER_THREADS=1`
— COI-116). The pool worker still runs the peer while the waiter polls.
Inside a `task::scope` with other tasks, `Stream.park` suspends the task
instead (it returns a park request), so a TLS handshake does not stall the
scheduler.

`Stream.attach` is a compile-time capability (`--allow-attach` / `HostGrants`,
default deny). It is not a process-wide switch and is not read from
`coil.toml`. Ungated source fails typecheck (`E0408`). Archived bytecode
with HostInvoke 120 runs attach; there is no VM `allow_attach` re-check.
`--allow-dload` does not grant attach. Function pointers must be symbols from a hashed (or trusted/host)
`dload`, not raw `i64` transmutes.

After `Stream.attach`, IO (`stream_read` / `stream_write` / close) dispatches
to the registered C vtable. The VM does not have a TLS-named stream kind and
does not call `coil_tls_*`. coil-tls enable is `dload` + attach. `WouldBlock`
from package IO is the tagged `IoError` and parks on the VM reactor; do not
handshake on a blocking `.so` thread. Stream close and GC send shutdown when
the fd is still usable, then Drop frees. If the fd is already gone, free-only
is OK (best-effort close_notify).

## HostInvoke ids (attach / park / clock / math)

| Native | HostInvoke id | Language |
|--------|---------------|----------|
| `stream_attach` | **119** | `Stream.attach` |
| `stream_park` | **120** | `Stream.park` |
| `clock_wall_nanos` | **121** | `clock::wall_nanos` (unix UTC nanos) |
| `clock_mono_nanos` | **122** | `clock::mono_nanos` (process Instant snapshot) |
| `clock_sleep_ms` | **123** | `clock::sleep_ms` (thread sleep; suspends the task inside a `task::scope`) |
| `result_unit_probe` | **124** | host `Result<(), E>` probe |
| `math_atan` … `math_tanh` | **125–135** | M1 `prelude::math` (see below) |
| `task_scope_open` … `task_yield` | **144–151** | embedded `task` module ([tasks](tasks.md)); archive minor 32 |
| `unwind_resume` | **152** | end of a `defer` cleanup pad ([tasks](tasks.md#unwinding)); archive minor 33 |
| `task_cancel`, `task_shield_enter`, `task_shield_exit` | **153–155** | embedded `task` module; archive minor 33 |

119/120 are live package-IO natives, not reserved TLS/crypto/regex panic stubs.
121–123 are process clocks (`use clock::{…}`); Instant is a Coil `int` of
`mono_nanos`, not a host HashMap. Current archive is **major 4 / minor 5**
(minor 5 = M1 math append). TLS (`tls_client_enable` … `tls_alpn_protocol`)
and virtual crypto slots were dropped before the major-4 reset (historical
minor 14); holes collapsed. Regex slots dropped earlier. Do not treat
COI-37 / COI-209 / COI-215 stub reservations as live for these ids.

M1 math (**125–135**, archive minor 5): `atan`, `atan2`, `asin`, `acos`,
`log10`, `log2`, `cbrt`, `rem`, `sinh`, `cosh`, `tanh`. Frozen `sin` … `pow`
stay **102–110**. Named `PI` / `E` / `TAU` live on coil-stdlib
[`num`](https://github.com/ardax-corp/coil-stdlib/blob/main/docs/modules.md)
(`static const`, not HostInvoke). Package process/WebSocket/collections APIs
belong in those package READMEs — this page only names host ids the VM owns.

Virtual-time names still occupy panic stubs earlier in the table so the time
block does not slide. Source of truth: `machine/src/host_natives.rs`.

## Env / knobs

IO waits inherit the same `Machine` as CPU work; pool size is still
`COIL_MAX_WORKER_THREADS` (CPU facet only).
