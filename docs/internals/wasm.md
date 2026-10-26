# coil on WebAssembly (wasm32-wasip1)

`coil` builds for `wasm32-wasip1`, the target the website playground runs in
the browser (inside a Web Worker, with a WASI shim that serves the stdlib and
the user's files from memory). The same build runs under any WASI host:

```
rustup target add wasm32-wasip1
cargo build --release --target wasm32-wasip1 -p coil
node --no-turbo-fast-api-calls scripts/wasi_coil.mjs \
    target/wasm32-wasip1/release/coil.wasm --root .deps/coil-stdlib/src hello.hy
```

`scripts/wasi_coil.mjs` preopens the current directory as `.` and the temp
directory as `/tmp`, so pass paths relative to the current directory. Node 22
needs `--no-turbo-fast-api-calls`: it can collect garbage inside a WASI fast
API call (`path_open`) and segfault walking the frames.

Release builds publish it as `coil-wasm32-wasip1.wasm` (edge and `v*` tags).

## What differs from a native build

| Area | On wasm |
|------|---------|
| `Value` | The word is a `u64`, not a pointer: wasm32 pointers are 32 bits, and ints and floats need all 64. `Value::bits()` reads the whole word on every target; build a `Value` from a `u64` with `Value::from(u64)`, never through `as *mut u8`. |
| FFI | No dynamic libraries: `machine/src/ffi/nolib.rs` stands in for libffi and libloading. `extern` declarations compile; loading a library or calling through FFI fails with `FfiError::Unsupported`. |
| Threads | No OS threads (no `atomics` target feature). The reactor starts no workers (`reactor::NO_THREADS`): `join`, a `recv` on an empty channel and the end of `main` run queued `thread::spawn` jobs on the current thread. A spawned thread therefore runs when something waits for it, not at once; output interleaving differs from native. A wait nothing can satisfy fails (`JoinFailed`, `Disconnected`) instead of hanging. Auto-par runs sequentially. |
| Tasks | Work as natively: tasks share one thread anyway. |
| Sockets | `io::net` calls fail with `IoError::Other` (WASI preview 1 has no sockets). |
| Files | Work against whatever the WASI host preopens. Symlinks are not supported. |
| Terminal | `io::tty`: nothing is a terminal; raw mode fails with `NotATerminal`. |
| stdio | WASI cannot `dup`, so a stdio stream owns descriptor 0/1/2; closing the stream never closes the process's own stdio. |
| `coil test` | Runs the test runner in the `coil` binary (no `coil-test` to re-exec). `coil mutate` is not available. |
| Process ids | `std::process::id` panics on wasm; code that mixes in the pid (temp names, seeds) uses 0 there. |

## Tests

CI's `wasm` job runs clippy for the target, builds `coil.wasm` and runs every
corpus under Node (`scripts/test_corpora.py --wasm PATH`). A test file that
needs something wasm lacks says so in its header and is skipped there:

```
// @needs sockets
```

Known features: `sockets`, `symlinks`, `ffi`. An unknown name fails the file.
