# 02-adventure — notes

## What it shows

Playable text adventure over **stdin/stdout**: rooms/items (`world.hy`),
byte-line command parse (`commands.hy`), save encode/decode (`save.hy`),
REPL + file/stdin IO in `main.hy`.

## Play (interactive)

The REPL reads **all of stdin** with `read_to_end` then splits on `\n`. On a
TTY type commands and end with **Ctrl+D** (EOF), or pipe a transcript:

```bash
./examples/projects/02-adventure/demo.sh
```

Commands: `look`, `go north` / `south` / `east` / `west`, `take` / `take key`,
`inventory`, `save`, `load`, `help`, `quit`.

## CI / canned transcript (always use `timeout`)

```bash
./examples/projects/02-adventure/demo.sh --ci
# or: ./examples/projects/run-demos.sh
```

Input file: `transcript.txt` (Hall → Library → key → Garden → quit).

Expected gist: Hall → Library (take key) → inventory → Hall → Garden → look →
`Bye.`

## Test

```bash
./examples/projects/run-tests.sh
# or: cd examples/projects/02-adventure && …/coil test
```

## Layout

| File | Role |
|------|------|
| `src/world.hy` | Player, rooms, move, key |
| `src/commands.hy` | `Cmd` + `parse_line` / `bytes_eq` |
| `src/save.hy` | Pure 2-byte encode/decode (`SaveData`) |
| `src/main.hy` | REPL + stdin/`open`/`write_all`/`read_to_end` |

## Ergonomics / gaps noticed

1. Stream IO stays in the entry file for clarity; dependency modules may call
   IO HostInvoke + `?` (see regression `multi_file_io_hostinvoke_try_in_dependency`).
2. No `read_line` builtin — batch `read_to_end` + `\n` split (Ctrl+D / pipe).
3. Commands compared as `[byte]` (not `from_bytes`).
4. Avoid `x < 0` for EOF/sentinels; use `==` / positive sentinels (`99`).
5. Prefer `let s = stdin(); read_to_end(s)` (nested HostInvoke was buggy; fixed).
6. Test harness is CWD-`./tests` only.
