# Key index natives

`key_index::*` (HostInvoke 191–199, archive 6.19) is an insertion-ordered hash index (the map table layout below, 217–226, is the one the stdlib uses) that a collection keyed by any `Hash + Eq` type builds on. `collections::index::KeyIndex<K>` in coil-stdlib wraps it, and `HashMap` / `HashSet` sit on `KeyIndex`. coil-json drives the natives directly for its member index.

Source: `machine/src/key_index.rs`, signatures and docs in `compiler/src/builtin_docs/key_index.coil`.

## Shape

The index owns no keys. It maps keys to dense slot numbers `0, 1, 2, …` in insertion order, and the caller keeps keys and payloads in its own `Vec`s indexed by slot. Beside them it keeps three plain `Vec<int>`s:

| Vec | Holds |
|-----|-------|
| `table` | `cap` probe cells (a power of two): `0` empty, `-1` a removed entry's place, `slot + 1` otherwise; then one cell counting occupied cells (live and removed) so `insert` knows when to grow without a scan |
| `hashes` | each slot's key hash, as the caller computed it |
| `live` | `1` while the slot holds an entry, `0` once removed |

All three are ordinary `Vec<int>`s: copy-on-write, counts and thread sends treat them like any other array. There is no `Object::KeyIndex`. A host native that writes the heap makes its `Vec` arguments unique first (the copy-on-write call summary marks every argument mutated).

Probing is linear from `home(h)` (the hash's high bits folded into the low ones, so float bit patterns spread). The table grows when it would pass half full. Slots never move on their own: only `compact` renumbers them, after the owner has compacted its own `Vec`s with `retain` in the same order.

## Natives

| Native | Id | Does |
|--------|----|------|
| `table(n)` | 191 | an empty table with room for `n` entries |
| `probe(table, hashes, h, after)` | 192 | the next live slot whose hash is `h`, after slot `after` (`-1` starts), or `-1` |
| `insert(table, hashes, live, h)` | 193 | a new slot for a key the caller checked is absent; grows the table |
| `remove(table, hashes, live, slot)` | 194 | marks `slot` removed; other slots keep their numbers |
| `compact(table, hashes, live)` | 195 | drops removed slots, renumbers in order, rebuilds the table |
| `retain(v, live)` | 196 | drops the elements of any `Vec` whose slot is removed |
| `clear(table, hashes, live)` | 197 | no entries; the table keeps its size |
| `find(table, hashes, keys, h, k, mode)` | 198 | the slot whose key equals `k`, comparing in the runtime, or `-1` |
| `eq_mode(k)` | 199 | how `find` may compare keys of `k`'s type |

All are direct natives (`DIRECT_HOST_NATIVES`): plain functions of their arguments that never park or switch tasks.

## Equality

`probe` returns candidates and the caller's `==` decides, so any `Hash + Eq` key works, a hand-written `Eq` included (it must agree with `Hash`).

For keys whose `==` the runtime runs itself, `find` compares too and a lookup is one call. `eq_mode(k)` names the compare:

| Mode | Key type | `find` compares |
|------|----------|-----------------|
| `0` (`KEY_EQ_COIL`) | anything else, and any type a body is generic in | nothing: the caller uses `probe` and `==` |
| `1` (`KEY_EQ_WORD`) | `int` | words |
| `2` (`KEY_EQ_STRING`) | `string` | contents |

The compiler folds `eq_mode(k)` to its constant in every body it plans (`hir_fold_key_eq_modes` in `codegen/emit_hir.rs`, run before the body's other rewrites): by the argument's type where it is ground, `0` where it still has type variables. The native answers `0` for a call left unfolded. So `KeyIndex::find_hashed` writes

```coil
let mode = ki_eq_mode(k);
if mode > 0 {
    return ki_find(self.table, self.hashes, self.keys, h, k, mode);
}
// probe + `==` loop
```

and each clone keeps only one side. Floats, tuples, enums and classes stay on mode `0`: float `==` is IEEE (`NaN`, `-0.0`), and an aggregate may hold a user `Eq`.

## Users

- `collections::index::KeyIndex<K>` (stdlib), and through it `HashMap` / `HashSet`.
- coil-json `Store`: one table for the whole store, keyed by a member key's hash mixed with its object id, for objects with at least 32 members. The candidate check compares the slot's object and key.

## Map tables

`key_index::map_*` (HostInvoke 217–226, archive 8.16; 227–228, archive 8.19, see "Map objects") is a second layout of the
same index, for collections that look keys up on every operation
(`collections::index::KeyIndex`, and through it `HashMap` / `HashSet`). One
plain `Vec<int>` per index holds everything the natives keep; the caller still
keeps keys and payloads in its own `Vec`s by slot.

Source: `machine/src/map_table.rs`; native code's in-place ops are in
`coil-native` (`lower_map_op`, see [native.md](native.md), "Map table ops").

| Words | Hold |
|-------|------|
| `ix[0]` | `mask`: `cap - 1`, `cap` a power of two, at least 8 |
| `ix[1]` | cells in use: live entries and tombstones |
| `ix[2]` | live entries |
| `ix[3]` | `0`, reserved |
| `ix[4 .. 4 + 2cap]` | cells `{hash, ref}`: `ref` is `slot + 1`, `0` empty, `-1` a tombstone |
| `ix[4 + 2cap ..]` | one word per slot, in insertion order: its cell, or `-1` once removed |

The hash sits beside the slot in the cell, so a probe compares hashes on the
cell's own cache line and reads a key only when the hash matches. The slot
words give liveness and remove-by-slot without a probe. Probing is linear
from the same `home(h)` as above. An insert that would take the cells past
half full, counting tombstones, rebuilds them at the size that keeps the
live entries under half full (never smaller), keeping every slot's number.
An insert reuses the first tombstone its probe passed. Only `map_compact`,
which the owner calls after it compacted its own `Vec`s the same way,
renumbers the slots.

| Native | Id | Does |
|--------|----|------|
| `map_table(n)` | 217 | an empty table with room for `n` entries |
| `map_find(ix, keys, h, k, mode)` | 218 | the slot whose key equals `k` by `mode` (`eq_mode`), or `-1`; mode `0` finds nothing |
| `map_upsert(ix, keys, h, k, mode)` | 219 | `map_find`, or registers slot `s` (the slot count) for an absent `k` and returns `-1 - s`; the caller pushes `k` and its value |
| `map_probe(ix, h, after)` | 220 | the next live slot whose hash is `h` after slot `after`, for keys whose `==` is coil code |
| `map_add(ix, h)` | 221 | registers a new slot for a key the caller checked is absent |
| `map_live(ix, s)` | 222 | whether slot `s` holds a key |
| `map_len(ix)` | 223 | the live count |
| `map_remove(ix, s)` | 224 | tombstones slot `s`; other slots keep their numbers |
| `map_compact(ix)` | 225 | drops removed slots, numbers the rest from `0` in order, rebuilds the cells |
| `map_clear(ix)` | 226 | no entries and no slots; the cells keep their count |

All are direct natives that write only numbers, into the table only
(`host_writes_only_numbers`, `writes_first_arg_only` in `hir/escape.rs`: the
keys `Vec` `map_upsert` is handed is not made unique for it), and keep
nothing they are passed (`NON_RETAINING_HOSTS`). The table is an ordinary
`Vec<int>`: copy-on-write separates it before a native writes it, and the
collector, counts and thread sends treat it like any other array. A
malformed table (a hand-made `Vec<int>`) finds nothing and adds nothing.

Native code runs `map_find`, `map_upsert` (when no growth is due and the
table's storage has room for the slot's word), `map_live` and `map_len` in
place, with no host call. The MIR interpreter, and so wasm and comptime,
calls the natives.

## Map objects

`key_index::map_put(m, h, k, v, mode, vmode, replace)` (HostInvoke 227,
archive 8.19) inserts through the map object itself: `m` is an instance
whose fields 0, 1 and 2 are the table, the keys `Vec` and the values `Vec`
(`collections::map::HashMap` is one). It finds `k` (hash `h`, key mode
`mode` from `eq_mode`). When `k` is there it returns its slot `s`, and
first replaces the value when `replace` is `1`. When `k` is absent it
registers a slot, pushes `k` and `v`, and returns `-1 - s`. `vmode` is what
`key_index::word_mode(v)` gave (228): `1` a number word, `2` one object
word, `0` anything else (a pair, a niche, a wide or packed value, a type
parameter). The compiler folds `word_mode` like `eq_mode`; an unfolded call
answers `0`. Callers use `map_put` only when both modes are above `0`.

`map_put` makes each field it writes unique itself, so the compiler emits
no copy-on-write for the call: the caller's `self` is passed as a place.
It is a listed host in `hir/escape.rs` with per-argument facts: the map
escapes (it is never stack-allocated: the host must find it in the heap)
and is not kept; `k` and `v` are kept. It writes the first argument only
(`writes_first_arg_only`), and counts the slots it writes itself, so the
dispatcher does not wrap it in `rc_before_all` / `rc_after_all`
(`host_writes_words` in `machine/src/native.rs`).

A malformed object (not an instance with three fields, fields that alias,
not arrays of one-word elements, a bad table, or keys and values not one
per slot for an add) gets `-1` and nothing changes. A mode outside `1` and
`2` is malformed too.

Native code runs `map_put` in place (see [native.md](native.md), "Map table
ops"): it reads the three fields, takes the host call when any of them is
shared, probes, and on a miss appends the key and value when no growth is
due and both `Vec`s have room. Growth (the table's rebuild and the two
`Vec`s' reallocation) stays in the runtime: it is amortized, about 38
instructions per new key at a million keys, and would be the same work in
generated code.

## Measured (callgrind, 2026-11-02)

Word count (`HashMap<string, int>`), the map hit bench (`map_keys`) and coil-json large-object reads:

| Bench | Before | After |
|-------|--------|-------|
| word count, `find` vs `probe` + `==` | 3.30G | 3.02G |
| map_keys int / string | 1.48G / 2.02G | 1.42G / 1.92G |
| JSON object of 2000 keys, read every key | 1.91G | 0.20G |

coil-toml already kept its own open-addressing child index in Coil; moving it onto the natives measured −1.5% on a 3000-key table and +1% on small documents, so it stays as it is.
