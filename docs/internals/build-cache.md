# Build cache

[#1881](https://github.com/ardax-corp/coil-lang/issues/1881). `coil FILE` and
`coil compile FILE` keep the archive of each compile and reuse it when the
program has not changed, so running a program again skips parsing,
typechecking and code generation of it and of the stdlib. The code is
`src/build_cache.rs`; the run from a hit is `coil_host::execute_cached`.

Measured on a program that uses `collections::map::HashMap` (release build,
2026-10-25): a compile and run takes 82 ms, and a hit takes 5 ms.

## Where

`build/` in the user cache root (`$COIL_CACHE_DIR`, else
`$XDG_CACHE_HOME/coil`, else `~/.cache/coil`), next to the comptime result
cache. One file per entry, `<key>.hyc`: a list of inputs with their sha256,
then the archive. Entries are written to a temporary file and renamed, so a
concurrent run sees a whole entry or none. At most 256 entries are kept, and
the least recently used go first.

## Key

A sha256 of:

- the entry format, the compiler build id (`coil_build_id::BUILD_ID`) and the
  archive version;
- the working directory, the entry file's canonical path and its bytes;
- the flags that can change the archive: `--include-tests`, `-O`, `-g`,
  `--contracts`, `--fold-calls`, the host grants and the `--root`s;
- every `COIL_*` environment variable (several switch optimizations).

So two versions of one file, or two builds of coil, never share an entry and
can compile at the same time without touching each other's.

## Inputs

The entry also lists what the compile read, with the sha256 of each when it
was stored; a hit needs every one unchanged:

- every source file the compile discovered (the entry, its modules, the
  stdlib modules it uses);
- the directory of each of them, and each module root, as their sorted
  entry names: a new file there can change which file a `use` finds;
- the entry's proof file (`FILE.hy.proof`), present or not.

A file added to a directory that holds sources therefore misses once. That
includes an output written beside the source, such as a new `-o` file.

## Not cached

- A compile that reported any diagnostic: a hit would not show its warnings
  again.
- A compile with compile-time IO grants (`--comptime-read`, `--comptime-env`,
  `--comptime-net`, `--comptime-write`): its result can
  depend on files and the environment the key does not cover. Those
  evaluations have their own result cache.
- `--opt-stats` / `--opt-stats-json`, which report on the compile itself.
- `--no-build-cache` turns the cache off for one command.
- `coil package`, `coil test` and the other helpers do not use it yet.
