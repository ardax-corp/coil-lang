# Showcase example projects

Three escalating apps under `examples/projects/`. Each has its own `coil.toml`,
`src/`, co-located `tests/`, and `NOTES.md`. **Do not** put these suites under
repo-root `tests/` (that tree is for language/compiler harness cases).

| Project | Focus | Demo |
|---------|--------|------|
| `01-todo` | Core language (classes, arrays, modules) | prints a seeded board summary |
| `02-adventure` | Interactive stdin REPL + modules + save/load | playable text adventure |
| `03-echo` | TCP + coroutines + protocol module | single-process echo → `ok` |

HTTP client/server is **[coil-http](https://github.com/ardax-corp/coil-http)** — install via [spool](https://github.com/ardax-corp/spool), not vendored here.

## Run demos (scripts)

From the repo root (builds a release binary if needed):

```bash
./examples/projects/run-demos.sh     # todo + adventure transcript + echo
./examples/projects/run-tests.sh     # co-located tests for all three
```

Per project:

```bash
./examples/projects/01-todo/demo.sh
./examples/projects/02-adventure/demo.sh          # interactive on a TTY
./examples/projects/02-adventure/demo.sh --ci     # pipe transcript.txt under timeout
./examples/projects/03-echo/demo.sh
```

Adventure CI input lives in `02-adventure/transcript.txt`.

### Playing the adventure

The adventure reads **all of stdin** then splits lines (batch/`read_to_end`).
On a TTY, type commands and send **Ctrl+D** when done (or pipe a transcript).

```bash
./examples/projects/02-adventure/demo.sh
```

Commands: `look`, `go north|south|east|west`, `take` / `take key`,
`inventory`, `save`, `load`, `help`, `quit` / `exit`.

Manual equivalent (no scripts):

```bash
rm -f out.hyc
cargo run --release -- examples/projects/01-todo/src/main.hy
timeout 10s cargo run --release -- --allow-read --allow-write examples/projects/02-adventure/src/main.hy \
  < examples/projects/02-adventure/transcript.txt
timeout 10s cargo run --release -- --allow-net examples/projects/03-echo/src/main.hy
```

## Per-project unit tests

Prefer `./examples/projects/run-tests.sh`. The harness only scans
**`./tests` relative to CWD**, so the script `cd`s into each project:

```bash
cd examples/projects/01-todo
cargo run --release --manifest-path ../../../Cargo.toml -- test
```

That awkward `cd` + `--manifest-path` is intentional documentation of current
ergonomics (no `coil test --project …` yet).

## Module layout (matches the plan)

| Project | Modules |
|---------|---------|
| `01-todo` | `board.hy` + `main.hy` |
| `02-adventure` | `world.hy` + `commands.hy` + `save.hy` + `main.hy` |
| `03-echo` | `protocol.hy` + `server.hy` + `client.hy` + `main.hy` |

## Rolled-up language / tooling gaps

See each project's `NOTES.md` for detail. Highlights:

1. No `read_line` builtin; adventure uses `read_to_end` + `\n` split (Ctrl+D / pipe).
2. No `\n` escapes in string literals (prompts use spaces).
3. Prefer `let s = stdin(); read_to_end(s)` — nested `read_to_end(stdin())` was a
   HostInvoke arg-order bug (fixed; regression: `examples/io_nested_host.hy`).
4. Avoid `x < 0` for EOF/sentinels; use `==` / positive sentinels.
5. Test harness is CWD-`./tests` only; pipe adventure transcripts under `timeout`.
6. Multi-file IO HostInvoke + `?` in a dependency is supported (regression:
   `multi_file_io_hostinvoke_try_in_dependency` in
   `compiler/tests/namespace.rs`); demos may still keep Stream IO in the
   entry for layout clarity.
