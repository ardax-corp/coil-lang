# Key index natives

`key_index::*` (HostInvoke 191–199, archive 6.19) is an insertion-ordered hash index that a collection keyed by any `Hash + Eq` type builds on. `collections::index::KeyIndex<K>` in coil-stdlib wraps it, and `HashMap` / `HashSet` sit on `KeyIndex`. coil-json drives the natives directly for its member index.

Source: `machine/src/key_index.rs`, signatures and docs in `compiler/src/builtin_docs/key_index.coil`.

## Shape

The index owns no keys. It maps keys to dense slot numbers `0, 1, 2, …` in insertion order, and the caller keeps keys and payloads in its own `Vec`s indexed by slot. Beside them it keeps three plain `Vec<int>`s:

| Vec | Holds |
|-----|-------|
| `table` | `cap` probe cells (a power of two): `0` empty, `-1` a removed entry's place, `slot + 1` otherwise; then one cell counting occupied cells (live and removed) so `insert` knows when to grow without a scan |
| `hashes` | each slot's key hash, as the caller computed it |
| `live` | `1` while the slot holds an entry, `0` once removed |

All three are ordinary `Vec<int>`s: copy-on-write, GC, evacuation and thread sends treat them like any other array. There is no `Object::KeyIndex`. A host native that writes the heap makes its `Vec` arguments unique first (the copy-on-write call summary marks every argument mutated).

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

## Measured (callgrind, 2026-11-02)

Word count (`HashMap<string, int>`), the map hit bench (`map_keys`) and coil-json large-object reads:

| Bench | Before | After |
|-------|--------|-------|
| word count, `find` vs `probe` + `==` | 3.30G | 3.02G |
| map_keys int / string | 1.48G / 2.02G | 1.42G / 1.92G |
| JSON object of 2000 keys, read every key | 1.91G | 0.20G |

coil-toml already kept its own open-addressing child index in Coil; moving it onto the natives measured −1.5% on a 3000-key table and +1% on small documents, so it stays as it is.
