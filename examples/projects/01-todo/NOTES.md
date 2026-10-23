# 01-todo — notes

## What it shows

In-memory todo board: a class with an enum field (`Status`), `match`,
`Vec` with `for`, `Result` / `raise` / `?`, `format` + `println`, and a
sibling module imported via `use board::{…}`.

## Run

```bash
./examples/projects/01-todo/demo.sh
# board: 3 tasks, 1 done
# 1: write tests [Doing]
# 2: ship demo [Todo]
# 3: nap [Done]
```

## Test

```bash
./examples/projects/run-tests.sh
# or: cd examples/projects/01-todo && …/coil test --root src --root <coil-stdlib>/src
```

## Ergonomics / gaps noticed

1. `main` returns `Result<(), string>` for the board's errors, so `println`'s
   `IoError` cannot go through the same `?` (coil-lang#1847); the prints use
   `let _ = println(…)`.
2. `coil test` only scans `./tests` relative to CWD: `cd` into the project.
