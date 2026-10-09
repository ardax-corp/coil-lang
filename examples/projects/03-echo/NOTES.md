# 03-echo — notes

## What it shows

Single-process TCP echo on `task::scope`: a server task (`accept_wait`, read
one frame, echo it) and a client task (`connect`, send a frame, read the
echo) run concurrently on one thread. Each `io::sync` wait suspends only its
own task. Length-prefixed framing is in `protocol.hy`; the server and client
policies are pure helpers.

## Run

```bash
./examples/projects/03-echo/demo.sh
# ok
```

Always under `timeout` (the script wraps it).

## Test

```bash
./examples/projects/run-tests.sh
# or: cd examples/projects/03-echo && …/coil test
```

## Layout

| File | Role |
|------|------|
| `src/protocol.hy` | `encode_frame` / `frame_len` / `payload_eq` (sibling calls) |
| `src/server.hy` | Pure echo policy (`echo_reply`) |
| `src/client.hy` | Pure request body |
| `src/main.hy` | listen on port 0 → server and client tasks in one `task::scope` |

## Notes

1. The listener binds port 0; `local_addr` gives the port to connect to.
2. The demo needs `--allow-net` (sockets are a capability).
3. Test harness is CWD-`./tests` only.
