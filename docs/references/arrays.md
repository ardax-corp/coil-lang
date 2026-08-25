# Arrays and `Vec`

coil splits fixed-size arrays from growable vectors (Rust-style).

## Fixed arrays — `[T; N]`

Homogeneous, fixed length. `N` is part of the type and is **inferred** from
literals when possible; otherwise it must be written explicitly.

```coil
let a = [1, 2, 3];          // [int; 3]
let b: [int; 3] = [0, 0, 0];
a[1] = 9;                   // element assign only
```

| Rule | Behavior |
|------|----------|
| Literal `[e, …]` | Infers `[T; N]` with `N =` element count |
| Annotation `[T]` | **Error** — use `[T; N]` or `Vec<T>` |
| Empty `[]` | Only under `Vec<T>` or `[T; 0]` |
| Growth | **Forbidden** — no `arr[] =`; use `Vec` |

Locals of type `[T; N]` (`N ≥ 1`) are laid out as **N consecutive frame slots**.
Each slot holds one `Value`:

| Element `T` | Slot contents |
|-------------|---------------|
| `int` / `float` / `bool` / `byte` | Immediate bits (fully on the stack) |
| `string`, class, enum, `Vec`, … | Heap pointer |
| Nested `[U; M]` | Heap pointer to an `ObjArray` row (nested data is **not** flattened) |

Escaping into a single-value context (call, return, store into a heap object)
boxes the spine into a non-growable heap array. Copying or assigning one stack
array local into another copies slots without boxing (forward per-element
`STORE`, so values never pile into the destination slot range).

`[T; 0]` stays a single empty heap array. Params/returns of `[T; N]` still pass
one heap pointer at the call boundary today.

`len(a)` folds to `N` when the length is static. Element-wise zip / LA helpers
still require fixed lengths.

## `Vec<T>` — growable heap vector

```coil
let v: Vec<int> = Vec::new();
v.push(1);
v.push(2);
let x = v[0];
v[0] = 7;
match v.pop() {
    Option::Some(n) => { /* … */ },
    Option::None => { /* … */ },
};
```

### Methods

| Method | Notes |
|--------|--------|
| `Vec::new()` | Empty vector |
| `Vec::with_capacity(n)` | Empty with reserved capacity |
| `Vec::from(arr)` | Copy a fixed `[T; N]` into a `Vec` |
| `v.push(x)` | Append |
| `v.pop()` | `Option<T>` |
| `v.insert(i, x)` | Insert at index (clamped to `len`) |
| `v.remove(i)` | `Option<T>` |
| `v.clear()` | Drop all elements |
| `v.reserve(n)` | Ensure capacity for `len + n` |
| `v.capacity()` / `v.len()` | Ints |
| `v[i]` / `v[i] = x` | Index get/set (see [Out-of-range index](#out-of-range-index)) |
| `(0..n).to_vec()` | Collect a numeric `Range` / `RangeInclusive` into `Vec<T>` |

Rest parameters `T... xs` pack into `Vec<T>`. Spread accepts both `[T; N]` and
`Vec<T>`.

Numeric ranges (`int` / `byte` / `float`) expose inherent `.to_vec()` — same
step and empty-decreasing rule as `for`. See [Ranges](syntax.md#ranges-lazy).

IO buffers (`to_bytes`, `read`/`write`) use `Vec<byte>`.

## Out-of-range index

Runtime `a[i]` / `v[i]` does not panic. A negative or too-large index, or a
non-array target, yields the integer `-1`. A write `v[i] = x` with a bad index
is a no-op; the expression still produces `x`. Literal OOB on `[T; N]` and
tuples is a compile error.

`Index` / `StoreIndex` keep that in-VM check for unproven sites. Proven counted-loop
reads/writes rewrite to unchecked opcodes (archive minor 12). Prefer
`i < len(a)` in loops.

---

## Related

- [Types](types.md)
- [Syntax](syntax.md)
- Example: `examples/vec.hy`
