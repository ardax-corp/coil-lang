# Known limitations and workarounds

Tracked in Linear: Known limitations.

Actionable gaps in the compiler, VM, and language surface. For opcode/archive rules see [AGENTS.md](../../AGENTS.md); for typechecker error codes see [error-codes.md](https://github.com/ardax-corp/coil-website/blob/main/src/content/docs/references/error-codes.md).

**Design rule:** prefer **method-based APIs** (`impl` methods on classes) over free functions for type-tied operations — stdlib, new language features, and codegen fixes should default to methods. Virtual-module host primitives (`io::read`) remain free functions. Free generic `fn f<T>(T) -> Option<T>` is allowed (shared bodies keep `T` raw since #825; E0127 for it was dropped in #884). A generic `fn` with a type parameter nested in its signature (`Vec<T>`, `Option<T>`, tuples, classes) works as a value when the value is bound by `let` and only called (`let g = wrap; g(2.5)`: each call is a direct call that reaches the mono clone for its argument types, #916) or passed to a parameter of a function type. Copying such a value on (`let k = g;`) is still `E0127`; wrap it in a lambda at a concrete type. A curried generic (`fn konst<T>(T a) -> T -> T`) held as a polymorphic value may be called with every argument at once (`let f = konst; f(1, 2)`) or one at a time (`f(1)(2)`): a `PolyFn` records its declared parameter count, and the closure a call returns is adapted to the call's ground type (#1589). A returned closure of more than one parameter is not adapted yet (`E0801`), and a non-curried generic's value records no count, so over-applying it (`let f = id; f(add2, 1, 2)`) is `E0127`.

**Priority key:** blocking = correctness crash or major feature blocked; high = reliability or significant DX gap; medium = partial support; low = deferred polish.

## Parser / surface (low–medium)

| Issue | Detail | Linear |
|-------|--------|--------|
| Project files | coil reads no project manifest or lock; everything is a flag (`--root`, `--allow-*`, `--dload-pin` / `--dload-trusted`, `--ffi-native`). The virtual prelude is compiler-owned; undefined names already fail typecheck (`E0100`). — [project-config.md](https://github.com/ardax-corp/coil-website/blob/main/src/content/docs/references/project-config.md) | COI-72 |
| `import` keyword | **Non-goal:** not a synonym of `use` and not a distinct construct. Module binding is `use` only. | COI-73 |
| `case` as `match` alias | **Non-goal:** not a synonym of `match`. Pattern matching is `match` only. | COI-74 |
| Range `collect` | **Implemented:** numeric `Range` / `RangeInclusive` (`int` / `byte` / `float`) expose inherent `.to_vec() -> Vec<T>`. Decreasing ranges collect empty (same as `for`). Non-numeric `Ord` may construct a range; `for` and `.to_vec()` are type errors (no `succ` protocol). Step syntax still deferred. | COI-75 |
| Let-polymorphism / Algorithm W | **Decided (explicit generics only):** `Env::generalize` is test-only; production `let` is `Scheme::mono`. Polymorphic values are `fn f<T>` / `class C<T>` (and trait bounds), not inferred at `let`. Headers no longer claim Algorithm W. | — |
| Member visibility | **Implemented:** checker rejects private field/method access outside the owner's `impl` (`E0128`). `pub` members are visible everywhere. Top-level `fn` stays universally exportable (no module-level `pub` in this cut). `fn drop` stays private for inlining but remains callable as a lifecycle hook. | — |
| Duplicate record fields | **Implemented:** parser rejects duplicate names in record literals, constructors, patterns, and enum variant field decls (`E0208`). Typechecker keeps the same check if parse is bypassed. | COI-76 |
| Module-qualified paths | **Implemented:** a compiled module's items resolve by path without a `use` of the item: types (`m::T`, `a::b::T<int>`, aliases; in params, returns, `let`, fields, generic args, `impl Trait for m::T`), calls (`m::f(…)`, `m::C::f(…)`), `new m::C(…)`, enum constructors and patterns (`m::E::V(…)`), and the trait of an `impl m::Trait for T` head in macro-generated code (hygiene); source written by hand gets an error pointing to `use m::Trait;` + `impl Trait for T`. Trait / type-param owners keep associated-type projection. The module must be in the compile (some `use` reaches it). Not yet: a function **value** by path (`let f = m::f`), virtual-module types by path (`io::IoError`). | — |
| Method rest parameters | **Implemented (#1729, #1734, #1754):** a method's `T... xs` or bare `... xs` packs its trailing arguments (a `Vec` or a tuple) as a free function's does; named fixed arguments, a tuple or literal spread, and a lone `...vec` into a `T... xs` work too. An inherent method's bare `... xs` (instance or static, generic impls too) is a per-call template like a free function's (#1281): each call shape gets its own copy. Not yet: another module cannot call a method's bare `... xs` (an error says so; free functions' copies are made across modules, #1386). | — |
| Binding patterns | **Implemented:** `for` reuses `LetPattern` (`for (k, v) in d`, `for _`, records). `if let P = e` / `while let P = e` reuse match `Pattern` (including `else` / `else if let`). C-style `for (;;)` stays a parse error. | COI-371 |

## Macros (medium)

See [macros.md](macros.md).

| Issue | Detail | Linear |
|-------|--------|--------|
| Types are as written | `TypeDecl` / `TypeRef` describe annotations, not resolved types; a derive cannot ask whether a field type implements a trait. The typechecker reports it on the generated code (at the `#[derive]`). | — |
| `macro` model shape | The model uses classes and strings only (no traits, aliases or payload enums) because those did not resolve across modules; `${…}` splices `.src()`, so strings need `lit(...)` / `raw(...)`. | — |
| Expansion cache is per process | Compiled expansion programs (one per provider set) and macro outputs are cached in memory; a fresh `coil` process compiles each provider set once more (~1–2 s in a debug build for the built-in derives). | — |
| Attribute-macro parameters | Parameters after the `FnDecl` / `TypeDecl` are `string`, `int` or `bool` (the input is decoded in the VM); other types are an error at the use site. | — |
| Function-style macro arguments | `name!(…)` arguments are expressions as written (source text + coarse kind); a macro cannot take a type, a block or tokens. The name is bare: `m::name!(…)` does not parse. | — |
| Provider statics | The expansion program has no `main`, so provider module statics are not initialised when macros run. | — |

## Lambdas / captures

Lambdas and `defer` isolate the env (`take_and_isolate`). File-level imports and **module-visible named `fn`s** rebind like globals — a lambda body may **CALL** them with empty captures (direct `CALL`, not a closure slot). Still require `use (…)` for outer **locals** / non-fn values and for other **function values** (anonymous lambdas or `let`-bound fn values). Nested named `fn`s inside another function are not module-visible and follow the local rule.

`thread::spawn` copies a closure's `use` captures to the new thread like its argument (sendable values only); partial applications (filled holes) are rejected. `thread::with_lock` and the readers-writer callbacks still reject capturing `ObjFn` at the host. Capture-free callbacks that only CALL named `fn`s are allowed.

| Issue | Detail | Linear |
|-------|--------|--------|
| Named `fn` CALL from lambda | **Implemented:** in-scope top-level named `fn`s are not captures. | COI-370 |

## Userland footguns

| Issue | Detail | Linear |
|-------|--------|--------|
| `Option` field moves | **Decided (copy-on-match):** `match` copies the scrutinee / field pointer; nested `match` on the same `Option` field is valid. Outer pattern bindings stay in scope (codegen merges `match_bindings`). Was a nested-match lookup hole, not a true move. | COI-77 |
| User trait calls | **Decided (dictionaries for generic bodies only):** ground Show/Length/Hash/user-trait methods with a static entry emit `CALL`. Operators on ground numeric/eq/ord types still lower to opcodes. Shared generic bodies keep dictionary passing. Spec: [types.md](https://github.com/ardax-corp/coil-website/blob/main/src/content/docs/references/types.md#call-site-dispatch). | COI-78 |
| `readonly` and calls | **Implemented (#1558 P4):** `hir/effects.rs` infers which parameters (`self` = 0) each body mutates, through writes, `Vec` mutators, callees, lambdas that capture them and values read from them; a call that passes a `readonly` value where the callee mutates it is `E0107`. What a method called on a `readonly` value returns is `readonly` too (`readonly R`, deep), since it may be `self` or reached from it (a getter, `Ref::get`, `HashMap::get`); a fresh result (`moved()`) is sealed as well, unless the method's trait declares `readonly A self`, which the impl is checked against (#1640). Trait methods declare it (`fn area(readonly A self)`); an undeclared trait method mutates every parameter. A trait operator on a user class (`a + b` through its `impl Add`) is checked as the call of its impl (#1645); on a type-variable operand it is not, since the built-in operator traits cannot declare `readonly`. A named argument is checked against the parameter the checker matched it to, and a spread of a tuple or fixed array item by item (#1646); arguments packed into a rest parameter (`T... xs`) are checked against it, a `Vec` spread among them included (#1681); a spread whose length is not known anywhere else counts as passing its items, and every later argument, to every parameter from its position on. A caller of a method that mutates only an object the caller made stays `pure` (design §3.7, #1648): `Summary::param_only` says which of a body's `mutate` comes only from writes of inert values through its parameter paths, and a call that mutates only owned objects (made of inert values, never stored, captured, aliased by a call result or returned but as `readonly`) discharges it. A write through a `let` alias of a parameter (`let it = self.items; it.push(1)`) or of a parameter value (`self.inner = i`) still keeps every caller impure. | — |
| `readonly class` | **Implemented (#1558 P5):** instances never change after `new` (`E0107` on any field write, `self` included), fields read `readonly`, `readonly C` is `C`, no `fn drop` (`E0126`). `==` is structural through an implied `Eq` (a `Vec<C>` field compares elements by identity, #1629). `Hash` is implied only when every field type hashes *as written* (primitives, tuples, type parameters, this module's hashing types): a `Vec`, array, map or another module's type leaves it out, so such a class needs its own `impl Hash`. `weak()` on an instance is refused (`E0119`, #1644 / #1670). An escaping local is built at each escaping use instead of up front (`hir::readonly_sroa`, #1562 step 1, below); one of two immediate fields returns in two slots like an `(int, float)` tuple (#1562 step 2, below). | — |
| GC drop storing `self` | **Decided (allow-once):** storing `self` from `fn drop()` can keep the cell alive after the sweep (post-drop re-mark). Drop still runs at most once (`finalized`, including explicit `obj.drop()`). Defined footgun, not a pin API — use `gc::root` / `Weak`. Spec: [gc.md](https://github.com/ardax-corp/coil-website/blob/main/src/content/docs/references/gc.md). | COI-79 |

## IL optimizations (low)

Tracked in Linear project Known limitations (milestone **IL / codegen model**). Long-form notes below stay the in-repo model.

| Topic | Linear |
|-------|--------|
| Symbolic-IL `cursor_model` gate (removed with `il::tell`, #1874) | COI-80 |
| Cursor split (`sp` vs `tell`) — **decided: keep** (operand height ≠ STORE floor) | COI-81 |
| Copy-prop / GVN beyond straight-line — **removed 2026-10** (`copy_prop`, `cfg_gvn`, `ssa_gvn` deleted: measurement showed no bench effect; MIR keeps CSE and GVN) | COI-82 |
| IL `MakeArray` frame scalarization (`escape_analysis` pass) — **removed 2026-10** (no bench effect); codegen `[T; N]` locals still scalarize, and the `escape_analysis` option now only gates HIR enum / tuple scalar replacement | COI-84 |
| Loop full unroll — **decided: counted trip ≤ 8**, on the HIR since 2026-10 (`hir::unroll`; break/call/closure refuse, inner loops that unroll first let the outer one follow; `LEQ` for **trip count** only — not an Index in-bounds proof; see bounds `LEQ` refusal, COI-85 / COI-98) | COI-98 |
| Optimization levels (`OptLevel`) — **implemented** (`-O0`…`-O3`, `-Os`, `-Og`; `Standard` default) | — |
| Iterative optimization — **removed** (re-running the pipeline is unsound: LICM is not idempotent) | COI-130 |
| Optimization statistics — **default off** (`collect_stats` / `--opt-stats`) | COI-131 |
| Slot promotion across loop back-edges — **decided: keep Unknown headers** (COI-97 measured: innermost mandelbrot has no self-stores; outer Seek splits FloatChain; `seek_back_edge` removed — its residual `Seek` blocked MIR dense) | COI-83 |
| Named-local class scalar replacement — **Q2**: unique named `new C` field-SROA; identity boxes **once** and reuses (`named_class_box_once_identity`). Field-only stays unboxed. **Q2 supersedes the COI-84 / #134 heap-from-`new` refuse** ([language-quirks.md](language-quirks.md)) | COI-320 / COI-84 / COI-324 |
| Bounds analysis vs `IndexUnchecked` — **superseded by [#192](https://github.com/ardax-corp/coil-lang/pull/192)**: original "Index stays checked" decision is not current. Unchecked exists for proven counted / stride loops (`LE` / `GT`); `LEQ`/`GEQ` are still not proofs. Helper-call coverage is COI-99 | COI-85 |
| Array pin / `IndexPin*` — **implemented** for proven loops (archive minor 13). Layout: [array-pin.md](array-pin.md). **Yield is a length-proof barrier** (`YieldCoro` / `YieldFromCoro`); pins are not saved across yield or on `ObjCoroutine`. Pins are the product; there is no second ArrayPtr. | COI-198 |
| Pure helper calls in counted loops — **implemented**: `analyze_pure_fns` lets length hoists and Unchecked / pin rewrites survive pure `CALL` sites | COI-99 |
| Impure calls in counted loops — **implemented for length-stable callees**: `purity::length_stability` (no `RESIZE` in the call-graph closure) replaces purity in `index_facts` and the `mir::licm` / `hir::bounds` length proofs. Resizing callees, method calls, `CallIndirect` still refuse; a resizing `fn drop()` anywhere in the compile falls back to pure-only | — |
| Caller-side predicate peel vs self-recursion — **decided: keep refusals** (self-recursive peel loses to the frame) | COI-86 |
| `*Jmpt` / fused invert — **implemented** (`*Jmpt` twins); the IL invert of a fused `*Jmpf; JMP` was removed 2026-10 | COI-87 |
| `multi_op_join_convoy` JMPF mis-sink — **moot: pass removed 2026-10** (no bench effect) | COI-91 / COI-400 |
| Loop `LEQ` headers / float identity refusals — **decided: keep numeric contract** (`i < bound` only; no `x - 0.0` / `x * 0.0`) | COI-93 |
| Enum escape elimination vs heap — **implemented for match-only locals** (`hir::enum_sroa`: tag local + shared field locals, not a second ABI). Whole-value uses (calls, returns, `==`, bindings), record variants and `fn drop()` enums keep the heap enum. Niches, two-slot CALL/RETURN, and scalar `#[repr]` are extra ABIs elsewhere (COI-92 / [#293](https://github.com/ardax-corp/coil-lang/pull/293) / [#297](https://github.com/ardax-corp/coil-lang/pull/297)) | COI-94 |
| GC drop storing `self` / resurrection — **decided: allow-once** (see Userland footguns) | COI-79 |
| Option/Result ABI — **one representation per static type**: pointer niches for heap `Option` / heap-heap `Result`, a two-slot calling convention for immediate pairs, boxed otherwise. Table and rules: [layout.md](layout.md) | COI-92 / COI-404 |
| Enum / generic `fn drop()` — **implemented**: inherent `impl E { fn drop() }` on non-scalar enums (payload variants boxed and tagged with `TagEnumType`, finalized once; unit variants are shared immortals and never drop); generic class drop already worked. `#[derive(Drop)]` is not a derive (drop is a hook, not a structural trait) | COI-26 |

Stack-IL GVN (`cfg_gvn`, `ssa_gvn`), the return / bin-join / multi-op convoys and `invert_guard_branch` were removed 2026-10: measurement showed no bench effect. MIR keeps CSE and GVN/PRE.

**Operand order.** `hir::fold` writes an `int` literal on the right of a commutative op or a flipped compare (`LE`↔`GT`, `LEQ`↔`GEQ`); fuse-select also packs const-left commute `CONST; LOAD; int-bin` into `BinSlotImm` (COI-384). The stack-IL `canon` pass (const-to-RHS, load/load slot order, `ConstPool` demote) was removed 2026-10: slot order had no measured effect and codegen already emits inline `CONST` for `0..=i32::MAX`. No float reassociation. **`LEQ`/`GEQ` loop headers are not treated as length proofs** (`i <= len` would allow an OOB index; COI-85 / COI-98).

**Tuples** (`hir::tuple_sroa`, after HIR inlining): a local tuple whose
every value is a tuple built in place (`let t = (a, b)`, `t = (c, d)`) and
whose only reads are constant indices (`t[0]`) or a destructuring
`let (x, y) = t` becomes one local per element, so `let t = (i, i + 1); …
t[1] - t[0]` in a loop allocates nothing. Any other use keeps the heap
tuple. (The stack-IL `escape_analysis` pass that did this before was removed
2026-10.)

**Local enums** (`hir::enum_sroa`, after HIR inlining): a local whose every
value is a variant built in place (`let s = E::A(x)`, `s = E::B(y)`) and
whose only reads are `match s` on variant patterns with plain bindings
becomes a tag local plus field locals, shared by variants by position and
type. A build sets the fields then the tag; the match switches on the tag
and each arm binds from the field locals. Arms of tags no build makes drop
out, and a local built once and never reassigned resolves its matches at
compile time. Fields are scalars or `string`; whole-value uses (calls,
returns, `==`, `??`), record variants and `fn drop()` enums keep the heap
enum.

**Readonly class locals** (`hir::readonly_sroa`, after HIR inlining,
#1562): a `let p = new P(..)` of a non-generic `readonly class` with 1 to
32 closed fields, reassigned only to `new P(..)`, whose whole value is
used (passed, returned, pushed, compared) becomes one local per field.
`p.x` reads its local and each whole use is a fresh `new P(..)` of the
locals, built where that use runs. Two instances with the same fields
cannot be told apart (structural `==`, no `drop`, no `weak()`), so a hot
path that only reads fields allocates nothing and a cold escape pays for
its own object. The split is refused when a whole use sits in a loop the
`let` is outside of, or when two whole uses can run on one path (they must
be in different arms of an `if` or `match`), since either would build more
objects than the one built up front; at most 8 whole uses. Plain classes
keep the one-object rule (`class_boxes`): a field write or identity `==`
must see the same object. `examples/perf/readonly_class_escape.hy` (2M
iterations, every 2000th point pushed to a `Vec`; release build, 15
interleaved runs, 4-core container): **0.251 / 0.276 s** min / median
before, **0.081 / 0.086 s** after. The non-escaping loop of #1554
(`step` reads `p.x + p.y`) and `licm_readonly_class_field`,
`s2j_class_sroa`, `mandelbrot`, `nbody`, `binary_trees` compile to
identical archives.

**Readonly classes as pairs** (`layout::readonly_pair_class`,
#1562): a non-generic `readonly class` with no `drop` and exactly two
`int` / `float` / `bool` / `byte` fields has the layout
`Layout::Pair(PairKind::Class)`, the arity-2 immediate product convention
(#302): a direct `CALL` / `RETURN` moves it as two words, a local bound
from such a call holds them in two slots, and `p.x` reads a word with no
object. Wherever it is one word (a parameter, which stays boxed as for
tuples; a field, a `Vec` element, an `Option` payload, a generic `T`, a
trait object, a method receiver, `==` and `Show`) it is an instance of
the class, built from the two words at that point (`box_pair_val`), and
read back with two field loads (`unbox_word`). Function values,
`CallIndirect`, coroutines and host calls keep the boxed one-word ABI as
for every pair. A field read of a one-word instance stays a field load, so
#1560's invariant readonly field loads still move out of loops. The
instance's `ClassWordKinds` describe its boxed form; the pair is two
scalars, which no frame map roots. `examples/perf/readonly_class_pair.hy`
(300 000 calls of a self-recursive `turn(Vec2, 8)` returning a
`readonly class Vec2 { x: float, y: float }`; release build, 15
interleaved runs, 4-core container): **0.356 / 0.373 s** min / median
before (main and step 1 alike), **0.176 / 0.186 s** after; a second round
on a busier host gave 0.516 / 0.592 s against 0.226 / 0.332 s.
`readonly_class_escape`, `licm_readonly_class_field`, `pair_int_churn`,
`s2j_class_sroa`, `mandelbrot`, `nbody` and `binary_trees` compile to the
same archives as before this step.

**MakeArray frame scalarization (Q1 / COI-334).** Codegen escape answer
(the IL `escape_analysis` pass was removed 2026-10): non-escaping `[T; N]` /
`MakeArray` → consecutive frame slots (arity ≤ 32); named escape → **box
once** and reuse that heap identity (codegen). Immediate elems SROA;
computed zip/ADD elems stay heap Index/StoreIndex (S2i — same escape
answer, not a `vec_array` type refuse) so sibling zips do not share slots.
Zip operands that are literals or stack-array locals still load slots. Unproven
`xs[k]` on a codegen `[T; N]` local uses a runtime `0 <= k < N` check then
slot-select; the cold arm is heap `Index` / `StoreIndex` (S2h). Other
`MakeArray` values stay heap. Growing `ArrayPush` dest and
private uses after an escape stay heap; grow on `[T; N]` is a **type error**
(Q3). Indexing `i % N` is Euclidean into `0..N` (Q4). Named class locals
are a **separate** HIR-lowering path (S2j / Q2 box-once).
Deliberately refused here:

| Refused | Why |
|---------|-----|
| Arity > 32 | Hard cap |
| Growing `ArrayPush` dest / private use after escape | Slots have no object identity; Q3 grow on `[T; N]` is a type error |
| Unproven `xs[k]` as a raw slot | Slots have no OOB; S2h uses heap Index/StoreIndex or a proven/weaker bound |
| Named class locals (this path) | Class SROA lives in HIR lowering (COI-320 / Q2) |

**Copy / destination propagation.** The stack-IL `copy_prop` and `dest_prop`
passes were removed 2026-10 (measurement showed no bench effect).
Numeric MIR has its own DestProp (COI-282): trivial SSA φ forwarding after
InstCombine. Disagreeing φs stay; no IL slot rewrite. Hit:
`examples/perf/mir_destprop.hy`.
Numeric MIR IV strength reduction (COI-283) turns float `iv * invariant`
and `cast(i) * C` (integer-valued const `C`) into add induction. It is
float-only: an int add recurrence could trap on an overflow the program never
performs ([int-overflow.md](int-overflow.md)). Non-const float factors and
`i*i` stay. Hit: `examples/perf/mir_iv_sr.hy`.

**Local slot promotion.** The stack-IL `slot_promote` pass (alias / producer forwarding, store-destination coalescing, latch-copy elision) was removed 2026-10. HIR lowering reads one-word locals in place instead of copying them (range-loop ends, stack-array store values, staged operands, scalar `match` scrutinees), enum scalar replacement aliases read-only arm bindings to field locals, and `hir::fold` drops stores to locals nothing reads. Copies that remain (a class field initialized from a local, a boxed stack array stored into a field) run once per use and showed no measurable cost; dense MIR registers cover hot loops.

**Direct-call specialization (B4).** Ground trait/instance methods with a static entry emit `CALL` (not `CodePtr`+`CallIndirect`), including user traits, `Show`, `Length`, and `Hash`. PolyFn locals, dictionary `Index` targets, host/FFI/coroutines, and non-static evidence stay on `CallIndirect` / generic paths. Dictionaries remain for shared generic bodies (COI-78 — see [Call-site dispatch](https://github.com/ardax-corp/coil-website/blob/main/src/content/docs/references/types.md#call-site-dispatch)).

**Enum constructor escape elimination.** HIR scalar replacement (`escape_analysis`) keeps enums and tuples that stay frame-local in slots. Enums that escape remain heap-backed (COI-94); the stack-IL `stack_dce` peephole that dropped a discarded `MakeEnum` or a unary `MakeEnum` read straight back was removed 2026-10 (no bench effect). Niches, two-slot CALL/RETURN, and scalar `#[repr]` are other ABIs — they do not widen this ceiling.

**Option/Result representations (COI-92) and two-slot direct CALL/RETURN.** One table and the rules (niches, the two-slot calling convention, shared generic bodies, host packing, the debug check, the representation matrix): [layout.md](layout.md).

**Function tree-shake.** Before the MIR tiers and lower, `il::treeshake` drops function bodies not reachable from `main` (plus harness `test_cases` when `include_tests`) via `Entry` / entry-label `Jump` / absolute call-like `Byte`s. Eager primitive dict thunks (`Hash__*` / `Show__*` / …) that nothing calls are removed from the archive; debug symbols shrink with them. Indirect `CallIndirect` only keeps `CodePtr` targets discovered on already-live paths.

**Unused-slot dead store.** `hir::fold` drops a `let` or assignment of a literal or local read to a local nothing reads (the stack-IL `dead_store_at` went with `slot_promote`, 2026-10).

**Float-chain fusion.** `FloatChainStore` and `BinSlotSlotConstJmpf` are not emitted (mandelbrot-shaped debt). Handlers panic on archive major 4. General peepholes (`BinSlotSlot`, `CmpJmpf`/`*Jmpt`, `IndexPin*`) stay.

**Constant folding (`hir::fold`).** HIR folds operators on literals after inlining: `int` results over the full 64-bit range, except an op that would trap at run time (overflow, division by zero, a shift count outside `0..=63`, a negative exponent), which stays so it still traps; `float` `+ - * /` only when the result is finite and the divisor nonzero; compares of two literals. Identities `x + 0`, `x - 0`, `x * 1`, `x / 1`, `x | 0`, `x ^ 0`, shifts by 0, `x & -1` keep `x` when it has the result's type; `x * 0` / `x & 0` become `0` only for a plain read. Float `x * 1.0` and `x / 1.0` are folded; `x + 0.0`, `x - 0.0` and `x * 0.0` are not (signed zero / NaN). `!!b`, `!lit`, `-lit`, `true && b`, `false || b` and `if` on a literal fold. The stack-IL `algebraic` peephole pass was removed 2026-10 (bench parity with the HIR fold). No reassociation.

**Class temporary / named-local scalar replacement (COI-84 / S2j COI-320; Q2).** Direct `new Class(args).field` still elides the instance (except `fn drop()`). Unique named `let p = new C(...)` locals unbox into consecutive field slots when eligible (`named_local_class_field_read_unboxes_when_no_escape`, `named_local_class_field_store_unboxes_when_no_escape`). Identity uses (call, return, method receiver, aggregate, host/FFI, compare, alias) **box once** and reuse that heap instance (`named_class_box_once_identity`, `named_class_sroa_return_boxes_once`). The box is emitted at the statement depth of the local's `let`: a first escape inside a loop body or `if` arm hoists before the enclosing statement at that depth (boxing inside the loop re-boxed stale slots every pass; `tests/memory/escape_box_depth.hy`). Same rule for Q1 `[T; N]` boxes. Later private field ops go through the box. The old COI-84 / #134 “named locals stay allocated from `new`” refuse is superseded — see [language-quirks.md](language-quirks.md). Still heap-from-construction: `fn drop()`, parameters, arity 0 or > 32. Method `self` in the callee stays a heap parameter. Option/Result call/return representation: [layout.md](layout.md).

**GC finalizers (v1).** Inherent `impl Foo { fn drop() { … } }` runs when a mark cycle completes (and at VM teardown), not at last use. Safepoint mark (no write barrier) + lazy sweep is the mutator path ([gc-incremental.md](gc-incremental.md)); `gc::collect` still drains a full cycle. Order is unspecified. **Resurrection (COI-79, decided allow-once):** storing `self` during drop can keep the cell alive after the sweep; drop still runs at most once. That is defined, not a pin API (`gc::root` / `Weak`). **Enums (COI-26):** `impl E { fn drop() }` works on non-scalar enums. A drop enum is always a boxed `ObjEnum` (the layout keeps a drop hook off the two-slot return and frame-local unbox, and `TagEnumType` after `MakeEnum` blocks constructor elision; an `impl` block without `drop` changes nothing about an enum's representation); payload-variant allocations carry the finalizer type id, unit variants stay shared immortals and never drop. An explicit `e.drop()` on a unit variant just runs the body. Scalar-backed (`#[repr]`) enums reject `fn drop` (E0126). Moving GC stays deferred.

**IL cursor split (COI-81, decided: keep).** Locals and operands share one `Stack<Value>`. Two quantities answer different questions; they are not the same number after `STORE`.

| Quantity | Meaning | After `CONST; STORE 5` (height was 1) | Nested `CALL` / `MakeCoro` |
|----------|---------|---------------------------------------|----------------------------|
| `il::sp` (`stack_delta`) | per-op operand-height delta (eval-stack depth) | **0** (pop only) | `ret_words - arity` |

`STORE` (packed `n`) pops `n` then floors tell to `max(tell, max_written_slot + 1)` — height does not follow that floor. `Seek s` sets tell to `s` (and poisons `sp`). `CALL` sets the callee frame base to `tell - arity`; the matching return seeks to that base and pushes, so the caller-relative tell effect is `-arity + 1`. Unifying the two would either make `sp` lie about height (break the height model) or make `tell` ignore STORE floors (break the cursor model). Split asserted in `sp.rs` / `tell.rs` (`store_does_not_raise_operand_height`, `store_floor_is_not_operand_height`, `store_floor_persists_across_call`, `call_after_store_floor_is_relative_to_the_cursor`, `store_floor_persists_across_make_coro`, `symbolic_il_seek_sets_the_cursor`). `Call` / `MakeCoro` use `call_arity_delta` (`1 - arity`), not JumpIfMatch's `signed_arity_delta` (`arity - 1`).

The tell model (`il::tell`) and its differential test against the VM's cursor trace were removed in #1874: nothing in the compiler read the cursor model once the stack-IL optimizer was gone.

**Slot promotion** (`slot_promote_tell`, then `slot_promote`) was removed 2026-10 (measurement showed no bench effect).

**Counted-loop bounds proofs run on HIR.** Two HIR passes after inlining replace the stack-IL `loop_bounds` pass (removed 2026-10). `mir::licm` moves `a.len()` of an array, `Vec` or string out of a loop that cannot change a length: element and field writes are fine (`StoreIndex` overwrites in place), an append, a call that may resize (`purity::LengthStability` plus HIR effect summaries without `RESIZE`), a yield or an unknown call is not. `hir::bounds` flags `a[i]` as in bounds (`HirFlags::IN_BOUNDS`, lowered to `IndexUnchecked` / `StoreIndexUnchecked`) in `while i < B { … }` and `for i in lo..B`, where `B` is `a.len()`, a local from `let n = a.len()` before the loop, or the `n` of a preceding `while i < n { a.push(x); i = i + 1 }` fill from an empty `a`; `i` starts at a non-negative literal (or a small value) and only grows by a positive literal or by a small local the loop does not write (`k = k + p` with `p` in `0..len`, so the sum cannot overflow); and the site runs before anything in the iteration writes `i`. A store's proof is used only when its value expression has no call or write (the value is evaluated first). Unproven dynamic indices keep the checked opcodes: out-of-range read or write **panics** (archive major 4). Constant-index OOB stays a compile error. When some `fn drop()` may resize an array, no length is treated as steady (finalizers run at allocation). **Length/index sidecar facts** (`index_facts`) still mark `arr[i]` NodeIds on the AST (counted loops, callee guards, proven caller pairs, length-stable `for-in`); lowering consumes them as `IndexUnchecked` / `ArrayPin` / `IndexPinUnchecked`. The IL pass's in-loop `ArrayPin` / `IndexPin*` rewrite went with it: loop sites take plain `IndexUnchecked`, and MIR-dense bodies use `DenseIndex` with the shared pin table (COI-372). Bench parity was measured before the removal. Refused, fail closed:

| Refused | Why |
|---------|-----|
| A call that may resize, a host native or an unknown callee in the loop | The callee could hold another reference to the array and `push`/`pop` it |
| `yield` / `resume` in the loop | Lengths may change while suspended |
| An append anywhere in the loop | Length can change (`tests/control_flow/while_len_grows.hy`) |
| A rebound array or bound local | A different array each pass |
| `i <= n` headers | `i <= len` allows `i == len` (COI-85 / COI-98) |
| A site after a write to `i` in the iteration, or inside a lambda / `defer` | `i` may already be past the bound |
| A stride of unknown size | `i + s` could overflow past the test |

**The caller-side predicate peel only pays when it spills nothing.** (Design record: the peel was removed 2026-10 with the AST codegen; HIR lowering does not peel. The measurements below are kept for anyone reviving it.) When a callee opens with a pure guard over its parameters and returns an immediate or a parameter from that arm, codegen evaluates the guard at the call site so base cases skip the frame. Arguments that compile to a single pure byte (one slot load, one constant) are re-materialized in both the guard and the argument prep instead of being stored to a temp, which drops one `STORE` plus one spill `LOAD` per argument and leaves the guard reading the caller's own locals (peel-heavy loop: 4.28G → 3.29G instructions, 189ms → 152ms). Anything longer than a byte still takes a temp, because the guard copy and the call copy would each pay for it.

That byte budget is the whole profitability margin, and it is what rules out peeling a *self*-recursive call. A frame in this VM costs about two dispatches — `CALL`, then a fused `LoadReturnSlot` / `ConstReturnImm` that returns the base-case value — while the callee's guard is usually one fused `BinSlotSlotJmpf`. A peeled site has to re-emit that guard unfused (the operands are now caller expressions, not callee slots), add a `JMP` over the base arm, and store the join value, so it costs more than the frame it avoids *and* non-base calls pay the guard twice. Measured on `tak`, where 54% of the 63,609 calls hit the base case immediately: peeling all three inner self-calls grew the body from 13 to 41 words and cost +73.5% VM instructions and +31.4% wall time. Deliberately refused:

| Refused | Why |
|---------|-----|
| Self-recursive call sites | Callee span is not recorded until its body is compiled; reading the in-progress body works, but the peel loses to the frame (above) |
| A base-case value that is not returned | The peel replaces the callee's `return`; a value that falls through to the join is a different result |
| An argument longer than one pure byte, in the guard | Re-materializing it duplicates real work; it keeps its spill slot |
| Any argument with a side effect | The guard reads some arguments before the others are evaluated, and the false path evaluates them again |
| Instance methods, rest params, coroutines, un-monomorphized generics | The peel replicates the callee ABI, and `CallIndirect` receivers are not covered |
| A call site that is not saturated | Partial application lowers to `MakeFn`, not `CALL` |

**`*Jmpt` twins of `*Jmpf` (COI-87, implemented).** `CmpJmpt` / `BinSlotImmJmpt` / `BinSlotSlotJmpt` / `BinSlotSlotConstJmpt` / `LogNotJmpt` share packing with the false forms; fuse-select emits the `*Jmpt` twin for a jump-if-true. The IL rewrite `opt::cfg::invert_branch_over_jump` (`JMPF A; JMP B; A:` → `JMPT B`) was removed 2026-10. Loop headers stay `*Jmpf`. Archive minor 10.

## Test / CI reliability (high–medium)

| Issue | Detail | Linear |
|-------|--------|--------|
| Stack-margin heap corruption (fixed) | Root-caused two *sibling* giant recursive-descent functions with oversized inline `match` arms: `Checker::infer_inner` (~3400 lines, ~102 arms — `Call` alone ~1000 lines) and `Compiler::do_compile` (~4400 lines — `Call` ~1020 lines, `Match` ~590 lines). Rust sizes a function's stack frame for the union of every arm's locals, so *every* recursive call paid for the biggest arm even on trivial input; a large-frame overflow can jump the guard page into adjacent memory instead of faulting cleanly, which is what surfaced as `corrupted double-linked list` / SIGSEGV / stack-smashing on random *unrelated* tests (confirmed via core dumps: crashing thread names were arbitrary, e.g. `examples/typecl`, `fallthrough_boo`). Fixed by extracting all oversized arms into their own `#[inline(never)]` methods in both functions, plus a `catch_unwind`-based recursion-depth guard (`infer_depth` / `codegen_depth`, `ErrorCode::ExpressionNestingTooDeep`) in each as defense-in-depth against any future/adversarial deep nesting. Cut `attr_on_async_fn_rejected_at_compile_time`'s minimum stack requirement from ~2 MiB to ~1 MiB (bisected); `RUST_MIN_STACK` / `run_example`'s thread stack are back down to 8 MiB (`ulimit -s` default) instead of the 32 MiB stopgap. | — |
| Parallel thread-churn crash / hang (fixed) | After the stack-margin fix above, parallel `cargo test -p compiler --test pipeline` still SIGSEGV'd or hung ~1/4–1/3 of the time. The crashing thread was always a `thread::spawn` example (`thread_join.hy`, `thread_mutex.hy`, `thread_channel.hy`, `thread_reply.hy`); a core dump pointed at `atomic_compare_exchange` in Rust's per-thread altstack `make_handler` under high churn — `run_example` spawned a named 8 MiB OS thread per example *and* each coil `thread::spawn` starts a reactor worker pool, so 40–70+ threads could be alive at once. Fixed by routing `run_example` through a reused 2-worker pool (8 MiB stacks) so peak example-runner threads stay bounded and the spawn/join `make_handler` churn is gone. `Reactor::shutdown()` (sibling row) still joins reactor workers at the end of each `Machine::run_with_pool`. Parallel pipeline runs no longer need `--test-threads=1`. Follow-up: `WorkerCap` defaults to one reactor OS worker when `CI` is set, and `.cargo/config.toml` sets `COIL_MAX_WORKER_THREADS=1` for local cargo tests (cuts residual macOS cargo `--list` EAGAIN pressure from per-VM pools). | COI-88 |
| Package attach handshake across Coil threads (fixed) | Coil↔Coil `Stream.attach` on a `thread::spawn` job and on the root both timed out under `COIL_MAX_WORKER_THREADS=1`. Handshake waits park on `reactor_wait_fd_no_help` (`Stream.park`) so a mid-handshake wait cannot nest-steal the peer job onto the same stack. Userland `Stream.attach` + `Stream.park` pumps one native step per `read` / `write` until Ready. Package `err_out` is a tagged name (`tls.h`), not an `i32`; WouldBlock stays a parked session, not a freed hard error. The VM has no TLS stream kind and does not call `coil_tls_*`. Close / GC send shutdown when the fd is still usable, then Drop frees. See [io-reactor.md](io-reactor.md). | COI-116 |
| Reactor immortal-thread leak (fixed) | `Reactor::shutdown` existed but was never called, so every `thread::spawn`'d coil program leaked `worker_cap` OS threads for the rest of the process — they hold their own `Arc<Reactor>` clone and poll forever, compounding thread counts across a `cargo test` run (a contributing factor to the crash above, though not the whole story). Fixed by calling `Reactor::shutdown()` (joins pool threads) at the end of `Machine::run_with_pool` once the root's `live_threads` registry drains. | — |
| Criterion vs `--all-targets` | Prefer `cargo test --workspace --lib --tests --bins` (CI and agent gate). `--all-targets` also builds `[[bench]]` targets; Criterion treats argv after `--` as its own CLI and aborts the suite. | — |
| Optional feature suites (fixed) | `cargo test --workspace --lib --tests --bins --no-default-features` is a real, non-hanging suite. Default features are `[]`, and workspace unification already turns on `dissect` / `debugger` / `coverage`, so it builds the same code as the default suite (CI runs that one). Time is userland [coil-time](https://github.com/ardax-corp/coil-time); TLS is [coil-tls](https://github.com/ardax-corp/coil-tls); crypto is [coil-crypto](https://github.com/ardax-corp/coil-crypto). | COI-90 |
| `ulimit` leak-smoke wraps `cargo run` (fixed) | Advertised leak-smoke is now `cargo build --bin coil && (ulimit -v 65536; ./target/debug/coil test)`; CI's default Linux job runs that after the debug build. Wrapping `cargo run` under `ulimit -v` OOMs on cargo's own machinery, not `coil test` — the built binary already passed under the same 64MB cap. | COI-89 |

## Tracking

Open items are Linear issues in the Known limitations project; investigation issues write the expected model first. Already tracked elsewhere: COI-26, COI-71.

COI-199 and COI-201 closed: sequential RSS is mapped `.text`, not `Heap::alloc`. TLS and crypto are no longer linked into this binary (userland [coil-tls](https://github.com/ardax-corp/coil-tls) / [coil-crypto](https://github.com/ardax-corp/coil-crypto)). Calendar/format time is userland [coil-time](https://github.com/ardax-corp/coil-time); process clocks are virtual `clock` HostInvoke **121–123** (`clock_wall_nanos` / `clock_mono_nanos` / `clock_sleep_ms`). Archive **major 4 / minor 5** appends M1 `prelude::math` **125–135**; `PI`/`E`/`TAU` are coil-stdlib `num`.

Most items have no inline `TODO`/`FIXME` — knowledge lives here and in [.cursor/skills/coil-contributor/reference.md](../../.cursor/skills/coil-contributor/reference.md). Update this file when closing a limitation.
