# Packed byte arrays

[#927](https://github.com/ardax-corp/coil-lang/issues/927): `Vec<byte>` and
`Vec<bool>` are stored one byte per element instead of one 8-byte word.

## Storage

`ObjArray` (`machine/src/memory/heap.rs`) holds `ArrayData::Words(Vec<Value>)`
or `ArrayData::Bytes(Vec<u8>)`. Nothing outside the type sees the raw vector:
callers use `len` / `get` / `iter` / `read_indexed` / `store_indexed` /
`push` / `insert` / `remove` / `pop`, or the byte copies `to_byte_vec` /
`write_bytes_at`. Fast paths that handle both storages read `words()` or
`bytes()`.

Byte storage is lossless: it only ever holds words whose raw value is
`0..=255`, and a read widens the byte back to that same word. A store that
does not fit (a larger int, a boxed value from a shared generic body, a heap
reference) unpacks the array to words first. Correctness therefore never
depends on the static element type: a mis-stamp only costs memory. Packed
arrays never hold references, so marking skips them.

Heap accounting: `GcSized::size` reports the storage size the heap has been
told about (`ObjArray::take_resize` / `Heap::account_array`), not the live
one, so a pack or unpack between accounting points can never underflow the
heap byte count at sweep.

Code that wants a word slice (the packed LA kernels' `elements_of`, `vload`)
must widen a packed array, never treat `words() == None` as "not an array":
#939 caught `~matrix([[34 as byte, …]])` reading packed rows as zeros.

## Construction stamp

`common::WORD_BYTE` (3) is an array element kind next to `WORD_POINTER`.
`TagArrayKind` / `MakeArrayK` with it packs the array (`set_elem_kind` →
`pack`, which keeps words if any element does not fit).

- Array literals: `emit_hir` stamps `MakeArrayK` with
  `value_layout::array_elem_stamp` (byte / bool → `WORD_BYTE`).
- `Vec::new` / `with_capacity` / `from`: `$bytes` thunk twins (next to the
  `$ptr` ones) end in `TagArrayKind WORD_BYTE`; call sites pick them by static
  type, in every tier (the MIR tiers call the same thunk).
- Not stamped yet: array literals built directly in MIR (`MirAllocKind::Array`
  carries no element kind). They stay words.

Runtimes that predate `WORD_BYTE` ignore the stamp, so no archive bump.

## Byte paths

`io::to_bytes`, `read_to_end` and stream reads build or fill packed arrays
with one copy; writes, `from_bytes` and FFI `Bytes` (tag 15) take
`to_byte_vec` (one copy when packed). FFI `Bytes` still copies into a
separate buffer and back: handing C the array's own buffer is unsafe if a
callback grows the array. Packed arrays cross threads and channels as
`PortableValue::Bytes`.

## Measurements

Release build, `COIL_AUTO_PAR=0`, 4-core cloud box (run-to-run spread about
±10%).

| Change | Bench | Before | After |
|---|---|---|---|
| Storage kind + API (#937) | gc_int_vec, binary_trees_nohoist, nsieve, fib (15 interleaved runs) | main | within noise (-3% to -8%) |
| Compiler stamp (#939) | deflate 1 MiB round trip (`perf-checkup/benches/stdlib/b_defl.hy`), max RSS | 55 MB | 26 MB |
| Compiler stamp (#939) | same, compress / inflate / zlib inflate | 6.3 / 1.8 / 1.5 s | 6.1 / 1.5 / 1.3 s |
| Byte paths | same | — | RSS 26 MB, times within noise of the stamp row (compute bound) |
