# 02-adventure — notes

## What it shows

Playable text adventure over **stdin/stdout**: `Room` / `Dir` enums and the
player (`world.hy`), parsing a line into a `Command` enum with
`text::split` (`commands.hy`), save encode/decode (`save.hy`), and the
`read_line` loop + file IO with `println(…)?` in `main.hy`.

## Play (interactive)

The REPL reads stdin a line at a time with `read_line` until EOF. On a TTY
type commands and end with **Ctrl+D**, or pipe a transcript:

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
| `src/world.hy` | `Room`, `Dir`, `Player`, moves and the key |
| `src/commands.hy` | `Command` enum + `parse_command` |
| `src/save.hy` | Two-byte save encode/decode (`SaveData`) |
| `src/main.hy` | REPL: `read_line`, `println`, save file `open` / `write_all` / `read_to_end` |

## Ergonomics / gaps noticed

1. `match` takes no string patterns, so `parse_command` compares words with
   `==` in an `if` chain.
2. `text::trim` / `to_lower` return `Result<string, string>` while `main`'s
   `?` is for `IoError`; `??` with the input as the fallback keeps them out of
   `?` (coil-lang#1847).
3. `return` is not an expression, so a `match` arm cannot return early;
   `handle` keeps a `playing` flag instead.
4. The test harness scans `./tests` relative to CWD only.
