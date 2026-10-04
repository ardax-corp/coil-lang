# String table, `string::format`, and retiring `print`

## Goals

1. Archive a dedicated **string table**; `STRING` is an index (no inline `DATA` runs).
2. Virtual **`string`** module: `format`, `from_bytes`, `to_bytes` (text helpers also remain as `io` aliases).
3. Remove **`print`** / **`format`** keywords; write via `io::write` / `write_all` on `stdout()`.
4. Keep `FORMAT` opcode for compile-time-checked formatting (`%v` / `Show`).

## Archive (version **35**)

`ArchivedProgram` gains `strings: Vec<String>`.  
`STRING` operand = index into that table. `DATA` stays as a tombstone discriminant (never emitted).

## Runtime

- Machine holds `program_strings` next to `program_constants`.
- `STRING` → `heap.intern(program_strings[idx])`.
- Runtime-built strings (`DynAdd` concat, `FORMAT`, `STRINGIFY`, `from_bytes`,
  thread-decoded strings) are **not** interned: `Heap::alloc_string` allocates
  them with a lazily computed, cached hash (`ObjString::hash_code`). String
  `==` compares content, and dict / field keys are interned on use
  (`intern_key` → `Heap::intern_ref`), so pointer identity only matters for
  intern-table keys.
- Concat appends in place (`memory/strbuf.rs`). Concat results are
  `StrData::Shared`: a prefix of a refcounted buffer with spare capacity.
  When the left operand ends at the buffer's claimed tail, the right operand
  is copied into the tail (claimed with a CAS, so steal-epoch workers stay
  disjoint) and the result shares the buffer. Otherwise both are copied into
  a new buffer with doubled capacity. `s = s + x` loops, `text::join` and
  `fmt::Buf` are amortized linear. Typed `+` lowers to `FORMAT "%s%s"`, so
  `FORMAT` with a leading `%s` takes the same path.
- Stdout/stderr `write`/`write_all` honor `Machine::with_output` via a thread-local redirect (tests keep capturing).

## Language surface

```coil
use io::{stdout};
use io::sync::{write_all};
use string::{format, to_bytes};

write_all(stdout(), to_bytes(format("%i", n)));
let s = format("%s-%i", name, n);
```

- `string::format` is a compiler intrinsic (same rules as old `format` / `print` specs).
- `io::{from_bytes,to_bytes}` remain aliases of the string natives for one cycle.
- Byte-offset helpers read the string in place (HostInvoke **139–143**,
  archive minor 31; `machine/src/str_bytes.rs`). Offsets are bytes, as
  `len(s)` is.

  | Function | Result |
  |---|---|
  | `byte_at(s, i) -> int` | Byte at `i`, or `-1` out of range |
  | `s[i] -> byte` | Byte at `i`; panics out of range (lowers to `byte_at`) |
  | `slice_bytes(s, start, end) -> Result<string, IoError>` | Offsets clamp to `[0, len(s)]`; `Err` inside a UTF-8 sequence |
  | `find_from(hay, needle, start) -> int` | First offset `>= start`, or `-1` |
  | `rfind(hay, needle) -> int` | Last offset, or `-1` |
  | `match_at(s, needle, at) -> bool` | `needle` occurs at `at` |

## Opcode policy

No middle inserts. Redefine `STRING` under the version bump; leave `DATA` / `PRINT` discriminants unused by the compiler.
