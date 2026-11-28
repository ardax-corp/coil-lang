# IO reactor

coil keeps two reactors on each root [`Runtime`](../../machine/src/runtime.rs):

| Facet | Module | Role |
|-------|--------|------|
| CPU | [`reactor.rs`](../../machine/src/reactor.rs) | Work-stealing Coil `Job`s (`spawn` / auto-par) |
| IO | [`io_reactor.rs`](../../machine/src/io_reactor.rs) | handle readiness for streams |

They share a lifecycle (cloned onto pool workers) but **never** put blocking IO onto stealable CPU jobs.

Host streams store a [`NativeHandle`](../../machine/src/io_handle.rs) (`File` / `TcpStream` / `TcpListener` / `UdpSocket`). The reactor waits on a copyable [`WaitHandle`](../../machine/src/io_handle.rs): Unix `poll(2)` on the fd, Windows `WSAPoll` for sockets and `WaitForSingleObject` for file/stdio handles.

## Async-first model

| Surface | Behavior |
|---------|----------|
| L0 `read` / `write` / `accept` | Always non-blocking; `WouldBlock` when not ready |
| `wait_readable` / `wait_writable` (old names `await_readable` / `await_writable` still work) | Inside a `task::scope` with other tasks: suspend the **task** and run others ([tasks](tasks.md)). Otherwise: block the thread until ready, inside a generator too (a generator never yields because of IO) |
| `drive()` / `wait_ready()` | **Deprecated** (warning `E0129`). Leftovers from manual multiplexing; nothing registers waiters for them any more. Use `task::scope` |
| **`task::block_on(coro)`** | Library function in `task`: resume until `done`. IO inside parks as above. Prefer tasks |
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

The blocking path and sync adapters call
[`IoReactor::wait_fd`](../../machine/src/io_reactor.rs) (via
[`reactor_wait_fd`](../../machine/src/io.rs)). A task wait uses
[`register_wait`](../../machine/src/io_reactor.rs) on the scheduler's own
`IoReactor` and [`take_ready`](../../machine/src/io_reactor.rs) after
`wait_any`.

When a CPU reactor is bound (`HostStateGuard`), those blocking waits use
[`wait_fd_helping`](../../machine/src/io_reactor.rs): short poll slices interleaved with
[`Reactor::help_once`](../../machine/src/reactor.rs).

A package stream such as coil-tls's `TlsStream` is a Coil class over the
`io::Read` / `io::Write` / `io::Close` traits
([`stream_io.coil`](../../compiler/src/prelude/stream_io.coil)) and its own
`extern` calls. The runtime has no package stream kind.

## HostInvoke ids (clock / math)

| Native | HostInvoke id | Language |
|--------|---------------|----------|
| `stream_attach` | **119** | removed `Stream.attach` (panic stub) |
| `stream_park` | **120** | removed `Stream.park` (panic stub) |
| `clock_wall_nanos` | **121** | `clock::wall_nanos` (unix UTC nanos) |
| `clock_mono_nanos` | **122** | `clock::mono_nanos` (process Instant snapshot) |
| `clock_sleep_ms` | **123** | `clock::sleep_ms` (thread sleep; suspends the task inside a `task::scope`) |
| `result_unit_probe` | **124** | host `Result<(), E>` probe |
| `math_atan` … `math_tanh` | **125–135** | M1 `prelude::math` (see below) |
| `task_scope_open` … `task_yield` | **144–151** | embedded `task` module ([tasks](tasks.md)); archive minor 32 |
| `unwind_resume` | **152** | end of a `defer` cleanup pad ([tasks](tasks.md#unwinding)); archive minor 33 |
| `task_cancel`, `task_shield_enter`, `task_shield_exit` | **153–155** | embedded `task` module; archive minor 33 |
| `task_cond_new`, `task_cond_wait`, `task_cond_notify` | **156–158** | `task::channel` wait conditions ([tasks](tasks.md#channels-and-other-threads)); archive minor 34 |
| `contract_fail` | **159** | a failed `requires`, blaming the caller ([contracts](contracts.md#checks)); archive minor 35 |

119/120 are panic stubs of the removed `Stream.attach` / `Stream.park`, kept
so later ids do not move.
121–123 are process clocks (`use clock::{…}`); Instant is a Coil `int` of
`mono_nanos`, not a host HashMap. TLS (`tls_client_enable` … `tls_alpn_protocol`)
and virtual crypto slots were dropped before the major-4 reset (historical
minor 14); holes collapsed. Regex slots dropped earlier. Do not treat
COI-37 / COI-209 / COI-215 stub reservations as live for these ids.

M1 math (**125–135**, archive minor 5): `atan`, `atan2`, `asin`, `acos`,
`log10`, `log2`, `cbrt`, `rem`, `sinh`, `cosh`, `tanh`. Frozen `sin` … `pow`
stay **102–110**. `math_pow` (**110**) and `math_rem` (**132**) keep their rows
but no surface name reaches them: `prelude::math::{pow, rem}` were removed in
favour of the `**` and `%` operators. Named `PI` / `E` / `TAU` live on coil-stdlib
[`num`](https://github.com/ardax-corp/coil-stdlib/blob/main/docs/modules.md)
(`const`, not HostInvoke). Package process/WebSocket/collections APIs
belong in those package READMEs — this page only names host ids the runtime owns.

Virtual-time names still occupy panic stubs earlier in the table so the time
block does not slide. Source of truth: `machine/src/host_natives.rs`.

## Env / knobs

IO waits inherit the same `Runtime` as CPU work; pool size is still
`COIL_MAX_WORKER_THREADS` (CPU facet only).
