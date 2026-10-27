# Value representation

How a value of a static type is held in a word, crosses a call, and reads
to the GC. The code is `compiler/src/layout/` (`rep.rs` decides,
`abi.rs` answers call edges); the tests that hold it to this page are the
generated matrix under `tests/representation/`.

## The rule

**`rep(T)` is a function of the static type only.** A value of type `T`
has the same word wherever it lives: ground code, a shared generic body, a
per-type clone, a dictionary or closure adapter, a host call, a frame map.
A bare type variable `V` in a shared body holds `rep(ground T)`: the raw
niche word for `Option<heap>` / `Result<heap, heap>`, the pointer for an
object, the immediate for a scalar. A shared body never interprets a bare
`V`; it moves it, stores it, roots it and passes it to dictionaries, which
are ground instances that already expect `rep(ground T)`.

`layout::rep::of` returns one `Layout` per type, and every other answer is
a view of it: `value_layout` (the one-word encoding codegen, `HostInvoke`
packing and FFI repacking use), `word_kind` / `array_elem_stamp` (what
the GC and array stamps read a word as), and `abi::two_word_kind` (the
two-slot call width).

## Representations

| Type | Word | Notes |
|------|------|-------|
| `int`, `float`, `bool`, `byte`, scalar `#[repr]` enum | immediate | |
| class instance, `string`, `Vec`, record, tuple, payload enum | pointer | |
| `Option<T>`, `T` a ground heap type | niche: `0` = `None`, else the pointer | `Layout::NicheOption` |
| `Result<(), E>`, `E` a ground heap type | niche: `0` = `Ok(())`, else the error pointer | `Layout::NicheUnitResult` |
| `Result<T, E>`, `T` and `E` ground heap types | niche: pointer = `Ok`, `pointer \| 1` = `Err` | `Layout::NicheResult`; never steals int bits |
| `Option<int>` (any immediate inner), `Result<immediate, _>`, `Result<(), immediate>`, closed payload enums with every arity `<= 1`, arity-2 immediate tuples, two-field immediate `readonly class` (#1562) | boxed object | `Layout::Pair`: two slots only under the call convention below |
| `Option<Option<_>>`, `Result<Result<_>>`, mixed heap/immediate, enums with `fn drop` | boxed `ObjEnum` / `ObjTuple` | `Layout::Word` |
| `Option<T>` / `Result<T, E>` with `T` open | boxed `ObjEnum` | see [Open enums](#open-enums) |
| bare `V` in a shared body | `rep(ground T)` | |

The niche and two-slot families are disjoint: a niche needs heap payloads,
a pair an immediate one. Values are not tagged; the static type picks the
decode (`CONST 0` / `LogNot` for Option, `BITAND` / `BITOR` / `XOR` for
Result). No pair or niche opcode is emitted (`ReturnPair`, `PairToHeap`,
`OptionNicheToHeap`, `HostInvokeNiche`, … are tombstones).

**GC.** A niche word is a pointer root: root marking strips bit 0 before
`find_object_by_addr`, and `values_eq` compares the stripped words. A
two-slot pair is two scalars, which no frame map roots.

**VM.** `BoxValue` / `UnboxValue` are representation-agnostic for one-word
values: boxing a niche word keeps the word (`0` and `pointer | 1` are kept
as values), and unboxing a non-boxed word passes it through. That is what
lets a shared body and a dictionary agree on `rep(ground T)` without a
per-context conversion.

## Shared generic bodies

A ground call of a generic function or method either clones the body per
type or calls the shared body through dictionaries. Both see the same
words:

- `hir_bare_niche_abi` (`codegen/emit_hir.rs`) gives a declared bare `V`
  parameter or return the niche layout when its ground argument is a niche
  type, so the call site passes the word unchanged.
- A bare `V` field, `Vec<V>` element or `HashMap` value read from ground
  code is the niche word (`hir_field_rep`'s bare-niche case).
- Dictionary, closure and `PolyFn` adapters box a niche word as itself
  (`Box(tag)` keeps it; `Unbox` passes it back).
- `hir_method_clone` clones niche types too: the per-type clone is a cost
  decision (speed), never a correctness one.

### Open enums

`Option<T>` / `Result<T, E>` whose payload is still a type variable *is*
interpreted by the shared body (it matches on it), so there the value is
the boxed `ObjEnum`. The edge from ground code converts once: `niche_args`
/ `hir_niche_arg` (`codegen/emit_hir_mir.rs`) box a niche word into an
`ObjEnum` on the way in, and `generic_enum_layout` (`codegen/compiler.rs`)
unboxes on the way out. A generic-head instance's `self` receiver (#1609)
is the main user. This is the only place a value changes representation at
a boundary, and it is keyed on the static type of the parameter (open
enum), not on the caller's context.

## The two-slot calling convention

The two-slot form of a `Layout::Pair` type is a calling convention, not a
representation. `abi::convention_ret` / `abi::convention_param` decide it
from `CalleeFacts` (`escaped`, `coroutine`, `host_native`,
`instance_method`, `inherent_method`, `overloaded_or_generic`):

- A *direct* `CALL` / `RETURN` of a ground callee whose address never
  escapes moves `[payload, tag]` (or `[a, b]` for products and pair
  classes, second word on top). `CALL` bit 31 (`Byte::CALL_RET2_BIT`) and
  `RETURN` operand `2` mark the width; both are additive, so old archives
  stay one word.
- "Escapes" is the fail-closed whole-compile `typechecking::fn_value_escape`
  walk: any function value, partial application, `thread::spawn` or FFI
  callback of that name, in any file of the package use-graph, keeps every
  caller on the one-word ABI.
- Coroutines, host natives and trait instance methods (dictionary entries)
  never return two slots. Inherent methods, overloaded and generic
  functions keep one-word parameters (a numeric range's two words
  excepted). Products and pair classes are never two-slot parameters.
- A local bound from a two-slot call keeps its two slots, so `match r`,
  `r?`, `return r`, `let (a, b) = r` and `r[0]` do not allocate. `?`
  compares the tag in place and shares one fail epilogue per function
  (#297, #307).
- Everywhere a value must be one word (a field, a capture, a container
  element, a shared body, a dictionary or closure call, `==`, `Show`) it
  is its box: `box_pair_val` in MIR builds it, and two field loads read it
  back. One pack/unpack pair serves every consumer.

`compute_two_word_return_kind`, `callee_param_pairs` and
`callee_has_unboxed_range_params` (`codegen/compiler.rs`) are the callers;
they build `CalleeFacts` and ask `abi`, so there is no second copy of the
policy.

## Host calls

Host natives pack their result into one word at the `HostInvoke` edge
(`machine::host_enum`, operand bits `[17:16]`), with layout bits derived
from `value_layout` of the invoke's result type: `1` for
`NicheOption` / `NicheUnitResult` (`Ok` = `0`), `2` for `NicheResult`,
`0` (boxed) otherwise and whenever the type is unsure. `Vec::pop` /
`remove` of a heap element and `gc::get` / `unroot` / `upgrade` pack once;
`Result<int, _>` and `Result<Option<int>, _>` stay boxed. Host natives are
never two-slot producers. Cross-module `Result<Path, IoError>` keeps
virtual-error tags for layout (COI-404).

## Debug check

Builds with `debug_assertions` or the `gc-stress` feature panic on an
`UnboxValue` whose boxed tag is not the one the unboxer expects
(`UnboxValue: boxed tag … where tag … is expected`, `machine/src/exec_rest.rs`).
Release builds keep returning the default value. The gc-stress CI run is
what catches a boxed-vs-niche mix-up that slipped past the matrix.

## Representation matrix

`scripts/gen_rep_matrix.py` writes one program per (representation,
position) cell into `tests/representation/`: 12 representations (`int`,
class instance, `string`, `Option<string>`, `Result<string, string>`,
`Option<int>`, `Result<(), E>`, `(int, float)`, pair readonly class,
`#[repr(int)]` enum, `Option<Option<string>>`, payload enum with
`fn drop`) times 20 positions (ground argument and return, unbounded and
bounded generic methods, generic return, bare field, `Vec` element,
`HashMap` value, tuple and record, closure capture and lambda parameter,
`thread::spawn` round trip, host returns, `==` / `show` / `Hash` through a
dictionary, the consumer `match` / `??`, clone vs shared body). Expected
output comes from the ground path. Refused cells go to
`tests/representation/compile_fail/` with their issue; crashing cells are
listed in `scripts/rep_matrix_known_wrong/`. CI's fmt job runs
`gen_rep_matrix.py --check`; `--probe` runs every cell under default,
`-O0` and no-inline.

State when the niche-vs-boxed series landed: 223 of 234 cells green (199
before). The red cells are `#[repr(int)]` enums in containers and across
threads (#1903, #1904) and `Result<(), E>` through a `HashMap` entry
(#1912, a typechecker bug).

## Performance

Steps of the series and the hit benches (release build, 4-core container,
15 interleaved runs, median with min in brackets):

| Bench | Two-slot convention via `abi` (#1914) |
|-------|------------------------------|
| `pair_int_churn` | 1.298 s (1.185 s) |
| `option_int_churn` | 1.019 s (0.939 s) |
| `readonly_class_pair` | 0.225 s (0.200 s) |

#1914 compiles `tests/`, `examples/` and `examples/perf/` to the same
archives as #1913, so these benches are unchanged by it. #1913 (niche `V`
in shared bodies) changed archives only for bodies that stopped boxing a
niche word; `examples/perf/` compiled byte-identical. The readonly pair
class layout's own numbers are in
[limitations.md](limitations.md) ("Readonly classes as pairs").
