# Collections: VM hoist vs userland

Plan for `HashMap`, `HashSet`, `List`, and `TreeMap` in the standard library.

## Already on the VM / compiler (no new opcodes)

| Primitive | Role for collections |
|-----------|----------------------|
| `Hash` / `Eq` / `Ord` traits | Key constraints (`hash()`, `==`, `<` / `>`) |
| Dynamic arrays `[T]`, `Vec::push`, `len` | Buckets, chains, growable storage |
| Classes + field mutation | Mutable map / list / set handles |
| Recursive enums | Persistent trees / functional lists |
| Bit ops (`&`, `<<`) and `%` | Bucket index from hash |
| `HostInvoke` (not opcodes) | Prefer this over new opcodes if a native ever lands |
| `#[max_depth(N)]` | Bound recursive tree / list walks |

**Do not add** map/set/list opcodes or a heap `Object::HashMap`. That would be benchmark-shaped surface area; AGENTS prefers alloc reduction and `HostInvoke` over new opcodes unless the pattern is universal.

## Userland (this change)

| Type | Module | Representation |
|------|--------|----------------|
| `HashMap<K,V>` | `collections::map` | Separate chaining: `heads: Vec<int>` + parallel `keys` / `vals` / `next` / `live` Vecs |
| `HashSet<T>` | `collections::set` | `HashMap<T, bool>` wrapper |
| `List<T>` | `collections::list` | Mutable singly-linked `Node` class (`Option<Node<T>>`) |
| `TreeMap<K,V>` | `collections::tree` | Mutable BST via parallel Vecs + child indices |

Constrained ops (`insert` / `get_or` / …) are **inherent methods** on
`impl HashMap<K: Eq + Hash, V>` (and the Ord analogues for `TreeMap`). **Rule:**
type-tied operations should be methods, not free functions — the compiler
applies `impl` type-param bounds to method schemes and emits dictionary
arguments on inherent method `CALL` (same ABI as free generics). Free generic
functions that return `Option<T>` use the boxed enum ABI (archive major 4).

**`Option` field match:** `match` copies the field (GC pointer / immediate). Nested `match` on the same `Option` field is valid and does not empty the field — no write-back. See [COI-77](https://linear.app/ardax/issue/COI-77) and [Enums and Match](https://github.com/ardax-corp/coil-website/blob/main/src/content/docs/manual/tutorial/03-enums-and-match.md#match-does-not-consume-the-scrutinee).

## Known language gaps (remaining)

| Gap | Impact | Recommended hoist |
|-----|--------|-------------------|
| `[Option<T>]` / `[Foo<K,V>]` | Nested generics in array element types parse | Done — write `[Option<int>]` directly |
| Free `fn f<T>(T) -> Option<T>` | Boxed `Option` ABI (archive major 4); same as inherent methods | — |
| Functional `List` recursion can panic on stack | Prefer mutable class list for now | VM stack / `max_depth` interaction audit |

## Future (only if measured)

- `HostInvoke` batch helpers (e.g. rehash) if userland grow shows up in profiles.
- Native open-addressing table as an opaque heap object — only with a cross-cutting need (serde, runtime internals), not for microbenchmarks.

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
[coil-stdlib](https://github.com/ardax-corp/coil-stdlib) (`src/collections.hy`).
