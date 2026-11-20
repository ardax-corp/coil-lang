# Collections: runtime vs userland

Plan for `HashMap`, `HashSet`, `List`, and `TreeMap` in the standard library.

## Already in the runtime / compiler

| Primitive | Role for collections |
|-----------|----------------------|
| `Hash` / `Eq` / `Ord` traits | Key constraints (`hash()`, `==`, `<` / `>`) |
| Dynamic arrays `[T]`, `Vec::push`, `len` | Buckets, chains, growable storage |
| Classes + field mutation | Mutable map / list / set handles |
| Recursive enums | Persistent trees / functional lists |
| Bit ops (`&`, `<<`) and `%` | Bucket index from hash |
| `HostInvoke` | Where a native lands, if one ever does |
| Recursion | Recursive tree / list walks, up to the stack limit |

**Do not add** map/set/list MIR ops or a heap `Object::HashMap`. That would be benchmark-shaped surface area.

The one native piece is the **key index** ([key-index.md](key-index.md), `key_index::*`, HostInvoke 191–199): probing over plain `Vec<int>`s, no new object kind, usable by any collection keyed by a `Hash + Eq` type. Keys and payloads stay in userland `Vec`s.

## Userland (this change)

| Type | Module | Representation |
|------|--------|----------------|
| `KeyIndex<K>` | `collections::index` | Keys by slot in a `Vec<K>` plus the `key_index` table / hashes / live Vecs |
| `HashMap<K,V>` | `collections::map` | `KeyIndex<K>` + `vals: Vec<V>` by slot |
| `HashSet<T>` | `collections::set` | `HashMap<T, bool>` wrapper |
| `List<T>` | `collections::list` | Mutable singly-linked `Node` class (`Option<Node<T>>`) |
| `TreeMap<K,V>` | `collections::tree` | Mutable BST via parallel Vecs + child indices |

Constrained ops (`insert` / `get_or` / …) are **inherent methods** on
`impl HashMap<K: Eq + Hash, V>` (and the Ord analogues for `TreeMap`). **Rule:**
type-tied operations should be methods, not free functions — the compiler
applies `impl` type-param bounds to method schemes and emits dictionary
arguments on inherent method calls (same ABI as free generics). Free generic
functions may return `Option<T>` (#884).
Ground `Option` / `Result` values use niche, two-slot, or boxed `ObjEnum` by
shape (COI-92) — not a single boxed ABI.

**`Option` field match:** `match` copies the field (GC pointer / immediate). Nested `match` on the same `Option` field is valid and does not empty the field — no write-back. See COI-77 and [Enums and Match](https://github.com/ardax-corp/coil-website/blob/main/src/content/docs/manual/tutorial/05-enums-and-match.md).

## Known language gaps (remaining)

| Gap | Impact | Recommended hoist |
|-----|--------|-------------------|
| `[Option<T>]` / `[Foo<K,V>]` | Nested generics in array element types parse | Done — write `[Option<int>]` directly |
| Functional `List` recursion past the stack limit panics with `stack overflow` | Prefer mutable class list for very long lists | — |

## Future (only if measured)

- A native deep compare for tuple / enum keys (`eq_mode` 0 today), if tuple-keyed maps show up in profiles.

## API shape

```coil
use collections::map::{HashMap};
use collections::set::{HashSet};
use collections::list::{List};
use collections::tree::{TreeMap};

let m = HashMap::new();
m.insert(1, "a");
let v = m.get_or(1, "?");

let xs = List::new();
xs.push_front(1);

let t = TreeMap::new();
t.insert(2, 20);
```

Existing `collections::{sort, reverse, collect_ints, …}` stays in
[coil-stdlib](https://github.com/ardax-corp/coil-stdlib) (`src/collections.coil`).
For `VecDeque`, `BitSet`, `windows` / `partition` / `sort_by`, see that
package’s [modules.md](https://github.com/ardax-corp/coil-stdlib/blob/main/docs/modules.md)
— do not duplicate the API list here.
